//! 自助注册与账号自助(2026-11-03)。
//!
//! ## 三种注册模式
//!
//! 存在 `org_setting.registration`,管理员改(`POST /org/registration`,要 ChangePolicy):
//!
//! - `closed`:不开放。`POST /auth/register` 一律 403。
//! - `approval`(**默认**):注册即建号,但 `pending = true`,**不能登录**;管理员批准
//!   (`POST /accounts/:aid/approve`)后得到 member@org,才算进了组织。
//! - `open`:注册即激活并授 member@org,直接返回令牌。
//!
//! ## 为什么默认是「待批准」
//!
//! muster.top 对着公网。`open` 等于任何人一注册就是组织成员,能看频道列表、进公开频道——
//! 对一个团队协作系统这不是默认该有的状态。待批准的账号在批准前**什么都做不了**:
//! 登录被拒,自然拿不到令牌,也就打不到任何需要身份的接口。
//!
//! ## 注册语义里 id 冲突必须告诉人
//!
//! 登录接口「账号不存在」与「口令错」回同一句话(见 org.rs),那是为了不当账号枚举器。
//! 注册不同:id 被占了必须说,否则人没法换一个。代价是能探测某个 id 存在与否——所有自助注册
//! 系统都一样,靠**按 IP 限流**(5 次 / 5 分钟)压低它的价值。
//!
//! 「待批准」只在**口令正确**时才告诉登录者:知道口令的就是注册者本人,别人看不出这个状态。
//!
//! ## 审计
//!
//! 注册、批准、改模式、改口令都落 `badge.update`(与建号 / 授权同一个事件,语义是
//! "某人的能力集变了"):注册与改口令的 changed_by 是本人,批准与改模式的是管理员。
//! 正文只留哈希(铁律三)。

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use muster_identity::{Action, PrincipalKind, Scope};

use crate::auth::{hash_password, verify_password, Identity};
use crate::db::now_ms;
use crate::org::{record_grant_change, require, OrgState};
use crate::{Db, Result, ServerError};

pub const REGISTRATION_KEY: &str = "registration";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationMode {
    Closed,
    Approval,
    Open,
}

impl RegistrationMode {
    pub const DEFAULT: Self = Self::Approval;

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "closed" => Self::Closed,
            "approval" => Self::Approval,
            "open" => Self::Open,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Approval => "approval",
            Self::Open => "open",
        }
    }
}

/// 当前注册模式。库里没有就是缺省;库里是个读不懂的值就当 **closed**——
/// 配置错了宁可不开放,也不能因为一个错字把注册开成 open。
pub async fn registration_mode(db: &Db) -> Result<RegistrationMode> {
    let row = sqlx::query_as::<_, (String,)>("SELECT value FROM org_setting WHERE key = $1")
        .bind(REGISTRATION_KEY)
        .fetch_optional(&db.pool)
        .await?;
    Ok(match row {
        None => RegistrationMode::DEFAULT,
        Some((v,)) => RegistrationMode::parse(&v).unwrap_or(RegistrationMode::Closed),
    })
}

/// 注册输入的校验。纯函数,可测。规则故意保守:账号 id 会进 URL、进审计链的 actor、
/// 进 LiveKit 的 identity,不收空白与大小写变体,免得同一个人出现两种写法。
pub fn validate_signup(id: &str, password: &str, display_name: &str) -> std::result::Result<(), String> {
    validate_id(id)?;
    validate_password(password)?;
    let name = display_name.trim();
    if name.is_empty() || name.chars().count() > 40 {
        return Err("显示名 1 到 40 个字符".into());
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("显示名不能包含控制字符".into());
    }
    Ok(())
}

fn validate_id(id: &str) -> std::result::Result<(), String> {
    let n = id.chars().count();
    if !(3..=32).contains(&n) {
        return Err("账号 id 3 到 32 个字符".into());
    }
    let ok = id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && id.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false);
    if !ok {
        return Err("账号 id 只能用小写字母、数字、. _ -,且以字母或数字开头".into());
    }
    // 保留给系统与工牌:系统说明行的作者、A-007 这类工牌号的形状
    if id == "system" || id == "系统" {
        return Err("这个账号 id 是保留的".into());
    }
    Ok(())
}

