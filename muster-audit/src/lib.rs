//! # muster-audit — 审计事件层 v0(任务 A9)
//!
//! 一句话定位:**这不是日志系统,是产品的证据层。**
//!
//! 三个消费者在设计前就已存在:第 8 幕演习报告、Capsule 锻造(从成功运行的
//! 事件链复现能力)、工牌审批追溯。因此验收标准是硬的:
//! **演示里出现的每一个数字,都必须能用一条 SQL 从审计表里查出来**
//! (对照表见 README「8 幕 → SQL」)。
//!
//! ## 五条设计原则(按重要性排序)
//!
//! 1. **run_id 是脊柱,Capsule-ready 是硬约束**:重放一个 run 所需的一切,
//!    要么在 payload 里,要么是内容寻址引用([`event::ReplayRefs`] 的字段
//!    全部非 Option——不满足约束的事件在类型上写不出来)。
//! 2. **外发字节结构化记账,不靠日志解析**:[`event::EgressBytes`] 只有
//!    `Measured(u64)` 与 `Unmetered` 两种取值;字节数不明按违规记、不按 0
//!    记([`queries::DrillReport::ok`] 的 fail-closed 语义,与 E2 同一哲学)。
//!    **口径边界**:这套记账覆盖的是 `model.call`。`command.run` 执行的是
//!    工作区里的代码,其出网在本进程之外,证据层看不见——所以演习报告的
//!    「外发字节」是 model 通道的数,不是整机的数,别把它读成「零外发」。
//!    真正的封堵属操作系统层(见 muster-runner 的 `command` 模块文档)。
//! 3. **决策事件记「依据」而不只记「结果」**:`route.decide` 携带有效密级、
//!    促成来源、策略版本与 [`muster_route::DowngradeReason`]——第 7 幕徽章
//!    悬浮文案直接由 `text_zh()` 供给;审批事件记「申请能力 vs 工牌能力」差值。
//! 4. **审计只存元数据和哈希,永不存正文**:prompt/输出正文留在 run 存储侧
//!    带自己的密级,审计表只引用 [`event::ContentHash`]。否则审计表被迫背上
//!    全组织最高密级,既是泄露面,也无法导出给外部审计方。
//! 5. **写侧穷举、读侧宽容**:写入用穷举的 [`event::EventBody`];读取经
//!    [`event::Parsed`],未知 `event_type` 落入 `Unknown` 而不是报错——
//!    旧版本读新库不崩,这就是 `schema_version` 存在的意义。
//!
//! ## 防篡改
//!
//! 每行携带 `prev_hash`/`hash` 组成 SHA-256 哈希链
//! ([`store::AuditStore::verify_chain`]);签名与外部锚定明确是 v1.x
//! (MVP 不做,见 README「不做清单」)。
//!
//! ## v1.x 预留事件类型(零成本占位,勿删)
//!
//! (`capsule.forge` / `capsule.verify` / `capsule.adopt` 已随 P4 落地,
//! 不再是预留;锻造前置条件见 [`queries::forgeable`])、
//! (`command.run` 已随 B2 落地,不再是预留;被拒的命令同样入链)、
//! `session.stream.start` / `stream.viewer.join` / `session.stream.stop`、
//! (`session.lock.raise` 已随 E3 于 v1 落地,不再是预留)、
//! `share.block`(密级拦截)、`convo.share`、`meeting.transcribe`。
//! 上线任一类型前,先过 README 的「Capsule-ready 检查单」。

pub mod event;
pub mod hash;
pub mod id;
pub mod queries;
pub mod store;

pub use event::{
    Actor, ActorKind, AuditEvent, ContentHash, EgressBytes, EventBody, ModelRef, NewEvent,
    Parsed, ReplayRefs, RunOutcome, Scope,
};
pub use queries::{
    actor_first_seen, capsules, day_throughput, decision_of, distinct_runs, downgrades_zh,
    drill_report, forgeable, pending_approval_list, pending_approvals, recent_events,
    recent_events_of, roster, run_chain, run_start_of, session_lock, CapsuleRow, DayThroughput,
    DrillReport, PendingApproval, RosterRow,
};
pub use store::{AuditStore, ChainError, StoreError};
