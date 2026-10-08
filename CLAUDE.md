# Muster — 可完全本地部署的 AI 原生团队协作系统（兼容 Codex 的技能与接口约定）

## 铁律（改代码前先读，违反即回滚）
1. 密级只升不降：SessionRatchet 无降低 API；Sensitivity 比较即楼层
2. 绝不静默升云：RoutePlan.fallbacks 类型上只收本地 provider
3. 审计只存哈希不存正文：ContentHash 进表，正文留 run 存储
4. fail-closed：外发字节测不到按违规记（EgressBytes::Unmetered）
5. serde_json 永不开 preserve_order（哈希链依赖 BTreeMap 键序）

## Crate 地图
- muster-provider：OpenAI 兼容传输层，Locality 元数据，外发计量唯一咽喉
- muster-route：E1 密级/E2 决策 decide() 纯函数/E3 会话棘轮
- muster-audit：append-only + SHA-256 链，"8幕→SQL"见其 README
- muster-eval：A7 评测，20 固定样本，A1 提示词改动后必须重跑
- muster-runner：B1 任务执行器；每 run 独立 worktree（写权限由隔离换取，主仓零污染，产出 diff）；重试属 Runner 不属路由；工作区根的 AGENTS.md（AGENTS.override.md 优先，32 KiB，与 Codex 同规则）常驻系统提示词、哈希进 run.start 的 tool_env；可写 run 自带出网闸（回环 HTTP 代理，照 Codex network-proxy 的形态不链接其代码：子进程 HTTP(S)_PROXY 指过去，白名单来自工牌 egress，默认全拒，每次连接尝试落 tool.egress；认代理的工具才受约束，硬封堵待 sandbox 第二刀）
- muster-skill：技能（SKILL.md，与 Codex 同一约定：仓库级 .agents/skills/ 与 .codex/skills/ + 个人级 ~/.muster/skills/ 与 ~/.agents/skills/，顺序即优先级）；只从本地发现不联网、目录进提示词而正文经 read_skill 按需读、审计只记哈希（skill.offer / skill.read）、自报密级即成为标签来源
- muster-workflow：自定义工作流（WORKFLOW.md：schedule / skills / egress / outputs / channel + 步骤正文；仓库级 .agents/workflows/ + 个人级 ~/.muster/workflows/）；工作流自有仓库产出自动合入（只合自己名下的仓库，不走审批）；http_get 只许白名单主机、每次落 tool.egress 计入演习报告；调度器 v0 在桌面壳里（错过补跑一次），入口在我的工作台 › 任务 › 工作流；PDF 靠本机 Chrome headless，pptx 未实现
- muster-roster：Agent 编制（AGENT.md 岗位卡：badge / name / role / aliases / skills / tools 只读或隔离可写 / commands 白名单 / egress 出网主机白名单 / model / sensitivity_max / default；仓库级 .agents/roster/ + 个人级 ~/.muster/roster/）；工牌号 = 审计 actor id；能力子集哈希进 approval.request，变了落 badge.update；密级上限是硬闸；指派顺序：消息开头 @ > 界面选中的工牌 > 频道默认 > 名册默认；名册为空退回内置 A-007；delegate 工具让主 Agent 把子任务委派给另一张工牌（同一隔离工作区、串行、只一层、档位取交集，父 run 落 run.delegate）；频道编制 channel_agents 存 state.db，引入/请离写 channel.roster，非空时只能点已引入的工牌、第一张是频道默认
- muster-gateway：对外 Responses / 对内 chat，让 Codex 用上 chat 系模型；自带治理（2026-10-08 起）：每个请求先过密级路由（与 Runner 同一个 Router / OrgPolicy，restricted 只能落本地，落不下就以 SSE response.failed 拒绝）再落 route.decide / route.refuse 与 model.call 到网关自己的链（~/.muster/gateway-audit.db）；密级取 --sensitivity、请求头 X-Muster-Sensitivity、落盘棘轮三者之高
- muster-prompt：A1 系统提示词唯一出处（改它必须重跑 A7 评测）
- muster-identity：P2 权限语义内核；can() 判定纯函数按 §4.4 顺序，五不变量穷举验证（13,500 组）
- muster-meeting-agent：会议 Agent（LiveKit Rust SDK 在 `livekit` feature 后面，默认不编）；常驻 daemon 订服务端 SSE 认领会议、本地 whisper 转写、@ 应答、会中派活、散会提炼
- muster-server：P2/P3 服务端（Axum + PostgreSQL）；权限判定复用 muster-identity 不另写一份；会议接口（建/入/离/散/纪要/密级/请 Agent）每条先落服务端自己的审计链再动库，正文只留哈希，e2e 在 tests/meetings_e2e.rs（需 DATABASE_URL）；
  服务端不持有源码、不做全局审计链（节点各自留链，组织侧靠锚定——尚未实现）；
  依赖用 deploy/docker-compose.yml 一条命令拉起
- apps/desktop：智能协作平台桌面壳（Tauri 2，独立工作区，pnpm tauri dev）

## 约定
- 动手前后都跑 cargo test（当前基线 314 项全绿；桌面壳另有 43 项需在 apps/desktop/src-tauri 下跑）
- 每个任务一个 commit；不顺手重构已锁死的公共接口
- 设计决策都在各 crate 的 lib.rs 文档注释里，先读再改
- 根目录 AGENTS.md 是给 Agent（Muster 工牌与 Codex）读的章程摘要，运行时每次都进提示词；改铁律时与本文件两处同步
- **会议线已正式纳入范围**（2026-10-08，撤销规划非目标第一条）：一个会议室实体两种模式（语音 / 视频），边界是转写只走本地 provider、restricted 禁录、会议需服务端；分阶段见 docs/会议线规划-2026-10-08.md（阶段 1 代码部分已于同日完成，欠 2 真人 + Agent 实测；界面上的开关状态只读 LiveKit SDK，不另存一份）
- **界面只放真的**（2026-10-08 评审第 8 步）：占位模块与概念示例默认隐藏，导轨底部的眼睛图标可切换「显示规划中的功能」（localStorage `muster.showPlanned`）；新功能先做真的再上界面，不再新增占位
- 总体规划在 docs/（16 周 P0–P6，任务号 P0-01…P6-08）；A2/A7/E1–E3/A9 等是其细化分解；../muster-codex 是 openai/codex 的**只读参考快照**（2026-10-08 起不再是 fork：固定在 rust-v0.161.0 / 979011409d，detached，不改一行；要看新版本就 `git fetch upstream tag <标签> --no-tags && git checkout --detach <标签>`，并把这里的标签改掉）
- **与 Codex 的关系（2026-10-08 评审后定为路 B）**：Muster 自带运行时，不以 Codex 为执行器；只借其 SKILL.md 技能约定、Responses 线协议（经网关）与会话分叉语义，并有选择地吸收其资产（读 AGENTS.md、兼读 ~/.agents/skills、复用沙箱与网络代理）。对外一律不再称"teams 版 codex"。评审与八步行动见 docs/评审-与Codex吻合度-2026-10-08.md
- **服务端动手前先读 docs/服务端架构.md**（六条硬边界 + 分阶段 + 未决三项）；改完回来更新它
