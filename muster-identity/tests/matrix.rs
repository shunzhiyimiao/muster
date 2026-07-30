//! 权限决策矩阵:穷举全部(主体 × 动作 × 作用域 × 策略)组合验证五条不变量。
//! 与 E2 的 `tests/matrix.rs` 同一手法——判定是纯函数,所以能这么验。

use muster_identity::{
    can, Action, Decision, Directory, OrgProhibitions, Principal, PrincipalKind, Role, RoleBinding,
    Scope,
};

const ROLES: [Role; 9] = [
    Role::OrgOwner,
    Role::OrgAdmin,
    Role::GroupAdmin,
    Role::Publisher,
    Role::Approver,
    Role::Member,
    Role::Guest,
    Role::Bot,
    Role::RunnerAccount,
];

fn actions() -> Vec<Action> {
    vec![
        Action::SendMessage,
        Action::CreateTask,
        Action::ApproveMerge,
        Action::ForgeCapsule,
        Action::AdoptCapsule,
        Action::ToggleDrill,
        Action::ChangePolicy,
        Action::ViewAudit,
        Action::LoginUi,
    ]
}

fn scopes() -> Vec<Scope> {
    vec![
        Scope::Org,
        Scope::Group("platform".into()),
        Scope::Group("pay".into()),
        Scope::Channel("platform-main".into()),
        Scope::Channel("pay-main".into()),
    ]
}

fn dir() -> Directory {
    Directory::default()
        .with_channel("platform-main", "platform")
        .with_channel("pay-main", "pay")
}

fn kinds() -> Vec<PrincipalKind> {
    vec![PrincipalKind::Human, PrincipalKind::Agent, PrincipalKind::Service]
}

fn principal(kind: PrincipalKind, binding: RoleBinding) -> Principal {
    let bindings = vec![binding];
    match kind {
        PrincipalKind::Human => Principal::human("u1", "测试人", bindings),
        PrincipalKind::Agent => Principal::agent("A-007", "小七", bindings),
        PrincipalKind::Service => {
            let mut p = Principal::service("runner-1", "Runner");
            p.bindings = bindings;
            p
        }
    }
}

/// 穷举:9 角色 × 5 授权作用域 × 3 主体类型 × 9 动作 × 5 目标作用域 × 2 策略
/// = 12,150 种组合,逐条验五条不变量。
#[test]
fn exhaustive_matrix_upholds_invariants() {
    let d = dir();
    let mut checked = 0usize;
    let mut allowed = 0usize;

    for role in ROLES {
        for grant_scope in scopes() {
            for kind in kinds() {
                let p = principal(kind, RoleBinding { role, scope: grant_scope.clone() });
                for action in actions() {
                    for target in scopes() {
                        for freeze in [false, true] {
                            let proh = OrgProhibitions {
                                freeze_writes: freeze,
                                task_forbidden_channels: vec![],
                            };
                            let dec = can(&p, &action, &target, &proh, &d);
                            checked += 1;
                            if dec.allowed() {
                                allowed += 1;
                            }

                            // I1:组织级禁止不可推翻——冻结时任何写操作都不许
                            if freeze {
                                let is_write = matches!(
                                    action,
                                    Action::SendMessage
                                        | Action::CreateTask
                                        | Action::ApproveMerge
                                        | Action::ForgeCapsule
                                        | Action::AdoptCapsule
                                        | Action::ChangePolicy
                                );
                                if is_write {
                                    assert!(
                                        !dec.allowed(),
                                        "I1 破:{role:?}@{grant_scope:?} 在冻结期做了 {action:?}"
                                    );
                                }
                            }

                            // I3:裁决必须由人
                            if action == Action::ApproveMerge && kind != PrincipalKind::Human {
                                assert!(!dec.allowed(), "I3 破:非人类裁决了合入({kind:?})");
                            }

                            // I4:服务账号不得登录 UI
                            if action == Action::LoginUi && kind == PrincipalKind::Service {
                                assert!(!dec.allowed(), "I4 破:服务账号登录了 UI");
                            }

                            // I2:允许时,授权作用域必须真的覆盖目标
                            if let Decision::Allow { via } = &dec {
                                assert_eq!(*via, role, "允许理由必须是本人持有的角色");
                                assert!(
                                    d.covers(&grant_scope, &target),
                                    "I2 破:{grant_scope:?} 不覆盖 {target:?} 却允许了 {action:?}"
                                );
                            }

                            // §4.3 硬限制:访客永不得发起任务(调用敏感 Runner)
                            if role == Role::Guest && action == Action::CreateTask {
                                assert!(!dec.allowed(), "访客不得调用敏感 Runner");
                            }

                            // I5:确定性
                            let again = can(&p, &action, &target, &proh, &d);
                            assert_eq!(dec, again, "I5 破:同输入不同输出");
                        }
                    }
                }
            }
        }
    }

    assert_eq!(checked, 9 * 5 * 3 * 9 * 5 * 2);
    assert!(allowed > 0 && allowed < checked, "矩阵不该全允许或全拒绝:{allowed}/{checked}");
    println!("穷举 {checked} 组,允许 {allowed} 组");
}

