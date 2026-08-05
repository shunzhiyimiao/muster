//! 组织 / 群组 / 频道 / 角色绑定(P2-05..08)。
//!
//! 每个写操作都先过 `muster_identity::can()`,**在动数据库之前**。
//! 这与桌面壳里 `decide_as` 的姿态一致:越权者被挡在副作用之前,系统状态零变化。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use muster_identity::{can, Action, Directory, OrgProhibitions, Scope};

use muster_audit::{Actor, ContentHash, EventBody, NewEvent, Scope as AuditScope};

use crate::audit::Audit;
use crate::auth::{hash_password, kind_str, parse_role, Identity};
use crate::db::now_ms;
use crate::{Db, Result, ServerError};

/// 组织管理接口的状态:库 + 链。**权限变更必须两者都动**——
/// 只改库不记链,等于"改谁能干什么"这件事悄无声息地发生了。
pub type OrgState = (Db, Audit);

/// 把一次权限变更记进链。用 `badge.update`:它的语义就是"某人的能力集变了",
/// 人和 Agent 一视同仁。**正文(具体授了什么)只留哈希**,与铁律三一致。
async fn record_grant_change(
    audit: &Audit,
    changed_by: &Identity,
    subject: &str,
    detail: &str,
) -> Result<()> {
    audit
        .append(NewEvent {
            ts_ms: None,
            // actor 是**被变更的人**,changed_by 进 payload——
            // 这样"谁的权限被动过"一句 SQL 就能按 actor_id 查出来
            actor: Actor::human(subject),
            scope: AuditScope::default(),
            run_id: None,
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body: EventBody::BadgeUpdate {
                changed_by: Actor::human(&changed_by.account_id),
                capabilities_hash: ContentHash::sha256(detail.as_bytes()),
                badge_version: 0,
            },
        })
        .await
        .map(|_| ())
}

#[derive(Serialize, sqlx::FromRow)]
pub struct TeamOut {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct ChannelOut {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub level: String,
    pub private: bool,
}

#[derive(Deserialize)]
pub struct NewChannel {
    pub id: String,
    pub team_id: String,
    pub name: String,
    #[serde(default = "default_level")]
    pub level: String,
    #[serde(default)]
    pub private: bool,
}
fn default_level() -> String {
    "open".into()
}

#[derive(Deserialize)]
pub struct NewTeam {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
pub struct NewAccount {
    pub id: String,
    pub display_name: String,
    pub password: String,
    #[serde(default = "default_kind")]
    pub kind: String,
}
fn default_kind() -> String {
    "human".into()
}

#[derive(Deserialize)]
pub struct NewBinding {
    pub account_id: String,
    pub role: String,
    /// org | group | channel
    pub scope_kind: String,
    pub scope_id: Option<String>,
}

/// 组织目录:把频道映射到它所属的组,供 `can()` 判断"组级授权是否覆盖该频道"。
/// 每次判定前现查——**不缓存**:缓存会让"刚把频道移出某组"这类变更晚生效,
/// 而权限判定晚生效就是安全漏洞。
pub async fn directory(db: &Db) -> Result<Directory> {
    let rows =
        sqlx::query_as::<_, (String, String)>("SELECT c.id, t.name FROM channel c JOIN team t ON t.id = c.team_id")
            .fetch_all(&db.pool)
            .await?;
    let mut dir = Directory::default();
    for (chan, team) in rows {
        dir = dir.with_channel(chan, team);
    }
    Ok(dir)
}

/// 统一的授权闸:判定在动库之前,拒绝时不留任何副作用。
pub async fn require(db: &Db, id: &Identity, action: &Action, target: &Scope) -> Result<()> {
    let p = id.principal(db).await?;
    let dir = directory(db).await?;
    let d = can(&p, action, target, &OrgProhibitions::default(), &dir);
    if d.allowed() {
        Ok(())
    } else {
        Err(ServerError::Forbidden(d.reason_zh()))
    }
}

pub async fn list_teams(State((db, _a)): State<OrgState>, _id: Identity) -> Result<Json<Vec<TeamOut>>> {
    let rows = sqlx::query_as::<_, TeamOut>("SELECT id, name FROM team ORDER BY name")
        .fetch_all(&db.pool)
        .await?;
    Ok(Json(rows))
}

pub async fn create_team(
    State((db, _a)): State<OrgState>,
    id: Identity,
    Json(t): Json<NewTeam>,
) -> Result<Json<TeamOut>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    sqlx::query("INSERT INTO team(id, name, created_ms) VALUES($1,$2,$3)")
        .bind(&t.id)
        .bind(&t.name)
        .bind(now_ms())
        .execute(&db.pool)
        .await?;
    Ok(Json(TeamOut { id: t.id, name: t.name }))
}

pub async fn list_channels(State((db, _a)): State<OrgState>, _id: Identity) -> Result<Json<Vec<ChannelOut>>> {
    let rows = sqlx::query_as::<_, ChannelOut>(
        "SELECT id, team_id, name, level, private FROM channel ORDER BY team_id, name",
    )
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

pub async fn create_channel(
    State((db, _a)): State<OrgState>,
    id: Identity,
    Json(c): Json<NewChannel>,
) -> Result<Json<ChannelOut>> {
    let team = sqlx::query_as::<_, (String,)>("SELECT name FROM team WHERE id = $1")
        .bind(&c.team_id)
        .fetch_optional(&db.pool)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("团队 {}", c.team_id)))?;
    require(&db, &id, &Action::ManageMembers, &Scope::Group(team.0)).await?;

