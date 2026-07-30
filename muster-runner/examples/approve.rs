//! 命令行裁决合入申请——与桌面壳走**同一条** `decide_as` 路径。
//!
//! 存在理由:无头环境、脚本化、以及在 UI 不可用时仍能处置待办。
//! 但语义不变:**裁决是人的决定**,本工具只是把那个决定录入系统,
//! 权限校验、审计留痕、worktree 回收一样不少。
//!
//! ```bash
//! cargo run -p muster-runner --example approve -- <审计库> <主仓> <run_id> [approve|reject] [说明]
//! ```

use std::path::Path;
use std::sync::{Arc, Mutex};

use muster_audit::{pending_approval_list, run_chain, AuditStore, Scope};
use muster_identity::{Directory, OrgProhibitions, Principal, Role, RoleBinding};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("用法:approve <审计库路径> <主仓路径> <run_id> [approve|reject] [说明]");
        std::process::exit(2);
    }
    let (db, base, run_id) = (&a[0], &a[1], &a[2]);
    let granted = a.get(3).map(String::as_str) != Some("reject");
    let note = a.get(4).cloned();

    let audit = Arc::new(Mutex::new(AuditStore::open(db)?));

    // 待裁决清单(先看清楚再裁决——这正是审批的意义)
    {
        let s = audit.lock().unwrap();
        let pending = pending_approval_list(s.conn(), None)?;
        println!("待裁决 {} 项:", pending.len());
        for p in &pending {
            println!("  {} · {} · {}", p.approval_id, p.actor_id, p.reason);
        }
        if !pending.iter().any(|p| p.run_id.as_deref() == Some(run_id.as_str())) {
            eprintln!("\n❌ {run_id} 不在待裁决列表中(可能已裁决或不存在)");
            std::process::exit(1);
        }
    }

    // 身份:与桌面壳同源的环境变量口径
    let role = match std::env::var("MUSTER_ROLE").unwrap_or_default().as_str() {
        "approver" => Role::Approver,
        "member" => Role::Member,
        "guest" => Role::Guest,
        _ => Role::OrgOwner,
    };
    let user = std::env::var("MUSTER_USER").unwrap_or_else(|_| "owner".into());
    let principal = Principal::human(user.clone(), user.clone(), vec![RoleBinding::org(role)]);

    // worktree 路径按 run_id 推导(与 Worktree::create 同一规则)
    let home = std::env::var("HOME")?;
    let root =
        std::env::var("MUSTER_WORKSPACE_ROOT").unwrap_or_else(|_| format!("{home}/.muster/worktrees"));
    let slug: String =
        run_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let wt = std::path::PathBuf::from(format!("{root}/run-{slug}"));

    println!("\n裁决人 {user}({role:?})· 动作 {}", if granted { "批准合入" } else { "拒绝" });
    let out = muster_runner::decide_as(
        &audit,
        Some((&principal, &Directory::default(), &OrgProhibitions::default())),
        &user,
        "policy-v1",
        run_id,
        Scope::default(),
        Path::new(base),
        wt.exists().then_some(wt.as_path()),
        granted,
        note.as_deref(),
    )?;

    println!("✓ {}", out.detail);
    println!("  worktree 已回收:{}", out.worktree_reclaimed);

    let s = audit.lock().unwrap();
    let chain: Vec<String> = run_chain(s.conn(), run_id)?
        .iter()
        .map(|e| e.payload["event_type"].as_str().unwrap_or("?").to_string())
        .collect();
    println!("\n审计链:{}", chain.join(" → "));
    println!("哈希链校验:{:?}", s.verify_chain()?);
    Ok(())
}
