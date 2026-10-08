//! 实时事件枢纽:按频道扇出的进程内广播([`Hub`])与推送事件类型([`Push`])。
//!
//! 曾经的 WebSocket 网关(P3-03)已在 C2 迁到 SSE(见 `events.rs`),`/ws` 路由
//! 与握手鉴权随之删除(2026-09-23)。模块名沿用 `ws` 是为了不动六处引用,
//! 内容只剩枢纽本身。
//!
//! v0 是**进程内广播**(tokio broadcast),不是分布式的:多实例部署时各实例
//! 只能推到连在自己身上的客户端。计划里的 NATS 就是补这个的,当前不上。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::broadcast;

use crate::message::MessageOut;

const CHANNEL_CAP: usize = 256;

/// 推给客户端的实时事件。加 `type` 标签,前端一个 switch 就能分派。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Push {
    Message(MessageOut),
    /// 任务过程事件转发(P3-06):Runner 在开发者机器上跑,过程经服务端回频道。
    TaskDelta { channel_id: String, run_id: String, text: String },
    /// 会议纪要行(转写落库后推送)。`lang` 是这行的语言;翻译行带 `origin_id`
    /// 指回原文行——字幕按自己选的语言筛,纪要面板把翻译挂在原文下面。
    Transcript {
        meeting_id: String,
        speaker_id: String,
        text: String,
        ts_ms: i64,
        #[serde(default)]
        id: i64,
        #[serde(default)]
        lang: String,
        #[serde(default)]
        origin_id: Option<i64>,
    },
    /// 在场状态(P3-07 的最小形态:只有进出,没有 TTL 聚合)。
    Presence { channel_id: String, account_id: String, online: bool },
    /// 行动项有了新状态(被提出 / 被裁决)。
    ///
    /// 提案不广播的话,**会上说完那句话,界面上什么都不会发生**——
    /// 人会以为 Agent 没听见,于是再说一遍,于是多出一条重复的待批项。
    ActionItem(crate::action::ActionItemOut),
    /// 会议本身的状态变化:`started` / `ended` / `agent`(请了 Agent 或请它离开)/
    /// `level`(密级抬升)/ `summary`(总结落库)/ `captions`(有人改了字幕语言,Agent 据此调整翻译目标)。大厅与侧栏徽标据此重拉,正在会里的人据此知道"会被结束了",
    /// agent-daemon 据此**立刻**认领而不是等下一轮轮询。
    Meeting {
        meeting_id: String,
        channel_id: String,
        event: String,
        title: String,
        level: String,
        wants_agent: bool,
    },
    /// 录制状态:`started` / `stopping` / `ready` / `failed`。**所有在场的人都要看见**——
    /// 被录而不知情,是会议产品最不能犯的错。
    Recording {
        meeting_id: String,
        channel_id: String,
        recording_id: String,
        state: String,
        by: String,
    },
}

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
    /// 全局流(C2 SSE):一条连接复用所有频道,所以还需要一个不分频道的通道。
    /// 载荷是 `(stream_seq, json)`——序号即 SSE 的事件 id。
    /// 非消息类事件(在线状态、转写)序号填 0:它们不参与重放,过期即无意义。
    all: Arc<Mutex<Option<broadcast::Sender<(i64, String)>>>>,
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

    fn all_sender(&self) -> broadcast::Sender<(i64, String)> {
        let mut g = self.all.lock().unwrap();
        g.get_or_insert_with(|| broadcast::channel(CHANNEL_CAP).0).clone()
    }

    /// 订阅全局流(SSE 用)。
    pub fn subscribe_all(&self) -> broadcast::Receiver<(i64, String)> {
        self.all_sender().subscribe()
    }

    pub fn broadcast(&self, channel_id: &str, msg: &MessageOut) {
        let ev = Push::Message(msg.clone());
        if let Ok(json) = serde_json::to_string(&ev) {
            let _ = self.sender(channel_id).send(json.clone());
            // 全局流带上序号:SSE 的事件 id 就是它,断线重连据此重放
            let _ = self.all_sender().send((msg.stream_seq, json));
        }
    }

    /// 推一条事件。**没有订阅者时静默丢弃**——这是广播不是队列;
    /// 需要"一条都不能少"的消费者应当走补拉(按 channel_seq),不是靠这里。
    pub fn push(&self, channel_id: &str, ev: &Push) {
        if let Ok(json) = serde_json::to_string(ev) {
            let _ = self.sender(channel_id).send(json.clone());
            // 瞬时事件序号填 0:它们不参与重放(过期即无意义),
            // 但仍要推给在线的人
            let _ = self.all_sender().send((0, json));
        }
    }
}
