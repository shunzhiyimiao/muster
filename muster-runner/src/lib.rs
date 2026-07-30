//! # muster-runner — 任务执行器 v0(任务 B1)
//!
//! 一句话:**把一句话变成一条可重放的审计链。**
//! `路由决策 → 开流 → 工具循环 → 续轮 → 审计事件链`,是三个基础 crate
//! 的第一个真实消费者。
//!
//! ## 关键设计决策
//!
//! 1. **落点合法性属于路由,续命策略属于 Runner**(与 muster-route 的边界
//!    约定一致):`Router::resolve()` 只做一次;中流失败由 Runner 决定
//!    ——v0 策略是"同一 provider 整回合重试一次,再败即 run.finish(Failed)"。
//!    绝不重新升级落点,链的合法性在 resolve 时已被类型保证。
//! 2. **写权限由隔离换取,而不是由审批换取**(P1-04):
//!    - 直连用户工作区 ⇒ 只读工具(`list_dir`/`read_file`/`grep`);
//!    - 每 run 独立 git worktree(见 [`worktree`])⇒ 额外启用
//!      `write_file`/`replace_in_file`。写发生在隔离分支的独立目录里,
//!      主仓一个字节都不动,产出 diff 供人复核。
//!
//!    需要审批的是**合入与 push**(见 [`approval`]):Runner 只提申请、
//!    永不自行合入,裁决与执行都由人触发;push/PR 的接口干脆不提供。
//!    路径一律经 canonicalize 圈禁(新文件对父目录取证),越界不是 panic
//!    而是把拒绝文本回给模型(模型应当看见边界)。
//! 3. **run.start 的 ReplayRefs 全部取真值**:git HEAD(或目录清单)哈希、
//!    依赖锁文件哈希、模型参数哈希、工具环境哈希。拿不到就用可复算的
//!    降级来源并如实标注前缀(`dir:` vs `git-head:`),**不伪造**。
//! 4. **事件回调而非事件总线**:`on_event: FnMut(RunnerEvent)` 由调用方
//!    (桌面壳 / CLI / 未来的调度器)自行转发;Runner 不绑定任何 UI 技术。
//! 5. **审计写失败即任务失败**:证据层不可用时不允许"先干了再说"
//!    (fail-closed,与 E2/E6 同一哲学)。
//!
//! ## 诚实边界(v0)
//!
//! - 路由拒绝已落审计(`route.refuse`,E4):不仅回答"为什么落在这里",
//!   也回答"为什么没有落点";分类口径由 `EventBody::route_refuse` 单一供给。
//! - 空闲看门狗未实现:总超时靠 provider 的 `timeout_secs`;流式空闲
//!   检测排在 v0.x(网关侧已有,见 muster-gateway)。
//! - worktree 保留策略三条已全部落地([`worktree::RetentionPolicy`] +
//!   [`approval::decide`]):无变更立即回收 / 超上限回收最旧 /
//!   **已处置(批准合入或拒绝丢弃)即回收**。有变更且未裁决的会保留——
//!   删掉会把「可操作的改动」降级成「一段文本」(没法 `git checkout` 编译、
//!   没法 `git merge` 合入)。
//! - 审批目前是单人裁决(裁决人固定为部署者):多人角色与授权范围属 P2。
//! - 字节记账是载荷近似(与桌面壳同口径);wire 级计量属 A2 后续。

pub mod approval;
pub mod capsule;
pub mod runner;
pub mod tools;
pub mod worktree;

pub use approval::{decide, request_merge, ApprovalError, DecisionOutcome, CAP_MERGE};
pub use capsule::{
    draft_spec, forge, forge_and_store, verify, CapsuleError, CapsuleSpec, CapsuleStore,
    ForgeOutcome, VerifyOutcome,
};
pub use runner::{run_task, RunSummary, RunnerConfig, RunnerError, RunnerEvent, TaskSpec};
pub use tools::ToolSet;
pub use worktree::{FileChange, RunDiff, Worktree, WorktreeError};
