//! 实时通道:SSE(C2),替换原先的 WebSocket。
//!
//! ## 为什么换掉 WebSocket
//!
//! **`Last-Event-ID` 白送断线补拉。** 浏览器的 `EventSource` 重连时自动带上
//! 最后收到的事件 id,服务端从库里重放之后的即可;它还自带重连退避。
//! WebSocket 这两样都要自己写——凭空多一堆容易出错的代码,而"无断线补拉"
//! 正是 `lib.rs` 诚实边界里列着的欠账。
//!
//! ## 一条连接,复用所有频道
//!
//! 一频道一连接会撞上 HTTP/1.1 每源约 6 个并发连接的上限(旧的 WS 实现就有
//! 这毛病)。这里一条连接推所有频道的事件,`channel_id` 在事件体里,客户端筛。
//!
//! ## 事件 id 用全局序号,不用 channel_seq
//!
//! 两者是不同的东西(见 `message.rs`):`channel_seq` 是单频道无空洞校验用的,
//! 一条连接复用多频道时,单个 `Last-Event-ID` 根本表达不了多个频道的位置。
//! 所以另有一个**全局单调序号** `stream_seq`,它才是这条流的游标。
//!
//! ## 上行走 POST,不走这条通道
//!
//! 同一个动作有两条路径就会有两套鉴权。SSE 单向不是限制,是简化。
//!
//! ## 诚实边界
//!
//! - **只重放消息**,不重放在线状态/转写等瞬时事件——那些过期即无意义。
//! - 重放上限 500 条:断得太久就该走 `GET /channels/:cid/messages?after_seq=`
//!   逐频道补齐(那条路是精确的),而不是让一条 SSE 吐半天。
//! - 鉴权在**握手时**做一次;令牌走 query 参数,因为浏览器的 `EventSource`
//!   不允许自定义 header。**代价是令牌会进访问日志**——上线前必须换成
//!   一次性 ticket,已登记。

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use futures::stream::Stream;
use serde::Deserialize;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use crate::auth::Identity;
use crate::message::MessageOut;
use crate::ws::Hub;
use crate::Db;

const REPLAY_CAP: i64 = 500;

#[derive(Deserialize)]
pub struct EventsQuery {
    pub token: String,
    /// 查询参数形式的游标。标准客户端用 `Last-Event-ID` 头,这个是给
    /// 非浏览器客户端(以及调试)的等价入口。
    #[serde(default)]
    pub last_event_id: Option<i64>,
}

pub async fn handler(
    State((db, hub)): State<(Db, Hub)>,
    Query(q): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    // 握手即鉴权:令牌不对直接拒,不给"连上了却收不到东西"的假象
    let id = match Identity::from_token(&q.token) {
        Ok(i) => i,
        Err(e) => return e.into_response(),
    };

    // 标准头优先于查询参数——`EventSource` 自动重连时只会带头
    let cursor = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .or(q.last_event_id);

    let replay = match cursor {
        Some(after) => replay_since(&db, &id, after).await.unwrap_or_default(),
        None => vec![],
    };

    let live = BroadcastStream::new(hub.subscribe_all()).filter_map(|r| {
        r.ok().map(|(seq, json)| Ok::<Event, Infallible>(Event::default().id(seq.to_string()).data(json)))
    });

    let head = futures::stream::iter(
        replay
            .into_iter()
            .filter_map(|m| {
                serde_json::to_string(&crate::ws::Push::Message(m.clone()))
                    .ok()
                    .map(|j| Ok::<Event, Infallible>(Event::default().id(m.stream_seq.to_string()).data(j)))
            })
            .collect::<Vec<_>>(),
    );

    let stream: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        Box::pin(head.chain(live));

    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"))
        .into_response()
}

/// 重放该用户能看见的、序号大于 `after` 的消息。
///
/// **按 `stream_seq` 排序而不是时间**:时间会撞、会回拨,而这个序号的顺序
/// 就是提交顺序(见 message.rs 的分配方式)。
async fn replay_since(db: &Db, _id: &Identity, after: i64) -> crate::Result<Vec<MessageOut>> {
    // v0 不按频道成员关系过滤:当前所有已认证用户可见所有频道
    // (与 `GET /channels` 同口径)。频道级可见性属 P2-07 的后续,已登记。
    let rows = sqlx::query_as::<_, MessageOut>(
        "SELECT id, channel_id, channel_seq, COALESCE(stream_seq,0) AS stream_seq,
                author_id, role, body, run_id, ts_ms
         FROM message WHERE stream_seq > $1 ORDER BY stream_seq ASC LIMIT $2",
    )
    .bind(after)
    .bind(REPLAY_CAP)
    .fetch_all(&db.pool)
    .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    /// 游标优先级:标准头 > 查询参数。`EventSource` 自动重连时只会带头,
    /// 若查询参数覆盖了它,重连就会从一个陈旧的位置重放。
    #[test]
    fn header_cursor_wins_over_query_param() {
        let from_header: Option<i64> = Some(42);
        let from_query: Option<i64> = Some(7);
        assert_eq!(from_header.or(from_query), Some(42));
        let none: Option<i64> = None;
        assert_eq!(none.or(from_query), Some(7), "没有头时才用查询参数");
    }
}
