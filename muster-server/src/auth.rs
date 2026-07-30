//! 身份:内置账号 + JWT。**OIDC 的插拔点就是这个模块**。
//!
//! 接企业 IdP(P2-03/04)时要换的只有 [`login`] 与 [`Identity::from_token`]——
//! 前者改成校验 IdP 的断言并按 `iss+sub` 找/建账号,后者改成验 IdP 的签名。
//! [`Identity`] 本身、以及它怎么变成 [`muster_identity::Principal`],都不变。
//!
//! ## 为什么 Principal 在这里组装而不是各处现搭
//!
//! 判定归 `muster_identity::can()`(纯函数内核,12,150 组穷举验证过)。服务端
//! 的活只有一件:**把一个 HTTP 请求如实翻译成 Principal**。翻译只有一处,
//! 判定就只有一处;两边各写一份的话,"桌面端说能、服务端说不能"这种问题
//! 永远查不清。

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use muster_identity::{Principal, PrincipalKind, Role, RoleBinding, Scope};

use crate::{db::now_ms, Db, ServerError};

const TOKEN_TTL_SECS: i64 = 12 * 3600;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// 账号 id
    pub sub: String,
    pub name: String,
    pub kind: String,
    pub exp: i64,
}

/// 签名密钥。**从环境读,缺失即拒绝启动**——默认密钥等于没有认证,
/// 而"开发时先用默认值"正是它最后出现在生产里的原因。
pub fn secret() -> Result<Vec<u8>, String> {
    validate_secret(std::env::var("MUSTER_JWT_SECRET").ok())
}

/// 校验逻辑单独成纯函数:可直接测,不必在测试里改进程环境——
/// 并行测试共用同一个进程,谁改环境谁就在踩别人。
pub fn validate_secret(v: Option<String>) -> Result<Vec<u8>, String> {
    let s = v.ok_or("MUSTER_JWT_SECRET 未设置——不提供默认密钥")?;
    if s.len() < 32 {
        return Err("MUSTER_JWT_SECRET 至少 32 字符".into());
    }
    Ok(s.into_bytes())
}

pub fn issue_token(account_id: &str, name: &str, kind: &str) -> Result<String, ServerError> {
    let key = secret().map_err(ServerError::Internal)?;
    let claims = Claims {
        sub: account_id.to_string(),
        name: name.to_string(),
        kind: kind.to_string(),
        exp: now_ms() / 1000 + TOKEN_TTL_SECS,
    };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(&key))
        .map_err(|e| ServerError::Internal(format!("签发失败:{e}")))
}

/// 已认证的调用方。Axum 提取器:任何需要身份的 handler 直接把它写进参数表,
/// 忘了写就拿不到身份——**让"忘记鉴权"在类型上尽量难发生**。
#[derive(Debug, Clone)]
pub struct Identity {
    pub account_id: String,
    pub display_name: String,
    pub kind: PrincipalKind,
}

impl Identity {
    pub fn from_token(token: &str) -> Result<Self, ServerError> {
        let key = secret().map_err(ServerError::Internal)?;
        let data = decode::<Claims>(token, &DecodingKey::from_secret(&key), &Validation::default())
            .map_err(|e| ServerError::Unauthenticated(format!("令牌无效:{e}")))?;
        Ok(Self {
            account_id: data.claims.sub,
            display_name: data.claims.name,
            kind: parse_kind(&data.claims.kind),
        })
    }

