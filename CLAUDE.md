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

## 约定
- 动手前后都跑 cargo test（当前基线 76 项全绿）
- 每个任务一个 commit；不顺手重构已锁死的公共接口
- rustc ≥1.85 可删 muster-provider/Cargo.toml 的 version-steering pin 块（README 有说明）
- 设计决策都在各 crate 的 lib.rs 文档注释里，先读再改
