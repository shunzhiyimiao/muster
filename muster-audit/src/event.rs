//! 事件信封与 payload 契约。
//!
//! 每个 [`EventBody`] 变体的字段列表**就是**该事件类型的 payload 契约;
//! 文档注释说明每个字段服务于哪个消费者(演示第几幕 / Capsule 锻造 / 审批追溯)。

use muster_provider::Locality;
use muster_route::{DowngradeReason, LabelSource, Sensitivity};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 当前 payload 契约版本。读侧遇到更高版本仍应尽量解析(向后兼容字段只增不改)。
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------- 行为主体

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// 人类成员(actor_id = 用户 id)。
    Human,
    /// Agent,actor_id = 工牌号(如 `A-007`)。
    Agent,
    /// 系统组件(路由中心、调度器…),actor_id = 组件名。
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    pub id: String,
}

impl Actor {
    pub fn human(id: impl Into<String>) -> Self {
        Self { kind: ActorKind::Human, id: id.into() }
    }
    pub fn agent(badge: impl Into<String>) -> Self {
        Self { kind: ActorKind::Agent, id: badge.into() }
    }
    pub fn system(name: impl Into<String>) -> Self {
        Self { kind: ActorKind::System, id: name.into() }
    }
}

/// 事件发生的组织作用域(均可缺省:个人空间事件 team/channel 皆 None)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub team: Option<String>,
    pub channel: Option<String>,
}

// ---------------------------------------------------------------- 基础值类型

/// 内容寻址引用:审计表**永不存正文**,只存哈希;正文留在 run 存储侧带自己的密级。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHash(pub String);

impl ContentHash {
    pub fn sha256(bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        Self(format!("sha256:{:x}", Sha256::digest(bytes)))
    }
}

/// 外发字节记账。**没有第三种取值**:测不到就是 `Unmetered`,
/// 而 `Unmetered` 在演习报告里按违规计——fail-closed,与 E2 同一哲学。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressBytes {
    Measured(u64),
    Unmetered,
}

/// 模型引用(Capsule 重放的一部分:同 provider + 同模型 + 同参数哈希)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider_id: String,
    pub model: String,
    /// 采样参数等的规范化哈希(温度、top_p、系统提示词版本…)。
    pub params_hash: ContentHash,
}

/// **Capsule-ready 硬约束的类型化形态**:重放 `run` 所需的一切,要么在此,
/// 要么经 [`ContentHash`] 内容寻址。全部字段非 Option——写不出不合规的 run.start。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayRefs {
    /// 仓库快照(如 git tree hash)。
    pub repo_snapshot: ContentHash,
    /// 依赖锁定文件哈希(Cargo.lock / package-lock…)。
    pub deps_lock: ContentHash,
    pub model: ModelRef,
    /// 工具执行环境描述的哈希(允许的命令、挂载、环境变量白名单…)。
    pub tool_env: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Failed { class: String },
    Cancelled,
}

// ---------------------------------------------------------------- 事件本体

