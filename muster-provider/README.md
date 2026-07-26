# muster-provider（任务 A2 · v0.1）

Muster 的模型层抽象：`ModelProvider` trait + OpenAI 兼容实现 + Mock + 配置注册表。
上层（Runner / E2 路由 / A9 审计）只见 trait 和内部类型，HTTP/SSE/厂商 wire 格式全部封在实现内部。

## 七条设计决策（评审逐条过，详见 `src/lib.rs` 文档）

1. **一个传输栈覆盖 A3/A4/A5**——DeepSeek / DashScope 兼容模式 / Ollama / vLLM 讲同一种协议，只有一个 `OpenAiCompatProvider` + 厂商预设。A3/A4 从"两套客户端"缩成"两组配置 + 各自的真机联调"，省出的时间进缓冲。
2. **`Locality` 是一等元数据**——`local | cloud` 由配置声明。E2 路由（restricted → 仅本地）、A8 出网白名单（`registry.endpoints()` 直接生成）、E4 外发审计共用这一个判据。
3. **错误分类服务于 fail-closed**——`ProviderError::should_failover()` 是"云不可达 → 降落本地"的唯一入口；Auth / 配置错误刻意**不**触发降级，必须当场炸响，防止密钥配错被静默降级掩盖。
4. **对象安全优先**——注册表分发 `Arc<dyn ModelProvider>`，激活哪个 provider 是运行时决定，所以用 `async_trait` 而非原生 AFIT。
5. **token 计量信任厂商回报**——不内置分词器；流式用 `stream_options.include_usage` 在末尾取回用量，供 E4 对账。
6. **密钥只经环境变量**——配置写变量名（`api_key_env`），启动解析、缺失即报出变量名快速失败；所有 `Debug` 输出脱敏。
7. **SSE 手写并穷举测试**——分块切断行 / 切断 UTF-8 码点 / CRLF / 多 data 行 / `[DONE]` / 无终止尾包，全部有单测（`src/sse.rs`）。

## 集成

```rust
let registry = ProviderRegistry::from_toml_str(&std::fs::read_to_string("providers.toml")?)?;
let provider = registry.get("deepseek").unwrap();          // Arc<dyn ModelProvider>
let resp = provider.chat(req).await?;                       // 或 chat_stream(...)
```

- 配置样例见 `provider.example.toml`。
- E2 路由拿 `provider.metadata().locality` 做密级判断；拿 `err.should_failover()` 做降级判断。
- A8 白名单直接消费 `registry.endpoints()` 里 `locality == Cloud` 的行。
- 冒烟 / G0′ 探针种子：`cargo run --example smoke -- provider.example.toml deepseek`。

## 测试

- `cargo test`：22 个单测 + 1 个文档测试，零告警，无网络依赖。
- `cargo test --test live_api -- --ignored`：真实 API 联调（需 `DEEPSEEK_API_KEY` / 本地 Ollama），
  其中 Ollama 用例兼作 A6 每周保活冒烟。

## 工具链注记

曾有一块 **version-steering pins**（`zeroize`/`ring`/`rustls`/`hyper`/`quinn` 等）专为让
cargo 1.75 解析依赖图而设。团队工具链标准化到 **rustc ≥ 1.85** 后已整体删除——连同
`reqwest`/`url` 的精确锁定与未被代码引用的 `url` 直接依赖，`rust-version` 相应升至 1.85。
代码从未依赖任何被 pin 的版本行为；细节见 git 历史。

## 已知边界（诚实清单）

- 无重试/退避：`is_retryable()` 已给出判据，重试策略属于调用方（Runner），不属于传输层。
- 未做请求级流控与并发限制（MVP 单 Runner 用不到）。
- DashScope 预设走的是其 OpenAI 兼容模式；若启用 DashScope 原生特性（如联网搜索开关），加在预设里，不要新开传输栈。
- 流式无空闲超时（总超时会杀长流）；如需空闲看门狗，加在 Runner 侧计时器。
