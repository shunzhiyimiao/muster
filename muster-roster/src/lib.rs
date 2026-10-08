//! # muster-roster — Agent 编制(工牌)
//!
//! 一句话:**把"哪个 Agent、能做什么"从代码常量变成可配置、可审计的名册。**
//!
//! 此前桌面壳里只有一个写死的 `A-007`;身份内核(工牌持有者 = `PrincipalKind::Agent`)、
//! 审计(`badge.update`、`approval.request` 的 `badge_capabilities_hash`)、编制页
//! (审计链里干过活的 actor)早就按"多个工牌"设计好了,缺的只是名册本身。
//!
//! ## 格式:与技能、工作流同一套目录约定
//!
//! `<根>/<工牌号>/AGENT.md`,仓库级 [`REPO_ROSTER_DIR`](随代码提交、团队共用),
//! 个人级 `~/.muster/roster/`。frontmatter 是岗位卡,正文是岗位说明书(进提示词):
//!
//! ```text
//! ---
//! badge: A-007            # 工牌号 = 目录名 = 审计 actor id
//! name: 小七               # 显示名
//! role: 代码评审员          # 岗位
//! description: 一句话职责   # 进 @ 提示与名册
//! aliases: [小七, 小齐]     # @ 与语音里的称呼
//! skills: [report-html]    # 默认提供的技能;不写 = 工作区里全部技能
//! tools: worktree          # read-only | worktree(隔离可写 + run_command)
//! commands: [cargo test]   # run_command 允许清单;不写用默认清单
//! egress: [crates.io]      # run_command 经出网闸可访问的主机(精确或子域);不写 = 全部拒绝
//! model: kimi              # 首选 provider;落点仍由密级决定
//! sensitivity_max: internal # 工牌能接触的最高密级;不写 = 不设上限
//! default: true            # 频道没指名时用它;整个名册只能有一个
//! ---
//! 岗位说明书……
//! ```
//!
//! ## 关键设计决策
//!
//! 1. **工牌号就是审计里的 actor id**。谁干的活写在链上,名册只负责把号码翻译成
//!    名字、岗位与能力;名册没有的号码照样能在编制页出现——链是事实,名册是注释。
//! 2. **能力哈希**([`AgentSpec::capabilities_hash`]):tools / commands / sensitivity_max /
//!    skills / model 的规范化 JSON 取 SHA-256。它进 `approval.request` 的
//!    `badge_capabilities_hash`;定义改了、哈希变了,由桌面壳落一条 `badge.update`——
//!    "工牌能力变更留痕"至此成真,不再是一个常量字符串。
//! 3. **人格只是提示词后缀**([`AgentSpec::persona_prompt`]):A1 的 `SYSTEM_PROMPT` 常量
//!    原样不动(它是 A7 闸门证据的前提),岗位说明书追加在后;工具与工作区那段仍由
//!    Runner 统一拼。
//! 4. **密级上限是硬闸**([`AgentSpec::allows_level`]):频道有效密级高于工牌上限就拒绝
//!    派遣,不是"提示模型注意"。工牌是身份,身份的边界不能靠模型自觉。
//! 5. **@ 指派**([`Roster::mention`]):消息开头 `@工牌号` / `@别名` / `@名字` 指名;
//!    没有就用默认工牌。不认识的 @ 当普通文本,不猜。
//! 6. **坏文件不让整批失败**:一张岗位卡解析失败只影响它自己,原因进 [`Roster::problems`];
//!    工牌号冲突先到先得;多个 `default: true` 以第一个为准并记为问题。
//!
//! 7. **委派**([`delegate`]):主 Agent 可以把子任务交给另一张工牌,在同一工作区里跑一个
//!    子 run;串行、只一层、档位取交集、密级上限照旧是硬闸;父 run 落 `run.delegate`,
//!    子 run 有自己的链。这是"一次任务里调度多个 Agent"的实现形态。
//!
//! ## 诚实边界(v0)
//!
//! - 委派是串行的、只有一层,子 Agent 不能再委派;并行改同一目录 v0 不做。
//! - 名册在文件里,服务端不下发:团队共用靠把 `.agents/roster/` 提交进仓库。
//! - 这里没有 Agent 的账号与密码——会议 daemon 用的服务端账号是另一回事(身份的
//!   **来源**属 P2 服务端),名册只管"在这台机器上派谁、给什么权限"。
//! - `tools` 只有两档:只读,或隔离工作区可写。没有"可写但禁某些目录"这种细粒度。
//! - 默认技能只是"提供"不是"必读":Agent 读不读仍由它自己决定(见 muster-skill)。