/// 写侧穷举的事件类型。`event_type` 采用点分命名空间,内部标签序列化
/// 使 payload JSON 自描述(含 `event_type` 字段,便于纯 SQL 消费)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum EventBody {
    /// 任务开始。`replay` 即 Capsule-ready 约束;`label`/`locality_planned`
    /// 供第 7 幕徽章与后续对账。
    #[serde(rename = "run.start")]
    RunStart {
        task_kind: String,
        replay: ReplayRefs,
        label: Sensitivity,
        locality_planned: Locality,
    },

    /// 任务结束。`output_hash` 指向 run 存储里的产物,审计侧不落正文。
    #[serde(rename = "run.finish")]
    RunFinish {
        outcome: RunOutcome,
        duration_ms: u64,
        output_hash: Option<ContentHash>,
    },

    /// 一次模型调用——**外发记账的唯一来源**(A2 provider 是全系统网络咽喉,
    /// 在传输层计量,不做日志解析)。第 8 幕「外发字节数」由本类型 SUM 得出。
    #[serde(rename = "model.call")]
    ModelCall {
        provider_id: String,
        model: String,
        locality: Locality,
        label: Sensitivity,
        tokens_in: Option<u64>,
        tokens_out: Option<u64>,
        bytes_in: u64,
        bytes_out: EgressBytes,
        latency_ms: u64,
        request_hash: ContentHash,
    },

    /// 路由决策——记**依据**而不只记结果:第 7 幕徽章悬浮文案 =
    /// `downgrade.map(DowngradeReason::text_zh)`;`deciders` 回答
    /// 「为什么是这个密级」。
    #[serde(rename = "route.decide")]
    RouteDecide {
        effective_label: Sensitivity,
        /// 促成有效密级的来源清单(E1 `effective_sensitivity` 的第二返回值)。
        deciders: Vec<LabelSource>,
        policy_version: String,
        locality: Locality,
        provider_id: String,
        /// 类型上只可能是本地 provider(E2 结构性保证),此处仅存 id 供追溯。
        fallbacks: Vec<String>,
        downgrade: Option<DowngradeReason>,
    },

    /// Agent 越权申请。记「申请能力 vs 工牌能力」的差值依据,
    /// 正文(完整命令)经哈希引用。
    #[serde(rename = "approval.request")]
    ApprovalRequest {
        approval_id: String,
        requested_capability: String,
        badge_capabilities_hash: ContentHash,
        command_hash: ContentHash,
        reason: String,
    },

    /// 审批裁决。envelope 的 actor 即裁决人;批准与拒绝**都**写审计。
    #[serde(rename = "approval.decision")]
    ApprovalDecision {
        approval_id: String,
        granted: bool,
        note_hash: Option<ContentHash>,
    },

    /// 工牌能力变更(actor = 被变更的 Agent;操作人进 payload)。
    #[serde(rename = "badge.update")]
    BadgeUpdate {
        changed_by: Actor,
        capabilities_hash: ContentHash,
        badge_version: u32,
    },

    /// 组织策略变更。envelope 的 policy_version 填**新**版本。
    #[serde(rename = "policy.update")]
    PolicyUpdate {
        changed_by: Actor,
        diff_hash: ContentHash,
    },

    /// E3 棘轮抬升:会话触碰更高密级资源的**时刻**(UI 置灰与徽章解释的
    /// 证据点——只看 route.decide 只能在下一次决策时发现,这里记的是污染
    /// 发生的瞬间)。cause 是资源标识,不是正文。
    #[serde(rename = "session.lock.raise")]
    SessionLockRaise {
        from_level: Sensitivity,
        to_level: Sensitivity,
        cause: LabelSource,
        turn: u64,
    },

    /// 主权演习开始/结束。演习报告以 SQL 为准(见 queries::drill_report),
    /// `drill.end` 可冗余存一份快照供 UI 直读,但不是 source of truth。
    #[serde(rename = "drill.start")]
    DrillStart { drill_id: String },
    #[serde(rename = "drill.end")]
    DrillEnd {
        drill_id: String,
        egress_bytes_snapshot: u64,
        unmetered_calls_snapshot: u64,
    },
}

impl EventBody {
    /// 点分事件类型名(与序列化 tag 一致;冗余为独立列以便索引)。
    pub fn event_type(&self) -> &'static str {
        match self {
            EventBody::RunStart { .. } => "run.start",
            EventBody::RunFinish { .. } => "run.finish",
            EventBody::ModelCall { .. } => "model.call",
            EventBody::RouteDecide { .. } => "route.decide",
            EventBody::ApprovalRequest { .. } => "approval.request",
            EventBody::ApprovalDecision { .. } => "approval.decision",
            EventBody::BadgeUpdate { .. } => "badge.update",
            EventBody::PolicyUpdate { .. } => "policy.update",
            EventBody::SessionLockRaise { .. } => "session.lock.raise",
            EventBody::DrillStart { .. } => "drill.start",
            EventBody::DrillEnd { .. } => "drill.end",
        }
    }
}

