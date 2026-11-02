//! 用户资料、登录流水、找回口令(2026-11-03;表结构见 `migrations/0006_profile.sql`,
//! 全库设计见 `docs/数据库设计.md`)。
//!
//! ## 账号与资料为什么分两张表
//!
//! `account` 是"能登录的主体",人、Agent 工牌、服务账号共用;`user_profile` 是"关于这个人的信息"
//! (邮箱、头像、部门、职位、简介、偏好的模型通道),只有人有。混在一张表里,工牌那一行就会
//! 挂着一堆永远为空的列,而"邮箱唯一"这类约束也得绕开它们写。
//!
//! ## 头像不进库
//!
//! 文件写在 `MUSTER_AVATARS_DIR`(容器里是 `/data/avatars`),库里只存文件名——与录像同一姿态:
//! 正文在文件系统,库里是元数据。上限 512 KiB,只认 PNG / JPEG / WebP,按魔数认不按扩展名。
//!
//! ## 登录流水
//!
//! 成功与失败都记(时间、IP、User-Agent、失败原因),**只记存在的账号**:不存在的账号不记,
//! 否则这张表就成了枚举器的日志。`account.last_login_ms / failed_logins` 是快照,列表页一眼看到。
//!
//! ## 找回口令
//!
//! 一次性令牌只存 SHA-256,30 分钟有效,用过即废。两条签发路径:本人凭邮箱申请
//! (`POST /auth/recover`,**永远回 200**,不泄露邮箱存不存在),或管理员签发
//! (`POST /accounts/:aid/reset-link`)后把链接转交本人。**邮件通道还没接**:自助申请现在只签发
//! 不投递,接口在响应里如实说 `delivery: "none"`,客户端据此提示找管理员。接 SMTP 时只改 `deliver()`。

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use muster_identity::{Action, PrincipalKind, Scope};

use crate::auth::{hash_password, Identity};
use crate::db::now_ms;
use crate::org::{record_grant_change, require, OrgState};
use crate::{Db, Result, ServerError};

pub const AVATAR_MAX_BYTES: usize = 512 * 1024;
pub const RESET_TTL_MS: i64 = 30 * 60 * 1000;

// ---------------------------------------------------------------- 校验(纯函数,可测)

/// 邮箱:修掉空白、转小写;空串 = 清空。只做形状检查,不发信验证(邮件通道未接)。
pub fn normalize_email(raw: &str) -> std::result::Result<Option<String>, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if s.chars().count() > 254 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("邮箱格式不对".into());
    }
    let (local, domain) = s.split_once('@').ok_or_else(|| "邮箱格式不对".to_string())?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') || !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err("邮箱格式不对".into());
    }
    Ok(Some(s.to_lowercase()))
}

/// 短文本字段:修空白、限长、不许控制字符(`multiline` 允许换行)。空串 = 清空。
pub fn clip(label: &str, raw: &str, max: usize, multiline: bool) -> std::result::Result<Option<String>, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if s.chars().count() > max {
        return Err(format!("{label}最多 {max} 个字符"));
    }
    if s.chars().any(|c| c.is_control() && !(multiline && c == '\n')) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(Some(s.to_string()))
}

/// 按魔数认图片格式,返回扩展名与 MIME。扩展名靠不住:客户端说什么都行。
pub fn sniff_image(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("png", "image/png"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else {
        None
    }
}

pub fn token_hash(token: &str) -> String {
    let d = Sha256::digest(token.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn avatars_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("MUSTER_AVATARS_DIR").unwrap_or_else(|_| "./deploy/avatars".into()))
}

/// 账号 id 进文件名前再挡一次路径分隔符:老账号(bootstrap 建的)没经过注册校验。
fn safe_file_stem(id: &str) -> Result<String> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(ServerError::BadRequest("账号 id 不能作文件名".into()));
    }
    Ok(id.to_string())
}

// ---------------------------------------------------------------- 资料

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ProfileOut {
    pub id: String,
    pub display_name: String,
    pub kind: String,
    pub disabled: bool,
    pub pending: bool,
    pub created_ms: i64,
    pub last_login_ms: Option<i64>,
    pub last_login_ip: Option<String>,
    pub failed_logins: i32,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub avatar_path: Option<String>,
    pub department: Option<String>,
    pub title: Option<String>,
    pub bio: Option<String>,
    pub preferred_provider: Option<String>,
    pub updated_ms: Option<i64>,
    /// 有头像时是 `/avatars/<id>`;sqlx 不填,查完再算
    #[sqlx(skip)]
    pub avatar_url: Option<String>,
}

