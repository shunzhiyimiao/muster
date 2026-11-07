# 跟随 cc-switch(计划,2026-11-08 用户定,多端测试后排)

> 目标:在 cc-switch 里一键切模型通道,Muster 桌面壳跟着切——像 Claude Code / Codex 一样,不必在 Muster 里再配一遍。

## 边界先说清

- cc-switch 不认识 Muster,只写 Claude Code / Codex / Gemini 的配置;所以是 **Muster 主动读** cc-switch 的当前选择,不是 cc-switch 推。
- 只管**这台机器上的桌面壳**(单机模式跑任务、本地作答)。会议 Agent 跑在服务器节点上,它的通道目录与密钥在服务器(边界六),cc-switch 够不着;要让会议用某条通道仍要进服务端目录 + 密钥放服务器。
- 密钥不落盘:从 cc-switch 的库里读出来只注入 Muster 自己的进程环境(`api_key_env` 指向一个运行时变量名),Muster 不写任何配置文件。

## 顺序(用户定)

### 第一步:跟随 Codex 类通道(OpenAI 协议,现有 openai_compat 直接能用)

- 数据源:`~/.cc-switch/cc-switch.db`(SQLite)。`providers` 表:`id, app_type, name, settings_config(JSON:auth.OPENAI_API_KEY + Codex 的 config.toml 文本), is_current`;`provider_endpoints` 表有 base_url。
  备选数据源:cc-switch 写出的 `~/.codex/config.toml` + `~/.codex/auth.json`(Codex 自己的公开格式,更稳,但只反映"当前"那一条)。
- 桌面壳:「跟随 cc-switch」开关(设置 / 连接卡附近)。开着时:
  1. 读 `app_type = 'codex' AND is_current = 1` 那条:base_url(provider_endpoints 或 config 里的 `[model_providers.*].base_url`)、model、密钥;
  2. 注册为本地 provider `cc-switch`(kind openai_compat,locality **cloud**,display_name 带 cc-switch 里的名字),密钥注入进程 env `MUSTER_CCSWITCH_API_KEY`;
  3. 轮询库文件 mtime(每 5 秒)或 watch,变了就重注册;界面上显示"当前跟随:<名字>";
  4. 连着服务端时目录以服务端为准(边界六),跟随只补进本地那份;界面要说明"会议 Agent 不受此影响"。
- 探活:注册后打一次 `/models`,通 / 401 / 连不上 显示在开关旁边。
- `wire_api = "responses"` 的通道能不能走 chat/completions 要探一次(viwo50 探过:两个接口都存在);不通就提示"这条通道只有 Responses 协议,等第二步"。

### 第二步:Anthropic 传输层(让 Claude 类通道也能跟随)

- muster-provider 加 `kind = "anthropic"`:Messages API(`/v1/messages`,`x-api-key` / `anthropic-version`,SSE 事件 `content_block_delta` 等),映射到现有 `ChatRequest / StreamEvent / ToolCall`;工具调用的 `tool_use` / `tool_result` 块要对上现有的 ToolCallAccumulator。
- 注册表与目录加 kind;路由层无感(只看 locality)。
- 跟随逻辑扩到 `app_type = 'claude'`:密钥在 settings_config 的 `env.ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_API_KEY`,地址 `ANTHROPIC_BASE_URL`。
- A7 评测(muster-eval)要能跑 anthropic 通道一遍,才算"可用"。

### 可选第三步:Responses 传输层

- 只有 Responses 协议的中转(Codex 专用)才需要;现有 gateway 是反方向(对外 Responses → 对内 chat),不能复用。

## 验收

- cc-switch 切换 → 5 秒内桌面壳「当前跟随」变化,下一条本地作答走新通道(审计里 provider id 可见)。
- 关掉跟随 → 退回自带 / 服务端目录,进程 env 里的密钥清掉。
- 密钥从不出现在 Muster 写的任何文件里(grep 验证)。
