//! 工具集:只读三件套 `list_dir`/`read_file`/`grep`,可写模式再加
//! `write_file`/`replace_in_file`/`run_command`,全部圈禁在工作区内。
//!
//! 约定:执行结果永远是 `String`(包括拒绝与错误)——模型应当看见边界与
//! 失败原因;是否据此调整策略是模型的事,Runner 不替它遮掩。
//!
//! `run_command` 的策略与理由在 [`crate::command`];此处只负责:它**只在
//! 隔离 worktree 里公示**(与写工具同一道闸),以及把它产生的审计事件带出去。

use std::path::{Path, PathBuf};

use muster_audit::EventBody;
use muster_provider::ToolSpec;

use crate::command::{self, CommandPolicy};

const READ_CAP_BYTES: usize = 16_000;
const LIST_CAP: usize = 200;
const GREP_CAP: usize = 50;
const GREP_FILE_CAP_BYTES: u64 = 1_000_000;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", ".muster"];

/// 一次工具调用的结果:给模型看的文本,以及**需要留痕时**的审计事件。
///
/// 读写文件不产生审计事件——变更本身已由 diff 与 `run.finish` 的
/// `output_hash` 覆盖。命令执行不同:它有退出码、耗时和「想跑什么」,
/// 这些在 diff 里一个字都看不出来,必须单独记。
pub struct ToolOutcome {
    pub text: String,
    pub audit: Option<EventBody>,
}

impl From<String> for ToolOutcome {
    fn from(text: String) -> Self {
        Self { text, audit: None }
    }
}

pub struct ToolSet {
    workspace: PathBuf,
    /// 是否启用写工具。**仅当工作区是隔离 worktree 时为真**——写在沙盒里
    /// 不需要审批,写用户的工作区才需要(见 crate 文档的设计决策 2)。
    writable: bool,
    /// 命令执行策略。与写工具同闸:只读模式下 `run_command` 既不公示也不执行。
    command: CommandPolicy,
}

/// 这条路径是不是**带根**的——即 join 到工作区上会把工作区整个顶掉。
///
/// 不能用 `Path::is_absolute()`:它在 Windows 上认不出 `/etc/hosts`,
/// 因为没有盘符就不算绝对路径。可 `join` 照样会因为它带根而丢掉工作区前缀,
/// 于是这条路径一样跑到工作区外面去——**判定说"它是相对路径",行为却不是**。
///
/// 还有更隐蔽的一种:`C:foo` 是**驱动器相对路径**,既非绝对、也无根,
/// 却相对于"C 盘的当前目录"解析,跟工作区没有任何关系。
///
/// 所以这里不问"是不是绝对路径",问的是"第一个组件是不是根或盘符"。
///
/// 真正的防线仍是后面的 canonicalize + starts_with——这一道是为了**早拒、
/// 且说得清**。不然 `/etc/hosts` 会以"文件不存在"的面目被拒绝,
/// 看日志的人完全不知道刚才发生的是一次越界尝试。
fn is_rooted(p: &Path) -> bool {
    use std::path::Component;
    matches!(p.components().next(), Some(Component::Prefix(_) | Component::RootDir))
}

impl ToolSet {
    /// 只读工具集(默认):用户工作区直连时的唯一合法形态。
    pub fn new(workspace: &Path) -> std::io::Result<Self> {
        Ok(Self {
            workspace: workspace.canonicalize()?,
            writable: false,
            command: CommandPolicy::default(),
        })
    }

    /// 可写工具集:**只应传入隔离 worktree 的路径**。
    /// 调用方(run_task)保证这一点;传用户工作区进来等于绕过审批。
    pub fn writable(workspace: &Path) -> std::io::Result<Self> {
        Ok(Self {
            workspace: workspace.canonicalize()?,
            writable: true,
            command: CommandPolicy::default(),
        })
    }

    /// 换一套命令允许清单(如 Capsule 里声明的 `shell.allow`)。
    /// 否决清单不受影响——见 [`crate::command`]。
    pub fn with_command_policy(mut self, p: CommandPolicy) -> Self {
        self.command = p;
        self
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// 向模型公示的工具清单(JSON Schema)。写工具仅在可写模式下出现。
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut v = self.read_specs();
        if self.writable {
            v.extend(self.write_specs());
        }
        v
    }

