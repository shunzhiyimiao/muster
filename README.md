# Muster · 点将台

**完全本地部署的团队 + Agent 协作系统。** 人、Agent、频道、任务、审批与审计在同一个平台上;
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

# 桌面端(点将台 UI:控制台 / 个人工作台 / 团队频道 / 审计中心 / 主权演习)
cd apps/desktop && KIMI_API_KEY=… pnpm tauri dev

# 模型网关(让只讲 Responses 的 Codex 用上任何 chat 系模型)
KIMI_API_KEY=… cargo run -p muster-gateway -- --config provider.example.toml --provider kimi

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
| `muster-gateway` | — | 模型网关:对外讲 Responses,对内讲 chat |
| `muster-eval` | A7 | 20 固定样本的工具调用评测,确定性判卷,退出码可挂 CI |
| `muster-prompt` | A1 | Agent 系统提示词的唯一出处(执行器与评测同源) |
| `apps/desktop` | P1-07~09 | 点将台桌面壳(Tauri 2 + React;独立工作区) |

设计决策写在各 crate 的 `lib.rs` 文档注释与 README 里,**改代码前先读**。
总体规划(16 周 P0–P6,任务号 P0-01…P6-08)与概念稿见 `docs/`;
Codex 受控 fork 在同级目录 `../muster-codex`(分仓,保持最小 diff 以便 rebase 上游)。
