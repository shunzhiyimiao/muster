//! # muster-skill — 技能(SKILL.md 约定的过程性知识包)
//!
//! 一句话:**把"怎么做"从提示词里搬出来,变成可发现、可哈希、可审计的文件。**
//!
//! ## 与 Capsule 的分工
//!
//! - Capsule(muster-runner 的 capsule 模块,P4)锻造自**一次成功的运行**,带重放引用与
//!   验真记录,回答的是"这件事以前做成过,照着再做一次"。
//! - 技能是**人写的过程性知识**(检查清单、步骤、约定),没有出处运行、没有验真,
//!   回答的是"做这类事的时候该怎么做"。它更轻,也更早——一个团队在跑出第一个
//!   可锻造的运行之前,就已经有一堆想让 Agent 遵守的做法。
//!
//! 两者不互相替代:技能可以被任何运行使用(那次运行的 `skill.offer` 会记下用了哪些、
//! 什么版本),但技能本身不进能力库。
//!
//! ## 格式:与 codex 同一份 SKILL.md
//!
//! `<根目录>/<name>/SKILL.md`,YAML frontmatter 里 `name` 与 `description` 必填,
//! 可选 `metadata.sensitivity`(open / internal / restricted)。与 openai/codex
//! (即 ../muster-codex)的 `codex-skills` 同一约定:仓库级技能放 [`REPO_SKILLS_DIR`],
//! **桌面壳与受控 fork 读的是同一份文件**,不必维护两套。
//!
//! 只解析 frontmatter 里用到的几个键(`name` / `description` / `metadata.*` 的单行
//! 取值),不引入 YAML 解析器:多行值、流式集合等一律拒绝并说明原因,而不是猜。
//!
//! ## 关键设计决策
//!
//! 1. **只从本地目录发现,不联网**。总体规划把"公网 MCP / Skill 下载"列为禁止项
//!    (内部镜像 allowlist 属后续);本 crate 连 HTTP 依赖都没有,想加也加不上。
//! 2. **渐进披露**:提示词里只放目录(名字 + 一句描述,见 [`render_catalog`]),正文由
//!    Agent 在需要时经 `read_skill` 工具读取。技能是公共上下文,每个字都在跟任务本身抢窗口。
//! 3. **审计只存哈希**(铁律 3):发现时对整个 SKILL.md 取 SHA-256,`skill.offer` /
//!    `skill.read` 事件只记名字与哈希,正文留磁盘。读取时**重新计算并比对**
//!    ([`SkillMeta::read_body`]):发现之后被改过的文件拒绝提供——与 Capsule 存储的
//!    `load` 同一姿态,审计里的哈希才是本次运行看到的版本。
//! 4. **声明密级即成为标签来源**(铁律 1):`metadata.sensitivity: restricted` 的技能
//!    一旦被提供给某次任务,就以 `LabelSource`(origin = Manual,subject = `skill:<name>`,
//!    见 [`SkillMeta::label_source`])进入路由决策,会话棘轮随之抬升——技能正文里的
//!    做法本身可能就是敏感信息,不能因为它"只是说明书"就放它上云。
//! 5. **坏文件不让整批失败**:一个技能解析失败只影响它自己,原因进 [`Discovery::problems`]
//!    给界面显示;同名冲突先到先得(根目录顺序即优先级),后者记为问题。
//!
//! ## 诚实边界(v0)
//!
//! - 没有版本号与升级机制:同名即同一技能,新旧只能靠哈希区分。
//! - 没有"从运行锻造技能":那是 Capsule 的路;把 Capsule 导出成 SKILL.md 属后续。
//! - 影子重放暂不携带技能:重放用的是空技能集,若源运行读过技能,比对结果可能因此
//!   有差异——`skill.read` 事件能看出这一点,但 v0 不自动对齐。
//! - 团队共享靠仓库(`.agents/skills/` 随代码提交);服务端分发未做。
//! - 技能目录里的脚本、参考文件不会被读取,Agent 只看得到 SKILL.md 本身。

pub mod frontmatter;

use std::path::{Path, PathBuf};

use muster_audit::{ContentHash, SkillRef};
use muster_route::{LabelOrigin, LabelSource, Sensitivity};