pub mod delegate;

use std::path::{Path, PathBuf};

use muster_audit::ContentHash;
use muster_route::Sensitivity;
use muster_skill::frontmatter::{flag, list, parse_frontmatter, scalar};

pub const AGENT_FILE: &str = "AGENT.md";
/// 仓库级编制目录(相对工作区),与 `.agents/skills`、`.agents/workflows` 并列。
pub const REPO_ROSTER_DIR: &str = ".agents/roster";
pub const DESCRIPTION_MAX: usize = 300;
pub const BADGE_MAX: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum RosterError {
    #[error("读取失败:{0}")]
    Io(String),
    #[error("格式错误:{0}")]
    Format(String),
}

/// 工具档位:只读工作区,或隔离 worktree 可写(含 run_command)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    ReadOnly,
    Worktree,
}

impl ToolMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" | "read_only" => Ok(ToolMode::ReadOnly),
            "worktree" | "write" => Ok(ToolMode::Worktree),
            other => Err(format!("tools 取值 `{other}` 不认识,只能是 read-only / worktree")),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolMode::ReadOnly => "read-only",
            ToolMode::Worktree => "worktree",
        }
    }
}

/// 一张岗位卡。正文(岗位说明书)在 `persona`。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSpec {
    pub badge: String,
    pub name: String,
    pub role: String,
    pub description: String,
    pub aliases: Vec<String>,
    /// 默认提供的技能名;空 = 由调用方决定(通常是工作区里全部技能)。
    pub skills: Vec<String>,
    pub tools: ToolMode,
    /// `run_command` 允许清单;`None` = 用 Runner 的默认清单。
    pub commands: Option<Vec<String>>,
    /// `run_command` 经出网闸可访问的主机;空 = 全部拒绝(默认)。进能力哈希。
    pub egress: Vec<String>,
    /// 首选 provider id;落点仍由密级路由决定。
    pub model: Option<String>,
    /// 工牌能接触的最高密级;`None` = 不设上限。
    pub sensitivity_max: Option<Sensitivity>,
    pub is_default: bool,
    /// 岗位说明书(frontmatter 之后的全部内容)。
    pub persona: String,
    pub path: PathBuf,
    pub root: PathBuf,
    /// 整个 AGENT.md 的 SHA-256。
    pub definition_hash: ContentHash,
    /// 能力子集的 SHA-256,见设计决策 2。
    pub capabilities_hash: ContentHash,
}

#[derive(Debug, Default, Clone)]
pub struct Roster {
    pub agents: Vec<AgentSpec>,
    pub problems: Vec<String>,
}

/// 工牌号规则:字母数字与连字符,首字符字母或数字,至多 [`BADGE_MAX`] 字符。
/// 它既是目录名也是审计 actor id,宽了会出事。
pub fn validate_badge(b: &str) -> Result<(), RosterError> {
    let bad = |why: &str| Err(RosterError::Format(format!("工牌号 `{b}` 不合规:{why}")));
    if b.len() < 2 || b.len() > BADGE_MAX {
        return bad(&format!("长度须在 2-{BADGE_MAX} 之间"));
    }
    if !b.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return bad("只允许字母、数字和连字符");
    }
    if !b.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        return bad("须以字母或数字开头");
    }
    Ok(())
}

fn parse_sensitivity(v: &str) -> Result<Sensitivity, RosterError> {
    match v.trim().to_ascii_lowercase().as_str() {
        "open" => Ok(Sensitivity::Open),
        "internal" => Ok(Sensitivity::Internal),
        "restricted" => Ok(Sensitivity::Restricted),
        other => Err(RosterError::Format(format!(
            "sensitivity_max 取值 `{other}` 不认识,只能是 open / internal / restricted"
        ))),
    }
}

