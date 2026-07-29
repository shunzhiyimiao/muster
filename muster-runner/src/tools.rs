//! 只读工具集 v0:`list_dir` / `read_file` / `grep`,全部圈禁在工作区内。
//!
//! 约定:执行结果永远是 `String`(包括拒绝与错误)——模型应当看见边界与
//! 失败原因;是否据此调整策略是模型的事,Runner 不替它遮掩。

use std::path::{Path, PathBuf};

use muster_provider::ToolSpec;

const READ_CAP_BYTES: usize = 16_000;
const LIST_CAP: usize = 200;
const GREP_CAP: usize = 50;
const GREP_FILE_CAP_BYTES: u64 = 1_000_000;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", ".muster"];

pub struct ToolSet {
    workspace: PathBuf,
}

impl ToolSet {
    /// `workspace` 必须已存在;canonicalize 一次,之后所有路径都对它取证。
    pub fn new(workspace: &Path) -> std::io::Result<Self> {
        Ok(Self { workspace: workspace.canonicalize()? })
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// 向模型公示的工具清单(JSON Schema)。
    pub fn specs(&self) -> Vec<ToolSpec> {
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
    pub fn execute(&self, name: &str, arguments: &str) -> String {
        let args: serde_json::Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => return format!("参数不是合法 JSON:{e}"),
        };
        match name {
            "list_dir" => self.list_dir(args["path"].as_str().unwrap_or(".")),
            "read_file" => match args["path"].as_str() {
                Some(p) => self.read_file(p),
                None => "缺少必填参数 path".into(),
            },
            "grep" => match args["pattern"].as_str() {
                Some(pat) => self.grep(pat, args["path"].as_str().unwrap_or(".")),
                None => "缺少必填参数 pattern".into(),
            },
            other => format!("未知工具 {other}"),
        }
    }

    /// 路径圈禁:相对路径 join 工作区后 canonicalize,必须仍在工作区内。
    fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        if Path::new(rel).is_absolute() {
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
                        hits.push(format!("{}:{}: {}", rel_path.display(), ln + 1, shown));
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
}