pub(crate) fn validate_password(pw: &str) -> std::result::Result<(), String> {
    let n = pw.chars().count();
    if n < 8 {
        return Err("口令至少 8 个字符".into());
    }
    if n > 128 {
        return Err("口令最多 128 个字符".into());
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct RegisterReq {
    pub id: String,
    pub password: String,
    pub display_name: String,
    /// 资料(2026-11-03,profile.rs):都可选
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /auth/register`(免鉴权)。按 IP 限流在建号之前——Argon2 很贵。
pub async fn register(
    State((db, audit)): State<OrgState>,
    axum::Extension(limiter): axum::Extension<std::sync::Arc<crate::ratelimit::LoginLimiter>>,
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: axum::http::HeaderMap,
    Json(r): Json<RegisterReq>,
) -> Result<Json<serde_json::Value>> {
    let ip = crate::ratelimit::client_ip(&headers, peer.map(|c| c.0));
    if let Err(wait) = limiter.check(&format!("reg:{ip}"), crate::ratelimit::MAX_REGISTER_PER_IP) {
        return Err(ServerError::TooManyRequests(format!("注册过于频繁,请 {} 秒后再试", wait.as_secs() + 1)));
    }
    let mode = registration_mode(&db).await?;
    if mode == RegistrationMode::Closed {
        return Err(ServerError::Forbidden("本组织不开放自助注册,请联系管理员开号".into()));
    }
    validate_signup(&r.id, &r.password, &r.display_name).map_err(ServerError::BadRequest)?;
    let name = r.display_name.trim().to_string();
    let email = match &r.email {
        Some(e) => crate::profile::normalize_email(e).map_err(ServerError::BadRequest)?,
        None => None,
    };
    let department = match &r.department {
        Some(v) => crate::profile::clip("部门", v, 60, false).map_err(ServerError::BadRequest)?,
        None => None,
    };
    let title = match &r.title {
        Some(v) => crate::profile::clip("职位", v, 60, false).map_err(ServerError::BadRequest)?,
        None => None,
    };
    // 邮箱冲突在建号**之前**查:建了号再发现冲突,就得回滚一个已经落库的账号
    if let Some(e) = &email {
        if crate::profile::email_taken(&db, e).await? {
            return Err(ServerError::Conflict("这个邮箱已被别的账号使用".into()));
        }
    }
    let pending = mode == RegistrationMode::Approval;
    let phc = hash_password(&r.password)?;

    let inserted = sqlx::query(
        "INSERT INTO account(id, display_name, password_hash, kind, created_ms, pending)
         VALUES($1, $2, $3, 'human', $4, $5) ON CONFLICT (id) DO NOTHING",
    )
    .bind(&r.id)
    .bind(&name)
    .bind(&phc)
    .bind(now_ms())
    .bind(pending)
    .execute(&db.pool)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(ServerError::Conflict(format!("账号 id {} 已被占用,换一个", r.id)));
    }
    crate::profile::create_profile(&db, &r.id, email.as_deref(), department.as_deref(), title.as_deref()).await?;
    let me = Identity { account_id: r.id.clone(), display_name: name.clone(), kind: PrincipalKind::Human };
    if !pending {
        grant_member(&db, &r.id).await?;
    }
    record_grant_change(&audit, &me, &r.id, &format!("self-register mode={}", mode.as_str())).await?;

    if pending {
        return Ok(Json(serde_json::json!({ "id": r.id, "display_name": name, "email": email, "pending": true })));
    }
    let token = crate::auth::issue_token(&r.id, &name, "human")?;
    Ok(Json(serde_json::json!({ "id": r.id, "display_name": name, "email": email, "pending": false, "token": token })))
}

async fn grant_member(db: &Db, account: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms)
         VALUES($1, 'member', 'org', NULL, $2) ON CONFLICT DO NOTHING",
    )
    .bind(account)
    .bind(now_ms())
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// `GET /auth/registration`(免鉴权):登录页据此决定要不要显示「注册」入口。
/// 只泄露"开不开放"这一个比特,它本来就得让想注册的人知道。
pub async fn registration_public(State((db, _a)): State<OrgState>) -> Result<Json<serde_json::Value>> {
    let mode = registration_mode(&db).await?;
    Ok(Json(serde_json::json!({ "mode": mode.as_str() })))
}

pub async fn registration_get(State((db, _a)): State<OrgState>, id: Identity) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let mode = registration_mode(&db).await?;
    let (pending,): (i64,) = sqlx::query_as("SELECT count(*) FROM account WHERE pending = TRUE")
        .fetch_one(&db.pool)
        .await?;
    Ok(Json(serde_json::json!({ "mode": mode.as_str(), "pending": pending })))
}

#[derive(Deserialize)]
pub struct SetMode {
    pub mode: String,
}

/// 改注册模式:这是组织策略(谁能进来),要 ChangePolicy,不是 ManageMembers。
pub async fn registration_set(
    State((db, audit)): State<OrgState>,
    id: Identity,
    Json(m): Json<SetMode>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ChangePolicy, &Scope::Org).await?;
    let mode = RegistrationMode::parse(&m.mode)
        .ok_or_else(|| ServerError::BadRequest("mode 只能是 closed | approval | open".into()))?;
    sqlx::query(
        "INSERT INTO org_setting(key, value, updated_by, updated_ms) VALUES($1,$2,$3,$4)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by, updated_ms = EXCLUDED.updated_ms",
    )
    .bind(REGISTRATION_KEY)
    .bind(mode.as_str())
    .bind(&id.account_id)
    .bind(now_ms())
    .execute(&db.pool)
    .await?;
    record_grant_change(&audit, &id, &id.account_id, &format!("registration mode={}", mode.as_str())).await?;
    Ok(Json(serde_json::json!({ "mode": mode.as_str() })))
}

/// `POST /accounts/:aid/approve`:待批准 → 正式成员(member@org)。管理员的活。
pub async fn approve(
    State((db, audit)): State<OrgState>,
    id: Identity,
    Path(aid): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let n = sqlx::query("UPDATE account SET pending = FALSE WHERE id = $1 AND pending = TRUE AND kind = 'human'")
        .bind(&aid)
        .execute(&db.pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ServerError::NotFound(format!("没有待批准的账号 {aid}")));
    }
    grant_member(&db, &aid).await?;
    record_grant_change(&audit, &id, &aid, "approve registration; grant member@org").await?;
    Ok(Json(serde_json::json!({ "id": aid, "pending": false })))
}

#[derive(Deserialize)]
pub struct ChangePassword {
    pub old_password: String,
    pub new_password: String,
}

/// `POST /me/password`:本人改口令,要验原口令——拿到一张还没过期的令牌
/// 不等于知道口令,不能凭令牌直接换锁。
pub async fn change_password(
    State((db, audit)): State<OrgState>,
    id: Identity,
    Json(c): Json<ChangePassword>,
) -> Result<Json<serde_json::Value>> {
    if id.kind != PrincipalKind::Human {
        return Err(ServerError::Forbidden("只有人的账号能自助改口令".into()));
    }
    let row = sqlx::query_as::<_, (Option<String>,)>("SELECT password_hash FROM account WHERE id = $1 AND disabled = FALSE")
        .bind(&id.account_id)
        .fetch_optional(&db.pool)
        .await?;
    let phc = row.and_then(|r| r.0).ok_or_else(|| ServerError::Unauthenticated("原口令不对".into()))?;
    if !verify_password(&c.old_password, &phc) {
        return Err(ServerError::Unauthenticated("原口令不对".into()));
    }
    validate_password(&c.new_password).map_err(ServerError::BadRequest)?;
    let new_phc = hash_password(&c.new_password)?;
    sqlx::query("UPDATE account SET password_hash = $1 WHERE id = $2")
        .bind(&new_phc)
        .bind(&id.account_id)
        .execute(&db.pool)
        .await?;
    record_grant_change(&audit, &id, &id.account_id, "password change").await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parses_only_the_three_words() {
        assert_eq!(RegistrationMode::parse("open"), Some(RegistrationMode::Open));
        assert_eq!(RegistrationMode::parse(" approval "), Some(RegistrationMode::Approval));
        assert_eq!(RegistrationMode::parse("closed"), Some(RegistrationMode::Closed));
        assert_eq!(RegistrationMode::parse("Open"), None, "不做大小写宽容:配置里的错字要暴露,不要猜");
        assert_eq!(RegistrationMode::DEFAULT, RegistrationMode::Approval, "公网服务器的缺省必须是待批准");
    }

    /// 账号 id 会进 URL、审计 actor、LiveKit identity:只收一种写法。
    #[test]
    fn id_rules() {
        assert!(validate_signup("alice", "password1", "Alice").is_ok());
        assert!(validate_signup("a.b_c-9", "password1", "x").is_ok());
        assert!(validate_signup("al", "password1", "Alice").is_err(), "太短");
        assert!(validate_signup("Alice", "password1", "Alice").is_err(), "大写");
        assert!(validate_signup("al ice", "password1", "Alice").is_err(), "空白");
        assert!(validate_signup("-alice", "password1", "Alice").is_err(), "开头符号");
        assert!(validate_signup("小七", "password1", "Alice").is_err(), "非 ASCII");
        assert!(validate_signup("system", "password1", "Alice").is_err(), "保留");
        assert!(validate_signup(&"a".repeat(33), "password1", "Alice").is_err(), "太长");
    }

    #[test]
    fn password_and_name_rules() {
        assert!(validate_signup("alice", "short", "Alice").is_err(), "口令太短");
        assert!(validate_signup("alice", &"p".repeat(129), "Alice").is_err(), "口令太长");
        assert!(validate_signup("alice", "password1", "   ").is_err(), "显示名空");
        assert!(validate_signup("alice", "password1", &"名".repeat(41)).is_err(), "显示名太长");
        assert!(validate_signup("alice", "password1", "A\u{7}lice").is_err(), "控制字符");
        assert!(validate_signup("alice", "password1", "  张伟  ").is_ok(), "前后空白会被修掉");
    }
}
