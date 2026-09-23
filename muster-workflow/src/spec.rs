//! WORKFLOW.md:frontmatter(扁平键值 + 列表)+ 正文(步骤)。与技能同一套目录约定。

use std::path::{Path, PathBuf};

use muster_audit::ContentHash;
use muster_route::{LabelOrigin, LabelSource, Sensitivity};

use crate::schedule::Schedule;
use crate::WorkflowError;

pub const WORKFLOW_FILE: &str = "WORKFLOW.md";
/// 仓库级工作流目录(相对工作区),与技能的 `.agents/skills` 并列。
pub const REPO_WORKFLOWS_DIR: &str = ".agents/workflows";
pub const DESCRIPTION_MAX: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Html,
    Pdf,
    Pptx,
}

impl OutputKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "html" => Ok(OutputKind::Html),
            "pdf" => Ok(OutputKind::Pdf),
            "pptx" | "ppt" => Ok(OutputKind::Pptx),
            other => Err(format!("outputs 里的 `{other}` 不认识,只能是 html / pdf / pptx")),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            OutputKind::Html => "html",
            OutputKind::Pdf => "pdf",
            OutputKind::Pptx => "pptx",
        }
    }
}

/// 一个已发现的工作流定义。正文(步骤)在 `body`;审计只记 `definition_hash`。
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowSpec {
    pub name: String,
    pub description: String,
    pub schedule: Schedule,
    /// 技能名(muster-skill 的 name),运行时原样提供给 Agent;找不到的技能是运行错误。
    pub skills: Vec<String>,
    /// `http_get` 允许访问的主机(小写,不带协议与路径);空 = 不联网。
    pub egress_hosts: Vec<String>,
    pub outputs: Vec<OutputKind>,
    /// 结果发到哪个频道;`None` = 个人空间。
    pub channel: Option<String>,
    /// 工作流自报的密级(同技能:进棘轮,只升不降)。
    pub sensitivity: Option<Sensitivity>,
    /// 步骤正文(frontmatter 之后的全部内容)。
    pub body: String,
    pub path: PathBuf,
    pub root: PathBuf,
    /// 整个 WORKFLOW.md 的 SHA-256。
    pub definition_hash: ContentHash,
    /// 定义文件的修改时间(毫秒):从没触发过的工作流从这个时刻往后算第一次。
    pub mtime_ms: u64,
}

#[derive(Debug, Default, Clone)]
pub struct Discovery {
    pub workflows: Vec<WorkflowSpec>,
    pub problems: Vec<String>,
}

// ---------------------------------------------------------------- frontmatter

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FmValue {
    Scalar(String),
    List(Vec<String>),
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 {
        let b = v.as_bytes();
        if (b[0] == b'"' && b[v.len() - 1] == b'"') || (b[0] == b'\'' && b[v.len() - 1] == b'\'') {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// 解析 frontmatter,返回 (键值对, 正文)。键值对保持出现顺序,重复键以后者为准。
pub fn parse_frontmatter(text: &str) -> Result<(Vec<(String, FmValue)>, String), WorkflowError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    match lines.next() {
        Some(l) if l.trim_end() == "---" => {}
        _ => return Err(WorkflowError::Format("缺少 frontmatter:文件必须以 --- 行开头".into())),
    }
    let mut entries: Vec<(String, FmValue)> = Vec::new();
    let mut list_key: Option<String> = None;
    let mut closed = false;
    let mut body_lines: Vec<&str> = Vec::new();
    for raw in lines {
        if closed {
            body_lines.push(raw);
            continue;
        }
        let line = raw.trim_end();
        if line == "---" {
            closed = true;
            continue;
        }
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            let t = line.trim();
            let item = t.strip_prefix("- ").map(str::trim).or(if t == "-" { Some("") } else { None });
            match (item, &list_key) {
                (Some(item), Some(k)) => {
                    if let Some((_, FmValue::List(v))) = entries.iter_mut().rev().find(|(key, _)| key == k) {
                        if !item.is_empty() {
                            v.push(unquote(item));
                        }
                    }
                }
                (Some(_), None) => {
                    return Err(WorkflowError::Format(format!("列表项 `{t}` 没有所属的键")));
                }
                (None, _) => {
                    return Err(WorkflowError::Format(format!("不支持嵌套键值:`{t}`(frontmatter 只认扁平键与列表)")));
                }
            }
            continue;
        }
        list_key = None;
        let Some((k, v)) = line.split_once(':') else {
            return Err(WorkflowError::Format(format!("看不懂的行:`{line}`(应为 键: 值)")));
        };
        let key = k.trim().to_string();
        let val = v.trim();
        let value = if val.is_empty() {
            list_key = Some(key.clone());
            FmValue::List(Vec::new())
        } else if val.starts_with('[') && val.ends_with(']') {
            FmValue::List(
                val[1..val.len() - 1]
                    .split(',')
                    .map(unquote)
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        } else if val.starts_with('|') || val.starts_with('>') {
            return Err(WorkflowError::Format(format!("字段 {key} 不支持多行值(| 或 >),请写成一行")));
        } else {
            FmValue::Scalar(unquote(val))
        };
        entries.retain(|(k2, _)| k2 != &key);
        entries.push((key, value));
    }
    if !closed {
        return Err(WorkflowError::Format("frontmatter 没有以 --- 行结束".into()));
    }
    Ok((entries, body_lines.join("\n").trim().to_string()))
}

fn scalar(entries: &[(String, FmValue)], key: &str) -> Option<String> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        FmValue::Scalar(s) => Some(s.clone()),
        FmValue::List(l) if l.len() == 1 => Some(l[0].clone()),
        _ => None,
    })
}

