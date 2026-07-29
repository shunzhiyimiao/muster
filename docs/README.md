# Muster 文档索引

## 现行总体规划
《Constellation_Codex_本地协作系统_可执行开发计划.docx》(v1.0,2026-07-19)——**muster 的正式规划**。立项早于定名,文档沿用了原始项目名"Constellation Codex"。纯文本版见《总体规划_文本版.txt》(textutil 抽取,便于全文检索)。

要点:16 周 P0–P6 分期,任务号 P0-01…P6-08,每阶段有硬闸门;已定架构决策——Fork openai/codex(与协作系统分仓)、Axum + PostgreSQL + Keycloak/OIDC、Tauri 2 + React、Transactional Outbox、每 Task Run 独立 Git Worktree、Runner 用服务账号 + mTLS。

## UI 愿景
《Muster点将台-概念稿.html》/《Muster点将台-概念稿-v2.html》——桌面端信息架构:编制管理(工牌 A-007 代码评审员等)、频道消息、会议室、能力库、审计中心、主权演习、路由中心。其中 RUN-2231、"外发 0 B"等叙事与 muster-audit README 的"8 幕 → SQL"对照表互锁。

## 历史概念(非现行计划)
`history/Constellation_M0_Verified_Workbench_Development_Plan_v0.2.md`——更早一代"可验证补丁工作台"概念稿。注意其中 A0/A1/B/C/U 是**验证等级**,与任务编号无关。

## 任务号对照:总规划 P0-x ↔ 细化分解 A/B/C/E(2026-07-29 快照)

| 总规划任务 | 细化编号 | 状态 |
|---|---|---|
| P0-01 Fork codex + upstream 同步策略 | — | 进行中:`~/muster-codex`,upstream=openai/codex,基点 fe01054a28 |
| P0-02 Third-party Notice 与许可证清单 | — | 未动(Apache-2.0,LICENSE/NOTICE 已随 fork 保留) |
| P0-03 本地 Provider | A2 muster-provider | ✅(wire=chat/completions;与"Responses API 网关"的对齐方式在 P0 内定) |
| P0-04/05 Offline Mode + 零公网验证 | (E4 外发计量可反哺) | 未动 |
| P0-06 thread/turn/流式事件验证 | — | 未动(依赖 fork 跑通) |
| P0-07 初始化 Rust Workspace | — | ✅ `cargo test --workspace` 76 项全绿 |
| P0-08 TaskEvent 与错误协议 | A9(证据侧已就绪) | 半:Runner 侧 TaskEvent 未定 |
| 风险 #1"本地模型 Tool Call 能力不足" | A7 muster-eval | 测量仪已备,待真机(需 API key / 本地 Ollama) |
| §8 安全设计 + P5-05/07 审批审计 | E1 E2 E3 + A9 | ✅ 提前拉动,比规划的二值 Offline Mode 更细(密级/棘轮/哈希链) |
| P1-02/03 Codex Client + Local Task Manager | B1 | 未动 |
| P1-04 Git Worktree Manager | — | 未动 |
| P1-05 SQLite 本地存储 | C1 | 未动 |
| P4 工作流共享 | 能力库 / Capsule | 未动(A9 已留 capsule.* 事件占位) |

细化分解的总表(A1–A9 / B / C / D / E / G0′ / W3 / 8幕 全集)不在磁盘上;找到后请入库本目录。
