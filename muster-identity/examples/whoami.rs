//! 快速查看某个角色能做什么(权限层的可验证性证明)。
//! 用法:MUSTER_ROLE=guest cargo run -p muster-identity --example whoami
use muster_identity::{can, Action, Directory, OrgProhibitions, Principal, Role, RoleBinding, Scope};

fn main() {
    let role = match std::env::var("MUSTER_ROLE").unwrap_or_default().as_str() {
        "admin" => Role::OrgAdmin,
        "group_admin" => Role::GroupAdmin,
        "publisher" => Role::Publisher,
        "approver" => Role::Approver,
        "member" => Role::Member,
        "guest" => Role::Guest,
        _ => Role::OrgOwner,
    };
    let p = Principal::human("probe", "探针", vec![RoleBinding::org(role)]);
    let (d, proh) = (Directory::default(), OrgProhibitions::default());
    for a in [
        Action::SendMessage, Action::CreateTask, Action::ApproveMerge,
        Action::ForgeCapsule, Action::ToggleDrill, Action::ChangePolicy, Action::ViewAudit,
    ] {
        let dec = can(&p, &a, &Scope::Org, &proh, &d);
        println!("  {:<12} {}", a.zh(), if dec.allowed() { "✓".to_string() } else { format!("✗ {}", dec.reason_zh()) });
    }
}
