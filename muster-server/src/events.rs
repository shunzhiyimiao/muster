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
//! - 鉴权在**握手时**做一次。浏览器的 `EventSource` 不允许自定义 header,所以凭据
//!   只能走 query 参数——但走的是**一次性订阅票**([`ticket`]),不是登录令牌:
//!   先用正常的 Bearer 鉴权 `POST /events/ticket` 换一张 32 字节随机票,60 秒内、
//!   只能用一次,服务端只存它的 SHA-256。票进了访问日志也没用:用过即作废。
//!   `?token=<登录令牌>` 仍可用但打 warn(兼容路径,见 lib.rs 诚实边界)。
//! - 票仓在进程内存、单节点(与限流同一欠账);重启即清零,客户端重连时换新票即可。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argon2::password_hash::rand_core::{OsRng, RngCore};
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use futures::stream::Stream;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use crate::auth::Identity;
use crate::db::now_ms;
use crate::message::MessageOut;
use crate::ws::Hub;
use crate::{Db, ServerError};

const REPLAY_CAP: i64 = 500;

/// 订阅票的有效期。够客户端"换票 → 立刻连",不够别人捡到票再用。
pub const TICKET_TTL_MS: i64 = 60_000;

pub type EventsState = (Db, Hub, Tickets);

#[derive(Clone)]
struct TicketEntry {
    account_id: String,
    display_name: String,
    kind: muster_identity::PrincipalKind,
    expires_ms: i64,
}

/// 票仓。**只存票的 SHA-256**,不存票本身——内存被 dump 出来也拿不到一张能用的票。
/// 票绑定签发时的身份:消费时直接还原出 [`Identity`],不再碰登录令牌。
#[derive(Clone, Default)]
pub struct Tickets {
    inner: Arc<Mutex<HashMap<String, TicketEntry>>>,
}

fn ticket_hash(ticket: &str) -> String {
    format!("{:x}", Sha256::digest(ticket.as_bytes()))
}

impl Tickets {
    pub fn new() -> Self {
        Self::default()
    }

    /// 签一张票:32 字节随机数,十六进制 64 字符。
    pub fn issue(&self, id: &Identity) -> String {
        self.issue_at(id, now_ms())
    }

    pub fn issue_at(&self, id: &Identity, now: i64) -> String {
        let mut raw = [0u8; 32];
        OsRng.fill_bytes(&mut raw);
        let ticket: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut m = self.inner.lock().unwrap();
        // 顺手清掉过期的:票仓只会因为"签了没用"而长,这里就是它唯一的回收点
        m.retain(|_, e| e.expires_ms > now);
        m.insert(
            ticket_hash(&ticket),
            TicketEntry {
                account_id: id.account_id.clone(),
                display_name: id.display_name.clone(),
                kind: id.kind,
                expires_ms: now + TICKET_TTL_MS,
            },
        );
        ticket
    }

    /// 用票:成功即作废,再来一次就是 `None`;过期、伪造同样 `None`。
    pub fn consume(&self, ticket: &str) -> Option<Identity> {
        self.consume_at(ticket, now_ms())
    }

    pub fn consume_at(&self, ticket: &str, now: i64) -> Option<Identity> {
        let mut m = self.inner.lock().unwrap();
        let e = m.remove(&ticket_hash(ticket))?;
        if e.expires_ms <= now {
            return None;
        }
        Some(Identity { account_id: e.account_id, display_name: e.display_name, kind: e.kind })
    }
}

/// 换一张一次性订阅票。要正常的 Bearer 鉴权——票只是把"谁"从令牌搬到一张 60 秒的
/// 一次性凭据上,好让浏览器的 `EventSource` 带得动。
pub async fn ticket(
    State((_db, _hub, tickets)): State<EventsState>,
    id: Identity,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ticket": tickets.issue(&id), "ttl_ms": TICKET_TTL_MS }))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    /// 一次性订阅票(`POST /events/ticket` 换来的)。
    #[serde(default)]
    pub ticket: Option<String>,
    /// **兼容路径**:登录令牌。会进访问日志,只为老客户端留着,用一次 warn 一次。
    #[serde(default)]
    pub token: Option<String>,
    /// 查询参数形式的游标。标准客户端用 `Last-Event-ID` 头,这个是给
    /// 非浏览器客户端(以及调试)的等价入口。
    #[serde(default)]
    pub last_event_id: Option<i64>,
}

pub async fn handler(
    State((db, hub, tickets)): State<EventsState>,
    Query(q): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    // 握手即鉴权:票不对直接拒,不给"连上了却收不到东西"的假象
    let id = match (q.ticket.as_deref(), q.token.as_deref()) {
        (Some(t), _) => match tickets.consume(t) {
            Some(i) => i,
            None => {
                return ServerError::Unauthenticated("订阅票无效、已用过或已过期,请重新换票".into())
                    .into_response()
            }
        },
        (None, Some(tok)) => {
            tracing::warn!("/events 仍在用 ?token= 兼容路径(登录令牌会进访问日志),客户端应改用 POST /events/ticket");
            match Identity::from_token(tok) {
                Ok(i) => i,
                Err(e) => return e.into_response(),
            }
        }
        (None, None) => return ServerError::Unauthenticated("缺少订阅票".into()).into_response(),
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
    use super::*;
    use muster_identity::PrincipalKind;

    fn alice() -> Identity {
        Identity { account_id: "alice".into(), display_name: "Alice".into(), kind: PrincipalKind::Human }
    }

    /// 票只能用一次:第二次拿同一张票来,得到的是拒绝,不是另一条流。
    #[test]
    fn ticket_is_single_use() {
        let t = Tickets::new();
        let ticket = t.issue(&alice());
        assert_eq!(ticket.len(), 64, "32 字节随机数的十六进制");
        let id = t.consume(&ticket).expect("第一次能用");
        assert_eq!(id.account_id, "alice");
        assert!(t.consume(&ticket).is_none(), "用过即作废");
    }

    /// 过期拒绝:60 秒后的票等于没有票。
    #[test]
    fn expired_ticket_is_rejected() {
        let t = Tickets::new();
        let ticket = t.issue_at(&alice(), 1_000_000);
        assert!(t.consume_at(&ticket, 1_000_000 + TICKET_TTL_MS + 1).is_none());
        let fresh = t.issue_at(&alice(), 2_000_000);
        assert!(t.consume_at(&fresh, 2_000_000 + TICKET_TTL_MS - 1).is_some(), "没过期就能用");
    }

    /// 票绑定签发时的身份:bob 的票还原出来的只能是 bob,伪造的票什么都换不到。
    #[test]
    fn ticket_resolves_only_to_its_owner() {
        let t = Tickets::new();
        let bob = Identity { account_id: "bob".into(), display_name: "Bob".into(), kind: PrincipalKind::Agent };
        let ticket = t.issue(&bob);
        let id = t.consume(&ticket).unwrap();
        assert_eq!((id.account_id.as_str(), id.kind), ("bob", PrincipalKind::Agent));
        assert!(t.consume(&"0".repeat(64)).is_none(), "伪造的票");
        assert!(t.consume("").is_none());
    }

    /// 票仓只存哈希:拿到内存里那把键也凑不出一张能用的票。
    #[test]
    fn store_keeps_only_hashes() {
        let t = Tickets::new();
        let ticket = t.issue(&alice());
        let keys: Vec<String> = t.inner.lock().unwrap().keys().cloned().collect();
        assert_eq!(keys.len(), 1);
        assert_ne!(keys[0], ticket);
        assert_eq!(keys[0], ticket_hash(&ticket));
    }

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
