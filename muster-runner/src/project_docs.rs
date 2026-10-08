//! 仓库说明(AGENTS.md):与 Codex 同一约定,常驻系统提示词。
//!
//! 规则照抄 codex 的 `codex-rs/core/src/agents_md.rs`:
//! - 从项目根到当前目录,每一级目录取**第一个存在**的候选文件:`AGENTS.override.md` 优先于 `AGENTS.md`;
//! - 按"根 → 当前目录"的顺序拼接;总字节预算 32 KiB,超出的部分截断并如实标注;空文件跳过;
//! - 不越过项目根。
//!
//! 与技能的分工:技能是"做某类事时怎么做",按需读;AGENTS.md 是"在这个仓库里永远要遵守什么",
//! 每次都在。Muster 的任务没有"当前目录"概念,运行在工作区根,所以通常只有根目录那一份;
//! 函数仍按链实现,留给以后带 cwd 的任务。一个已经按 Codex 规范写好 AGENTS.md 的仓库,
//! 换成 Muster 的 Agent 来做事,看到的是同一份说明。

use std::path::{Path, PathBuf};

use muster_audit::ContentHash;

/// 与 Codex 的 `project_doc_max_bytes` 默认值一致。
pub const PROJECT_DOC_MAX_BYTES: usize = 32 * 1024;
/// 每一级目录按此顺序取第一个存在的文件。
pub const PROJECT_DOC_CANDIDATES: [&str; 2] = ["AGENTS.override.md", "AGENTS.md"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDocs {
    /// 拼接后的正文(可能被截断)。
    pub text: String,
    /// 实际读到的文件,根在前。
    pub sources: Vec<PathBuf>,
    /// 读入的字节数(截断后)。
    pub bytes: usize,
    pub truncated: bool,
    /// 正文哈希:进 run.start 的 tool_env——有没有仓库说明、说明是哪一版,是不同的运行。
    pub hash: ContentHash,
}

/// 从 `root` 到 `cwd`(含两端)逐级收集仓库说明。`cwd` 不在 `root` 之下时只看 `cwd`。
/// 一个都没有(或全是空文件)⇒ `None`。
pub fn load_project_docs(root: &Path, cwd: &Path) -> Option<ProjectDocs> {
    load_project_docs_with_budget(root, cwd, PROJECT_DOC_MAX_BYTES)
}

pub fn load_project_docs_with_budget(root: &Path, cwd: &Path, max_bytes: usize) -> Option<ProjectDocs> {
    if max_bytes == 0 {
        return None;
    }
    let dirs = search_dirs(root, cwd);
    let mut remaining = max_bytes;
    let mut parts: Vec<String> = Vec::new();
    let mut sources = Vec::new();
    let mut truncated = false;
    for dir in dirs {
        if remaining == 0 {
            truncated = true;
            break;
        }
        let Some(path) = PROJECT_DOC_CANDIDATES.iter().map(|n| dir.join(n)).find(|p| p.is_file()) else { continue };
        let Ok(mut data) = std::fs::read(&path) else { continue };
        if data.len() > remaining {
            data.truncate(remaining);
            truncated = true;
        }
        remaining -= data.len();
        let text = String::from_utf8_lossy(&data).to_string();
        if text.trim().is_empty() {
            continue;
        }
        parts.push(text);
        sources.push(path);
    }
    if parts.is_empty() {
        return None;
    }
    let text = parts.join("\n\n");
    Some(ProjectDocs {
        bytes: text.len(),
        hash: ContentHash::sha256(text.as_bytes()),
        text,
        sources,
        truncated,
    })
}

/// 根 → cwd 的目录链;cwd 不在 root 下就只有 cwd 自己(与 codex 找不到项目根时一致)。
fn search_dirs(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut cursor = cwd.to_path_buf();
    loop {
        dirs.push(cursor.clone());
        if cursor == root {
            break;
        }
        match cursor.parent() {
            Some(p) if cwd.starts_with(root) => cursor = p.to_path_buf(),
            _ => break,
        }
    }
    dirs.reverse();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_in_the_same_dir_and_chain_goes_root_first() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::write(root.join("AGENTS.md"), "根说明\n").unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/AGENTS.md"), "a 层说明\n").unwrap();
        std::fs::write(root.join("a/AGENTS.override.md"), "a 层覆盖\n").unwrap();
        std::fs::write(root.join("a/b/AGENTS.md"), "   \n").unwrap(); // 空文件跳过
        let docs = load_project_docs(root, &root.join("a/b")).unwrap();
        assert_eq!(docs.text, "根说明\n\n\na 层覆盖\n");
        assert_eq!(docs.sources, vec![root.join("AGENTS.md"), root.join("a/AGENTS.override.md")]);
        assert!(!docs.truncated);
        assert_eq!(docs.hash, ContentHash::sha256(docs.text.as_bytes()));
    }

    #[test]
    fn missing_docs_give_none_and_budget_truncates_honestly() {
        let d = tempfile::tempdir().unwrap();
        assert!(load_project_docs(d.path(), d.path()).is_none());
        std::fs::write(d.path().join("AGENTS.md"), "x".repeat(100)).unwrap();
        let docs = load_project_docs_with_budget(d.path(), d.path(), 40).unwrap();
        assert_eq!(docs.bytes, 40);
        assert!(docs.truncated, "超预算必须明说,不能悄悄砍");
        assert!(load_project_docs_with_budget(d.path(), d.path(), 0).is_none(), "预算 0 = 关闭");
    }

    #[test]
    fn cwd_outside_root_only_reads_cwd() {
        let d = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "根\n").unwrap();
        std::fs::write(other.path().join("AGENTS.md"), "别处\n").unwrap();
        let docs = load_project_docs(d.path(), other.path()).unwrap();
        assert_eq!(docs.text, "别处\n", "不越过项目根:cwd 不在 root 下就只看 cwd");
    }
}
