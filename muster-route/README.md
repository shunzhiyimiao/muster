# muster-route(任务 E1 类型层 + E2 · v0.1)

敏感度标签模型 + 路由决策器。一句话:**标签跟数据走,降级可以自动,升级永不自动。**

## 五条不变量(穷举验证,`tests/matrix.rs`,16,384 种输入组合)

- **I1** restricted 或演习中 ⇒ 落点必为本地,否则拒绝;云端绝不出现在落点。
- **I2** fail-closed 降落带(fallbacks)里永远没有云端——链上只有首位可以是云。
  「本地挂了升云端」在数据结构上没有表达方式,不靠运行时 if 守卫。
- **I3** 只要配置了本地 provider,除"未知 id"配置错外,决策永不拒绝。
- **I4** 云端未被排除时,用户点名被原样尊重。
- **I5** 决策是确定性纯函数:同输入必同输出。

## 决策优先级(降级原因 = 前端文案键,`DowngradeReason::text_zh()`)

1. `egress_locked`(E6 演习/纯内网)→ 仅本地
2. 有效密级 = restricted → 仅本地(**硬编码不变量**,策略不可放开)
3. 有效密级 > 组织 `cloud_max` → 仅本地
4. 其余 → 云/本地皆可

有效密级 = max(频道, 仓库, 手动, 会话锁),并回传促成者清单——徽章悬浮
与审计都能回答"为什么是这个级别、为什么落在这里"。

## 各泳道怎么接

- **D6 徽章**:`RoutePlan { primary, primary_locality, downgraded }` + `text_zh()`。
- **E3 棘轮**:状态机落 E3;锁定后往 `sources` 注入
  `LabelSource { origin: SessionLock, level: Restricted, subject: "session:…" }` 即生效。
- **E6 演习**:`Router::set_egress_locked(true/false)`,运行时全局翻转,
  复用同一条 fail-closed 路径(演习不是特殊逻辑,是策略的一个取值)。
- **E4 审计**:`RoutePlan` 与 `Attempt` 全量 `Serialize`,含策略快照,直接落库对账。
- **Runner(B1)**:`Router::resolve()` 返回探活成功的 `Arc<dyn ModelProvider>` +
  完整轨迹;拿去开流即可。

## 诚实边界

- **中流失败不归路由层**:流建立后的断线由 Runner 决定重试/落败;路由层只保证
  「落点合法 + 落点探活」。理由:路由层重试会把半份输出变成用户可见的重放鬼故事。
- 探活(health_check)≠ 该 provider 能成功完成推理;它挡得住"进程没起/断网",
  挡不住"模型超载 503"。后者会以 `should_failover()` 错误浮给 Runner,由 Runner
  决定是否用同一条链重开(链本身已保证合法性)。
- 默认密级 = Open 是**产品决策**(未标注仓库要能走云端,演示第 2 幕);
  保守部署把 `cloud_max` 压到 Open 之下即等效禁云。
- E1 的持久化 CRUD(标签存 SQLite)在 C1 落地;本 crate 只交类型与语义。