/// I2 的正面用例:组级授权覆盖**本组**频道,但绝不跨组。
#[test]
fn group_grant_covers_own_channels_only() {
    let d = dir();
    let p = Principal::human("alice", "Alice", vec![RoleBinding::group(Role::GroupAdmin, "platform")]);

    let own = can(&p, &Action::CreateTask, &Scope::Channel("platform-main".into()), &Default::default(), &d);
    assert!(own.allowed(), "{}", own.reason_zh());

    let other = can(&p, &Action::CreateTask, &Scope::Channel("pay-main".into()), &Default::default(), &d);
    assert!(!other.allowed());
    assert!(other.reason_zh().contains("作用域"), "{}", other.reason_zh());

    let other_group = can(&p, &Action::CreateTask, &Scope::Group("pay".into()), &Default::default(), &d);
    assert!(!other_group.allowed(), "GroupAdmin 不得跨组");
}

/// Channel 级授权**不会向上放大**成组级——否则"开一个频道"会变成"管整个组"。
#[test]
fn channel_grant_never_widens_to_group() {
    let d = dir();
    let p = Principal::human("bob", "Bob", vec![RoleBinding::channel(Role::GroupAdmin, "platform-main")]);
    assert!(can(&p, &Action::CreateTask, &Scope::Channel("platform-main".into()), &Default::default(), &d).allowed());
    assert!(!can(&p, &Action::CreateTask, &Scope::Group("platform".into()), &Default::default(), &d).allowed());
}

/// 拒绝理由必须分层:三种问题的解法完全不同。
#[test]
fn denials_distinguish_policy_role_and_scope() {
    let d = dir();

    // 角色不够
    let guest = Principal::human("g", "访客", vec![RoleBinding::org(Role::Guest)]);
    let r = can(&guest, &Action::ChangePolicy, &Scope::Org, &Default::default(), &d);
    assert!(r.reason_zh().contains("角色"), "{}", r.reason_zh());

    // 作用域不覆盖(角色是有的)
    let ga = Principal::human("a", "A", vec![RoleBinding::group(Role::GroupAdmin, "platform")]);
    let r = can(&ga, &Action::CreateTask, &Scope::Group("pay".into()), &Default::default(), &d);
    assert!(r.reason_zh().contains("作用域"), "{}", r.reason_zh());

    // 组织策略挡的(角色与作用域都够)
    let owner = Principal::human("o", "O", vec![RoleBinding::org(Role::OrgOwner)]);
    let proh = OrgProhibitions { freeze_writes: true, ..Default::default() };
    let r = can(&owner, &Action::CreateTask, &Scope::Org, &proh, &d);
    assert!(r.reason_zh().contains("组织策略"), "{}", r.reason_zh());
    // 只读动作不受冻结影响
    assert!(can(&owner, &Action::ViewAudit, &Scope::Org, &proh, &d).allowed());
}

/// §4.3 的两条硬限制:访客不能调敏感 Runner;禁任务频道对谁都禁。
#[test]
fn role_and_channel_hard_limits() {
    let d = dir();
    let guest = Principal::human("g", "访客", vec![RoleBinding::org(Role::Guest)]);
    assert!(can(&guest, &Action::SendMessage, &Scope::Org, &Default::default(), &d).allowed());
    let r = can(&guest, &Action::CreateTask, &Scope::Org, &Default::default(), &d);
    assert!(!r.allowed() && r.reason_zh().contains("访客"), "{}", r.reason_zh());

    let owner = Principal::human("o", "O", vec![RoleBinding::org(Role::OrgOwner)]);
    let proh = OrgProhibitions {
        freeze_writes: false,
        task_forbidden_channels: vec!["platform-main".into()],
    };
    let r = can(&owner, &Action::CreateTask, &Scope::Channel("platform-main".into()), &proh, &d);
    assert!(!r.allowed() && r.reason_zh().contains("组织策略"), "连 OrgOwner 也挡:{}", r.reason_zh());
}
