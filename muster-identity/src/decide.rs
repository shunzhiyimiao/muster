//! 权限判定纯函数——总规划 §4.4「权限计算顺序」的可执行形态。
//!
//! ```text
//! 组织级禁止策略(最高优先级)
//!   ↓ Group 角色与仓库范围
//!   ↓ Channel 可见性和成员关系
//!   ↓ Workflow 权限模板
//!   ↓ Task 发起人权限
//!   ↓ Runner 能力与环境限制
//!   ↓ 具体 Tool / Shell / 文件 / 网络操作审批
//! ```
//!
//! **顺序本身是语义**:组织级禁止在最前,意味着任何角色都推翻不了它;
//! 越靠后的层级越具体,只能在前面允许的范围内继续收窄——**不能放宽**。

use serde::{Deserialize, Serialize};

use crate::model::{Directory, Principal, PrincipalKind, Role, Scope};

/// 被判定的动作。粒度对齐"会写进审计的事情"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// 在频道发消息。
    SendMessage,
    /// 发起会改动工作区的任务。
    CreateTask,
    /// 裁决合入申请(P5)。
    ApproveMerge,
    /// 锻造 Capsule(P4)。
    ForgeCapsule,
    /// 跨团队引入 Capsule。
    AdoptCapsule,
    /// 启停主权演习(E6)。
    ToggleDrill,
    /// 修改组织策略(cloud_max 等)。
    ChangePolicy,
    /// 查看审计。
    ViewAudit,
    /// 登录桌面 UI。
    LoginUi,
}

impl Action {
    pub fn zh(&self) -> &'static str {
        match self {
            Action::SendMessage => "发消息",
            Action::CreateTask => "发起任务",
            Action::ApproveMerge => "裁决合入",
            Action::ForgeCapsule => "锻造能力",
            Action::AdoptCapsule => "引入能力",
            Action::ToggleDrill => "启停主权演习",
            Action::ChangePolicy => "修改组织策略",
            Action::ViewAudit => "查看审计",
            Action::LoginUi => "登录界面",
        }
    }
}

/// 组织级禁止策略(§4.4 最高优先级)。**任何角色都推翻不了**。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrgProhibitions {
    /// 全组织冻结写操作(如安全事件响应期间)。
    pub freeze_writes: bool,
    /// 这些频道任何人不得发起任务(如正在审计的敏感频道)。
    pub task_forbidden_channels: Vec<String>,
}

/// 判定结果。**拒绝必须带理由**——UI 要显示,审计要记录,
/// "没有权限"四个字对使用者毫无帮助。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Allow {
        /// 凭哪条角色允许的(可追溯)。
        via: Role,
    },
    Deny {
        /// 在 §4.4 的哪一层被拒(便于定位是策略问题还是角色问题)。
        layer: &'static str,
        reason: String,
    },
}

impl Decision {
    pub fn allowed(&self) -> bool {
        matches!(self, Decision::Allow { .. })
    }
    pub fn reason_zh(&self) -> String {
        match self {
            Decision::Allow { via } => format!("允许(凭 {})", via.zh()),
            Decision::Deny { layer, reason } => format!("拒绝[{layer}]:{reason}"),
        }
    }
}

fn deny(layer: &'static str, reason: impl Into<String>) -> Decision {
    Decision::Deny { layer, reason: reason.into() }
}

/// 该动作是否属于"写"(受组织级冻结影响)。
fn is_write(action: &Action) -> bool {
    matches!(
        action,
        Action::SendMessage
            | Action::CreateTask
            | Action::ApproveMerge
            | Action::ForgeCapsule
            | Action::AdoptCapsule
            | Action::ChangePolicy
    )
}

