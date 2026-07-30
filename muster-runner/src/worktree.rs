//! 每 Task Run 一个独立 git worktree(总规划 §7.4)。
//!
//! **为什么 worktree 是"允许写"的前提**:B1 v0 只给只读工具,理由是"无审批
//! 不写"。worktree 化解了这个张力——写发生在**隔离分支的独立目录**里,用户
//! 的工作区一个字节都不动;产出 diff 供人审阅;真正需要审批的是**合入与
//! push**,而不是在沙盒里写字。所以本模块落地后,写工具才被允许启用。
//!
//! ## §7.4 的规则如何落地
//!
//! | 规则 | 实现 |
//! |---|---|
//! | 一个 Task Run 一个独立 Worktree,禁止共享可写目录 | [`Worktree::create`] 以 run_id 命名目录与分支 |
//! | 分支名含 task_id | `muster/run-<run_id>` |
//! | 执行前检测基础仓库状态 | 非 git 仓 / 分支已存在 → 明确报错,不猜 |
//! | 命令 cwd 约束在 worktree 内 | 工具集 canonicalize 圈禁(见 [`crate::tools`]) |
//! | 结束先保存 Diff 再按策略清理 | [`Worktree::diff`] 先于 [`Worktree::cleanup`] |
//! | 自动 Push / PR 需单独授权 | **本模块不提供**,连接口都不给 |

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("不是 git 仓库:{0}")]
    NotGitRepo(String),
    #[error("git {op} 失败:{stderr}")]
    Git { op: String, stderr: String },
    #[error("worktree 已存在(run_id 重复?):{0}")]
    Exists(String),
}

/// 一个 run 的隔离工作区。`Drop` 不自动清理——diff 必须先被取走
/// (§7.4:先保存证据,再依保留策略清理)。
#[derive(Debug)]
pub struct Worktree {
    /// 基础仓库路径。
    pub base: PathBuf,
    /// worktree 目录(命令的 cwd)。
    pub path: PathBuf,
    /// 分支名,含 run_id。
    pub branch: String,
    /// 建 worktree 时基础仓的 HEAD(diff 的对照基线)。
    pub base_commit: String,
}

/// 单个文件的变更摘要(UI 直接消费)。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// `A` 新增 / `M` 修改 / `D` 删除 / `R` 重命名。
    pub status: String,
    pub added: u32,
    pub removed: u32,
}

/// 一个 run 的完整变更(diff 正文属 run 存储侧,审计只存其哈希)。
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct RunDiff {
    pub files: Vec<FileChange>,
    /// 统一 diff 全文(可能很长,UI 侧自行折叠)。
    pub patch: String,
    pub files_changed: usize,
    pub insertions: u32,
    pub deletions: u32,
}