impl AgentSpec {
    /// 从文件内容解析(不读磁盘,便于测试)。`badge` 缺省取目录名。
    pub fn parse(text: &str, path: &Path, root: &Path) -> Result<Self, RosterError> {
        let (entries, persona) = parse_frontmatter(text).map_err(RosterError::Format)?;
        let dir_name = path
            .parent()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let badge = scalar(&entries, "badge").filter(|s| !s.is_empty()).unwrap_or_else(|| dir_name.clone());
        validate_badge(&badge)?;
        let name = scalar(&entries, "name")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| RosterError::Format("缺少必填字段 name(显示名)".into()))?;
        let role = scalar(&entries, "role")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| RosterError::Format("缺少必填字段 role(岗位)".into()))?;
        let description = scalar(&entries, "description")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| RosterError::Format("缺少必填字段 description(一句话职责)".into()))?;
        if description.chars().count() > DESCRIPTION_MAX {
            return Err(RosterError::Format(format!("description 超过 {DESCRIPTION_MAX} 字符")));
        }
        let mut aliases: Vec<String> = list(&entries, "aliases");
        if !aliases.iter().any(|a| a == &name) {
            aliases.insert(0, name.clone());
        }
        let skills = list(&entries, "skills");
        for s in &skills {
            muster_skill::validate_name(s).map_err(|e| RosterError::Format(format!("skills:{e}")))?;
        }
        let tools = match scalar(&entries, "tools") {
            None => ToolMode::Worktree,
            Some(v) => ToolMode::parse(&v).map_err(RosterError::Format)?,
        };
        let commands = {
            let c = list(&entries, "commands");
            if c.is_empty() { None } else { Some(c) }
        };
        let egress: Vec<String> = list(&entries, "egress").into_iter().map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect();
        let model = scalar(&entries, "model").filter(|s| !s.is_empty());
        let sensitivity_max = match scalar(&entries, "sensitivity_max") {
            None => None,
            Some(v) if v.is_empty() => None,
            Some(v) => Some(parse_sensitivity(&v)?),
        };
        let is_default = flag(&entries, "default");
        if persona.is_empty() {
            return Err(RosterError::Format("正文为空:frontmatter 之后要写岗位说明书".into()));
        }
        let capabilities_hash = capabilities_hash(&tools, &commands, &egress, &sensitivity_max, &skills, &model);
        Ok(AgentSpec {
            badge,
            name,
            role,
            description,
            aliases,
            skills,
            tools,
            commands,
            egress,
            model,
            sensitivity_max,
            is_default,
            persona,
            path: path.to_path_buf(),
            root: root.to_path_buf(),
            definition_hash: ContentHash::sha256(text.as_bytes()),
            capabilities_hash,
        })
    }

    /// 追加在 A1 系统提示词之后的人格段。常量本身不动。
    pub fn persona_prompt(&self) -> String {
        format!(
            "你的工牌:{};岗位:{};称呼:{}。以下是你的岗位说明书,与上文冲突时以上文为准。\n{}",
            self.badge, self.role, self.name, self.persona
        )
    }

    /// 这个工牌能不能接触该密级。`None` 上限 = 不设限。
    pub fn allows_level(&self, level: Sensitivity) -> bool {
        self.sensitivity_max.is_none_or(|max| level <= max)
    }

    /// 工牌号(不分大小写)、显示名或任一别名匹配。
    pub fn matches(&self, key: &str) -> bool {
        let k = key.trim();
        self.badge.eq_ignore_ascii_case(k) || self.name == k || self.aliases.iter().any(|a| a == k)
    }
}

/// 能力子集的规范化哈希。字段顺序由 serde_json 的 BTreeMap 保证(铁律 5)。
fn capabilities_hash(
    tools: &ToolMode,
    commands: &Option<Vec<String>>,
    egress: &[String],
    sensitivity_max: &Option<Sensitivity>,
    skills: &[String],
    model: &Option<String>,
) -> ContentHash {
    let v = serde_json::json!({
        "tools": tools.as_str(),
        "commands": commands,
        "egress": egress,
        "sensitivity_max": sensitivity_max.map(|l| format!("{l:?}").to_lowercase()),
        "skills": skills,
        "model": model,
    });
    ContentHash::sha256(v.to_string().as_bytes())
}

/// 读一张岗位卡。`badge` 必须与所在目录同名。
pub fn load(path: &Path, root: &Path) -> Result<AgentSpec, RosterError> {
    let bytes = std::fs::read(path).map_err(|e| RosterError::Io(e.to_string()))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| RosterError::Format("不是 UTF-8 文本".into()))?;
    let spec = AgentSpec::parse(text, path, root)?;
    let dir_name = path
        .parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if spec.badge != dir_name {
        return Err(RosterError::Format(format!(
            "badge 为 `{}` 但所在目录是 `{dir_name}`,两者必须一致",
            spec.badge
        )));
    }
    Ok(spec)
}

