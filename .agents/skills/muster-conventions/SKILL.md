---
name: muster-conventions
description: 在 muster 仓库里改代码前必读的约定:五条铁律、改前改后跑 cargo test、设计决策读 lib.rs、一个任务一个 commit。
---

# muster 仓库约定

改任何代码之前先读根目录 CLAUDE.md;这里只列 Agent 最容易漏掉的几条。

## 动手前
1. 读目标 crate 的 `src/lib.rs` 顶部文档:设计决策都写在那里,先读再改。
2. 跑一次 `cargo test --workspace`,记下基线数字;基线不绿就先报告,不要在红的基线上改。

## 改动中
- 五条铁律违反即回滚:密级只升不降;绝不静默升云;审计只存哈希不存正文;fail-closed;serde_json 不开 preserve_order。
- 不顺手重构已锁死的公共接口;给结构体加字段优先保证 `..Default::default()` 的旧构造点不破。
- 改了 muster-prompt 的 SYSTEM_PROMPT 必须重跑 A7 评测。

## 交付前
- 再跑 `cargo test --workspace`,把改前改后的数字写进结论;桌面壳另有测试要在 apps/desktop/src-tauri 下跑。
- 一个任务一个 commit,主题用中文说清"为何而改"。
