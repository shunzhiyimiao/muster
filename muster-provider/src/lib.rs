//! # muster-provider — Muster 的模型层抽象（任务 A2）
//!
//! Muster 上层（Runner、路由、审计）只依赖 [`ModelProvider`] trait 与本 crate 的
//! 内部类型；HTTP、SSE、厂商 wire 格式全部封在 provider 实现内部。
//!
//! ## 关键设计决策（评审时逐条过）
//!
//! 1. **一个传输栈覆盖 A3/A4/A5**：DeepSeek、DashScope 兼容模式、Ollama、vLLM 讲同一种
//!    `/chat/completions` 协议，因此只有一个 [`openai_compat::OpenAiCompatProvider`] +
//!    每厂商预设；厂商差异进预设，不新开传输栈。
//! 2. **`Locality` 是一等元数据**：E2 路由（restricted → 仅本地）、A8 出网白名单、
//!    E4 外发审计都以 [`provider::Locality`] 为判据；由配置声明、注册表暴露
//!    （[`registry::ProviderRegistry::endpoints`]）。
//! 3. **错误分类服务于 fail-closed**：[`error::ProviderError::should_failover`] 是
//!    "云不可达 → 降落本地" 的唯一判定入口；Auth/配置错误刻意不触发降级，必须炸响。
//! 4. **对象安全优先**：注册表分发 `Arc<dyn ModelProvider>`，激活哪个 provider 永远是
//!    运行时决定（配置 / 路由策略），因此用 `async_trait` 而非原生 AFIT。
//! 5. **token 计量信任厂商回报值**：不内置分词器；`stream_options.include_usage`
//!    在流式末尾取回用量，供 E4 对账。
//! 6. **密钥只经环境变量**：配置文件写变量名（`api_key_env`），启动时解析、缺失即
//!    快速失败；`Debug` 输出全量脱敏。
//! 7. **SSE 手写并单测**：唯一的协议脆弱点（分块切断行/切断码点/CRLF/`[DONE]`）
//!    在 [`sse`] 模块内被穷举测试。
//!
//! ## 集成入口
//!
//! ```no_run
//! use muster_provider::{ChatMessage, ChatRequest, ProviderRegistry};
//!
//! # async fn demo() -> Result<(), muster_provider::ProviderError> {
//! let toml_text = std::fs::read_to_string("providers.toml").unwrap();
//! let registry = ProviderRegistry::from_toml_str(&toml_text)?;
//! let provider = registry.default_provider().expect("default configured");
//!
//! let req = ChatRequest {
//!     messages: vec![ChatMessage::user("列出本仓库的构建步骤")],
//!     ..Default::default()
//! };
//! let resp = provider.chat(req).await?;
//! println!("{:?}", resp.message.content);
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod mock;
pub mod openai_compat;
pub mod provider;
pub mod registry;
pub mod sse;
pub mod types;

pub use error::ProviderError;
pub use mock::MockProvider;
pub use openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
pub use provider::{collect_stream, Locality, ModelProvider, ProviderMetadata};
pub use registry::{ProviderRegistry, RegistryConfig};
pub use types::{
    ChatMessage, ChatRequest, ChatResponse, FinishReason, Role, StreamEvent, TokenUsage, ToolCall,
    ToolCallAccumulator, ToolChoice, ToolSpec,
};
