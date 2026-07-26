//! # muster-route — 敏感度标签 + 路由决策器 v0(任务 E1 类型层 + E2)
//!
//! 一句话:**标签跟数据走,降级可以自动,升级永不自动。**
//!
//! ## 关键设计决策
//!
//! 1. **「绝不静默升云」是结构性的**:决策产物是一条执行链
//!    `primary + fallbacks`,fallbacks 类型上只收本地 provider——本地挂了
//!    "顶到云端"在数据结构里没有表达方式,不靠运行时 if 守卫。
//! 2. **决策是纯函数**([`decision::decide`]):无 IO、无时钟、无健康探测,
//!    因此可被穷举——`tests/matrix.rs` 枚举全部输入组合逐条验不变量,
//!    这就是任务验收「决策矩阵单测全绿」的形态。
//! 3. **有效密级 = max(所有来源)**,并回传促成者清单(provenance):
//!    UI 徽章与审计都能回答"为什么是这个级别、为什么落在这里"。
//! 4. **组织策略两层决策**:组织定边界([`policy::OrgPolicy`],含
//!    cloud_max 与 egress_locked),用户在边界内选;restricted 永不上云
//!    是硬编码不变量,不是策略取值。
//! 5. **E3 已落地**([`session::SessionRatchet`]):会话棘轮往 sources 注入
//!    `LabelOrigin::SessionLock`,只升不降、跨轮次持久、无降低 API;
//!    E6 主权演习 = `Router::set_egress_locked`,复用同一条 fail-closed 路径。
//! 6. **中流失败不归路由层**:流建立后的断线属于 Runner 重试策略;路由层
//!    只保证"落点合法 + 落点探活",避免半份输出的重放鬼故事。

pub mod decision;
pub mod label;
pub mod policy;
pub mod router;
pub mod session;

pub use decision::{decide, Downgrade, DowngradeReason, RoutePlan, RouteRefusal, RouteRequest};
pub use label::{effective_sensitivity, LabelOrigin, LabelSource, Sensitivity, DEFAULT_SENSITIVITY};
pub use policy::{OrgPolicy, PolicyError};
pub use router::{Attempt, Resolution, RouteError, Router};
pub use session::{LockState, Raise, SessionRatchet};