/// 在若干根目录下发现岗位卡:`<root>/<badge>/AGENT.md`。规则与技能相同。
pub fn discover(roots: &[PathBuf]) -> Roster {
    let mut out = Roster::default();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut dirs: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            if !dir.is_dir() || dir.file_name().is_none_or(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            let path = dir.join(AGENT_FILE);
            if !path.is_file() {
                continue;
            }
            match load(&path, root) {
                Ok(spec) => {
                    if let Some(prev) = out.agents.iter().find(|a| a.badge.eq_ignore_ascii_case(&spec.badge)) {
                        out.problems.push(format!(
                            "{}:工牌号与 {} 重复,已忽略(先发现的优先)",
                            path.display(),
                            prev.path.display()
                        ));
                        continue;
                    }
                    let mut spec = spec;
                    if spec.is_default && out.agents.iter().any(|a| a.is_default) {
                        out.problems.push(format!(
                            "{}:多张岗位卡都写了 default: true,以先发现的为准,这张按非默认处理",
                            path.display()
                        ));
                        spec.is_default = false;
                    }
                    out.agents.push(spec);
                }
                Err(e) => out.problems.push(format!("{}:{e}", path.display())),
            }
        }
    }
    out
}

impl Roster {
    /// 默认工牌:标了 `default: true` 的;没有就是第一张;名册为空则 `None`。
    pub fn default_agent(&self) -> Option<&AgentSpec> {
        self.agents.iter().find(|a| a.is_default).or_else(|| self.agents.first())
    }

    /// 按工牌号 / 显示名 / 别名找。
    pub fn find(&self, key: &str) -> Option<&AgentSpec> {
        self.agents.iter().find(|a| a.matches(key))
    }