    let mut tx = db.pool.begin().await?;
    sqlx::query(
        "INSERT INTO channel(id, team_id, name, level, private, created_ms) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(&c.id)
    .bind(&c.team_id)
    .bind(&c.name)
    .bind(&c.level)
    .bind(c.private)
    .bind(now_ms())
    .execute(&mut *tx)
    .await?;
    // 发号器与频道同事务建立,免得第一条消息撞上"没有游标行"
    sqlx::query("INSERT INTO channel_cursor(channel_id, next_seq) VALUES($1, 1)")
        .bind(&c.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(Json(ChannelOut {
        id: c.id,
        team_id: c.team_id,
        name: c.name,
        level: c.level,
        private: c.private,
    }))
}

pub async fn create_account(
    State((db, audit)): State<OrgState>,
    id: Identity,
    Json(a): Json<NewAccount>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let phc = hash_password(&a.password)?;
    sqlx::query(
        "INSERT INTO account(id, display_name, password_hash, kind, created_ms) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(&a.id)
    .bind(&a.display_name)
    .bind(&phc)
    .bind(&a.kind)
    .bind(now_ms())
    .execute(&db.pool)
    .await?;
    record_grant_change(&audit, &id, &a.id, &format!("create account kind={}", a.kind)).await?;
    Ok(Json(serde_json::json!({ "id": a.id, "kind": a.kind })))
}

pub async fn grant_role(
    State((db, audit)): State<OrgState>,
    id: Identity,
    Json(b): Json<NewBinding>,
) -> Result<Json<serde_json::Value>> {
    // 授权本身是组织级动作:能改别人角色的人必须有组织级管理权
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    // 库里不许出现服务端读不懂的角色——否则判定时会被静默丢弃,
    // 表面上"授过权"实际没生效,这类错最难查
    parse_role(&b.role).ok_or_else(|| ServerError::BadRequest(format!("未知角色 {}", b.role)))?;
    if b.scope_kind != "org" && b.scope_id.is_none() {
        return Err(ServerError::BadRequest("group/channel 作用域必须给 scope_id".into()));
    }
    sqlx::query(
        "INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms)
         VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
    )
    .bind(&b.account_id)
    .bind(&b.role)
    .bind(&b.scope_kind)
    .bind(&b.scope_id)
    .bind(now_ms())
    .execute(&db.pool)
    .await?;
    record_grant_change(
        &audit,
        &id,
        &b.account_id,
        &format!("grant {} @ {}:{}", b.role, b.scope_kind, b.scope_id.as_deref().unwrap_or("-")),
    )
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// **撤销角色**。此前只能授不能收——一个讲治理的系统里,这是个大洞:
/// 人离职、账号被盗,唯一的补救是登进数据库手动 DELETE。
pub async fn revoke_role(
    State((db, audit)): State<OrgState>,
    id: Identity,
    Json(b): Json<NewBinding>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;

    // 不许撤掉最后一个组织所有者——否则这台服务器就再没人能管了,
    // 且没有任何补救路径(bootstrap 只在空库可用)
    if parse_role(&b.role) == Some(muster_identity::Role::OrgOwner) && b.scope_kind == "org" {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM role_binding WHERE role IN ('owner','org_owner') AND scope_kind='org'",
        )
        .fetch_one(&db.pool)
        .await?;
        if n <= 1 {
            return Err(ServerError::BadRequest(
                "这是最后一个组织所有者,撤掉之后没人能再管理这台服务器。请先授权另一位所有者。".into(),
            ));
        }
    }

    let n = sqlx::query(
        "DELETE FROM role_binding WHERE account_id=$1 AND role=$2 AND scope_kind=$3
         AND scope_id IS NOT DISTINCT FROM $4",
    )
    .bind(&b.account_id)
    .bind(&b.role)
    .bind(&b.scope_kind)
    .bind(&b.scope_id)
    .execute(&db.pool)
    .await?
    .rows_affected();

    // 没删到也记一笔:"有人试图撤销一个不存在的绑定"同样是信息
    record_grant_change(
        &audit,
        &id,
        &b.account_id,
        &format!(
            "revoke {} @ {}:{} (affected={n})",
            b.role,
            b.scope_kind,
            b.scope_id.as_deref().unwrap_or("-")
        ),
    )
    .await?;
    Ok(Json(serde_json::json!({ "revoked": n })))
}

#[derive(Deserialize)]
pub struct SetDisabled {
    pub disabled: bool,
}

/// **停用/启用账号**。停用而不是删除:删账号会让历史里的 author_id 变成孤儿,
/// 而"这条消息是谁发的"是不能丢的。
pub async fn set_account_disabled(
    State((db, audit)): State<OrgState>,
    id: Identity,
    Path(aid): Path<String>,
    Json(d): Json<SetDisabled>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    if d.disabled && aid == id.account_id {
        return Err(ServerError::BadRequest("不能停用自己".into()));
    }
    let n = sqlx::query("UPDATE account SET disabled = $1 WHERE id = $2")
        .bind(d.disabled)
        .bind(&aid)
        .execute(&db.pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ServerError::NotFound(format!("账号 {aid}")));
    }
    record_grant_change(&audit, &id, &aid, if d.disabled { "disable" } else { "enable" }).await?;
    Ok(Json(serde_json::json!({ "id": aid, "disabled": d.disabled })))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct AccountOut {
    pub id: String,
    pub display_name: String,
    pub kind: String,
    pub disabled: bool,
    pub created_ms: i64,
}

/// 列出账号。**看不见就管不了**——此前连"现在有哪些账号"都问不出来。
pub async fn list_accounts(
    State((db, _a)): State<OrgState>,
    id: Identity,
) -> Result<Json<Vec<AccountOut>>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let rows = sqlx::query_as::<_, AccountOut>(
        "SELECT id, display_name, kind, disabled, created_ms FROM account ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct BindingOut {
    pub account_id: String,
    pub role: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
}

/// 列出角色绑定(可按账号过滤)。回答「现在谁有什么权限」——
/// 治理系统里这个问题必须能一眼看清,而不是靠翻库。
pub async fn list_bindings(
    State((db, _a)): State<OrgState>,
    id: Identity,
    Query(q): Query<BindingQuery>,
) -> Result<Json<Vec<BindingOut>>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let rows = sqlx::query_as::<_, BindingOut>(
        "SELECT account_id, role, scope_kind, scope_id FROM role_binding
         WHERE ($1::text IS NULL OR account_id = $1) ORDER BY account_id, role",
    )
    .bind(&q.account_id)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct BindingQuery {
    #[serde(default)]
    pub account_id: Option<String>,
}

/// 我是谁 + 我能做什么(前端据此禁用按钮,而不是点了才报错)。
pub async fn whoami(State((db, _a)): State<OrgState>, id: Identity) -> Result<Json<serde_json::Value>> {
    let p = id.principal(&db).await?;
    let dir = directory(&db).await?;
    let proh = OrgProhibitions::default();
    let cap = |a: Action| can(&p, &a, &Scope::Org, &proh, &dir).allowed();
    Ok(Json(serde_json::json!({
        "id": p.id,
        "display_name": p.display_name,
        "kind": kind_str(p.kind),
        "bindings": p.bindings.len(),
        "can": {
            "approve_merge": cap(Action::ApproveMerge),
            "manage_members": cap(Action::ManageMembers),
            "toggle_drill": cap(Action::ToggleDrill),
        }
    })))
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub id: String,
    pub password: String,
}

pub async fn login(
    State((db, _a)): State<OrgState>,
    axum::Extension(limiter): axum::Extension<std::sync::Arc<crate::ratelimit::LoginLimiter>>,
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: axum::http::HeaderMap,
    Json(r): Json<LoginReq>,
) -> Result<Json<serde_json::Value>> {
    // 限流在**验口令之前**:Argon2 很贵,让没通过限流的请求也去算一遍哈希,
    // 等于把防爆破的机制变成了拒绝服务的把手。
    //
    // 两个维度都查:按 IP 挡"一个来源喷一堆账号",按账号挡"一堆来源猜一个账号"。
    // 只做前者换 IP 就绕过,只做后者一个 IP 能慢慢遍历所有账号。
    // 两维额度不同:账号卡得紧(没人会 5 分钟输错 10 次自己的密码),
    // IP 必须宽得多——**一间办公室共用一个出口 IP**
    let ip = crate::ratelimit::client_ip(&headers, peer.map(|c| c.0));
    use crate::ratelimit::{MAX_PER_ACCOUNT, MAX_PER_IP};
    for (key, max) in [(format!("ip:{ip}"), MAX_PER_IP), (format!("acct:{}", r.id), MAX_PER_ACCOUNT)]
    {
        if let Err(wait) = limiter.check(&key, max) {
            return Err(ServerError::TooManyRequests(format!(
                "登录尝试过多,请 {} 秒后再试",
                wait.as_secs() + 1
            )));
        }
    }

    let row = sqlx::query_as::<_, (String, Option<String>, String)>(
        // 停用的账号连"账号或口令不对"都不必区分——统一走同一条拒绝路径
        "SELECT display_name, password_hash, kind FROM account WHERE id = $1 AND disabled = FALSE",
    )
    .bind(&r.id)
    .fetch_optional(&db.pool)
    .await?;

    // 账号不存在与口令错误返回**同一句话**:否则接口就成了账号枚举器
    let fail = || ServerError::Unauthenticated("账号或口令不对".into());
    let (name, phc, kind) = row.ok_or_else(fail)?;
    let phc = phc.ok_or_else(fail)?;
    if !crate::auth::verify_password(&r.password, &phc) {
        return Err(fail());
    }
    let token = crate::auth::issue_token(&r.id, &name, &kind)?;
    Ok(Json(serde_json::json!({ "token": token, "id": r.id, "display_name": name })))
}

pub async fn channel_level(db: &Db, channel_id: &str) -> Result<String> {
    sqlx::query_as::<_, (String,)>("SELECT level FROM channel WHERE id = $1")
        .bind(channel_id)
        .fetch_optional(&db.pool)
        .await?
        .map(|r| r.0)
        .ok_or_else(|| ServerError::NotFound(format!("频道 {channel_id}")))
}

pub async fn get_channel(
    State((db, _a)): State<OrgState>,
    _id: Identity,
    Path(cid): Path<String>,
) -> Result<Json<ChannelOut>> {
    sqlx::query_as::<_, ChannelOut>(
        "SELECT id, team_id, name, level, private FROM channel WHERE id = $1",
    )
    .bind(&cid)
    .fetch_optional(&db.pool)
    .await?
    .map(Json)
    .ok_or_else(|| ServerError::NotFound(format!("频道 {cid}")))
}