    fn write_specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "write_file".into(),
                description: "写入文件(覆盖全文;目录会自动创建)。仅限当前任务的隔离工作区。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "相对工作区的文件路径" },
                        "content": { "type": "string", "description": "文件的完整新内容" }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolSpec {
                name: "run_command".into(),
                description: format!(
                    "在当前隔离工作区里执行一条命令,用来**验证你改的东西**(编译、测试、检查)。\
                     不经 shell:不支持管道、重定向、变量替换、通配符,一次只给一条命令。\
                     允许清单:{}。",
                    self.command.allow_text()
                ),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "完整命令行,例如 \"cargo test\"" }
                    },
                    "required": ["command"]
                }),
            },
            ToolSpec {
                name: "replace_in_file".into(),
                description: "把文件中的一段旧文本替换为新文本。old 必须在文件中唯一出现,否则拒绝。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "相对工作区的文件路径" },
                        "old": { "type": "string", "description": "要被替换的原文(须唯一)" },
                        "new": { "type": "string", "description": "替换后的新文本" }
                    },
                    "required": ["path", "old", "new"]
                }),
            },
        ]
    }

    fn read_specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "list_dir".into(),
                description: "列出工作区内某目录的条目(目录带 / 后缀)。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "相对工作区的目录路径,\".\" 表示根" }
                    },
                    "required": ["path"]
                }),
            },
            ToolSpec {
                name: "read_file".into(),
                description: "读取工作区内的文本文件(最多 16KB,超出截断并注明)。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "相对工作区的文件路径" }
                    },
                    "required": ["path"]
                }),
            },
            ToolSpec {
                name: "grep".into(),
                description: "在工作区内递归查找包含子串的行(区分大小写,最多 50 条命中)。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "要查找的子串" },
                        "path": { "type": "string", "description": "限定的相对目录,缺省为整个工作区" }
                    },
                    "required": ["pattern"]
                }),
            },
        ]
    }

    /// 执行一次调用。`arguments` 是模型原样产出的 JSON 字符串。
    /// 只要文本、不要审计的调用方用 [`ToolSet::execute`]。
    pub fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        let args: serde_json::Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => return format!("参数不是合法 JSON:{e}").into(),
        };
        let text = match name {
            "list_dir" => self.list_dir(args["path"].as_str().unwrap_or(".")),
            "read_file" => match args["path"].as_str() {
                Some(p) => self.read_file(p),
                None => "缺少必填参数 path".into(),
            },
            "grep" => match args["pattern"].as_str() {
                Some(pat) => self.grep(pat, args["path"].as_str().unwrap_or(".")),
                None => "缺少必填参数 pattern".into(),
            },
            "write_file" | "replace_in_file" if !self.writable => {
                format!("拒绝:{name} 是写操作,当前工作区为只读(写入需在隔离 worktree 中进行)")
            }
            // 命令执行与写工具同闸:只读模式下连**尝试**都不放行。
            // 隔离才是它的安全前提,没有隔离就没有它。
            "run_command" if !self.writable => {
                "拒绝:run_command 只在任务的隔离工作区里可用(隔离是它的安全前提)".into()
            }
            "run_command" => {
                return match args["command"].as_str() {
                    Some(line) => {
                        let o = command::run(line, &self.workspace, &self.command);
                        ToolOutcome { text: o.text, audit: Some(o.audit) }
                    }
                    None => "缺少必填参数 command".to_string().into(),
                };
            }
            "write_file" => match (args["path"].as_str(), args["content"].as_str()) {
                (Some(p), Some(c)) => self.write_file(p, c),
                _ => "缺少必填参数 path / content".into(),
            },
            "replace_in_file" => {
                match (args["path"].as_str(), args["old"].as_str(), args["new"].as_str()) {
                    (Some(p), Some(o), Some(n)) => self.replace_in_file(p, o, n),
                    _ => "缺少必填参数 path / old / new".into(),
                }
            }
            other => format!("未知工具 {other}"),
        };
        text.into()
    }

    /// 只取文本的简便形式(测试与不需要留痕的场合)。
    pub fn execute(&self, name: &str, arguments: &str) -> String {
        self.call(name, arguments).text
    }

    /// 路径圈禁:相对路径 join 工作区后 canonicalize,必须仍在工作区内。
    fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        if is_rooted(Path::new(rel)) {
            return Err(format!("拒绝绝对路径 {rel}:仅允许工作区内的相对路径"));
        }
        let joined = self.workspace.join(rel);
        let canon = joined
            .canonicalize()
            .map_err(|e| format!("路径 {rel} 不可用:{e}"))?;
        if !canon.starts_with(&self.workspace) {
            return Err(format!("路径越界:{rel} 解析到工作区之外,已拒绝"));
        }
        Ok(canon)
    }

    /// 写入路径的圈禁:目标文件可能还不存在,故对**父目录**取证,
    /// 再拼回文件名。父目录不存在时逐级向上找已存在的祖先来验证,
    /// 这样 `../` 逃逸与符号链接逃逸都会在祖先这一步被挡下。
    fn resolve_for_write(&self, rel: &str) -> Result<PathBuf, String> {
        if is_rooted(Path::new(rel)) {
            return Err(format!("拒绝绝对路径 {rel}:仅允许工作区内的相对路径"));
        }
        let joined = self.workspace.join(rel);
        let file_name = joined
            .file_name()
            .ok_or_else(|| format!("路径 {rel} 没有文件名"))?
            .to_owned();
        let mut dir = joined.parent().unwrap_or(&self.workspace).to_path_buf();
        let mut tail = Vec::new();
        // 向上找到第一个已存在的祖先
        while !dir.exists() {
            let Some(name) = dir.file_name().map(|n| n.to_owned()) else {
                return Err(format!("路径 {rel} 无法解析"));
            };
            tail.push(name);
            let Some(parent) = dir.parent().map(|p| p.to_path_buf()) else {
                return Err(format!("路径 {rel} 无法解析"));
            };
            dir = parent;
        }
        let anchor = dir.canonicalize().map_err(|e| format!("路径 {rel} 不可用:{e}"))?;
        if !anchor.starts_with(&self.workspace) {
            return Err(format!("路径越界:{rel} 解析到工作区之外,已拒绝"));
        }
        let mut out = anchor;
        for seg in tail.into_iter().rev() {
            out.push(seg);
        }
        out.push(file_name);
        Ok(out)
    }

    fn write_file(&self, rel: &str, content: &str) -> String {
        let path = match self.resolve_for_write(rel) {
            Ok(p) => p,
            Err(e) => return e,
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return format!("创建目录失败:{e}");
            }
        }
        match std::fs::write(&path, content) {
            Ok(()) => format!("已写入 {rel}({} 字节)", content.len()),
            Err(e) => format!("写入失败:{e}"),
        }
    }

    fn replace_in_file(&self, rel: &str, old: &str, new: &str) -> String {
        let path = match self.resolve_for_write(rel) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => return format!("读取失败:{e}"),
        };
        match src.matches(old).count() {
            0 => format!("未找到待替换文本,文件未改动。请先 read_file 确认原文(含空白与缩进)。"),
            1 => match std::fs::write(&path, src.replacen(old, new, 1)) {
                Ok(()) => format!("已替换 {rel} 中的 1 处"),
                Err(e) => format!("写入失败:{e}"),
            },
            n => format!("拒绝:待替换文本在 {rel} 中出现 {n} 次,不唯一。请扩大 old 的上下文使其唯一。"),
        }
    }

    fn list_dir(&self, rel: &str) -> String {
        let dir = match self.resolve(rel) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => return format!("读取目录失败:{e}"),
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| {
                let mut n = e.file_name().to_string_lossy().into_owned();
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    n.push('/');
                }
                n
            })
            .collect();
        names.sort();
        let total = names.len();
        names.truncate(LIST_CAP);
        let mut out = names.join("\n");
        if total > LIST_CAP {
            out.push_str(&format!("\n…(共 {total} 项,已截断至 {LIST_CAP})"));
        }
        if out.is_empty() {
            out = "(空目录)".into();
        }
        out
    }

    fn read_file(&self, rel: &str) -> String {
        let path = match self.resolve(rel) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return format!("读取失败:{e}"),
        };
        let truncated = bytes.len() > READ_CAP_BYTES;
        let slice = if truncated { &bytes[..READ_CAP_BYTES] } else { &bytes[..] };
        let mut text = String::from_utf8_lossy(slice).into_owned();
        if truncated {
            text.push_str(&format!("\n…(文件共 {} 字节,已截断至 {READ_CAP_BYTES})", bytes.len()));
        }
        text
    }

    fn grep(&self, pattern: &str, rel: &str) -> String {
        let root = match self.resolve(rel) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let mut hits = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            if hits.len() >= GREP_CAP {
                break;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                if hits.len() >= GREP_CAP {
                    break;
                }
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(ft) = entry.file_type() else { continue };
                if ft.is_dir() {
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(path);
                    }
                    continue;
                }
                if !ft.is_file() {
                    continue;
                }
                if entry.metadata().map(|m| m.len() > GREP_FILE_CAP_BYTES).unwrap_or(true) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else { continue };
                let text = String::from_utf8_lossy(&bytes);
                for (ln, line) in text.lines().enumerate() {
                    if line.contains(pattern) {
                        let rel_path = path.strip_prefix(&self.workspace).unwrap_or(&path);
                        let mut shown: String = line.trim().chars().take(200).collect();
                        if line.trim().chars().count() > 200 {
                            shown.push('…');
                        }
                        // 分隔符一律用 `/`:这些路径要进提示词,模型还会原样传回来。
                        // 让同一个仓库在 Windows 上显示成 `sub\a.rs`、在别处显示成
                        // `sub/a.rs`,等于给模型两套路径写法。Windows 也吃 `/`。
                        let shown_path = rel_path.to_string_lossy().replace('\\', "/");
                        hits.push(format!("{shown_path}:{}: {shown}", ln + 1));
                        if hits.len() >= GREP_CAP {
                            break;
                        }
                    }
                }
            }
        }
        if hits.is_empty() {
            format!("未找到包含「{pattern}」的行")
        } else {
            let n = hits.len();
            let mut out = hits.join("\n");
            if n >= GREP_CAP {
                out.push_str(&format!("\n…(已达 {GREP_CAP} 条上限,可缩小范围再查)"));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> (tempfile::TempDir, ToolSet) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "你好,Muster\n第二行 needle").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.rs"), "fn main() { /* needle */ }").unwrap();
        let tools = ToolSet::new(dir.path()).unwrap();
        (dir, tools)
    }

    #[test]
    fn list_read_grep_roundtrip() {
        let (_d, t) = ws();
        let ls = t.execute("list_dir", r#"{"path":"."}"#);
        assert!(ls.contains("hello.txt") && ls.contains("sub/"), "{ls}");
        let rf = t.execute("read_file", r#"{"path":"hello.txt"}"#);
        assert!(rf.contains("你好,Muster"), "{rf}");
        let gr = t.execute("grep", r#"{"pattern":"needle"}"#);
        assert!(gr.contains("hello.txt:2") && gr.contains("sub/a.rs:1"), "{gr}");
    }

    /// 带根路径的判定,**逐平台列举**。
    ///
    /// 这条测试存在的理由:`is_rooted` 很容易被"简化"回 `Path::is_absolute()`,
    /// 而那个改动在 macOS 上一个测试都不会红——`/etc/hosts` 在 Unix 上
    /// 本来就是绝对路径。只有把 Windows 特有的形状写进来,这个退化才拦得住。
    #[test]
    fn rooted_paths_are_recognised_on_every_platform() {
        use super::is_rooted;
        use std::path::Path;

        // 两个平台都必须认出来
        assert!(is_rooted(Path::new("/etc/hosts")));
        assert!(is_rooted(Path::new("/")));

        // 老实的相对路径:一律放行到下一道闸(canonicalize + starts_with)
        for ok in ["a.txt", "sub/a.rs", "./a.txt", "../outside.txt", "..", "."] {
            assert!(!is_rooted(Path::new(ok)), "{ok} 不该被当成带根路径");
        }

        #[cfg(windows)]
        {
            assert!(is_rooted(Path::new(r"C:\Windows\System32")));
            assert!(is_rooted(Path::new(r"\Windows")), "反斜杠开头也是根");
            // **驱动器相对路径**:既非绝对、也无根,却相对于"C 盘的当前目录"
            // 解析——跟工作区毫无关系,是最容易漏掉的一种
            assert!(is_rooted(Path::new("C:foo")), "驱动器相对路径必须挡住");
            // UNC:直接指向网络共享,连本机都不在
            assert!(is_rooted(Path::new(r"\\server\share\f")), "UNC 路径必须挡住");
        }
        #[cfg(unix)]
        {
            // 在 Unix 上这些只是普通文件名,不该被误判成越界
            assert!(!is_rooted(Path::new("C:foo")));
            assert!(!is_rooted(Path::new(r"C:\Windows")));
        }
    }

    #[test]
    fn escape_attempts_are_refused_as_text() {
        let (_d, t) = ws();
        let abs = t.execute("read_file", r#"{"path":"/etc/hosts"}"#);
        assert!(abs.contains("拒绝绝对路径"), "{abs}");
        let up = t.execute("read_file", r#"{"path":"../outside.txt"}"#);
        assert!(up.contains("不可用") || up.contains("越界"), "{up}");
        let bad = t.execute("read_file", "not-json");
        assert!(bad.contains("不是合法 JSON"), "{bad}");
    }

    /// 只读工具集里写工具既不公示、也不执行——"无审批不写"的双重保证。
    #[test]
    fn read_only_set_refuses_writes() {
        let (_d, t) = ws();
        assert!(!t.is_writable());
        let names: Vec<_> = t.specs().iter().map(|s| s.name.clone()).collect();
        assert!(!names.iter().any(|n| n.starts_with("write") || n.starts_with("replace")), "{names:?}");
        let r = t.execute("write_file", r#"{"path":"x.txt","content":"x"}"#);
        assert!(r.contains("拒绝") && r.contains("只读"), "{r}");
        assert!(!_d.path().join("x.txt").exists(), "拒绝后不得留下副作用");
    }

    /// 命令执行与写工具同闸:没有隔离就没有它,既不公示也不执行。
    #[test]
    fn read_only_set_refuses_commands() {
        let (_d, t) = ws();
        assert!(!t.specs().iter().any(|s| s.name == "run_command"), "只读模式不该公示 run_command");
        let r = t.call("run_command", r#"{"command":"cargo test"}"#);
        assert!(r.text.contains("拒绝") && r.text.contains("隔离"), "{}", r.text);
        assert!(r.audit.is_none(), "没执行就不该伪造一条执行记录");
    }

    /// 可写模式下命令执行公示、可跑,并且**带回审计事件**。
    #[test]
    fn writable_set_runs_commands_and_returns_an_audit_record() {
        let (d, t) = ws_rw();
        assert!(t.specs().iter().any(|s| s.name == "run_command"));

        let t = t.with_command_policy(crate::CommandPolicy::with_allow(["git status"]));
        std::process::Command::new("git").arg("init").arg("-q").current_dir(d.path()).status().unwrap();
        let o = t.call("run_command", r#"{"command":"git status --short"}"#);
        assert!(o.text.contains("退出码 0"), "{}", o.text);
        assert!(matches!(o.audit, Some(EventBody::CommandRun { .. })), "命令执行必须留痕");

        // 文件读写不产生审计事件:变更本身已由 diff 覆盖,不重复记账
        assert!(t.call("read_file", r#"{"path":"main.rs"}"#).audit.is_none());
        assert!(t.call("write_file", r#"{"path":"a.txt","content":"x"}"#).audit.is_none());
    }

    fn ws_rw() -> (tempfile::TempDir, ToolSet) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn add(a: i32, b: i32) -> i32 { a - b }\n").unwrap();
        let t = ToolSet::writable(dir.path()).unwrap();
        (dir, t)
    }

    #[test]
    fn writable_set_creates_and_replaces() {
        let (d, t) = ws_rw();
        assert!(t.specs().iter().any(|s| s.name == "write_file"));

        // 新建(含新目录)
        let r = t.execute("write_file", r#"{"path":"src/new.rs","content":"fn x() {}\n"}"#);
        assert!(r.contains("已写入"), "{r}");
        assert_eq!(std::fs::read_to_string(d.path().join("src/new.rs")).unwrap(), "fn x() {}\n");

        // 唯一替换
        let r = t.execute("replace_in_file", r#"{"path":"main.rs","old":"a - b","new":"a + b"}"#);
        assert!(r.contains("已替换"), "{r}");
        assert!(std::fs::read_to_string(d.path().join("main.rs")).unwrap().contains("a + b"));
    }

    #[test]
    fn replace_refuses_ambiguous_and_missing_targets() {
        let (d, t) = ws_rw();
        std::fs::write(d.path().join("dup.rs"), "x = 1;\nx = 1;\n").unwrap();

        let dup = t.execute("replace_in_file", r#"{"path":"dup.rs","old":"x = 1;","new":"x = 2;"}"#);
        assert!(dup.contains("不唯一"), "{dup}");
        assert_eq!(std::fs::read_to_string(d.path().join("dup.rs")).unwrap(), "x = 1;\nx = 1;\n", "歧义时不得改动");

        let miss = t.execute("replace_in_file", r#"{"path":"main.rs","old":"不存在","new":"y"}"#);
        assert!(miss.contains("未找到"), "{miss}");
    }

    /// 写路径的圈禁:目标文件尚不存在,仍必须挡住 `../` 逃逸。
    #[test]
    fn write_escape_is_refused_without_creating_anything() {
        let (d, t) = ws_rw();
        let outside = d.path().parent().unwrap().join("evil.txt");
        let _ = std::fs::remove_file(&outside);

        let abs = t.execute("write_file", r#"{"path":"/tmp/evil.txt","content":"x"}"#);
        assert!(abs.contains("拒绝绝对路径"), "{abs}");
        let up = t.execute("write_file", r#"{"path":"../evil.txt","content":"x"}"#);
        assert!(up.contains("越界"), "{up}");
        assert!(!outside.exists(), "越界写必须不产生任何文件");
    }
}