/// 每个技能目录里的定义文件名。
pub const SKILL_FILE: &str = "SKILL.md";
/// 仓库级技能目录(相对工作区),与 codex 的 `.agents/skills` 同一约定。
pub const REPO_SKILLS_DIR: &str = ".agents/skills";
/// `name` 长度上限(Agent Skills 约定)。
pub const NAME_MAX: usize = 64;
/// `description` 字符数上限:它会原样进提示词目录,写长了就是在挤任务本身。
pub const DESCRIPTION_MAX: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("读取失败:{0}")]
    Io(String),
    #[error("格式错误:{0}")]
    Format(String),
    /// **不是"读不到",是"读到的不是发现时那份"**——两者的处置完全不同。
    #[error("技能 {name} 的正文与发现时的哈希不符(期望 {expect},实际 {actual})——文件在运行期间被改过,拒绝提供")]
    Tampered { name: String, expect: String, actual: String },
}

/// frontmatter 里我们关心的字段。其余键(license、compatibility、allowed-tools…)
/// 原样忽略:codex 的技能放进来不该因为多写了几个键就被拒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: String,
    pub description: String,
    pub sensitivity: Option<Sensitivity>,
}

/// 一个已发现的技能。**不含正文**——正文经 [`SkillMeta::read_body`] 按需读取并校验。
#[derive(Debug, Clone, PartialEq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    /// SKILL.md 的路径。
    pub path: PathBuf,
    /// 在哪个发现根下找到的(界面据此区分"仓库级 / 个人级")。
    pub root: PathBuf,
    /// 整个 SKILL.md 文件字节的 SHA-256:进审计的就是它。
    pub content_hash: ContentHash,
    /// 技能自报的密级;`None` = 不参与密级计算。
    pub sensitivity: Option<Sensitivity>,
}

/// 一次发现的结果:能用的技能 + 没能加载的原因。
#[derive(Debug, Default, Clone)]
pub struct Discovery {
    pub skills: Vec<SkillMeta>,
    /// 逐条可读的问题(路径 + 原因),给界面显示,不让人猜"我的技能怎么没出现"。
    pub problems: Vec<String>,
}

// ---------------------------------------------------------------- 解析

fn unquote(v: &str) -> &str {
    let v = v.trim();
    if v.len() >= 2 {
        let b = v.as_bytes();
        if (b[0] == b'"' && b[v.len() - 1] == b'"') || (b[0] == b'\'' && b[v.len() - 1] == b'\'') {
            return &v[1..v.len() - 1];
        }
    }
    v
}

fn scalar(key: &str, raw: &str) -> Result<String, SkillError> {
    let v = raw.trim();
    if v.starts_with('|') || v.starts_with('>') {
        return Err(SkillError::Format(format!("字段 {key} 不支持多行值(| 或 >),请写成一行")));
    }
    if v.starts_with('[') || v.starts_with('{') {
        return Err(SkillError::Format(format!("字段 {key} 不支持列表或映射,请写成一行文本")));
    }
    Ok(unquote(v).to_string())
}

/// `name` 规则(Agent Skills 约定):小写字母、数字、连字符;不以连字符开头结尾,
/// 不连续两个连字符;至多 [`NAME_MAX`] 字符。它既是目录名也是工具参数,宽了会出事。
pub fn validate_name(name: &str) -> Result<(), SkillError> {
    let bad = |why: &str| Err(SkillError::Format(format!("name `{name}` 不合规:{why}")));
    if name.is_empty() {
        return bad("不能为空");
    }
    if name.len() > NAME_MAX {
        return bad(&format!("超过 {NAME_MAX} 字符"));
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return bad("只允许小写字母、数字和连字符");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return bad("不能以连字符开头或结尾");
    }
    if name.contains("--") {
        return bad("不能有连续的连字符");
    }
    Ok(())
}

fn parse_sensitivity(v: &str) -> Result<Sensitivity, SkillError> {
    match v {
        "open" => Ok(Sensitivity::Open),
        "internal" => Ok(Sensitivity::Internal),
        "restricted" => Ok(Sensitivity::Restricted),
        other => Err(SkillError::Format(format!(
            "metadata.sensitivity 取值 `{other}` 不认识,只能是 open / internal / restricted"
        ))),
    }
}