pub async fn load_profile(db: &Db, id: &str) -> Result<ProfileOut> {
    let mut p = sqlx::query_as::<_, ProfileOut>(
        "SELECT a.id, a.display_name, a.kind, a.disabled, a.pending, a.created_ms,
                a.last_login_ms, a.last_login_ip, a.failed_logins,
                p.email, p.email_verified, p.avatar_path, p.department, p.title, p.bio, p.preferred_provider, p.updated_ms
         FROM account a LEFT JOIN user_profile p ON p.account_id = a.id WHERE a.id = $1",
    )
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or_else(|| ServerError::NotFound(format!("账号 {id}")))?;
    p.avatar_url = p.avatar_path.as_ref().map(|_| format!("/avatars/{}", p.id));
    Ok(p)
}

pub async fn me(State((db, _a)): State<OrgState>, id: Identity) -> Result<Json<ProfileOut>> {
    Ok(Json(load_profile(&db, &id.account_id).await?))
}

/// 改资料:字段给了就改(空串 = 清空),没给的不动。
#[derive(Debug, Deserialize, Default)]
pub struct ProfilePatch {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub department: Option<String>,
    pub title: Option<String>,
    pub bio: Option<String>,
    pub preferred_provider: Option<String>,
}

async fn ensure_profile_row(db: &Db, id: &str) -> Result<()> {
    sqlx::query("INSERT INTO user_profile(account_id, created_ms, updated_ms) VALUES($1,$2,$2) ON CONFLICT (account_id) DO NOTHING")
        .bind(id)
        .bind(now_ms())
        .execute(&db.pool)
        .await?;
    Ok(())
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}

pub async fn update_me(
    State((db, _a)): State<OrgState>,
    id: Identity,
    Json(p): Json<ProfilePatch>,
) -> Result<Json<ProfileOut>> {
    if id.kind != PrincipalKind::Human {
        return Err(ServerError::Forbidden("只有人的账号有资料".into()));
    }
    let bad = ServerError::BadRequest;
    if let Some(n) = &p.display_name {
        let n = clip("显示名", n, 40, false).map_err(bad)?.ok_or_else(|| bad("显示名不能为空".into()))?;
        sqlx::query("UPDATE account SET display_name = $1 WHERE id = $2").bind(&n).bind(&id.account_id).execute(&db.pool).await?;
    }
    ensure_profile_row(&db, &id.account_id).await?;
    let cur = load_profile(&db, &id.account_id).await?;
    let email = match &p.email {
        Some(e) => normalize_email(e).map_err(bad)?,
        None => cur.email.clone(),
    };
    let department = match &p.department {
        Some(v) => clip("部门", v, 60, false).map_err(bad)?,
        None => cur.department.clone(),
    };
    let title = match &p.title {
        Some(v) => clip("职位", v, 60, false).map_err(bad)?,
        None => cur.title.clone(),
    };
    let bio = match &p.bio {
        Some(v) => clip("简介", v, 500, true).map_err(bad)?,
        None => cur.bio.clone(),
    };
    let preferred = match &p.preferred_provider {
        Some(v) if v.trim().is_empty() => None,
        Some(v) => {
            let v = v.trim().to_string();
            let ok: Option<(bool,)> = sqlx::query_as("SELECT enabled FROM provider WHERE id = $1").bind(&v).fetch_optional(&db.pool).await?;
            match ok {
                Some((true,)) => Some(v),
                _ => return Err(bad(format!("模型通道 {v} 不在目录里或已停用"))),
            }
        }
        None => cur.preferred_provider.clone(),
    };
    // 邮箱变了就当未验证——验证的是旧地址
    let verified = if email != cur.email { false } else { cur.email_verified.unwrap_or(false) };
    let r = sqlx::query(
        "UPDATE user_profile SET email = $1, email_verified = $2, department = $3, title = $4, bio = $5,
                preferred_provider = $6, updated_ms = $7 WHERE account_id = $8",
    )
    .bind(&email)
    .bind(verified)
    .bind(&department)
    .bind(&title)
    .bind(&bio)
    .bind(&preferred)
    .bind(now_ms())
    .bind(&id.account_id)
    .execute(&db.pool)
    .await;
    match r {
        Ok(_) => {}
        Err(e) if is_unique_violation(&e) => return Err(ServerError::Conflict("这个邮箱已被别的账号使用".into())),
        Err(e) => return Err(e.into()),
    }
    Ok(Json(load_profile(&db, &id.account_id).await?))
}

