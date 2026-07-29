# muster-gateway(model-gateway v0)

**对外讲 Responses,对内讲 chat。** 让上游 Codex(已删除 chat wire,只讲
Responses API)用上任何 chat 系模型——Kimi / DeepSeek / Qwen / Ollama 兼容模式。

## 跑法

```bash
# 1) 起网关(密钥只经环境变量)
KIMI_API_KEY=… cargo run -p muster-gateway -- \
  --config provider.example.toml --provider kimi --port 8787

# 2) codex 侧:用隔离的 CODEX_HOME,别污染 ~/.codex
mkdir -p /tmp/codex-home && cat > /tmp/codex-home/config.toml <<'EOF'
model = "kimi-k3"
model_provider = "muster"

[model_providers.muster]
name = "Muster Gateway"
base_url = "http://127.0.0.1:8787/v1"
wire_api = "responses"
requires_openai_auth = false
EOF

CODEX_HOME=/tmp/codex-home codex exec "把 add 函数的减法改成加法"
```

## 为什么是独立进程,而不是改 Codex fork

fork 保持最小 diff 才能持续 rebase 上游(FORK.md 的同步策略)。把协议差异
收敛进网关,fork 侧只需一段 provider 配置,零代码改动。

后端复用 `muster-provider` 而不是新写 HTTP 客户端 ⇒ SSE 重组、错误分类、
token 计量、`Locality` 元数据全部继承,网关因此天然是**外发计量的同一个咽喉**
(E4 对账口径不分裂)。

## 实测得到的两条硬约束

| 约束 | 现象 | 处理 |
|---|---|---|
| **namespace 必须展平** | Codex 把 shell 等核心工具装在 `{type:"namespace", tools:[…]}` 里;早期版本整包丢弃 → agent 无工具可用、空转到超时 | `ns__tool` 展平 + **反查表**,出向还原 `name` + `namespace` |
| **必须有空闲看门狗** | 上游"已开流但不再吐字节",实测静默 64 分钟;A2 总超时管不住挂起流 | 默认 120s 空闲即 `response.failed` 并写明是挂起 |

### 工具名映射的取舍(对齐业界实现)

函数名的通行约束是 `^[a-zA-Z0-9_-]{1,64}$`——`.`/`/`/`:` 都非法,长度也有上限。

- **分隔符取 `__`**:与 [Docker MCP Gateway](https://github.com/docker/mcp-gateway/pull/263)
  一致(它正是因该 regex 从 `:` 改过来)。MCP 的
  [SEP-986](https://modelcontextprotocol.io/seps/986-specify-format-for-tool-names)
  允许 `.` 与 `/`,但那是 MCP 层的约束,函数名层更严,不能照搬。
- **建反查表而不只靠名字编码**:名字编码是"猜",查表是"记"。工具名自身含
  `__`(如 `my__tool`)会被启发式误拆;超 64 字符截断后更无从还原。做法参考
  [Roo-Code 的 `sanitizedNameRegistry`](https://github.com/RooCodeInc/Roo-Code/pull/10054)。
  表在单次请求内构建并随流传递,网关本身仍无状态。
- **超长名截断 + 稳定短哈希后缀**:保证长度合法且同名同结果,反查表兜底还原。
- `split_ns` 保留为**兜底启发式**,仅在无表时使用。

## 端点

- `POST /v1/responses` — SSE:`response.created` → `output_text.delta` →
  `output_item.done`(message / function_call)→ `response.completed`(带 usage)
- `GET /v1/models` — 单模型列表(探活用)
- `GET /health`

## 观测

`RUST_LOG=muster_gateway=info`(默认即此)每轮打印:收到请求(输入项/消息数/
工具数)→ 上游已开流(耗时)→ 上游首个事件 → 本轮完成(耗时/文本长度/工具调用名)。

## 诚实边界

- **无鉴权**:面向 127.0.0.1 本机进程;暴露到网络前必须加。
- 无非流式分支(Codex 恒用 `stream: true`)。
- `previous_response_id` / `store` 语义不实现(Codex 每轮回传完整 input)。
- reasoning 回灌、local_shell、web_search 等**显式记名丢弃并 warn**,不静默假装成功。
- **不做治理**:密级路由属 E2、审计属 A9;网关是哑管道,这样才能被 Codex
  与其它客户端共用而不重复实现治理。
