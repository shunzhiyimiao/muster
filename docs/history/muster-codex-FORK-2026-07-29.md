# muster-codex — openai/codex 的受控 fork

- **upstream**:https://github.com/openai/codex.git(remote 名即 `upstream`)
- **fork 基点**:`fe01054a28`(2026-07-29 克隆;`--filter=blob:none` 部分克隆,提交图完整,可正常 fetch/rebase)
- **origin**:待定——GitHub org 或内网 Git 定下后 `git remote add origin …`
- **工具链**:随仓库 `codex-rs/rust-toolchain.toml` pin(当前 1.95.0),不改动

## 改动范围(总规划 §10.1,超出必须先加 ADR)

只允许:Offline Mode、本地 Provider 策略、企业审计 Hook、必要的协议扩展。
Group / Channel / Chat 等协作功能一律不进本仓——写在 muster 协作层。

## 同步策略

- 我方补丁全部落在 **`muster` 分支**,与 `upstream/main` 保持最小 diff
- 定期(建议每周):`git fetch upstream && git rebase upstream/main`;冲突时优先改我方补丁以贴合上游
- 每次 rebase 后必须原样 `cargo build`(codex-rs/)+ 冒烟,再继续开发
- 许可证:Apache-2.0;保留上游 LICENSE / NOTICE;对外分发前补 Third-party Notice 清单(总规划 P0-02)