/// 解析 SKILL.md 的 frontmatter。文件必须以 `---` 行开头,并以 `---` 行结束 frontmatter。
pub fn parse_frontmatter(text: &str) -> Result<Frontmatter, SkillError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    match lines.next() {
        Some(l) if l.trim_end() == "---" => {}
        _ => return Err(SkillError::Format("缺少 frontmatter:文件必须以 --- 行开头".into())),
    }
    let mut name = None;
    let mut description = None;
    let mut sensitivity = None;
    let mut in_metadata = false;
    let mut closed = false;
    for raw in lines {
        let line = raw.trim_end();
        if line == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            in_metadata = false;
        }
        let Some((k, v)) = line.split_once(':') else {
            return Err(SkillError::Format(format!("看不懂的行:`{}`(应为 键: 值)", line.trim())));
        };
        let key = k.trim();
        if indented {
            // 只认 metadata 下面的键;别的嵌套块整段忽略
            if in_metadata && key == "sensitivity" {
                sensitivity = Some(parse_sensitivity(unquote(v))?);
            }
            continue;
        }
        match key {
            "name" => name = Some(scalar("name", v)?),
            "description" => description = Some(scalar("description", v)?),
            "metadata" => {
                if !v.trim().is_empty() {
                    return Err(SkillError::Format("metadata 必须是缩进的键值块".into()));
                }
                in_metadata = true;
            }
            _ => {}
        }
    }
    if !closed {
        return Err(SkillError::Format("frontmatter 没有以 --- 行结束".into()));
    }
    let name = name
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SkillError::Format("缺少必填字段 name".into()))?;
    validate_name(&name)?;
    let description = description
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SkillError::Format("缺少必填字段 description".into()))?;
    if description.chars().count() > DESCRIPTION_MAX {
        return Err(SkillError::Format(format!(
            "description 超过 {DESCRIPTION_MAX} 字符——它会整段进提示词,请精简"
        )));
    }
    Ok(Frontmatter { name, description, sensitivity })
}

// ---------------------------------------------------------------- 发现

/// 读一个 SKILL.md 并取哈希。`name` 必须与所在目录同名:这是唯一能让
/// "read_skill 参数 → 磁盘上哪个文件"不需要额外索引的办法。
pub fn load(path: &Path, root: &Path) -> Result<SkillMeta, SkillError> {
    let bytes = std::fs::read(path).map_err(|e| SkillError::Io(e.to_string()))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| SkillError::Format("不是 UTF-8 文本".into()))?;
    let fm = parse_frontmatter(text)?;
    let dir_name = path
        .parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if fm.name != dir_name {
        return Err(SkillError::Format(format!(
            "name 为 `{}` 但所在目录是 `{dir_name}`,两者必须一致",
            fm.name
        )));
    }
    Ok(SkillMeta {
        name: fm.name,
        description: fm.description,
        path: path.to_path_buf(),
        root: root.to_path_buf(),
        content_hash: ContentHash::sha256(&bytes),
        sensitivity: fm.sensitivity,
    })
}

/// 在若干根目录下发现技能:`<root>/<name>/SKILL.md`。
///
/// - 根目录不存在很正常(多数仓库没有技能),静默跳过;
/// - 没有 SKILL.md 的子目录不是技能,静默跳过;以 `.` 开头的目录(如 codex 的
///   `.system` 缓存)也跳过;
/// - 解析失败的记进 `problems`,不影响其他技能;
/// - 同名先到先得:`roots` 的顺序就是优先级。
pub fn discover(roots: &[PathBuf]) -> Discovery {
    let mut out = Discovery::default();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut dirs: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            if dir.file_name().is_none_or(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            let path = dir.join(SKILL_FILE);
            if !path.is_file() {
                continue;
            }
            match load(&path, root) {
                Ok(meta) => {
                    if let Some(prev) = out.skills.iter().find(|s| s.name == meta.name) {
                        out.problems.push(format!(
                            "{}:与 {} 同名,已忽略(先发现的优先)",
                            path.display(),
                            prev.path.display()
                        ));
                    } else {
                        out.skills.push(meta);
                    }
                }
                Err(e) => out.problems.push(format!("{}:{e}", path.display())),
            }
        }
    }
    out
}

