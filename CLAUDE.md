# Muster — 本地部署的团队 + Agent 协作系统（"teams 版 codex"）

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
- muster-runner：B1 任务执行器；每 run 独立 worktree（写权限由隔离换取，主仓零污染，产出 diff）；重试属 Runner 不属路由
- muster-skill：技能（SKILL.md，与 codex 同一约定：仓库级 .agents/skills/ + 个人级 ~/.muster/skills/）；只从本地发现不联网、目录进提示词而正文经 read_skill 按需读、审计只记哈希（skill.offer / skill.read）、自报密级即成为标签来源
- muster-workflow：自定义工作流（WORKFLOW.md：schedule / skills / egress / outputs / channel + 步骤正文；仓库级 .agents/workflows/ + 个人级 ~/.muster/workflows/）；工作流自有仓库产出自动合入（只合自己名下的仓库，不走审批）；http_get 只许白名单主机、每次落 tool.egress 计入演习报告；调度器 v0 在桌面壳里（错过补跑一次），入口在我的工作台 › 任务 › 工作流；PDF 靠本机 Chrome headless，pptx 未实现
- muster-roster：Agent 编制（AGENT.md 岗位卡：badge / name / role / aliases / skills / tools 只读或隔离可写 / commands 白名单 / model / sensitivity_max / default；仓库级 .agents/roster/ + 个人级 ~/.muster/roster/）；工牌号 = 审计 actor id；能力子集哈希进 approval.request，变了落 badge.update；密级上限是硬闸；点将顺序：消息开头 @ > 界面选中的工牌 > 频道默认 > 名册默认；名册为空退回内置 A-007；delegate 工具让主 Agent 把子任务委派给另一张工牌（同一隔离工作区、串行、只一层、档位取交集，父 run 落 run.delegate）；频道编制 channel_agents 存 state.db，引入/请离写 channel.roster，非空时只能点已引入的工牌、第一张是频道默认
- muster-gateway：对外 Responses / 对内 chat，让 codex 用上 chat 系模型
- muster-prompt：A1 系统提示词唯一出处（改它必须重跑 A7 评测）
- muster-identity：P2 权限语义内核；can() 判定纯函数按 §4.4 顺序，五不变量穷举验证（13,500 组）
- muster-server：P2/P3 服务端（Axum + PostgreSQL）；权限判定复用 muster-identity 不另写一份；
  服务端不持有源码、不做全局审计链（节点各自留链，组织侧靠锚定——尚未实现）；
  依赖用 deploy/docker-compose.yml 一条命令拉起
- apps/desktop：智能协作平台桌面壳（Tauri 2，独立工作区，pnpm tauri dev）

## 约定
- 动手前后都跑 cargo test（当前基线 293 项全绿；桌面壳另有 42 项需在 apps/desktop/src-tauri 下跑）
- 每个任务一个 commit；不顺手重构已锁死的公共接口
- 设计决策都在各 crate 的 lib.rs 文档注释里，先读再改
- 总体规划在 docs/（16 周 P0–P6，任务号 P0-01…P6-08）；A2/A7/E1–E3/A9 等是其细化分解；codex fork 分仓在 ../muster-codex
- **服务端动手前先读 docs/服务端架构.md**（六条硬边界 + 分阶段 + 未决三项）；改完回来更新它
