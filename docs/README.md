# Muster 文档索引

## 现行总体规划
《Constellation_Codex_本地协作系统_可执行开发计划.docx》(v1.0,2026-07-19)——**muster 的正式规划**。立项早于定名,文档沿用了原始项目名"Constellation Codex"。纯文本版见《总体规划_文本版.txt》(textutil 抽取,便于全文检索)。

要点:16 周 P0–P6 分期,任务号 P0-01…P6-08,每阶段有硬闸门;已定架构决策——Fork openai/codex(与协作系统分仓)、Axum + PostgreSQL + Keycloak/OIDC、Tauri 2 + React、Transactional Outbox、每 Task Run 独立 Git Worktree、Runner 用服务账号 + mTLS。

## UI 愿景
《Muster点将台-概念稿.html》/《Muster点将台-概念稿-v2.html》——桌面端信息架构:编制管理(工牌 A-007 代码评审员等)、频道消息、会议室、能力库、审计中心、主权演习、路由中心。其中 RUN-2231、"外发 0 B"等叙事与 muster-audit README 的"8 幕 → SQL"对照表互锁。

## 历史概念(非现行计划)
`history/Constellation_M0_Verified_Workbench_Development_Plan_v0.2.md`——更早一代"可验证补丁工作台"概念稿。注意其中 A0/A1/B/C/U 是**验证等级**,与任务编号无关。

## 任务号对照:总规划 P0-x ↔ 细化分解 A/B/C/E(2026-09-23 快照;上一版 2026-07-29)

| 总规划任务 | 细化编号 | 状态 |
|---|---|---|
| P0-01 Fork codex + upstream 同步策略 | — | ✅ `~/muster-codex`,upstream=openai/codex,基点 fe01054a28,策略见其 FORK.md |
| P0-02 Third-party Notice 与许可证清单 | — | ✅ THIRD-PARTY-NOTICES.md(依赖变更后需重新生成);Muster 自身 AGPL-3.0-or-later(2026-09-23 定) |
| P0-03 本地 Provider | A2 muster-provider | ✅ wire=chat/completions;与 Responses API 的对齐由 muster-gateway 完成 |
| P0-04/05 Offline Mode + 零公网验证 | (E4 外发计量可反哺) | **未过**:providers.toml 已配 Ollama 本地位,但本机未装 Ollama,A7 从未在本地模型上跑过 |
| P0-06 thread/turn/流式事件验证 | — | ✅ 2026-07-30 P1 闸门:fork 经 muster-gateway 用 Kimi 完整改代码 |
| P0-07 初始化 Rust Workspace | — | ✅ 11 个 crate,`cargo test --workspace` 246 项全绿 |
| P0-08 TaskEvent 与错误协议 | A9 + B1 | ✅ 证据侧 EventBody;Runner 侧 RunnerEvent / RunnerError |
| 风险 #1"本地模型 Tool Call 能力不足" | A7 muster-eval | 测量仪就绪;云端 kimi-k3 95%(a1-v2);**本地模型未测**(同 P0-04/05) |
| §8 安全设计 + P5-05/07 审批审计 | E1 E2 E3 + A9 | ✅ 密级/棘轮/哈希链;审批流 request_merge/decide 落地,拒绝也写审计 |
| P1-02/03 Codex Client + Local Task Manager | B1 | Runner 全部落地(工具循环、隔离 worktree、diff、审批);Codex 经网关已过闸;桌面壳未接 codex app-server(用的是 muster-runner) |
| P1-04 Git Worktree Manager | — | ✅ 每 run 独立 worktree + 保留策略 + 合入后回收 |
| P1-05 SQLite 本地存储 | C1 | ✅ `~/.muster/desktop-state.db`,与审计库分离 |
| P2 权限语义内核 / P2-P3 服务端 | muster-identity / muster-server | 内核 ✅(can() 纯函数,五不变量穷举);服务端阶段 1–5 落地(见《服务端架构.md》§8);锚定、审批上服务端、OIDC 未做 |
| 会议链路(《服务端架构.md》§7) | muster-meeting-agent | ✅ 常驻 daemon 认领、听会、本地 whisper 转写、@应答、会中派活;切句仍是能量阈值,未换 Silero VAD |
| P4 工作流共享 | 能力库 / Capsule | ✅ 锻造 → 存储(哈希防篡改)→ 影子重放验真 → 跨团队引入 |
| 技能(规划外,与 codex 同一 SKILL.md 约定) | muster-skill | ✅ v0(2026-09-22):本地发现、目录进提示词、read_skill、skill.offer / skill.read 只记哈希 |
| 自定义工作流(规划外,入口:我的工作台 › 任务 › 工作流) | muster-workflow | ✅ v0(2026-09-23):定时(cron)+ 技能 + 白名单 http_get + 自有仓库自动合入 + html/pdf 产物;调度器在桌面壳内;pptx 未实现 |
| Agent 编制(多 Agent 第一步) | muster-roster | ✅ v0(2026-09-23):写死的 A-007 变成名册;聊天/任务/工作流按工牌装配提示词、技能、命令白名单、能力哈希;Agent 档案页列名册;第二步 ✅ 委派(delegate 工具:同一隔离工作区里的子 run,run.delegate 相连);第三步 ✅ 频道编制(频道顶部引入/请离工牌,写 channel.roster;非空时只能点已引入的) |

细化分解的总表(A1–A9 / B / C / D / E / G0′ / W3 / 8幕 全集)不在磁盘上;找到后请入库本目录。
