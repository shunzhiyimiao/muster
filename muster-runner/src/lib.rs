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
//! 2. **工具是真实的、只读的、工作区圈禁的**:`list_dir` / `read_file` /
//!    `grep`,路径一律经 canonicalize 并强制落在工作区内;越界不是 panic
//!    而是把拒绝文本作为工具结果回给模型(模型应当看见边界)。
//!    写操作 v0 不提供——写入必须先过 A9 审批事件(P5),不做无审批的写。
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
//!   检测排在 v0.x。
//! - 字节记账是载荷近似(与桌面壳同口径);wire 级计量属 A2 后续。

pub mod runner;
pub mod tools;

pub use runner::{run_task, RunSummary, RunnerConfig, RunnerError, RunnerEvent, TaskSpec};
pub use tools::ToolSet;
