# Muster workspace

- `muster-provider` — A2 模型层抽象(设计决策见其 README 与 lib.rs)
- `muster-eval` — A7 工具调用评测集(G0′ 闸门证据产出)
- `muster-route` — E1 类型层 + E2 路由决策器(五不变量穷举验证)+ E3 会话棘轮
- `muster-audit` — A9 审计事件层("8 幕 → SQL"对照见其 README)

总体规划与概念稿见 `docs/`(任务号 P0-01…P6-08 出自总规划);codex fork 在同级目录 `../muster-codex`。
跑全部测试:`cargo test --workspace`