impl RunDiff {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.patch.is_empty()
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| WorktreeError::Git { op: args.join(" "), stderr: e.to_string() })?;
    if !out.status.success() {
        return Err(WorktreeError::Git {
            op: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Worktree {
    /// 在 `base` 仓库上为 `run_id` 建一个独立 worktree(从当前 HEAD)。
    ///
    /// `parent` 是 worktree 的落地根目录(总规划的 `WORKSPACE_ROOT`)。
    pub fn create(base: &Path, parent: &Path, run_id: &str) -> Result<Self, WorktreeError> {
        Self::create_at(base, parent, run_id, None)
    }

    /// 从**指定基线** commit 建 worktree——影子重放靠它把工作区对齐到锻造时的
    /// 代码状态,而不是拿今天的 HEAD 去跑昨天的能力。
    /// `at = None` 等价于 [`create`](Self::create)。
    pub fn create_at(
        base: &Path,
        parent: &Path,
        run_id: &str,
        at: Option<&str>,
    ) -> Result<Self, WorktreeError> {
        let base = base
            .canonicalize()
            .map_err(|e| WorktreeError::NotGitRepo(format!("{}: {e}", base.display())))?;
        // 必须是 git 仓:非 git 目录不猜、不降级,直接报错(上层可选择不用 worktree)
        let head = match at {
            // 指定基线必须真实存在——不存在就报错,绝不静默退回 HEAD
            // (那会让"在锻造基线上重放"变成"在今天的代码上重放"而无人察觉)
            Some(rev) => git(&base, &["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
                .map_err(|e| WorktreeError::Git {
                    op: format!("解析基线 {rev}"),
                    stderr: format!("{e};该 commit 在本仓库中不存在"),
                })?
                .trim()
                .to_owned(),
            None => git(&base, &["rev-parse", "HEAD"])
                .map_err(|_| WorktreeError::NotGitRepo(base.display().to_string()))?
                .trim()
                .to_owned(),
        };

        let slug: String =
            run_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let branch = format!("muster/run-{slug}");
        let path = parent.join(format!("run-{slug}"));
        if path.exists() {
            return Err(WorktreeError::Exists(path.display().to_string()));
        }
        std::fs::create_dir_all(parent).map_err(|e| WorktreeError::Git {
            op: "mkdir workspace root".into(),
            stderr: e.to_string(),
        })?;

        git(&base, &["worktree", "add", "-b", &branch, &path.display().to_string(), &head])?;
        Ok(Self { base, path: path.canonicalize().unwrap_or(path), branch, base_commit: head })
    }

    /// 相对建树时 HEAD 的全部变更(含未跟踪文件)。
    ///
    /// 先 `add -A` 进索引再 `diff --cached`:未跟踪的新文件也要计入,
    /// 否则"Agent 新建了文件"这种最常见的产出会被漏掉。索引改动只发生在
    /// worktree 内,不影响基础仓。
    pub fn diff(&self) -> Result<RunDiff, WorktreeError> {
        git(&self.path, &["add", "-A"])?;
        let numstat = git(&self.path, &["diff", "--cached", "--numstat", &self.base_commit])?;
        let namestatus = git(&self.path, &["diff", "--cached", "--name-status", &self.base_commit])?;
        let patch = git(&self.path, &["diff", "--cached", &self.base_commit])?;

        let mut status_of = std::collections::HashMap::new();
        for line in namestatus.lines() {
            let mut it = line.split('\t');
            if let (Some(st), Some(p)) = (it.next(), it.last()) {
                status_of.insert(p.to_owned(), st.chars().next().unwrap_or('M').to_string());
            }
        }

        let mut files = Vec::new();
        let (mut ins, mut del) = (0u32, 0u32);
        for line in numstat.lines() {
            let mut it = line.split('\t');
            let (a, r, p) = (it.next(), it.next(), it.last());
            let (Some(a), Some(r), Some(p)) = (a, r, p) else { continue };
            // 二进制文件 numstat 为 "-"
            let added: u32 = a.parse().unwrap_or(0);
            let removed: u32 = r.parse().unwrap_or(0);
            ins += added;
            del += removed;
            files.push(FileChange {
                status: status_of.get(p).cloned().unwrap_or_else(|| "M".into()),
                path: p.to_owned(),
                added,
                removed,
            });
        }

        Ok(RunDiff { files_changed: files.len(), files, patch, insertions: ins, deletions: del })
    }

    /// 把已暂存的改动提交到隔离分支。
    ///
    /// **不提交则分支等于没动过**:diff 只 `add -A` 进索引,分支 HEAD 仍停在
    /// 建树时的 commit,合入时 `git merge` 会是一场空(实测踩到)。§7.4 的
    /// 「保存 Diff、日志、证据**与 Commit**」里,Commit 这一项就是指这里。
    ///
    /// 作者署名固定为 Agent 工牌:主仓历史里必须能一眼看出这是机器产出的。
    pub fn commit(&self, badge: &str, message: &str) -> Result<String, WorktreeError> {
        git(&self.path, &["add", "-A"])?;
        // 无改动时 commit 会失败,交由调用方先判空(diff().is_empty())
        git(
            &self.path,
            &[
                "-c",
                &format!("user.name=Muster Agent {badge}"),
                "-c",
                "user.email=agent@muster.local",
                "commit",
                "-q",
                "-m",
                message,
            ],
        )?;
        Ok(git(&self.path, &["rev-parse", "HEAD"])?.trim().to_owned())
    }

    /// 移除 worktree 与其分支。**必须在 [`diff`](Self::diff) 之后调用**。
    pub fn cleanup(self) -> Result<(), WorktreeError> {
        git(&self.base, &["worktree", "remove", "--force", &self.path.display().to_string()])?;
        // 分支删除失败不算致命(可能已被手动处理),但要如实回报
        git(&self.base, &["branch", "-D", &self.branch]).map(|_| ())
    }
}

/// 保留策略(总规划 §7.4「先保存证据,再依保留策略清理」的后半句)。
///
/// **为什么不是"跑完就删"**:删掉 worktree 会把「可操作的改动」降级成
/// 「一段文本」——patch 还在,但没法 `git checkout` 检出来编译、没法
/// `git merge` 合入(只能 `git apply`,失去三方合并与冲突提示)。
/// 所以有变更的 run 必须留到**处置完毕**(合入或丢弃,即 P5 审批的结论)。
///
/// 三条规则,前两条不依赖审批即可执行:
/// 1. 无变更 ⇒ 立即清理(没有任何保留价值);
/// 2. 数量超过 `keep` ⇒ 回收最旧的(兜底,防止审批流失灵时无限堆积);
/// 3. 有变更且未处置 ⇒ 保留(等审批结论)。
#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    /// 最多保留几个"有变更但未处置"的 worktree。
    pub keep: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self { keep: 20 }
    }
}

/// 回收 `parent` 下超出保留上限的 worktree(按目录 mtime,最旧先回收)。
/// 返回被回收的目录名。**只动 `run-*` 目录**,不碰其它内容。
pub fn enforce_retention(
    base: &Path,
    parent: &Path,
    policy: RetentionPolicy,
) -> Result<Vec<String>, WorktreeError> {
    let Ok(rd) = std::fs::read_dir(parent) else { return Ok(Vec::new()) };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf, String)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("run-") {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path(), name))
        })
        .collect();
    if dirs.len() <= policy.keep {
        return Ok(Vec::new());
    }
    dirs.sort_by_key(|(m, _, _)| *m); // 最旧在前
    let excess = dirs.len() - policy.keep;
    let mut removed = Vec::new();
    for (_, path, name) in dirs.into_iter().take(excess) {
        // 清理失败不中断后续回收——尽力而为,但如实返回成功的那些
        if git(base, &["worktree", "remove", "--force", &path.display().to_string()]).is_ok() {
            let branch = format!("muster/{name}");
            let _ = git(base, &["branch", "-D", &branch]);
            removed.push(name);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        git(p, &["init", "-q", "-b", "main"]).unwrap();
        git(p, &["config", "user.email", "t@t"]).unwrap();
        git(p, &["config", "user.name", "t"]).unwrap();
        std::fs::write(p.join("main.rs"), "fn add(a: i32, b: i32) -> i32 { a - b }\n").unwrap();
        git(p, &["add", "-A"]).unwrap();
        git(p, &["commit", "-qm", "init"]).unwrap();
        d
    }

    #[test]
    fn isolated_write_does_not_touch_base_repo() {
        let base = repo();
        let root = tempfile::tempdir().unwrap();
        let wt = Worktree::create(base.path(), root.path(), "RUN-1").unwrap();
        assert!(wt.branch.contains("RUN-1"), "分支名必须含 run_id:{}", wt.branch);

        // 在 worktree 里改文件 + 新建文件
        std::fs::write(wt.path.join("main.rs"), "fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();
        std::fs::write(wt.path.join("NEW.md"), "新文件\n").unwrap();

        let d = wt.diff().unwrap();
        assert_eq!(d.files_changed, 2, "{:?}", d.files);
        assert!(d.patch.contains("a + b") && d.patch.contains("-fn add"), "{}", d.patch);
        let new_file = d.files.iter().find(|f| f.path == "NEW.md").expect("未跟踪的新文件必须计入");
        assert_eq!(new_file.status, "A");
        assert!(d.insertions >= 2);

        // 基础仓一个字节都没变——这是"允许写"的前提
        let base_src = std::fs::read_to_string(base.path().join("main.rs")).unwrap();
        assert!(base_src.contains("a - b"), "基础仓被污染了:{base_src}");
        assert!(!base.path().join("NEW.md").exists());

        wt.cleanup().unwrap();
    }

    #[test]
    fn non_git_dir_is_refused_not_guessed() {
        let plain = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let err = Worktree::create(plain.path(), root.path(), "RUN-2").unwrap_err();
        assert!(matches!(err, WorktreeError::NotGitRepo(_)), "{err:?}");
    }

    /// 保留策略的兜底规则:超过上限时回收最旧的,worktree 与分支一起清。
    #[test]
    fn retention_reclaims_oldest_beyond_limit() {
        let base = repo();
        let root = tempfile::tempdir().unwrap();
        for i in 0..3 {
            let wt = Worktree::create(base.path(), root.path(), &format!("RUN-{i}")).unwrap();
            // 制造 mtime 差异,保证"最旧先回收"可判定
            std::fs::write(wt.path.join("x.txt"), format!("{i}")).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        assert_eq!(git(base.path(), &["worktree", "list"]).unwrap().lines().count(), 4);

        let removed = enforce_retention(base.path(), root.path(), RetentionPolicy { keep: 1 }).unwrap();
        assert_eq!(removed.len(), 2, "3 个留 1 个应回收 2 个:{removed:?}");
        assert!(removed.contains(&"run-RUN-0".to_string()), "最旧的必须先走:{removed:?}");
        assert!(!root.path().join("run-RUN-0").exists());
        assert!(root.path().join("run-RUN-2").exists(), "最新的必须留下");

        // 分支也一并回收,不留孤儿
        let branches = git(base.path(), &["branch", "--list", "muster/run-*"]).unwrap();
        assert!(!branches.contains("RUN-0") && branches.contains("RUN-2"), "{branches}");
    }

    /// 影子重放的地基:能把工作区切到**指定基线**,而不是拿今天的 HEAD 去跑。
    #[test]
    fn worktree_can_be_created_at_a_given_baseline() {
        let base = repo();
        let root = tempfile::tempdir().unwrap();
        let old = git(base.path(), &["rev-parse", "HEAD"]).unwrap().trim().to_owned();

        // 主仓继续前进(模拟"改动已合入")
        std::fs::write(base.path().join("main.rs"), "fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();
        git(base.path(), &["add", "-A"]).unwrap();
        git(base.path(), &["commit", "-qm", "已修复"]).unwrap();
        let head = git(base.path(), &["rev-parse", "HEAD"]).unwrap().trim().to_owned();
        assert_ne!(old, head);

        // 从旧基线建树:内容应是修复**之前**的样子
        let wt = Worktree::create_at(base.path(), root.path(), "RUN-AT", Some(&old)).unwrap();
        assert_eq!(wt.base_commit, old);
        let src = std::fs::read_to_string(wt.path.join("main.rs")).unwrap();
        assert!(src.contains("a - b"), "必须回到锻造时的代码状态:{src}");
        // 此时 diff 相对旧基线为空(工作区就是那个基线)
        assert!(wt.diff().unwrap().is_empty());
        wt.cleanup().unwrap();

        // 不指定基线 ⇒ 用当前 HEAD(已修复的样子)
        let wt2 = Worktree::create(base.path(), root.path(), "RUN-HEAD").unwrap();
        assert!(std::fs::read_to_string(wt2.path.join("main.rs")).unwrap().contains("a + b"));
        wt2.cleanup().unwrap();
    }

    /// 指定的基线不存在时**必须报错**,绝不静默退回 HEAD——
    /// 那会让"在锻造基线上重放"变成"在今天的代码上重放"而无人察觉。
    #[test]
    fn nonexistent_baseline_is_an_error_not_a_silent_fallback() {
        let base = repo();
        let root = tempfile::tempdir().unwrap();
        let err = Worktree::create_at(base.path(), root.path(), "RUN-X", Some("0000000000000000000000000000000000000000"))
            .unwrap_err();
        assert!(err.to_string().contains("不存在"), "{err}");
        assert!(!root.path().join("run-RUN-X").exists(), "失败不得留下半成品");
    }

    #[test]
    fn retention_is_noop_within_limit() {
        let base = repo();
        let root = tempfile::tempdir().unwrap();
        let wt = Worktree::create(base.path(), root.path(), "RUN-K").unwrap();
        let removed = enforce_retention(base.path(), root.path(), RetentionPolicy::default()).unwrap();
        assert!(removed.is_empty());
        assert!(wt.path.exists(), "未超上限不得动任何东西");
    }

    #[test]
    fn no_change_yields_empty_diff() {
        let base = repo();
        let root = tempfile::tempdir().unwrap();
        let wt = Worktree::create(base.path(), root.path(), "RUN-3").unwrap();
        let d = wt.diff().unwrap();
        assert!(d.is_empty(), "{d:?}");
        assert_eq!((d.insertions, d.deletions), (0, 0));
        wt.cleanup().unwrap();
    }
}