    /// 消息开头的 `@某某` 指派:返回被指派的工牌与去掉称呼后的正文;
    /// 没有 @ 或 @ 的不是名册里的人,返回 `None`(当普通文本,不猜)。
    pub fn mention<'a>(&'a self, text: &str) -> Option<(&'a AgentSpec, String)> {
        let t = text.trim_start();
        let rest = t.strip_prefix('@').or_else(|| t.strip_prefix('＠'))?;
        let end = rest
            .char_indices()
            .find(|(_, c)| c.is_whitespace() || matches!(c, ',' | '，' | ':' | '：' | '、' | '!' | '！'))
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let key = &rest[..end];
        let agent = self.find(key)?;
        let body = rest[end..]
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | ':' | '：' | '、' | '!' | '！'))
            .trim()
            .to_string();
        Some((agent, body))
    }

    pub fn badges(&self) -> Vec<&str> {
        self.agents.iter().map(|a| a.badge.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "---\nbadge: A-007\nname: 小七\nrole: 代码评审员\ndescription: 审查改动\naliases: [小齐, 晓琪]\nskills: [report-html]\ntools: worktree\ncommands: [cargo test, git status]\nmodel: kimi\nsensitivity_max: internal\ndefault: true\n---\n\n# 岗位说明书\n先评审再动手。\n";

    fn write_card(root: &Path, badge: &str, text: &str) -> PathBuf {
        let dir = root.join(badge);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(AGENT_FILE);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn parses_every_field_and_hashes_capabilities() {
        let a = AgentSpec::parse(CARD, Path::new("/x/A-007/AGENT.md"), Path::new("/x")).unwrap();
        assert_eq!(a.badge, "A-007");
        assert_eq!(a.name, "小七");
        assert_eq!(a.role, "代码评审员");
        assert_eq!(a.aliases, vec!["小七", "小齐", "晓琪"], "显示名自动算别名");
        assert_eq!(a.skills, vec!["report-html"]);
        assert_eq!(a.tools, ToolMode::Worktree);
        assert_eq!(a.commands.as_deref(), Some(&["cargo test".to_string(), "git status".to_string()][..]));
        assert_eq!(a.model.as_deref(), Some("kimi"));
        assert_eq!(a.sensitivity_max, Some(Sensitivity::Internal));
        assert!(a.is_default);
        assert!(a.persona.starts_with("# 岗位说明书"));
        assert!(a.persona_prompt().contains("工牌:A-007") && a.persona_prompt().contains("先评审再动手"));
        assert!(a.allows_level(Sensitivity::Internal) && !a.allows_level(Sensitivity::Restricted));
        // 能力哈希:改 tools 就变,改正文不变
        let b = AgentSpec::parse(&CARD.replace("tools: worktree", "tools: read-only"), Path::new("/x/A-007/AGENT.md"), Path::new("/x")).unwrap();
        assert_ne!(a.capabilities_hash, b.capabilities_hash);
        let c = AgentSpec::parse(&CARD.replace("先评审再动手", "先想再做"), Path::new("/x/A-007/AGENT.md"), Path::new("/x")).unwrap();
        assert_eq!(a.capabilities_hash, c.capabilities_hash);
        assert_ne!(a.definition_hash, c.definition_hash);
    }

    #[test]
    fn defaults_and_rejections() {
        let min = "---\nname: 阿岩\nrole: 实现工程师\ndescription: 写代码\n---\n照约定写。\n";
        let a = AgentSpec::parse(min, Path::new("/x/A-012/AGENT.md"), Path::new("/x")).unwrap();
        assert_eq!(a.badge, "A-012", "badge 缺省取目录名");
        assert_eq!(a.tools, ToolMode::Worktree);
        assert!(a.commands.is_none() && a.model.is_none() && a.sensitivity_max.is_none() && !a.is_default);
        assert!(a.allows_level(Sensitivity::Restricted), "不设上限");
        for (text, why) in [
            ("---\nname: x\ndescription: d\n---\n正文\n", "role"),
            ("---\nname: x\nrole: r\n---\n正文\n", "description"),
            ("---\nname: x\nrole: r\ndescription: d\n---\n", "正文"),
            ("---\nname: x\nrole: r\ndescription: d\ntools: sudo\n---\n正文\n", "tools"),
            ("---\nname: x\nrole: r\ndescription: d\nsensitivity_max: secret\n---\n正文\n", "sensitivity_max"),
            ("---\nbadge: bad badge\nname: x\nrole: r\ndescription: d\n---\n正文\n", "工牌号"),
        ] {
            let e = AgentSpec::parse(text, Path::new("/x/A-012/AGENT.md"), Path::new("/x")).unwrap_err();
            assert!(e.to_string().contains(why), "{why}: {e}");
        }
    }

    #[test]
    fn discover_handles_mismatch_duplicates_and_multiple_defaults() {
        let root = tempfile::tempdir().unwrap();
        write_card(root.path(), "A-007", CARD);
        write_card(root.path(), "A-012", "---\nname: 阿岩\nrole: 实现工程师\ndescription: 写代码\ndefault: true\n---\n照约定写。\n");
        write_card(root.path(), "A-099", "---\nbadge: A-007\nname: 冒名\nrole: r\ndescription: d\n---\n正文\n");
        let r = discover(&[root.path().to_path_buf(), PathBuf::from("/nonexistent")]);
        assert_eq!(r.badges(), vec!["A-007", "A-012"]);
        assert_eq!(r.default_agent().unwrap().badge, "A-007");
        assert!(!r.find("A-012").unwrap().is_default, "第二个 default 被降级");
        assert_eq!(r.problems.len(), 2, "{:?}", r.problems);
        assert!(r.problems.iter().any(|p| p.contains("目录")));
        assert!(r.problems.iter().any(|p| p.contains("default")));
    }

    #[test]
    fn find_and_mention() {
        let root = tempfile::tempdir().unwrap();
        write_card(root.path(), "A-007", CARD);
        write_card(root.path(), "A-012", "---\nname: 阿岩\nrole: 实现工程师\ndescription: 写代码\naliases: [实现工程师]\n---\n照约定写。\n");
        let r = discover(&[root.path().to_path_buf()]);
        assert_eq!(r.find("a-012").unwrap().name, "阿岩");
        assert_eq!(r.find("晓琪").unwrap().badge, "A-007");
        assert!(r.find("路人").is_none());
        let (a, body) = r.mention("@阿岩 把 add 改成加法").unwrap();
        assert_eq!((a.badge.as_str(), body.as_str()), ("A-012", "把 add 改成加法"));
        let (a, body) = r.mention("  @小齐,看看这个 diff").unwrap();
        assert_eq!((a.badge.as_str(), body.as_str()), ("A-007", "看看这个 diff"));
        let (a, body) = r.mention("＠实现工程师:改一下").unwrap();
        assert_eq!((a.badge.as_str(), body.as_str()), ("A-012", "改一下"));
        assert!(r.mention("@路人 你好").is_none(), "不认识的 @ 当普通文本");
        assert!(r.mention("没有点名").is_none());
        assert_eq!(r.mention("@阿岩").unwrap().1, "", "只点名不带话");
    }

    /// 仓库自带的三张岗位卡必须能加载,且恰好一张默认。
    #[test]
    fn bundled_roster_loads_cleanly() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let r = discover(&[repo.join(REPO_ROSTER_DIR)]);
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert_eq!(r.badges(), vec!["A-007", "A-012", "A-021"]);
        assert_eq!(r.default_agent().unwrap().badge, "A-007");
        assert_eq!(r.agents.iter().filter(|a| a.is_default).count(), 1);
        assert_eq!(r.find("A-021").unwrap().tools, ToolMode::ReadOnly);
        assert_eq!(r.find("小析").unwrap().skills, vec!["report-html"]);
    }
}