impl SkillMeta {
    /// 读取全文(含 frontmatter),**先比对哈希**:与发现时不一致即拒绝——
    /// 审计里 `skill.offer` 记的哈希才是本次运行看到的版本,不能让 Agent 读到另一份。
    pub fn read_body(&self) -> Result<String, SkillError> {
        let bytes = std::fs::read(&self.path).map_err(|e| SkillError::Io(e.to_string()))?;
        let actual = ContentHash::sha256(&bytes);
        if actual.0 != self.content_hash.0 {
            return Err(SkillError::Tampered {
                name: self.name.clone(),
                expect: self.content_hash.0.clone(),
                actual: actual.0,
            });
        }
        String::from_utf8(bytes).map_err(|_| SkillError::Format("不是 UTF-8 文本".into()))
    }

    /// 自报了密级的技能作为标签来源参与路由(铁律 1:只会抬高,不会降低)。
    pub fn label_source(&self) -> Option<LabelSource> {
        self.sensitivity
            .map(|level| LabelSource::new(LabelOrigin::Manual, level, format!("skill:{}", self.name)))
    }

    /// 进审计的引用:名字 + 哈希,没有正文。
    pub fn audit_ref(&self) -> SkillRef {
        SkillRef { name: self.name.clone(), content_hash: self.content_hash.clone() }
    }
}

// ---------------------------------------------------------------- 提示词目录

