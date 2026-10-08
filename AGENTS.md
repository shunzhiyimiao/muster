# Muster 仓库说明(给 Agent 读)

与 Codex 同一约定的 AGENTS.md:Muster 自己的工牌(小七、阿岩……)和 Codex 都会把它读进上下文。
这里只放"在这个仓库里永远要守什么";细节与设计决策在各 crate 的 `lib.rs` 文档注释和 README 里,改之前先读。

## 五条铁律(违反即回滚)

1. 密级只升不降:`SessionRatchet` 没有降低 API;`Sensitivity` 的比较就是楼层。
2. 绝不静默升云:`RoutePlan.fallbacks` 在类型上只收本地 provider。
3. 审计只存哈希不存正文:`ContentHash` 进表,正文留在 run 存储侧。
4. fail-closed:外发字节测不到就按违规记(`EgressBytes::Unmetered`)。
5. `serde_json` 永不开 `preserve_order`:哈希链依赖 BTreeMap 的键序。

## 做事的规矩

- 改前改后都跑 `cargo test`(工作区在根目录跑;桌面壳在 `apps/desktop/src-tauri` 下另跑);没跑过的改动不交。
- 一个任务一个 commit;合入主仓一律经人审批,Agent 不自己 commit、不 push、不开 PR。
- 不顺手重构已锁死的公共接口;拿不准就如实说"不确定"。
- 技能放 `.agents/skills/<name>/SKILL.md`,编制放 `.agents/roster/<工牌>/AGENT.md`,工作流放 `.agents/workflows/<name>/WORKFLOW.md`;目录名必须与 name 一致。
- 与人沟通用中文,简洁直接;评审意见落到文件与行号。