fn list(entries: &[(String, FmValue)], key: &str) -> Vec<String> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| match v {
            FmValue::Scalar(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
            FmValue::List(l) => l.clone(),
        })
        .unwrap_or_default()
}

fn validate_host(h: &str) -> Result<String, WorkflowError> {
    let h = h.trim().to_ascii_lowercase();
    if h.is_empty()
        || h.contains("://")
        || h.contains('/')
        || h.contains(' ')
        || !h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':')
    {
        return Err(WorkflowError::Format(format!(
            "egress 里的 `{h}` 不是主机名:只写主机(可带端口),不带 http:// 与路径"
        )));
    }
    Ok(h)
}

impl WorkflowSpec {
    /// 从文件内容解析(不读磁盘,便于测试)。`mtime_ms` 由调用方给。
    pub fn parse(text: &str, path: &Path, root: &Path, mtime_ms: u64) -> Result<Self, WorkflowError> {
        let (entries, body) = parse_frontmatter(text)?;
        let name = scalar(&entries, "name")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WorkflowError::Format("缺少必填字段 name".into()))?;
        muster_skill::validate_name(&name).map_err(|e| WorkflowError::Format(e.to_string()))?;
        let description = scalar(&entries, "description")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WorkflowError::Format("缺少必填字段 description".into()))?;
        if description.chars().count() > DESCRIPTION_MAX {
            return Err(WorkflowError::Format(format!("description 超过 {DESCRIPTION_MAX} 字符")));
        }
        let schedule = scalar(&entries, "schedule")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WorkflowError::Format("缺少必填字段 schedule(不想定时就写 manual)".into()))
            .and_then(|s| Schedule::parse(&s).map_err(|e| WorkflowError::Format(format!("schedule:{e}"))))?;
        let skills = list(&entries, "skills");
        for s in &skills {
            muster_skill::validate_name(s).map_err(|e| WorkflowError::Format(format!("skills:{e}")))?;
        }
        let mut egress_hosts = Vec::new();
        for h in list(&entries, "egress") {
            let h = validate_host(&h)?;
            if !egress_hosts.contains(&h) {
                egress_hosts.push(h);
            }
        }
        let mut outputs = Vec::new();
        for o in list(&entries, "outputs") {
            let k = OutputKind::parse(&o).map_err(WorkflowError::Format)?;
            if !outputs.contains(&k) {
                outputs.push(k);
            }
        }
        if outputs.is_empty() {
            outputs.push(OutputKind::Html);
        }
        let channel = scalar(&entries, "channel").filter(|s| !s.is_empty());
        let sensitivity = match scalar(&entries, "sensitivity").as_deref() {
            None | Some("") => None,
            Some("open") => Some(Sensitivity::Open),
            Some("internal") => Some(Sensitivity::Internal),
            Some("restricted") => Some(Sensitivity::Restricted),
            Some(other) => {
                return Err(WorkflowError::Format(format!(
                    "sensitivity 取值 `{other}` 不认识,只能是 open / internal / restricted"
                )))
            }
        };
        if body.is_empty() {
            return Err(WorkflowError::Format("正文为空:frontmatter 之后要写工作流步骤".into()));
        }
        Ok(WorkflowSpec {
            name,
            description,
            schedule,
            skills,
            egress_hosts,
            outputs,
            channel,
            sensitivity,
            body,
            path: path.to_path_buf(),
            root: root.to_path_buf(),
            definition_hash: ContentHash::sha256(text.as_bytes()),
            mtime_ms,
        })
    }

    /// 自报了密级的工作流作为标签来源参与路由(只升不降)。
    pub fn label_source(&self) -> Option<LabelSource> {
        self.sensitivity
            .map(|level| LabelSource::new(LabelOrigin::Manual, level, format!("workflow:{}", self.name)))
    }

    /// 下一次该跑的时刻:从 max(上次触发, 定义修改时间) 往后算。
    /// 手动工作流返回 `None`。调用方拿它与"现在"比:到了就跑,错过了也只补这一次。
    pub fn next_due(&self, last_trigger_ms: Option<u64>) -> Option<chrono::DateTime<chrono::Local>> {
        let anchor = last_trigger_ms.unwrap_or(0).max(self.mtime_ms);
        self.schedule.next_after(crate::ms_to_local(anchor))
    }

    pub fn wants(&self, kind: OutputKind) -> bool {
        self.outputs.contains(&kind)
    }
}

