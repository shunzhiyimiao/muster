//! WebSocket 网关(P3-03):按频道扇出。
//!
//! v0 是**进程内广播**(tokio broadcast),不是分布式的:多实例部署时各实例
//! 只能推到连在自己身上的客户端。计划里的 NATS 就是补这个的,当前不上。
//!
//! 鉴权在**握手时**做一次:令牌走 query 参数(浏览器 WebSocket API 不允许
//! 自定义 header)。代价是令牌会进服务端访问日志——上线前必须换成
//! 一次性 ticket,已登记。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::auth::Identity;
use crate::message::MessageOut;
use crate::Db;

const CHANNEL_CAP: usize = 256;

/// 推给客户端的实时事件。加 `type` 标签,前端一个 switch 就能分派。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Push {
    Message(MessageOut),
    /// 任务过程事件转发(P3-06):Runner 在开发者机器上跑,过程经服务端回频道。
    TaskDelta { channel_id: String, run_id: String, text: String },
    /// 会议纪要行(转写落库后推送)。
    Transcript { meeting_id: String, speaker_id: String, text: String, ts_ms: i64 },
    /// 在场状态(P3-07 的最小形态:只有进出,没有 TTL 聚合)。
    Presence { channel_id: String, account_id: String, online: bool },
}

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    fn sender(&self, channel_id: &str) -> broadcast::Sender<String> {
        let mut m = self.inner.lock().unwrap();
        m.entry(channel_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAP).0)
            .clone()
    }

    pub fn subscribe(&self, channel_id: &str) -> broadcast::Receiver<String> {
        self.sender(channel_id).subscribe()
    }

    pub fn broadcast(&self, channel_id: &str, msg: &MessageOut) {
        self.push(channel_id, &Push::Message(msg.clone()));
    }

    /// 推一条事件。**没有订阅者时静默丢弃**——这是广播不是队列;
    /// 需要"一条都不能少"的消费者应当走补拉(按 channel_seq),不是靠这里。
    pub fn push(&self, channel_id: &str, ev: &Push) {
        if let Ok(json) = serde_json::to_string(ev) {
            let _ = self.sender(channel_id).send(json);
        }
    }
}

#[derive(Deserialize)]
pub struct WsQuery {
    pub token: String,
    pub channel: String,
}

pub async fn handler(
    ws: WebSocketUpgrade,
    State((db, hub)): State<(Db, Hub)>,
    Query(q): Query<WsQuery>,
) -> Response {
    // 握手即鉴权:令牌不对直接拒绝升级,不给一个"连上了但收不到东西"的假象
    match Identity::from_token(&q.token) {
        Ok(id) => {
            let hub2 = hub.clone();
            let chan = q.channel.clone();
            ws.on_upgrade(move |socket| pump(socket, hub2, chan, id.account_id, db))
        }
        Err(e) => e.into_response_owned(),
    }
}

impl crate::ServerError {
    fn into_response_owned(self) -> Response {
        axum::response::IntoResponse::into_response(self)
    }
}

async fn pump(socket: WebSocket, hub: Hub, channel: String, account_id: String, _db: Db) {
    let mut rx = hub.subscribe(&channel);
    let (mut tx, mut incoming) = socket.split();

    hub.push(&channel, &Push::Presence { channel_id: channel.clone(), account_id: account_id.clone(), online: true });

    let send = async {
        while let Ok(json) = rx.recv().await {
            if tx.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    };
    // 客户端方向只用来感知断开(v0 不接受上行指令:上行走 HTTP,
    // 免得同一个动作有两条路径、两套鉴权)
    let recv = async {
        while let Some(Ok(m)) = incoming.next().await {
            if matches!(m, Message::Close(_)) {
                break;
            }
        }
    };
    tokio::select! { _ = send => {}, _ = recv => {} }

    hub.push(&channel, &Push::Presence { channel_id: channel.clone(), account_id, online: false });
}
