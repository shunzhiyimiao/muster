# muster-audit(任务 A9)— 审计事件层 v0

**定位:这不是日志系统,是产品的证据层。** 三个消费者在设计前已存在:
第 8 幕演习报告、Capsule 锻造(从成功运行的事件链复现能力)、工牌审批追溯。

**G1 验收口径:演示里出现的每一个数字,都能用一条 SQL 从审计表里查出来。**
`tests/acts.rs` 就是这句话的可执行形态——事件脚本即 8 幕的后端剧本。

## 8 幕 → SQL 对照表

| 演示时刻 | 数字/文案 | 出处 |
|---|---|---|
| 第 3 幕 会话锁定置灰 | 「会话曾引用 restricted 资源」+ 肇因 | `SQL_SESSION_LOCK`(`session_lock`,取该 session 最近一次抬升) |
| 第 7 幕 徽章悬浮 | 「数据密级为 restricted:已强制本地执行…」 | `SQL_DOWNGRADES` + `DowngradeReason::text_zh()`(`downgrades_zh`) |
| 第 7 幕 红色拒绝态 | 「本地不可用,fail-closed 拒绝」+ 逐落点失败轨迹 | `route.refuse` 事件(class + reason + attempts) |
| 第 8 幕 演习报告 | 外发字节数 **0 B**、本地/云端调用数 | `SQL_DRILL_REPORT`(`drill_report`,fail-closed:任何 `unmetered` 判不达标) |
| 工牌页三宫格 | 待审批 **1 项 → 0 项** | `SQL_PENDING_APPROVALS`(`pending_approvals`) |
| Capsule 锻造入口 | RUN-2231 完整事件链 + ReplayRefs | `SQL_RUN_CHAIN`(`run_chain`) |
| 尽调问「能改吗」 | 哈希链逐行校验,篡改定位到行 | `AuditStore::verify_chain` |

## 表结构(单表 append-only)

信封列:`event_id`(ULID,字典序=时间序) / `ts_ms` / `actor_kind`+`actor_id`
(human / agent 工牌号 / system 组件) / `event_type`(点分命名空间) /
`run_id` / `session_id` / `team`+`channel` / `label` / `locality` /
`policy_version` / `schema_version` / `payload`(JSON,自描述) /
`prev_hash`+`hash`(SHA-256 链)。

索引由 8 幕反推:`run_id`、`ts_ms`、`(actor_id, ts_ms)`、`(event_type, ts_ms)`。

## 事件类型 v1

`run.start`(携带 **ReplayRefs**,见下) / `run.finish` / `model.call`
(**外发记账唯一来源**,A2 传输层计量) / `route.decide`(记依据:有效密级
+促成来源+策略版本+DowngradeReason) / `route.refuse`(**拒绝也是证据**:
分类 refused:\*/exhausted + 完整理由 + Exhausted 时内嵌决策与逐落点失败,
构造经 `EventBody::route_refuse` 单一出处) / `approval.request`(记「申请
能力 vs 工牌能力」差值) / `approval.decision` / `badge.update` /
`policy.update` / `session.lock.raise`(E3 棘轮抬升,记污染发生的**瞬间**) /
`drill.start` / `drill.end`。

v1.x 预留(占位注释在 `lib.rs`,零成本):`capsule.forge/verify/adopt`、
`session.stream.start`、`stream.viewer.join`、`session.stream.stop`、
`share.block`、`convo.share`、`meeting.transcribe`。

## Capsule-ready 检查单(新事件类型上线前必过)

1. 重放所需的一切,要么在 payload,要么是 `ContentHash` 内容寻址引用?
2. 正文(prompt/输出/命令原文)**没有**进审计表,只有哈希?
3. `EventBody` 变体已加入(写侧穷举),serde tag 与 `event_type()` 一致?
4. 读侧旧版本遇到该类型会落入 `Parsed::Unknown` 而不是报错?(自动满足)
5. 该类型服务的"演示数字"写进了 `tests/acts.rs` 或对照表?

## 明确不做(MVP)

- **签名与外部锚定**:哈希链防篡改够 MVP 叙事;私钥管理是 v1.x。
- **多节点合并**:ULID 单调生成器按单节点设计。
- **保留/删除策略**:append-only 与删除权冲突,已登记 open question——方向
  是正文侧加密擦除、审计只留哈希,不在本 crate 解。
- **serde_json 的 `preserve_order` feature 永久禁用**:规范化哈希依赖
  BTreeMap 键序,开启即历史哈希全部失效。

## 与既有 crate 的关系

类型全部复用:`Sensitivity`/`LabelSource`/`DowngradeReason` 来自 muster-route,
`Locality` 来自 muster-provider——审计和路由在类型系统层面说同一种话。
本次对 muster-route 的唯一改动:`DowngradeReason` 补 `Deserialize`(读路径需要,
非破坏性)。