/// 给系统提示词用的技能目录:每个技能一行,名字 + 描述(+ 密级)。
/// 空技能集返回空串,调用方据此决定要不要加这一段。
pub fn render_catalog(skills: &[SkillMeta]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::from("可用技能(过程性知识,按需取用):");
    for sk in skills {
        s.push_str(&format!("\n- {}:{}", sk.name, sk.description));
        if let Some(level) = sk.sensitivity {
            s.push_str(&format!("(密级 {})", format!("{level:?}").to_lowercase()));
        }
    }
    s.push_str(
        "\n任务与某个技能相关时,先调用 read_skill 读取它的全文,再按其中的步骤执行;\
         与任务无关的技能不必读。",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "---\nname: demo\ndescription: 演示技能\nmetadata:\n  sensitivity: internal\n  short-description: demo\n---\n\n# Demo\n\n第一步:先跑 cargo test。\n";

    fn write_skill(root: &Path, name: &str, text: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(SKILL_FILE);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn frontmatter_parses_required_and_sensitivity() {
        let fm = parse_frontmatter(GOOD).unwrap();
        assert_eq!(fm.name, "demo");
        assert_eq!(fm.description, "演示技能");
        assert_eq!(fm.sensitivity, Some(Sensitivity::Internal));
    }

    #[test]
    fn frontmatter_rejects_missing_fields_and_multiline() {
        assert!(parse_frontmatter("# no frontmatter").is_err());
        assert!(parse_frontmatter("---\ndescription: x\n---\n").unwrap_err().to_string().contains("name"));
        assert!(parse_frontmatter("---\nname: a\n---\n").unwrap_err().to_string().contains("description"));
        assert!(parse_frontmatter("---\nname: a\ndescription: >\n  多行\n---\n").is_err());
        assert!(parse_frontmatter("---\nname: a\ndescription: x\n").is_err(), "没有结束的 ---");
        assert!(parse_frontmatter("---\nname: Bad_Name\ndescription: x\n---\n").is_err());
        assert!(parse_frontmatter("---\nname: a\ndescription: x\nmetadata:\n  sensitivity: secret\n---\n")
            .is_err());
    }

    #[test]
    fn unknown_keys_and_quotes_are_tolerated() {
        let fm = parse_frontmatter(
            "---\nname: \"a-b\"\ndescription: 'x y'\nlicense: MIT\nmetadata:\n  foo: bar\n---\n",
        )
        .unwrap();
        assert_eq!(fm.name, "a-b");
        assert_eq!(fm.description, "x y");
        assert_eq!(fm.sensitivity, None);
    }

    #[test]
    fn discover_keeps_good_reports_bad_and_dedups_by_root_order() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write_skill(a.path(), "demo", GOOD);
        write_skill(a.path(), "broken", "---\nname: broken\n---\n");
        write_skill(a.path(), "mismatch", "---\nname: other\ndescription: x\n---\n");
        write_skill(b.path(), "demo", "---\nname: demo\ndescription: 个人版\n---\n");
        write_skill(b.path(), "extra", "---\nname: extra\ndescription: 额外\n---\n");
        std::fs::create_dir_all(a.path().join("not-a-skill")).unwrap();
        std::fs::create_dir_all(a.path().join(".system/hidden")).unwrap();

        let d = discover(&[a.path().to_path_buf(), b.path().to_path_buf(), PathBuf::from("/nonexistent/x")]);
        let names: Vec<&str> = d.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["demo", "extra"]);
        assert_eq!(d.skills[0].description, "演示技能", "仓库级(先到)优先于个人级");
        assert_eq!(d.skills[0].root, a.path());
        assert_eq!(d.problems.len(), 3, "{:?}", d.problems);
        assert!(d.problems.iter().any(|p| p.contains("broken") && p.contains("description")));
        assert!(d.problems.iter().any(|p| p.contains("mismatch") && p.contains("目录")));
        assert!(d.problems.iter().any(|p| p.contains("同名")));
    }

    #[test]
    fn read_body_verifies_hash_and_detects_tampering() {
        let a = tempfile::tempdir().unwrap();
        let p = write_skill(a.path(), "demo", GOOD);
        let d = discover(&[a.path().to_path_buf()]);
        let sk = &d.skills[0];
        assert!(sk.content_hash.0.starts_with("sha256:"));
        assert!(sk.read_body().unwrap().contains("先跑 cargo test"));

        std::fs::write(&p, GOOD.replace("cargo test", "rm -rf /")).unwrap();
        let err = sk.read_body().unwrap_err();
        assert!(matches!(err, SkillError::Tampered { .. }), "{err}");
    }

    #[test]
    fn label_source_only_when_declared() {
        let a = tempfile::tempdir().unwrap();
        write_skill(a.path(), "demo", GOOD);
        write_skill(a.path(), "plain", "---\nname: plain\ndescription: 无密级\n---\n");
        let d = discover(&[a.path().to_path_buf()]);
        let demo = d.skills.iter().find(|s| s.name == "demo").unwrap();
        let plain = d.skills.iter().find(|s| s.name == "plain").unwrap();
        let src = demo.label_source().unwrap();
        assert_eq!(src.level, Sensitivity::Internal);
        assert_eq!(src.origin, LabelOrigin::Manual);
        assert_eq!(src.subject, "skill:demo");
        assert!(plain.label_source().is_none());
        assert_eq!(demo.audit_ref().name, "demo");
    }

    /// 仓库自带的示例技能(.agents/skills/muster-conventions)必须能被加载——
    /// 它既是给 Agent 看的约定,也是这个格式的活样例;坏了要在这里炸,而不是界面上悄悄少一个。
    #[test]
    fn bundled_example_skill_loads_cleanly() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let d = discover(&[repo.join(REPO_SKILLS_DIR)]);
        assert!(d.problems.is_empty(), "{:?}", d.problems);
        let sk = d.skills.iter().find(|s| s.name == "muster-conventions").expect("示例技能应被发现");
        assert!(sk.sensitivity.is_none(), "示例技能不该抬升任何会话的密级");
        assert!(sk.read_body().unwrap().contains("cargo test"));
    }

    #[test]
    fn catalog_lists_each_skill_once_and_is_empty_without_skills() {
        assert_eq!(render_catalog(&[]), "");
        let a = tempfile::tempdir().unwrap();
        write_skill(a.path(), "demo", GOOD);
        let d = discover(&[a.path().to_path_buf()]);
        let s = render_catalog(&d.skills);
        assert!(s.contains("- demo:演示技能(密级 internal)"), "{s}");
        assert!(s.contains("read_skill"));
        assert!(!s.contains("先跑 cargo test"), "目录里不该出现正文");
    }
}
