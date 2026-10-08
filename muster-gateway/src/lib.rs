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
//! 4. **治理在网关内,与桌面壳同一套**(2026-10-08 起,见 [`govern`]):每个请求先过密级路由
//!    (与 Runner 同一个 Router / OrgPolicy,restricted 只能落本地,落不下就 403 拒绝),再落
//!    `route.decide` / `route.refuse` 与 `model.call` 到网关自己的审计链。此前它是"哑管道",
//!    经它走的 Codex 流量绕开了密级路由与审计——一个本该只用本地模型的仓库换成 Codex 来做事
//!    就悄悄上了云。密级从三处取高:启动参数 `--sensitivity`、请求头 `X-Muster-Sensitivity`
//!    (Codex 的 `model_providers.<id>.http_headers` 可静态配上,只能抬高)、落盘的会话棘轮。
//!
//! ## 用法
//!
//! ```bash
//! KIMI_API_KEY=… cargo run -p muster-gateway -- --config provider.example.toml \
//!   --provider kimi --port 8787 --sensitivity internal --cloud-max internal
//! # codex 侧:base_url = http://127.0.0.1:8787/v1  wire_api = "responses"
//! #   需要更高密级时:http_headers = { "X-Muster-Sensitivity" = "restricted" }
//! # 审计链默认在 ~/.muster/gateway-audit.db(--audit 可改),棘轮在 ~/.muster/gateway-ratchet.json
//! ```
//!
//! ## 实测得到的两条硬约束(踩坑记录,勿轻易回退)
//!
//! - **namespace 必须展平**:Codex 把 shell 等核心工具装进
//!   `{type:"namespace", tools:[…]}` 容器,chat 协议没有这一层。早期版本把
//!   整个 namespace 当"不支持项"丢掉,结果 agent 无工具可用、空转到超时。
//!   现按 `ns__tool` 展平并**建反查表**([`translate::NameMap`]):名字编码
//!   是猜、查表是记,工具名自身含 `__` 或超 64 字符被截断时靠表才能还原。
//!   分隔符与长度上限的取值同 Docker MCP Gateway 等实现(`[A-Za-z0-9_-]{1,64}`)。
//! - **必须有空闲看门狗**:上游可能"已开流但不再吐字节"(实测静默 64 分钟),
//!   A2 的总超时管不住这种挂起流。见 [`server::DEFAULT_IDLE_TIMEOUT_SECS`]。
//!
//! ## 诚实边界
//!
//! - 无鉴权:面向 127.0.0.1 的本机进程;暴露到网络前必须加(A8 白名单同理)。
//! - 无 `/responses` 的非流式分支(Codex 恒用 `stream: true`)。
//! - 多轮 previous_response_id / store 语义不实现:Codex 每轮回传完整 input。
//! - 密级来源有限:网关看不见 Codex 工作区里的数据,只认声明(启动参数 / 请求头 / 棘轮);
//!   仓库级标签要靠 Codex 侧配置请求头。actor 是启动参数给的 id,不是经认证的身份。

pub mod govern;
pub mod server;
pub mod translate;

pub use govern::{Governance, GovernError, POLICY_VERSION, SENSITIVITY_HEADER};
pub use server::{serve, GatewayState};
pub use translate::{to_chat, ResponsesRequest};