/// 注册时顺手建资料行(signup.rs 调用)。邮箱冲突由调用方在建号前查过,这里只写。
pub async fn create_profile(db: &Db, id: &str, email: Option<&str>, department: Option<&str>, title: Option<&str>) -> Result<()> {
    sqlx::query(
        "INSERT INTO user_profile(account_id, email, department, title, created_ms, updated_ms)
         VALUES($1,$2,$3,$4,$5,$5) ON CONFLICT (account_id) DO NOTHING",
    )
    .bind(id)
    .bind(email)
    .bind(department)
    .bind(title)
    .bind(now_ms())
    .execute(&db.pool)
    .await?;
    Ok(())
}

pub async fn email_taken(db: &Db, email: &str) -> Result<bool> {
    let r: Option<(String,)> = sqlx::query_as("SELECT account_id FROM user_profile WHERE lower(email) = lower($1)")
        .bind(email)
        .fetch_optional(&db.pool)
        .await?;
    Ok(r.is_some())
}

// ---------------------------------------------------------------- 头像

pub async fn upload_avatar(
    State((db, _a)): State<OrgState>,
    id: Identity,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    if id.kind != PrincipalKind::Human {
        return Err(ServerError::Forbidden("只有人的账号有头像".into()));
    }
    if body.len() > AVATAR_MAX_BYTES {
        return Err(ServerError::BadRequest(format!("头像最大 {} KiB", AVATAR_MAX_BYTES / 1024)));
    }
    let (ext, _mime) = sniff_image(&body).ok_or_else(|| ServerError::BadRequest("只认 PNG / JPEG / WebP 图片".into()))?;
    let _ = headers; // content-type 不作数,按魔数认
    let stem = safe_file_stem(&id.account_id)?;
    let dir = avatars_dir();
    tokio::fs::create_dir_all(&dir).await.map_err(|e| ServerError::Internal(format!("头像目录建不了:{e}")))?;
    // 换格式时把旧文件清掉,免得一个账号留下三个扩展名的文件
    for old in ["png", "jpg", "webp"] {
        if old != ext {
            let _ = tokio::fs::remove_file(dir.join(format!("{stem}.{old}"))).await;
        }
    }
    let name = format!("{stem}.{ext}");
    tokio::fs::write(dir.join(&name), &body).await.map_err(|e| ServerError::Internal(format!("头像写不进去:{e}")))?;
    ensure_profile_row(&db, &id.account_id).await?;
    sqlx::query("UPDATE user_profile SET avatar_path = $1, updated_ms = $2 WHERE account_id = $3")
        .bind(&name)
        .bind(now_ms())
        .bind(&id.account_id)
        .execute(&db.pool)
        .await?;
    Ok(Json(serde_json::json!({ "avatar_url": format!("/avatars/{}", id.account_id), "bytes": body.len() })))
}

pub async fn get_avatar(State((db, _a)): State<OrgState>, _id: Identity, Path(aid): Path<String>) -> Result<axum::response::Response> {
    let row: Option<(Option<String>,)> = sqlx::query_as("SELECT avatar_path FROM user_profile WHERE account_id = $1")
        .bind(&aid)
        .fetch_optional(&db.pool)
        .await?;
    let name = row.and_then(|r| r.0).ok_or_else(|| ServerError::NotFound(format!("{aid} 没有头像")))?;
    safe_file_stem(&name)?;
    let bytes = tokio::fs::read(avatars_dir().join(&name)).await.map_err(|_| ServerError::NotFound(format!("{aid} 的头像文件不在了")))?;
    let mime = sniff_image(&bytes).map(|(_, m)| m).unwrap_or("application/octet-stream");
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "private, max-age=300")],
        bytes,
    )
        .into_response())
}

// ---------------------------------------------------------------- 登录流水

