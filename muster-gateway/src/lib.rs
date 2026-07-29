//! # muster-gateway — 模型网关(总规划 services/model-gateway)
//!
//! 一句话:**对外讲 Responses,对内讲 chat。**
//!
//! 存在理由(实测结论,非设计偏好):上游 Codex 已删除 chat wire,只讲
//! Responses API;而国内可用的 chat 系端点(Kimi/DeepSeek/Qwen/Ollama 兼容
//! 模式)没有 `/responses`。网关把这条鸿沟收敛到**一个进程、一套翻译**,
//! 而不是往 Codex fork 里凿洞——fork 保持最小 diff 才能持续 rebase 上游。
//!
//! ## 设计决策
//!
//! 1. **复用 A2 而不是新写 HTTP 客户端**:后端走 `muster-provider`,于是
//!    SSE 重组、错误分类、token 计量、`Locality` 元数据全部继承;网关因此
//!    天然是**外发计量的同一个咽喉**(E4 对账口径不分裂)。
//! 2. **翻译是纯函数**([`translate`]):可穷举单测,不需要起服务就能验协议。
//! 3. **不支持项显式丢弃并 warn**:reasoning 回灌、local_shell、web_search
//!    等一律记名丢弃,绝不静默假装成功——能力边界要看得见。
//! 4. **只做协议翻译,不做策略**:密级路由属 E2、审计属 A9;网关是哑管道,
//!    这样它才能被 Codex 与其它客户端共用而不重复实现治理。
//!
//! ## 用法
//!
//! ```bash
//! KIMI_API_KEY=… cargo run -p muster-gateway -- --config provider.example.toml \
//!   --provider kimi --port 8787
//! # codex 侧:base_url = http://127.0.0.1:8787/v1  wire_api = "responses"
//! ```
//!
//! ## 诚实边界
//!
//! - 无鉴权:面向 127.0.0.1 的本机进程;暴露到网络前必须加(A8 白名单同理)。
//! - 无 `/responses` 的非流式分支(Codex 恒用 `stream: true`)。
//! - 多轮 previous_response_id / store 语义不实现:Codex 每轮回传完整 input。

pub mod server;
pub mod translate;

pub use server::{serve, GatewayState};
pub use translate::{to_chat, ResponsesRequest};