/// 允许该动作的角色集合(按 §4.3 的职责表)。
fn roles_for(action: &Action) -> &'static [Role] {
    use Role::*;
    match action {
        Action::SendMessage => &[OrgOwner, OrgAdmin, GroupAdmin, Publisher, Approver, Member, Guest, Bot],
        // Guest 列在候选里、再由第 4 层明确拒绝——不是笔误:这样拒绝理由是
        // "访客不能调用敏感 Runner(设计使然)"而非"你缺成员角色(像是配错了)"。
        Action::CreateTask => &[OrgOwner, OrgAdmin, GroupAdmin, Publisher, Approver, Member, Guest],
        // §4.3:Approver「只能批准授权范围」;管理员天然具备
        Action::ApproveMerge => &[OrgOwner, OrgAdmin, GroupAdmin, Approver],
        Action::ForgeCapsule => &[OrgOwner, OrgAdmin, GroupAdmin, Publisher],
        Action::AdoptCapsule => &[OrgOwner, OrgAdmin, GroupAdmin, Publisher],
        // 演习切断全组织外联,只有组织级角色能开
        Action::ToggleDrill => &[OrgOwner, OrgAdmin],
        Action::ChangePolicy => &[OrgOwner],
        Action::ViewAudit => &[OrgOwner, OrgAdmin, GroupAdmin, Approver],
        Action::LoginUi => &[OrgOwner, OrgAdmin, GroupAdmin, Publisher, Approver, Member, Guest],
    }
}

/// **权限判定**:按 §4.4 的顺序逐层收窄,任一层拒绝即终止。
///
/// 纯函数:无 IO、无时钟、无全局状态,因此可被穷举验证
/// (`tests/matrix.rs`,与 E2 决策矩阵同一手法)。
pub fn can(
    principal: &Principal,
    action: &Action,
    target: &Scope,
    prohibitions: &OrgProhibitions,
    dir: &Directory,
) -> Decision {
    // ---- 第 0 层:身份属性(不是策略,是"这类主体天生不能做的事")
    if principal.kind == PrincipalKind::Service && *action == Action::LoginUi {
        return deny("身份", "Runner 服务账号不能作为人类账号登录 UI(§4.3)");
    }
    // 裁决必须由人做出:Agent 自己批准自己的申请会让 P5 审批形同虚设
    if principal.kind != PrincipalKind::Human && *action == Action::ApproveMerge {
        return deny("身份", "裁决必须由人做出,Agent 与服务账号不得自批");
    }

    // ---- 第 1 层:组织级禁止策略(最高优先级,任何角色推翻不了)
    if prohibitions.freeze_writes && is_write(action) {
        return deny("组织策略", "全组织写操作已冻结");
    }
    if *action == Action::CreateTask {
        if let Scope::Channel(c) = target {
            if prohibitions.task_forbidden_channels.iter().any(|x| x == c) {
                return deny("组织策略", format!("频道 {c} 已被组织策略禁止发起任务"));
            }
        }
    }

    // ---- 第 2~3 层:角色 + 作用域(Group 角色与仓库范围 / Channel 成员关系)
    let candidates = roles_for(action);
    let hit = candidates.iter().copied().find(|r| principal.has(*r, target, dir));
    let Some(via) = hit else {
        // 区分"角色不够"与"作用域不覆盖"——两种问题的解法完全不同
        let has_role_elsewhere = principal
            .bindings
            .iter()
            .any(|b| candidates.contains(&b.role));
        return if has_role_elsewhere {
            deny(
                "作用域",
                format!(
                    "持有可执行「{}」的角色,但授权范围未覆盖此处",
                    action.zh()
                ),
            )
        } else {
            deny(
                "角色",
                format!(
                    "执行「{}」需要以下角色之一:{}",
                    action.zh(),
                    candidates.iter().map(|r| r.zh()).collect::<Vec<_>>().join(" / ")
                ),
            )
        };
    };

    // ---- 第 4 层:角色自身的硬限制(§4.3「限制」列)
    if via == Role::Guest && *action == Action::CreateTask {
        return deny("角色限制", "访客不能调用敏感 Runner(§4.3)");
    }

    Decision::Allow { via }
}
