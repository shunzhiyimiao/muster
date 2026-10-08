# muster-skill — 技能(SKILL.md 约定的过程性知识包)

一句话:**把"怎么做"从提示词里搬出来,变成可发现、可哈希、可审计的文件。**

技能是人写的过程性知识(检查清单、步骤、约定),格式与 openai/codex 的
codex-skills 同一约定——桌面壳与受控 fork 读的是同一份文件,不必维护两套。

## 与 Capsule 的分工

- **技能**回答"做这类事的时候该怎么做":没有出处运行、没有验真,更轻也更早——
  团队在跑出第一个可锻造的运行之前,就已经有一堆想让 Agent 遵守的做法。
- **Capsule**(muster-runner 的 capsule 模块)锻造自一次成功的运行,带重放引用与
  验真记录,回答"这件事以前做成过,照着再做一次"。

两者不互相替代:技能可以被任何运行使用(那次运行的 `skill.offer` 会记下用了哪些、
什么版本),但技能本身不进能力库。

## 技能文件格式

`<根目录>/<name>/SKILL.md`,YAML frontmatter 里 `name` 与 `description` 必填,
可选 `metadata.sensitivity`(open / internal / restricted)。仓库级技能目录约定为
`.agents/skills/`(常量 `REPO_SKILLS_DIR`),随代码提交即完成团队共享。

最小示例(`.agents/skills/demo/SKILL.md`):

```markdown
---
name: demo
description: 演示技能:改代码前先跑 cargo test 记基线
metadata:
  sensitivity: internal
---

# Demo

第一步:先跑 cargo test。
```

硬性约定:

- `name` 必须与所在目录同名——这是"read_skill 参数 → 磁盘上哪个文件"不需要
  额外索引的唯一办法。
- `name` 规则:小写字母、数字、连字符;不以连字符开头结尾,不连续两个连字符;
  至多 64 字符(`NAME_MAX`)。它既是目录名也是工具参数,宽了会出事。
- `description` 至多 1024 字符(`DESCRIPTION_MAX`):它原样进提示词目录,
  写长了就是在挤任务本身。
- 只解析 frontmatter 里用到的几个键的单行标量取值,不引入 YAML 解析器:
  多行值(`|` / `>`)、流式集合(`[...]` / `{...}`)一律拒绝并说明原因,而不是猜。
  其余键(license、compatibility、allowed-tools…)原样忽略,codex 的技能放进来
  不该因为多写了几个键就被拒。

## 基本用法

### 1. 发现:`discover`

`REPO_SKILLS_DIR` 是相对仓库根目录的常量(`.agents/skills`),`discover` 拿到什么
路径就在什么路径下找——直接用 `PathBuf::from(REPO_SKILLS_DIR)` 是**相对当前
工作目录**的,进程不在仓库根启动就会找不到技能。正确做法是**拼上仓库根**:

```rust
use std::path::Path;

// 仓库根 = crate 清单目录的上一级;集成测试 / 例子都这么写,二进制按部署约定换
let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
let discovery = muster_skill::discover(&[repo_root.join(muster_skill::REPO_SKILLS_DIR)]);
for skill in &discovery.skills {
    println!("{}:{}", skill.name, skill.description);
}
for problem in &discovery.problems {
    eprintln!("技能未加载:{problem}");
}
```

结果分 `skills` 与 `problems`:一个技能解析失败只影响它自己,原因进 `problems`
给界面显示,不拖垮整批;同名冲突先到先得(根目录顺序即优先级),后者记为问题。
根目录不存在、没有 SKILL.md 的子目录、以 `.` 开头的目录(如 codex 的 `.system`
缓存)都静默跳过。

### 2. 进系统提示词:`render_catalog`

```rust
let catalog = muster_skill::render_catalog(&discovery.skills);
// catalog 非空时拼进系统提示词;空技能集返回空串,调用方自行决定是否加这一段
```

渐进披露:提示词里只放目录(每个技能一行,名字 + 一句描述 + 密级),正文由
Agent 在需要时经 `read_skill` 工具读取。技能是公共上下文,每个字都在跟任务
本身抢窗口。

### 3. 按需读正文:`SkillMeta::read_body`

```rust
match skill.read_body() {
    Ok(body) => { /* 交给 Agent 按其中的步骤执行 */ }
    Err(muster_skill::SkillError::Tampered { name, .. }) => {
        eprintln!("{name} 在发现之后被改过,拒绝提供");
    }
    Err(e) => eprintln!("读取失败:{e}"),
}
```

`read_body` 会先对整个文件重算 SHA-256 并与发现时比对:不一致即返回 `Tampered`
错误拒绝提供——审计里 `skill.offer` 记的哈希才是本次运行看到的版本,不能让
Agent 读到另一份。**"读不到"与"读到的不是发现时那份"是两种错误,处置完全不同。**

### 4. 审计与路由:`audit_ref` / `label_source`

```rust
let reference = skill.audit_ref(); // SkillRef { name, content_hash }:只存哈希不存正文(铁律 3)

if let Some(source) = skill.label_source() {
    // 自报了 sensitivity 的技能作为 LabelSource 进入路由决策(铁律 1:只会抬高,不会降低)
}
```

`audit_ref()` 给的是名字 + 哈希,正文留磁盘;`label_source()` 把声明了密级的技能
变成标签来源(origin = Manual,subject = `skill:<name>`),会话棘轮随之抬升——
技能正文里的做法本身可能就是敏感信息,不能因为它"只是说明书"就放它上云。

## 关键设计决策

1. **只从本地目录发现,不联网**。总体规划把"公网 MCP / Skill 下载"列为禁止项
   (内部镜像 allowlist 属后续);本 crate 连 HTTP 依赖都没有,想加也加不上。
2. **渐进披露**:目录进提示词,正文按需读。
3. **审计只存哈希**(铁律 3):发现时取 SHA-256,读取时重新计算并比对,与 Capsule
   存储的 `load` 同一姿态。
4. **声明密级即成为标签来源**(铁律 1)。
5. **坏文件不让整批失败**。

## 诚实边界(v0)

- 没有版本号与升级机制:同名即同一技能,新旧只能靠哈希区分。
- 没有"从运行锻造技能":那是 Capsule 的路;把 Capsule 导出成 SKILL.md 属后续。
- 影子重放暂不携带技能:重放用的是空技能集,若源运行读过技能,比对结果可能因此
  有差异——`skill.read` 事件能看出这一点,但 v0 不自动对齐。
- 团队共享靠仓库(`.agents/skills/` 随代码提交);服务端分发未做。
- 技能目录里的脚本、参考文件不会被读取,Agent 只看得到 SKILL.md 本身。

## 构建与测试

在仓库根目录执行:

```bash
# 只编译本 crate(开发期快速检查)
cargo build -p muster-skill

# 本 crate 的全部测试(含 lib.rs 末尾的单元测试与集成测试)
cargo test -p muster-skill

# 整个工作区的测试(改动影响多个 crate 时跑这个,口径与根 README 一致)
cargo test --workspace
```

`Cargo.toml` 里 `rust-version = "1.85"`,需要 Rust 1.85 或更高。
