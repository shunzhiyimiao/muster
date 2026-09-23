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
//!      `write_file`/`replace_in_file`/`run_command`。写与执行都发生在隔离
//!      分支的独立目录里,主仓一个字节都不动,产出 diff 供人复核。
//!      `run_command`(见 [`command`])与写工具同闸不是巧合:隔离正是它的
//!      安全前提,没有隔离就不该有它——改完代码要能跑一下才知道能不能用,
//!      否则交出去的是一份没人跑过的 diff。
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
//! 6. **技能是条件,不是过程**(见 muster-skill):`RunnerConfig::skills` 里的技能以
//!    目录形式进系统提示词,正文经 `read_skill` 工具按需读取;哪些技能被提供、
//!    哪些真的被读了,分别落 `skill.offer` / `skill.read`(只记哈希)。工具清单因此
//!    随技能有无而变,`tool_env` 哈希如实反映——有技能的运行与没技能的运行不是同一种运行。
//! 7. **调用方可以带自己的异步工具**([`ExtraTool`]):Runner 只负责公示、分派、把它
//!    产生的审计事件落链(`tool.egress` 一律记 cloud);策略(白名单、计量)在工具自己
//!    那边。第一个用户是 muster-workflow 的 `http_get`。
//! 8. **工牌是配置,不是常量**(见 muster-roster):`RunnerConfig` 里 `badge` 之外还有岗位
//!    说明书(提示词后缀)、命令白名单、能力哈希;审批申请里记的就是这个哈希,
//!    名册改了能力,审批人看到的申请依据随之变。工具档位(只读 / 隔离可写)由调用方
//!    用 `workspace_root` 表达,Runner 不认识名册。
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
//! - 审批的角色与作用域校验已落地([`approval::decide_as`] + muster-identity
//!   的 `can()`):裁决人不再固定为部署者,越权者在 git 操作与落库**之前**被挡下。
//!   仍未做的是身份的**来源**——Principal 目前由本机配置装配,OIDC/目录同步属 P2 服务端。
//! - 命令执行([`command`])的网络封堵**做不到零外发**:能剥的环境变量都剥了、
//!   能否决的出网命令都否决了,但 `cargo test` 跑的是工作区里的任意代码。
//!   真正的封堵在进程之外(sandbox-exec / netns / 防火墙)。按铁律四的口径:
//!   **测不到就不算作零**——演习报告的「外发字节」只覆盖 model.call,
//!   不要把它读成「本次运行零外发」。
//! - 字节记账是载荷近似(与桌面壳同口径);wire 级计量属 A2 后续。
//! - 影子重放(`capsule::verify`)暂不携带技能:重放以空技能集运行。源运行若读过技能
//!   (链上有 `skill.read`),比对差异可能由此而来;v0 只让它可见,不自动对齐。

pub mod approval;
pub mod capsule;
pub mod command;
pub mod runner;
pub mod tools;
pub mod worktree;

pub use approval::{
    decide, decide_as, request_merge, request_merge_with_caps, ApprovalError, DecisionOutcome, CAP_MERGE,
};
pub use capsule::{
    adopt, draft_spec, forge, forge_and_store, run_capsule, verify, AdoptOutcome, CapsuleError,
    CapsuleSpec, CapsuleStore, ForgeOutcome, VerifyOutcome,
};
pub use runner::{
    run_task, run_task_at, RunSummary, RunnerConfig, RunnerError, RunnerEvent, TaskSpec, RATE_LIMIT_ATTEMPTS,
};
pub use command::CommandPolicy;
pub use tools::{ExtraTool, ExtraToolFuture, ToolOutcome, ToolSet};
pub use worktree::{FileChange, RunDiff, Worktree, WorktreeError};