/// 读侧解析结果:未知 `event_type` 落入 [`Parsed::Unknown`] 而不是报错——
/// 旧版本读新库不崩(设计原则 5)。
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Known(EventBody),
    Unknown { event_type: String, payload: Value },
}

pub fn parse_payload(payload: &Value) -> Parsed {
    match serde_json::from_value::<EventBody>(payload.clone()) {
        Ok(body) => Parsed::Known(body),
        Err(_) => Parsed::Unknown {
            event_type: payload
                .get("event_type")
                .and_then(Value::as_str)
                .unwrap_or("<missing>")
                .to_string(),
            payload: payload.clone(),
        },
    }
}

// ---------------------------------------------------------------- 信封

/// 待写入事件(id/hash 由 store 生成)。
#[derive(Debug, Clone)]
pub struct NewEvent {
    /// 毫秒时间戳;`None` 取系统时钟(测试注入确定值)。
    pub ts_ms: Option<u64>,
    pub actor: Actor,
    pub scope: Scope,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    /// 决策时生效的策略版本(信封冗余,便于索引;详情在各 payload)。
    pub policy_version: Option<String>,
    pub label: Option<Sensitivity>,
    pub locality: Option<Locality>,
    pub body: EventBody,
}

impl NewEvent {
    pub fn new(actor: Actor, body: EventBody) -> Self {
        Self {
            ts_ms: None,
            actor,
            scope: Scope::default(),
            run_id: None,
            session_id: None,
            policy_version: None,
            label: None,
            locality: None,
            body,
        }
    }
    pub fn at(mut self, ts_ms: u64) -> Self {
        self.ts_ms = Some(ts_ms);
        self
    }
    pub fn run(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }
    pub fn session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
    pub fn scope(mut self, team: impl Into<String>, channel: Option<&str>) -> Self {
        self.scope = Scope { team: Some(team.into()), channel: channel.map(str::to_string) };
        self
    }
    pub fn labeled(mut self, label: Sensitivity, locality: Locality) -> Self {
        self.label = Some(label);
        self.locality = Some(locality);
        self
    }
    pub fn policy(mut self, v: impl Into<String>) -> Self {
        self.policy_version = Some(v.into());
        self
    }
}

/// 已落库事件(读侧)。
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub event_id: String,
    pub ts_ms: u64,
    pub actor: Actor,
    pub scope: Scope,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub policy_version: Option<String>,
    pub label: Option<Sensitivity>,
    pub locality: Option<Locality>,
    pub schema_version: u32,
    pub payload: Value,
    pub prev_hash: String,
    pub hash: String,
}

impl AuditEvent {
    pub fn parsed(&self) -> Parsed {
        parse_payload(&self.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_self_describing_and_round_trips() {
        let body = EventBody::DrillStart { drill_id: "D-1".into() };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["event_type"], "drill.start");
        assert_eq!(parse_payload(&v), Parsed::Known(body));
    }

    #[test]
    fn unknown_event_type_is_tolerated_not_fatal() {
        let v = serde_json::json!({ "event_type": "session.stream.start", "channel": "#platform" });
        match parse_payload(&v) {
            Parsed::Unknown { event_type, .. } => assert_eq!(event_type, "session.stream.start"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn egress_bytes_serde_shape_is_sql_friendly() {
        // SQL 侧依赖这两个形状:object{measured} / 字符串 "unmetered"。
        assert_eq!(
            serde_json::to_value(EgressBytes::Measured(3100)).unwrap(),
            serde_json::json!({ "measured": 3100 })
        );
        assert_eq!(
            serde_json::to_value(EgressBytes::Unmetered).unwrap(),
            serde_json::json!("unmetered")
        );
    }

    #[test]
    fn downgrade_reason_round_trips_for_read_path() {
        // 依赖本次给 muster-route 补的 Deserialize(第 7 幕文案的读路径)。
        let r = DowngradeReason::EgressLocked;
        let v = serde_json::to_value(r).unwrap();
        let back: DowngradeReason = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
        assert!(!back.text_zh().is_empty());
    }
}