    /// 装配成权限内核认识的 Principal。角色绑定实时从库里读——
    /// **不进 JWT**:令牌 12 小时有效,把角色烤进令牌等于降权要等 12 小时才生效。
    pub async fn principal(&self, db: &Db) -> Result<Principal, ServerError> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>)>(
            "SELECT role, scope_kind, scope_id FROM role_binding WHERE account_id = $1",
        )
        .bind(&self.account_id)
        .fetch_all(&db.pool)
        .await?;

        let bindings: Vec<RoleBinding> = rows
            .iter()
            .filter_map(|(role, kind, id)| {
                let role = parse_role(role)?;
                let scope = match (kind.as_str(), id) {
                    ("org", _) => Scope::Org,
                    ("group", Some(t)) => Scope::Group(t.clone()),
                    ("channel", Some(c)) => Scope::Channel(c.clone()),
                    _ => return None,
                };
                Some(RoleBinding { role, scope })
            })
            .collect();

        Ok(Principal {
            id: self.account_id.clone(),
            display_name: self.display_name.clone(),
            kind: self.kind,
            bindings,
        })
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Identity {
    type Rejection = ServerError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let raw = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| ServerError::Unauthenticated("缺少 Bearer 令牌".into()))?;
        Identity::from_token(raw)
    }
}

fn parse_kind(s: &str) -> PrincipalKind {
    match s {
        "agent" => PrincipalKind::Agent,
        "service" => PrincipalKind::Service,
        _ => PrincipalKind::Human,
    }
}

pub fn kind_str(k: PrincipalKind) -> &'static str {
    match k {
        PrincipalKind::Human => "human",
        PrincipalKind::Agent => "agent",
        PrincipalKind::Service => "service",
    }
}

/// 角色名 → 内核枚举。**不认识的角色返回 None 而不是兜底成最小角色**:
/// 库里存着一个服务端读不懂的角色,是配置错误,该被丢弃并暴露,
/// 不该被悄悄降级成"成员"然后当作正常情况跑下去。
pub fn parse_role(s: &str) -> Option<Role> {
    Some(match s {
        "owner" | "org_owner" => Role::OrgOwner,
        "admin" | "org_admin" => Role::OrgAdmin,
        "group_admin" => Role::GroupAdmin,
        "publisher" => Role::Publisher,
        "approver" => Role::Approver,
        "member" => Role::Member,
        "guest" => Role::Guest,
        _ => return None,
    })
}

pub fn hash_password(pw: &str) -> Result<String, ServerError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ServerError::Internal(format!("口令哈希失败:{e}")))
}

pub fn verify_password(pw: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .map(|parsed| Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn password_roundtrip_and_rejection() {
        let phc = hash_password("正确口令").unwrap();
        assert!(verify_password("正确口令", &phc));
        assert!(!verify_password("错误口令", &phc));
        assert!(!verify_password("正确口令", "not-a-phc-string"), "格式不对必须判否,不得 panic");
    }

    #[test]
    fn token_roundtrip_carries_identity() {
        // SAFETY: 单进程内并行测试共享环境,这里只设不删,且值与其它测试一致
        unsafe { std::env::set_var("MUSTER_JWT_SECRET", TEST_SECRET) };
        let t = issue_token("alice", "Alice", "human").unwrap();
        let id = Identity::from_token(&t).unwrap();
        assert_eq!((id.account_id.as_str(), id.display_name.as_str()), ("alice", "Alice"));
        assert_eq!(id.kind, PrincipalKind::Human);
        assert!(Identity::from_token("garbage").is_err());
    }

    /// 密钥没配就不许签发/校验。**默认密钥等于没有认证**,
    /// 而"开发时先用个默认值"正是它最后出现在生产里的原因。
    #[test]
    fn missing_or_short_secret_is_refused() {
        assert!(validate_secret(None).is_err(), "缺密钥必须拒绝");
        assert!(validate_secret(Some("short".into())).is_err(), "短密钥必须拒绝");
        assert!(validate_secret(Some("x".repeat(31))).is_err(), "差一个字符也得拒");
        assert!(validate_secret(Some(TEST_SECRET.into())).is_ok());
    }

    /// 不认识的角色被丢弃,不兜底成最小角色。
    #[test]
    fn unknown_role_is_dropped_not_downgraded() {
        assert_eq!(parse_role("approver"), Some(Role::Approver));
        assert_eq!(parse_role("owner"), Some(Role::OrgOwner));
        assert_eq!(parse_role("超级管理员"), None);
        assert_eq!(parse_role(""), None);
    }
}