/// 读一个 WORKFLOW.md。`name` 必须与所在目录同名(与技能同一规则)。
pub fn load(path: &Path, root: &Path) -> Result<WorkflowSpec, WorkflowError> {
    let bytes = std::fs::read(path).map_err(|e| WorkflowError::Io(e.to_string()))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| WorkflowError::Format("不是 UTF-8 文本".into()))?;
    let mtime_ms = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let spec = WorkflowSpec::parse(text, path, root, mtime_ms)?;
    let dir_name = path
        .parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if spec.name != dir_name {
        return Err(WorkflowError::Format(format!(
            "name 为 `{}` 但所在目录是 `{dir_name}`,两者必须一致",
            spec.name
        )));
    }
    Ok(spec)
}

/// 在若干根目录下发现工作流:`<root>/<name>/WORKFLOW.md`。规则与技能相同:
/// 根不存在静默跳过、坏文件只影响自己、同名先到先得。
pub fn discover(roots: &[PathBuf]) -> Discovery {
    let mut out = Discovery::default();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut dirs: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            if !dir.is_dir() || dir.file_name().is_none_or(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            let path = dir.join(WORKFLOW_FILE);
            if !path.is_file() {
                continue;
            }
            match load(&path, root) {
                Ok(spec) => {
                    if let Some(prev) = out.workflows.iter().find(|w| w.name == spec.name) {
                        out.problems.push(format!(
                            "{}:与 {} 同名,已忽略(先发现的优先)",
                            path.display(),
                            prev.path.display()
                        ));
                    } else {
                        out.workflows.push(spec);
                    }
                }
                Err(e) => out.problems.push(format!("{}:{e}", path.display())),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const EXAMPLE: &str = "---\nname: daily-brief\ndescription: 每天中午的市场简报\nschedule: daily 12:00\nskills:\n  - report-html\n  - \"finance-sources\"\negress: [query1.finance.yahoo.com, api.frankfurter.app]\noutputs: [html, pdf]\nchannel: personal\nsensitivity: internal\n---\n\n# 步骤\n\n1. 抓指数。\n2. 比昨天。\n";

    #[test]
    fn parses_lists_in_both_forms_and_all_fields() {
        let s = WorkflowSpec::parse(EXAMPLE, Path::new("/x/daily-brief/WORKFLOW.md"), Path::new("/x"), 7).unwrap();
        assert_eq!(s.name, "daily-brief");
        assert_eq!(s.schedule.describe_zh(), "每天 12:00");
        assert_eq!(s.skills, vec!["report-html", "finance-sources"]);
        assert_eq!(s.egress_hosts, vec!["query1.finance.yahoo.com", "api.frankfurter.app"]);
        assert_eq!(s.outputs, vec![OutputKind::Html, OutputKind::Pdf]);
        assert_eq!(s.channel.as_deref(), Some("personal"));
        assert_eq!(s.sensitivity, Some(Sensitivity::Internal));
        assert!(s.body.starts_with("# 步骤"));
        assert!(s.body.contains("比昨天"));
        assert_eq!(s.mtime_ms, 7);
        assert_eq!(s.label_source().unwrap().subject, "workflow:daily-brief");
        assert!(s.definition_hash.0.starts_with("sha256:"));
    }

    #[test]
    fn defaults_and_rejections() {
        let ok = "---\nname: a\ndescription: x\nschedule: manual\n---\n做点事\n";
        let s = WorkflowSpec::parse(ok, Path::new("/x/a/WORKFLOW.md"), Path::new("/x"), 0).unwrap();
        assert_eq!(s.outputs, vec![OutputKind::Html], "没写 outputs 默认 html");
        assert!(s.egress_hosts.is_empty() && s.skills.is_empty() && s.channel.is_none());
        assert!(s.next_due(None).is_none(), "手动工作流没有下一次");

        let bad = [
            ("---\nname: a\ndescription: x\n---\n步骤\n", "schedule"),
            ("---\nname: a\ndescription: x\nschedule: daily 12:00\n---\n", "正文"),
            ("---\nname: a\ndescription: x\nschedule: daily 12:00\negress: [https://x.com/api]\n---\n步骤\n", "egress"),
            ("---\nname: a\ndescription: x\nschedule: daily 12:00\noutputs: [docx]\n---\n步骤\n", "outputs"),
            ("---\nname: a\ndescription: x\nschedule: daily 12:00\nsensitivity: secret\n---\n步骤\n", "sensitivity"),
            ("---\nname: a\ndescription: x\nschedule: daily 12:00\nmeta:\n  nested: 1\n---\n步骤\n", "嵌套"),
            ("---\nname: Bad\ndescription: x\nschedule: manual\n---\n步骤\n", "name"),
        ];
        for (text, why) in bad {
            let e = WorkflowSpec::parse(text, Path::new("/x/a/WORKFLOW.md"), Path::new("/x"), 0).unwrap_err();
            assert!(e.to_string().contains(why), "{why}: {e}");
        }
    }

    #[test]
    fn next_due_anchors_on_last_trigger_or_definition_mtime() {
        use chrono::{Datelike, TimeZone, Timelike};
        let s = WorkflowSpec::parse(EXAMPLE, Path::new("/x/daily-brief/WORKFLOW.md"), Path::new("/x"), 0).unwrap();
        let t = |y, mo, d, h, mi| chrono::Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap();
        let ms = |dt: chrono::DateTime<chrono::Local>| dt.timestamp_millis() as u64;
        // 从没跑过、定义写于 23 日 15:00 ⇒ 第一次是 24 日 12:00,不会一发现就跑
        let mut fresh = s.clone();
        fresh.mtime_ms = ms(t(2026, 9, 23, 15, 0));
        let n = fresh.next_due(None).unwrap();
        assert_eq!((n.day(), n.hour()), (24, 12));
        // 上次 22 日 12:00 触发,定义更早 ⇒ 下次 23 日 12:00;哪怕现在已经 23 日 20:00,也只补这一次
        let n = fresh.next_due(Some(ms(t(2026, 9, 22, 12, 0)))).unwrap();
        assert_eq!((n.day(), n.hour()), (24, 12), "锚点取 max(上次触发, 定义修改时间)");
        let mut old = s.clone();
        old.mtime_ms = ms(t(2026, 9, 1, 0, 0));
        let n = old.next_due(Some(ms(t(2026, 9, 22, 12, 0)))).unwrap();
        assert_eq!((n.day(), n.hour()), (23, 12));
    }

    #[test]
    fn discover_requires_dir_name_to_match() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("daily-brief")).unwrap();
        std::fs::write(root.path().join("daily-brief/WORKFLOW.md"), EXAMPLE).unwrap();
        std::fs::create_dir_all(root.path().join("wrong")).unwrap();
        std::fs::write(root.path().join("wrong/WORKFLOW.md"), EXAMPLE).unwrap();
        let d = discover(&[root.path().to_path_buf(), PathBuf::from("/nonexistent")]);
        assert_eq!(d.workflows.len(), 1);
        assert_eq!(d.workflows[0].name, "daily-brief");
        assert!(d.workflows[0].mtime_ms > 0);
        assert_eq!(d.problems.len(), 1);
        assert!(d.problems[0].contains("目录"));
    }
}
