//! P5 审批:diff 的处置(合入 / 丢弃)必须经人裁决。
//!
//! **这是"写权限由隔离换取"的最后一环**:写在沙盒不需要审批,把沙盒里的
//! 改动落进主仓才需要。Runner 自己**永远不合入**——它只会提出申请
//! ([`request_merge`]),裁决与执行都由人触发([`decide`])。
//!
//! ## 铁律落地
//!
//! - 批准与拒绝**都写审计**(`approval.decision`),拒绝不是"什么都没发生"。
//! - 审批是 append-only 事件流:**已裁决的不能再裁决一次**([`decide`] 先查
//!   [`muster_audit::decision_of`]),不靠删行去重。
//! - 申请记「申请能力 vs 工牌能力」的依据:`requested_capability` +
//!   `badge_capabilities_hash`,与 A9 的字段设计一致。
//! - 正文不进审计:diff 正文只存 [`ContentHash`],合入靠 git 分支而非 patch 文本。

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use muster_audit::{
    decision_of, Actor, AuditStore, ContentHash, EventBody, NewEvent, Scope,
};

use crate::worktree::{RunDiff, WorktreeError};

/// 申请合入的能力名(工牌默认不具备,故必须审批)。
pub const CAP_MERGE: &str = "merge_to_main";

#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("审计写入失败:{0}")]
    Audit(String),
    #[error("该审批已被裁决为 {0},不可重复裁决")]
    AlreadyDecided(&'static str),
    #[error("合入失败:{0}")]
    Merge(String),
    #[error("worktree 操作失败:{0}")]
    Worktree(#[from] WorktreeError),
}

/// 由 run_id 推导审批号——一个 run 一次合入申请,天然幂等可追溯。
pub fn approval_id_for(run_id: &str) -> String {
    format!("APR-{run_id}")
}

/// 由 run_id 推导隔离分支名(与 [`crate::worktree::Worktree::create`] 同一规则)。
pub fn branch_for(run_id: &str) -> String {
    let slug: String =
        run_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    format!("muster/run-{slug}")
}

/// 任务产出变更后提出合入申请。**只提申请,不做任何写入主仓的动作**。
pub fn request_merge(
    audit: &Arc<Mutex<AuditStore>>,
    badge: &str,
    policy_version: &str,
    run_id: &str,
    scope: Scope,
    diff: &RunDiff,
) -> Result<String, ApprovalError> {
    let approval_id = approval_id_for(run_id);
    let reason = format!(
        "任务 {run_id} 在隔离分支 {} 上产出 {} 个文件变更(+{} −{}),申请合入主仓",
        branch_for(run_id),
        diff.files_changed,
        diff.insertions,
        diff.deletions
    );
    audit
        .lock()
        .unwrap()
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::agent(badge),
            scope,
            run_id: Some(run_id.to_owned()),
            session_id: None,
            policy_version: Some(policy_version.to_owned()),
            label: None,
            locality: None,
            body: EventBody::ApprovalRequest {
                approval_id: approval_id.clone(),
                requested_capability: CAP_MERGE.into(),
                // 工牌当前能力:只读工具 + 沙盒写,**不含**合入主仓
                badge_capabilities_hash: ContentHash::sha256(
                    br#"["read_workspace","write_worktree"]"#,
                ),
                // 申请内容 = diff 正文的哈希(正文留 run 存储侧)
                command_hash: ContentHash::sha256(diff.patch.as_bytes()),
                reason,
            },
        })
        .map_err(|e| ApprovalError::Audit(e.to_string()))?;
    Ok(approval_id)
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionOutcome {
    pub approval_id: String,
    pub granted: bool,
    /// 批准时:合入后主仓的新 HEAD。
    pub merged_commit: Option<String>,
    /// 处置完成后 worktree 是否已回收(保留策略第三条)。
    pub worktree_reclaimed: bool,
    pub detail: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, ApprovalError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| ApprovalError::Merge(format!("{}: {e}", args.join(" "))))?;
    if !out.status.success() {
        return Err(ApprovalError::Merge(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 人的裁决:批准则合入并回收,拒绝则直接回收。**两者都写审计**。
///
/// `decider` 是裁决人(进审计信封的 actor);`base` 是主仓路径;
/// `worktree_path` 用于回收(`None` 表示已不存在)。
pub fn decide(
    audit: &Arc<Mutex<AuditStore>>,
    decider: &str,
    policy_version: &str,
    run_id: &str,
    scope: Scope,
    base: &Path,
    worktree_path: Option<&Path>,
    granted: bool,
    note: Option<&str>,
) -> Result<DecisionOutcome, ApprovalError> {
    let approval_id = approval_id_for(run_id);
    // append-only:已裁决的不可再裁决
    {
        let store = audit.lock().unwrap();
        if let Some(prev) = decision_of(store.conn(), &approval_id)
            .map_err(|e| ApprovalError::Audit(e.to_string()))?
        {
            return Err(ApprovalError::AlreadyDecided(if prev { "批准" } else { "拒绝" }));
        }
    }

    let branch = branch_for(run_id);
    let mut merged_commit = None;
    let mut detail;

    if granted {
        // 合入用 git merge(而非 apply patch):保住三方合并与冲突提示。
        // --no-ff 让这次合入在主仓历史里留下明确的一笔。
        git(base, &["merge", "--no-ff", "-m", &format!("合入 {run_id}(经审批)"), &branch])?;
        let head = git(base, &["rev-parse", "HEAD"])?.trim().to_owned();
        detail = format!("已合入分支 {branch} → {}", &head[..head.len().min(8)]);
        merged_commit = Some(head);
    } else {
        detail = format!("已拒绝合入分支 {branch},改动被丢弃");
    }

    // 处置完成 ⇒ 回收 worktree(保留策略第三条,此前缺的就是这里)
    let mut reclaimed = false;
    if let Some(p) = worktree_path {
        match git(base, &["worktree", "remove", "--force", &p.display().to_string()]) {
            Ok(_) => {
                let _ = git(base, &["branch", "-D", &branch]);
                reclaimed = true;
            }
            Err(e) => detail.push_str(&format!(";worktree 回收失败:{e}")),
        }
    }

    // 裁决写审计——**批准与拒绝一视同仁**
    audit
        .lock()
        .unwrap()
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::human(decider),
            scope,
            run_id: Some(run_id.to_owned()),
            session_id: None,
            policy_version: Some(policy_version.to_owned()),
            label: None,
            locality: None,
            body: EventBody::ApprovalDecision {
                approval_id: approval_id.clone(),
                granted,
                note_hash: note.map(|n| ContentHash::sha256(n.as_bytes())),
            },
        })
        .map_err(|e| ApprovalError::Audit(e.to_string()))?;

    Ok(DecisionOutcome {
        approval_id,
        granted,
        merged_commit,
        worktree_reclaimed: reclaimed,
        detail,
    })
}