/// 记一次登录尝试;成功时顺手更新快照。**只对存在的账号调用。**
pub async fn record_login(db: &Db, account: &str, ok: bool, reason: Option<&str>, ip: &str, user_agent: Option<&str>) -> Result<()> {
    let ts = now_ms();
    sqlx::query("INSERT INTO login_event(account_id, ok, reason, ip, user_agent, ts_ms) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(account)
        .bind(ok)
        .bind(reason)
        .bind(ip)
        .bind(user_agent.map(|u| u.chars().take(200).collect::<String>()))
        .bind(ts)
        .execute(&db.pool)
        .await?;
    if ok {
        sqlx::query("UPDATE account SET last_login_ms = $1, last_login_ip = $2, failed_logins = 0 WHERE id = $3")
            .bind(ts)
            .bind(ip)
            .bind(account)
            .execute(&db.pool)
            .await?;
    } else {
        sqlx::query("UPDATE account SET failed_logins = failed_logins + 1 WHERE id = $1").bind(account).execute(&db.pool).await?;
    }
    Ok(())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct LoginEventOut {
    pub ok: bool,
    pub reason: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub ts_ms: i64,
}

#[derive(Deserialize)]
pub struct LoginsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

async fn recent_logins(db: &Db, aid: &str, limit: i64) -> Result<Vec<LoginEventOut>> {
    Ok(sqlx::query_as::<_, LoginEventOut>(
        "SELECT ok, reason, ip, user_agent, ts_ms FROM login_event WHERE account_id = $1 ORDER BY ts_ms DESC LIMIT $2",
    )
    .bind(aid)
    .bind(limit.clamp(1, 500))
    .fetch_all(&db.pool)
    .await?)
}

/// 管理员看某个账号:资料 + 最近 20 次登录。
pub async fn admin_profile(State((db, _a)): State<OrgState>, id: Identity, Path(aid): Path<String>) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let p = load_profile(&db, &aid).await?;
    let logins = recent_logins(&db, &aid, 20).await?;
    Ok(Json(serde_json::json!({ "profile": p, "logins": logins })))
}

pub async fn admin_logins(
    State((db, _a)): State<OrgState>,
    id: Identity,
    Path(aid): Path<String>,
    Query(q): Query<LoginsQuery>,
) -> Result<Json<Vec<LoginEventOut>>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    Ok(Json(recent_logins(&db, &aid, q.limit.unwrap_or(50)).await?))
}

/// 本人看自己的登录记录。
pub async fn my_logins(State((db, _a)): State<OrgState>, id: Identity, Query(q): Query<LoginsQuery>) -> Result<Json<Vec<LoginEventOut>>> {
    Ok(Json(recent_logins(&db, &id.account_id, q.limit.unwrap_or(20)).await?))
}

// ---------------------------------------------------------------- 找回口令

async fn issue_reset(db: &Db, account: &str, issued_by: Option<&str>) -> Result<(String, i64)> {
    let token = new_token();
    let expires = now_ms() + RESET_TTL_MS;
    sqlx::query(
        "INSERT INTO password_reset(id, account_id, token_hash, issued_by, expires_ms, created_ms) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(account)
    .bind(token_hash(&token))
    .bind(issued_by)
    .bind(expires)
    .bind(now_ms())
    .execute(&db.pool)
    .await?;
    Ok((token, expires))
}

/// 管理员签发重置链接:没有邮件通道时的正路。令牌只回给签发者这一次。
pub async fn admin_reset_link(State((db, audit)): State<OrgState>, id: Identity, Path(aid): Path<String>) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let row: Option<(String, bool)> = sqlx::query_as("SELECT kind, disabled FROM account WHERE id = $1").bind(&aid).fetch_optional(&db.pool).await?;
    match row {
        None => return Err(ServerError::NotFound(format!("账号 {aid}"))),
        Some((k, _)) if k != "human" => return Err(ServerError::BadRequest("只能给人的账号签发重置链接".into())),
        Some((_, true)) => return Err(ServerError::BadRequest("账号已停用,先启用再重置".into())),
        _ => {}
    }
    let (token, expires_ms) = issue_reset(&db, &aid, Some(&id.account_id)).await?;
    record_grant_change(&audit, &id, &aid, "password reset link issued").await?;
    Ok(Json(serde_json::json!({ "id": aid, "token": token, "expires_ms": expires_ms, "ttl_secs": RESET_TTL_MS / 1000 })))
}

#[derive(Deserialize)]
pub struct RecoverReq {
    pub email: String,
}

/// 邮件通道。还没接:签发了令牌也投不出去,如实返回 "none"。接 SMTP 时只改这里。
fn deliver(_account: &str, _email: &str, _token: &str) -> &'static str {
    "none"
}

