//! 组织 / 群组 / 频道 / 角色绑定(P2-05..08)。
//!
//! 每个写操作都先过 `muster_identity::can()`,**在动数据库之前**。
//! 这与桌面壳里 `decide_as` 的姿态一致:越权者被挡在副作用之前,系统状态零变化。

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use muster_identity::{can, Action, Directory, OrgProhibitions, Scope};

use crate::auth::{hash_password, kind_str, parse_role, Identity};
use crate::db::now_ms;
use crate::{Db, Result, ServerError};

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

pub async fn list_teams(State(db): State<Db>, _id: Identity) -> Result<Json<Vec<TeamOut>>> {
    let rows = sqlx::query_as::<_, TeamOut>("SELECT id, name FROM team ORDER BY name")
        .fetch_all(&db.pool)
        .await?;
    Ok(Json(rows))
}

pub async fn create_team(
    State(db): State<Db>,
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

pub async fn list_channels(State(db): State<Db>, _id: Identity) -> Result<Json<Vec<ChannelOut>>> {
    let rows = sqlx::query_as::<_, ChannelOut>(
        "SELECT id, team_id, name, level, private FROM channel ORDER BY team_id, name",
    )
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

pub async fn create_channel(
    State(db): State<Db>,
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
    State(db): State<Db>,
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
    Ok(Json(serde_json::json!({ "id": a.id, "kind": a.kind })))
}

pub async fn grant_role(
    State(db): State<Db>,
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
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 我是谁 + 我能做什么(前端据此禁用按钮,而不是点了才报错)。
pub async fn whoami(State(db): State<Db>, id: Identity) -> Result<Json<serde_json::Value>> {
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

pub async fn login(State(db): State<Db>, Json(r): Json<LoginReq>) -> Result<Json<serde_json::Value>> {
    let row = sqlx::query_as::<_, (String, Option<String>, String)>(
        "SELECT display_name, password_hash, kind FROM account WHERE id = $1",
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
    State(db): State<Db>,
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
