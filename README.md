<p align="center"><img src="icon/muster-icon.png" width="112" alt="Muster"></p>

# Muster · 智能协作平台

**可完全本地部署的 AI 原生团队协作系统,兼容 Codex 的技能与接口约定。** 人、Agent、频道、任务、审批与审计在同一个平台上;
模型可以是云端的,也可以是内网/本机的——**由数据密级决定,而不是由用户随手选**。

三条产品主张,都做成了代码里的结构性保证而非约定:

| 主张 | 实现形态 |
|---|---|
| **密级只升不降** | 会话棘轮无降低 API;有效密级 = max(频道, 仓库, 手动, 会话锁) |
| **绝不静默升云** | 降落带类型上只收本地 provider——"本地挂了顶到云端"在数据结构里写不出来;落不下就 fail-closed 拒绝 |
| **每个数字都能查** | append-only + SHA-256 哈希链的审计层;演示里的每个数字都由一条 SQL 从审计表查出,只存哈希不存正文 |

## 现在能跑什么

```bash
cargo test --workspace                     # 全部测试

# 桌面端(智能协作平台 UI:控制台 / 个人工作台 / 团队频道 / 审计中心 / 主权演习)
cd apps/desktop && KIMI_API_KEY=… pnpm tauri dev

# 模型网关(让只讲 Responses 的 Codex 用上任何 chat 系模型;每个请求过密级路由并落审计链)
KIMI_API_KEY=… cargo run -p muster-gateway -- --config provider.example.toml --provider kimi --sensitivity internal

# 工具调用评测(闸门证据)
cargo run -p muster-eval -- --list
```

## Crate 地图

| Crate | 任务 | 职责 |
|---|---|---|
| `muster-provider` | A2 | OpenAI 兼容传输层;`Locality` 一等元数据;**外发计量唯一咽喉** |
| `muster-route` | E1/E2/E3 | 敏感度标签 + 路由决策纯函数(五不变量穷举验证)+ 会话棘轮 |
| `muster-audit` | A9 | 审计事件层:append-only + 哈希链;"8 幕 → SQL"对照见其 README |
| `muster-runner` | B1 | 任务执行器:路由 → 流式工具循环 → Capsule-ready 审计链 |
| `muster-skill` | — | 技能:SKILL.md 过程性知识包(与 Codex 同一约定,仓库级 `.agents/skills`/`.codex/skills`、个人级 `~/.muster/skills`/`~/.agents/skills` 都读);目录进提示词、正文经 `read_skill` 按需读取、审计只记哈希 |
| `muster-workflow` | — | 自定义工作流:WORKFLOW.md 定义 + cron 定时 + 自有仓库自动合入 + 白名单抓取(tool.egress 计量)+ 报告产物(html / pdf) |
| `muster-roster` | — | Agent 编制:AGENT.md 岗位卡(工牌、岗位、别名、技能、工具档位、命令白名单、密级上限);能力哈希进审批与 badge.update;@别名 指派;delegate 把子任务委派给另一张工牌;频道可引入多张工牌 |
| `muster-gateway` | — | 模型网关:对外讲 Responses,对内讲 chat |
| `muster-eval` | A7 | 20 固定样本的工具调用评测,确定性判卷,退出码可挂 CI |
| `muster-prompt` | A1 | Agent 系统提示词的唯一出处(执行器与评测同源) |
| `apps/desktop` | P1-07~09 | 智能协作平台桌面壳(Tauri 2 + React;独立工作区) |

## 许可与第三方组件

第三方依赖清单与许可证记录见 [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md)
(`cargo metadata` 生成,依赖变更后需重新生成)。同级目录 `../muster-codex` 是 openai/codex 的只读参考快照
(未修改、不随 Muster 分发,因此没有 Apache-2.0 §4(b) 的修改声明义务);日后若引入其代码,按 THIRD-PARTY-NOTICES 流程登记。

Muster 自身采用 **AGPL-3.0-or-later**(2026-09-23 定,全文见根目录 `LICENSE`):自托管部署
与内部修改不受影响;把修改版作为网络服务提供给他人时,须向使用者提供对应源码(§13)。
各 crate 的 `Cargo.toml` 与桌面壳 `package.json` 均已声明同一 SPDX 标识。

设计决策写在各 crate 的 `lib.rs` 文档注释与 README 里,**改代码前先读**。
总体规划(16 周 P0–P6,任务号 P0-01…P6-08)与概念稿见 `docs/`。

## 与 Codex 的关系

Muster 自带运行时(muster-runner),**不以 Codex 为执行器**;从 Codex 借用的是三样约定:
SKILL.md 技能格式(仓库级 `.agents/skills/` 与 Codex 同一份文件)、Responses 线协议(经 muster-gateway
让只讲 Responses 的 Codex 用上 chat 系模型;经网关的流量同样过密级路由并落审计链)、会话分叉语义,并按同一规则读仓库根的 AGENTS.md
(`AGENTS.override.md` 优先,32 KiB 预算)。个人技能也兼读 Codex 的 `~/.agents/skills`;命令执行的出网闸照它 network-proxy 的形态实现(回环代理 + 白名单 + 逐连接记账,不链接其代码)。
下一步:进程级沙箱(sandbox-exec / netns)把出网只放给代理端口。同级目录 `../muster-codex` 是固定在 rust-v0.161.0 的只读参考快照,2026-10-08 起不再是 fork(原 FORK.md 存档在 `docs/history/`)。
这一定位在 2026-10-08 的吻合度评审后确定(路 B),见 `docs/评审-与Codex吻合度-2026-10-08.md`。