/// 本人凭邮箱申请。**永远回 200,响应体与邮箱存不存在无关**。
pub async fn recover(
    State((db, _a)): State<OrgState>,
    axum::Extension(limiter): axum::Extension<std::sync::Arc<crate::ratelimit::LoginLimiter>>,
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(r): Json<RecoverReq>,
) -> Result<Json<serde_json::Value>> {
    let ip = crate::ratelimit::client_ip(&headers, peer.map(|c| c.0));
    if let Err(wait) = limiter.check(&format!("rec:{ip}"), crate::ratelimit::MAX_RECOVER_PER_IP) {
        return Err(ServerError::TooManyRequests(format!("申请过于频繁,请 {} 秒后再试", wait.as_secs() + 1)));
    }
    let mut delivery = "none";
    if let Ok(Some(email)) = normalize_email(&r.email) {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT a.id FROM account a JOIN user_profile p ON p.account_id = a.id
             WHERE lower(p.email) = $1 AND a.kind = 'human' AND a.disabled = FALSE",
        )
        .bind(&email)
        .fetch_optional(&db.pool)
        .await?;
        if let Some((aid,)) = row {
            let (token, _) = issue_reset(&db, &aid, None).await?;
            delivery = deliver(&aid, &email, &token);
            tracing::info!(account = %aid, delivery, "找回口令:已签发重置令牌");
        }
    }
    Ok(Json(serde_json::json!({ "ok": true, "delivery": delivery })))
}

#[derive(Deserialize)]
pub struct ResetReq {
    pub token: String,
    pub new_password: String,
}

pub async fn reset(
    State((db, audit)): State<OrgState>,
    axum::Extension(limiter): axum::Extension<std::sync::Arc<crate::ratelimit::LoginLimiter>>,
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(r): Json<ResetReq>,
) -> Result<Json<serde_json::Value>> {
    let ip = crate::ratelimit::client_ip(&headers, peer.map(|c| c.0));
    if let Err(wait) = limiter.check(&format!("rst:{ip}"), crate::ratelimit::MAX_RESET_PER_IP) {
        return Err(ServerError::TooManyRequests(format!("尝试过于频繁,请 {} 秒后再试", wait.as_secs() + 1)));
    }
    crate::signup::validate_password(&r.new_password).map_err(ServerError::BadRequest)?;
    let bad = || ServerError::Unauthenticated("重置链接无效或已过期".into());
    let row: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT id, account_id FROM password_reset WHERE token_hash = $1 AND used_ms IS NULL AND expires_ms > $2",
    )
    .bind(token_hash(r.token.trim()))
    .bind(now_ms())
    .fetch_optional(&db.pool)
    .await?;
    let (rid, aid) = row.ok_or_else(bad)?;
    let phc = hash_password(&r.new_password)?;
    sqlx::query("UPDATE account SET password_hash = $1, failed_logins = 0 WHERE id = $2").bind(&phc).bind(&aid).execute(&db.pool).await?;
    sqlx::query("UPDATE password_reset SET used_ms = $1 WHERE id = $2").bind(now_ms()).bind(rid).execute(&db.pool).await?;
    let me = Identity { account_id: aid.clone(), display_name: aid.clone(), kind: PrincipalKind::Human };
    record_grant_change(&audit, &me, &aid, "password reset via link").await?;
    Ok(Json(serde_json::json!({ "ok": true, "id": aid })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_normalized_and_shape_checked() {
        assert_eq!(normalize_email("  Alice@Example.COM ").unwrap(), Some("alice@example.com".into()));
        assert_eq!(normalize_email("   ").unwrap(), None, "空 = 清空");
        for bad in ["alice", "@x.com", "a@", "a@b", "a b@x.com", "a@@x.com", "a@.com", "a@x."] {
            assert!(normalize_email(bad).is_err(), "{bad} 应被拒");
        }
    }

    #[test]
    fn clip_limits_length_and_control_chars() {
        assert_eq!(clip("部门", "  平台组 ", 60, false).unwrap(), Some("平台组".into()));
        assert!(clip("部门", &"x".repeat(61), 60, false).is_err());
        assert!(clip("部门", "a\tb", 60, false).is_err(), "制表符是控制字符");
        assert!(clip("简介", "第一行\n第二行", 500, true).is_ok(), "简介允许换行");
        assert!(clip("部门", "a\nb", 60, false).is_err(), "单行字段不许换行");
    }

    #[test]
    fn images_are_recognized_by_magic_not_extension() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\n....").map(|x| x.0), Some("png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]).map(|x| x.0), Some("jpg"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 ").map(|x| x.0), Some("webp"));
        assert_eq!(sniff_image(b"<svg xmlns=...>"), None, "SVG 能带脚本,不收");
        assert_eq!(sniff_image(b"GIF89a"), None);
    }

    #[test]
    fn reset_tokens_are_long_random_and_stored_hashed() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_eq!(token_hash(&a).len(), 64, "sha256 hex");
        assert_ne!(token_hash(&a), a);
    }

    #[test]
    fn file_stem_rejects_path_tricks() {
        assert!(safe_file_stem("../x").is_err());
        assert!(safe_file_stem("a/b").is_err());
        assert!(safe_file_stem("").is_err());
        assert!(safe_file_stem("A-007").is_ok());
    }
}
