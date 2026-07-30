//! 身份与权限的类型层(总规划 §4.3 角色 / §4.4 权限计算顺序)。
//!
//! 设计要点:**作用域是角色的一部分,不是附属信息**。"Group Admin" 本身不是
//! 权限,"平台组的 Group Admin" 才是——所以 [`RoleBinding`] 把两者绑成一体,
//! 类型上不存在"没有作用域的角色"。§4.3 里那句"范围仅限所属 Group"因此
//! 无需运行时守卫,它在数据结构里就成立了。

use serde::{Deserialize, Serialize};

/// §4.3 的九个角色。顺序即权力大小(仅用于展示与排序,判定不靠比大小)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 组织策略、管理员、审计、部署配置。
    OrgOwner,
    /// 用户、Group、模型与 Runner 策略;**不能改系统签名与根审计**。
    OrgAdmin,
    /// 管理 Group / Channel / 成员 / 仓库绑定;**范围仅限所属 Group**。
    GroupAdmin,
    /// 提交、评审、发布工作流;**不得绕过审批与权限模板**。
    Publisher,
    /// 批准命令、网络、Secret、Push;**只能批准授权范围内的**。
    Approver,
    /// 发消息、创建任务、运行允许的工作流;不能改组织策略。
    Member,
    /// 受限频道只读或有限发言;**不能调用敏感 Runner**。
    Guest,
    /// 发系统消息、触发自动化;使用独立服务账号。
    Bot,
    /// 注册、心跳、领取回传任务;**不能作为人类账号登录 UI**。
    RunnerAccount,
}

impl Role {
    pub fn zh(&self) -> &'static str {
        match self {
            Role::OrgOwner => "组织所有者",
            Role::OrgAdmin => "组织管理员",
            Role::GroupAdmin => "组管理员",
            Role::Publisher => "工作流发布者",
            Role::Approver => "审批人",
            Role::Member => "成员",
            Role::Guest => "访客",
            Role::Bot => "机器人",
            Role::RunnerAccount => "Runner 服务账号",
        }
    }
}

/// 权限的作用域。`Org` 覆盖一切,`Group`/`Channel` 逐级收窄。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    Org,
    Group(String),
    Channel(String),
}

impl Scope {
    /// 本作用域是否覆盖目标资源所在的作用域。
    ///
    /// **不做跨级向上覆盖**:Channel 级授权管不到整个 Group,
    /// 否则"给某人开一个频道的权限"会悄悄放大成"管整个组"。
    pub fn covers(&self, target: &Scope) -> bool {
        match (self, target) {
            (Scope::Org, _) => true,
            (Scope::Group(a), Scope::Group(b)) => a == b,
            // 组级覆盖组内频道需要归属信息,由 [`Directory`] 提供,此处保守不覆盖
            (Scope::Group(_), Scope::Channel(_)) => false,
            (Scope::Channel(a), Scope::Channel(b)) => a == b,
            _ => false,
        }
    }
}

/// 一条授权:**角色 + 作用域**,缺一不可。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBinding {
    pub role: Role,
    pub scope: Scope,
}

impl RoleBinding {
    pub fn org(role: Role) -> Self {
        Self { role, scope: Scope::Org }
    }
    pub fn group(role: Role, group: impl Into<String>) -> Self {
        Self { role, scope: Scope::Group(group.into()) }
    }
    pub fn channel(role: Role, channel: impl Into<String>) -> Self {
        Self { role, scope: Scope::Channel(channel.into()) }
    }
}

/// 主体类型。人与非人必须在类型上分开——§4.3 要求
/// "Runner 服务账号不能作为人类账号登录 UI",这不是策略而是身份属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    Human,
    /// Agent(工牌持有者)。
    Agent,
    /// 服务账号(Runner / 集成)。
    Service,
}

/// 一个可发起动作的主体。
///
/// `id` 对人类是稳定用户 id(**不是邮箱**——§4.1 明确要求外部身份用
/// `iss + sub` 组合做稳定键,邮箱只是可变资料),对 Agent 是工牌号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub kind: PrincipalKind,
    pub display_name: String,
    pub bindings: Vec<RoleBinding>,
}

impl Principal {
    pub fn human(id: impl Into<String>, name: impl Into<String>, bindings: Vec<RoleBinding>) -> Self {
        Self {
            id: id.into(),
            kind: PrincipalKind::Human,
            display_name: name.into(),
            bindings,
        }
    }
    pub fn agent(badge: impl Into<String>, name: impl Into<String>, bindings: Vec<RoleBinding>) -> Self {
        Self {
            id: badge.into(),
            kind: PrincipalKind::Agent,
            display_name: name.into(),
            bindings,
        }
    }
    pub fn service(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: PrincipalKind::Service,
            display_name: name.into(),
            bindings: vec![RoleBinding::org(Role::RunnerAccount)],
        }
    }

    /// 是否持有某角色且其作用域覆盖目标。
    pub fn has(&self, role: Role, target: &Scope, dir: &Directory) -> bool {
        self.bindings
            .iter()
            .any(|b| b.role == role && dir.covers(&b.scope, target))
    }
}

/// 组织结构目录:回答"这个频道属于哪个组"。
///
/// 判定是纯函数,但"频道归属"是事实而非策略,必须由外部提供;
/// 没有它就无法安全地让组级授权覆盖组内频道。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Directory {
    /// channel_id → group_id
    pub channel_group: std::collections::BTreeMap<String, String>,
}

impl Directory {
    pub fn with_channel(mut self, channel: impl Into<String>, group: impl Into<String>) -> Self {
        self.channel_group.insert(channel.into(), group.into());
        self
    }

    /// 在已知归属的前提下判断覆盖:组级授权可覆盖**本组内**的频道。
    pub fn covers(&self, holder: &Scope, target: &Scope) -> bool {
        if holder.covers(target) {
            return true;
        }
        match (holder, target) {
            (Scope::Group(g), Scope::Channel(c)) => {
                self.channel_group.get(c).is_some_and(|owner| owner == g)
            }
            _ => false,
        }
    }

    /// 资源所在的组(用于把 Channel 级资源归位到组)。
    pub fn group_of<'a>(&'a self, scope: &'a Scope) -> Option<&'a str> {
        match scope {
            Scope::Group(g) => Some(g.as_str()),
            Scope::Channel(c) => self.channel_group.get(c).map(String::as_str),
            Scope::Org => None,
        }
    }
}
