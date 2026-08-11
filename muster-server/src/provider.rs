//! Provider 目录:**服务端下发,节点不自己声明**。
//!
//! ## 为什么这件事必须在服务端
//!
//! `locality` 决定 restricted 密级的内容能不能出门。在此之前它由各节点自己的
//! 配置文件声明——谁在自己机器上把一个云端 `base_url` 标成 `local`,
//! restricted 的会议内容就照常发出去,**而系统会报告"本地"**。
//!
//! 铁律二说"绝不静默升云",代码严格执行了它;错的是判断依据的**来源方位**。
//! 一台机器不该有权决定组织的密级边界。
//!
//! ## 服务端仍然一个密钥都不存
//!
//! 表里只有 `api_key_env`——环境变量的**名字**。值永远只在各节点自己的环境里。
//! 服务端被攻破也拿不到任何模型凭据,与"服务端不持有源码"是同一条姿态。
//!
//! 这也不是权宜:节点调模型时密钥总要在它自己内存里,服务端存一份不会让它
//! 少出现一个地方,只会多一个。
//!
//! ## 写目录 = 改策略
//!
//! 用 [`Action::ChangePolicy`],与 `cloud_max` 同一个授权面:
//! 一个定"什么密级能上云",一个定"什么算云"。组管理员能往自己组里加人,
//! 但不该能把一条云通道标成本地。
//!
//! 读目录只要求已认证:每个要跑模型的节点都得拿到它,而里面没有秘密。

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use muster_audit::{Actor, ContentHash, EventBody, NewEvent, Scope as AuditScope};
use muster_identity::{Action, Scope as IdScope};

use crate::audit::Audit;
use crate::auth::Identity;
use crate::db::now_ms;
use crate::org::require;
use crate::{Db, Result, ServerError};

pub type ProviderState = (Db, Audit);

/// 一条目录项。字段与 `muster_provider::RegistryConfig` 的 wire 形状对齐——
/// 节点直接把 [`catalog`] 的返回喂给 `ProviderRegistry`,中间没有翻译层。
///
/// 形状对齐这件事有测试守着(见本模块 `wire_tests`):翻译层是会漂的,
/// 而漂了的表现是"服务端说 X、节点解析出 Y",极难查。
#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderOut {
    pub id: String,
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub locality: String,
    pub display_name: Option<String>,
    /// 环境变量**名**,不是值。
    pub api_key_env: Option<String>,
    pub timeout_secs: i64,
    pub enabled: bool,
    pub is_default: bool,
}

#[derive(Deserialize)]
pub struct NewProvider {
    pub id: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub locality: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: i64,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub is_default: bool,
}

fn default_kind() -> String {
    "openai_compat".into()
}
fn default_timeout() -> i64 {
    120
}
fn yes() -> bool {
    true
}

/// 节点启动时拉的东西。形状即 `muster_provider::RegistryConfig`。
#[derive(Serialize)]
pub struct Catalog {
    /// 路由无偏好时用哪条。
    pub default: Option<String>,
    pub providers: std::collections::HashMap<String, CatalogEntry>,
}

/// `RegistryConfig.providers` 的值。`kind` 是 serde 的内部标签,
/// 所以这里不能有独立的 `kind` 字段——它由 `#[serde(tag)]` 那侧决定。
#[derive(Serialize)]
pub struct CatalogEntry {
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub locality: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    pub timeout_secs: i64,
}

/// 拿目录。**停用的不下发**——停用的意思就是"别再用它",
/// 下发之后再让节点自己过滤,等于把同一条策略实现两遍。
pub async fn catalog(
    State((db, _a)): State<ProviderState>,
    _id: Identity,
) -> Result<Json<Catalog>> {
    let rows = sqlx::query_as::<_, ProviderOut>(
        "SELECT id, kind, base_url, model, locality, display_name, api_key_env,
                timeout_secs, enabled, is_default
         FROM provider WHERE enabled ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await?;

    let default = rows.iter().find(|r| r.is_default).map(|r| r.id.clone());
    let providers = rows
        .into_iter()
        .map(|r| {
            (
                r.id,
                CatalogEntry {
                    kind: r.kind,
                    base_url: r.base_url,
                    model: r.model,
                    locality: r.locality,
                    display_name: r.display_name,
                    api_key_env: r.api_key_env,
                    timeout_secs: r.timeout_secs,
                },
            )
        })
        .collect();
    Ok(Json(Catalog { default, providers }))
}

/// 管理视图:含停用项。要 `ChangePolicy`——能看见全部通道就能推断组织的
/// 模型布局,那不是每个成员都该看的。
pub async fn list_all(
    State((db, _a)): State<ProviderState>,
    id: Identity,
) -> Result<Json<Vec<ProviderOut>>> {
    require(&db, &id, &Action::ChangePolicy, &IdScope::Org).await?;
    Ok(Json(
        sqlx::query_as::<_, ProviderOut>(
            "SELECT id, kind, base_url, model, locality, display_name, api_key_env,
                    timeout_secs, enabled, is_default
             FROM provider ORDER BY id",
        )
        .fetch_all(&db.pool)
        .await?,
    ))
}

/// 新增或更新一条。
pub async fn upsert(
    State((db, audit)): State<ProviderState>,
    id: Identity,
    Json(p): Json<NewProvider>,
) -> Result<Json<ProviderOut>> {
    require(&db, &id, &Action::ChangePolicy, &IdScope::Org).await?;

    if p.id.trim().is_empty() {
        return Err(ServerError::BadRequest("provider id 不能为空".into()));
    }
    if !matches!(p.locality.as_str(), "local" | "cloud") {
        return Err(ServerError::BadRequest(format!(
            "locality 只能是 local 或 cloud,收到 `{}`",
            p.locality
        )));
    }
    // **密钥不许经这个接口。** 有人会顺手把真钥匙贴进 api_key_env,
    // 那一刻服务端就成了密钥库——而这张表本来是不必保护的
    if let Some(v) = &p.api_key_env {
        if looks_like_a_secret(v) {
            return Err(ServerError::BadRequest(format!(
                "api_key_env 要填环境变量的**名字**(如 KIMI_API_KEY),不是密钥本身。\
                 收到的值像一把真钥匙,已拒绝——服务端不存密钥。"
            )));
        }
    }

    let now = now_ms();
    // 同一事务里挪默认位:先清后设。分开做的话中间那一刻没有默认通道,
    // 恰好在此时启动的节点会拿到一份没有默认项的目录
    let mut tx = db.pool.begin().await?;
    if p.is_default {
        sqlx::query("UPDATE provider SET is_default = FALSE WHERE is_default")
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO provider(id, kind, base_url, model, locality, display_name,
                              api_key_env, timeout_secs, enabled, is_default, created_ms, updated_ms)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$11)
         ON CONFLICT (id) DO UPDATE SET
           kind=$2, base_url=$3, model=$4, locality=$5, display_name=$6,
           api_key_env=$7, timeout_secs=$8, enabled=$9, is_default=$10, updated_ms=$11",
    )
    .bind(&p.id)
    .bind(&p.kind)
    .bind(&p.base_url)
    .bind(&p.model)
    .bind(&p.locality)
    .bind(&p.display_name)
    .bind(&p.api_key_env)
    .bind(p.timeout_secs)
    .bind(p.enabled)
    .bind(p.is_default)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // 进审计链:改这张表等于改组织的密级边界,必须留痕。
    // 存哈希不存正文(铁律三),但 id 与 locality 是判读所必需的
    audit
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::human(&id.account_id),
            scope: AuditScope::default(),
            run_id: None,
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            // 用 PolicyUpdate:改这张表就是改组织策略,和改 cloud_max 一回事。
            //
            // **哈希把 provider id 也算进去**(铁律三:只存哈希不存正文)。
            // 于是链上看不出改的是哪条通道——查证时得拿着当时的配置来复算。
            // 这是铁律三的既定代价,不在这里为了方便破例。
            body: EventBody::PolicyUpdate {
                changed_by: Actor::human(&id.account_id),
                diff_hash: ContentHash::sha256(
                    format!(
                        "provider:{}|{}|{}|{}|{}",
                        p.id, p.base_url, p.model, p.locality, p.enabled
                    )
                    .as_bytes(),
                ),
            },
        })
        .await?;

    Ok(Json(ProviderOut {
        id: p.id,
        kind: p.kind,
        base_url: p.base_url,
        model: p.model,
        locality: p.locality,
        display_name: p.display_name,
        api_key_env: p.api_key_env,
        timeout_secs: p.timeout_secs,
        enabled: p.enabled,
        is_default: p.is_default,
    }))
}

/// 停用一条(不删)。删掉就查不出"上周那次任务用的是哪条通道"。
pub async fn disable(
    State((db, audit)): State<ProviderState>,
    id: Identity,
    Path(pid): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ChangePolicy, &IdScope::Org).await?;
    let n = sqlx::query("UPDATE provider SET enabled=FALSE, is_default=FALSE, updated_ms=$2 WHERE id=$1")
        .bind(&pid)
        .bind(now_ms())
        .execute(&db.pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ServerError::NotFound(format!("provider {pid}")));
    }
    audit
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::human(&id.account_id),
            scope: AuditScope::default(),
            run_id: None,
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body: EventBody::PolicyUpdate {
                changed_by: Actor::human(&id.account_id),
                diff_hash: ContentHash::sha256(format!("provider:{pid}|disabled").as_bytes()),
            },
        })
        .await?;
    Ok(Json(serde_json::json!({ "disabled": pid })))
}

/// 这串东西看着像不像一把真钥匙。
///
/// 判据是**形状**,不是名单:环境变量名按惯例是大写加下划线、不长;
/// 而钥匙通常带 `sk-` 之类的前缀、含小写与连字符、且长。
///
/// 宁可偶尔误伤(填 `MY_KEY_2` 之类不会被拦),也不能让真钥匙进库。
/// 拦不住所有情况——这不是安全边界,是**防手滑**:真正的边界是这张表
/// 从设计上就不存值。
pub fn looks_like_a_secret(v: &str) -> bool {
    let v = v.trim();
    if v.len() > 64 {
        return true; // 环境变量名不会这么长
    }
    // 常见密钥前缀
    if v.starts_with("sk-") || v.starts_with("pk-") || v.starts_with("ghp_") || v.starts_with("xai-")
    {
        return true;
    }
    // 环境变量名的惯例形状:大写字母 / 数字 / 下划线
    !v.is_empty() && !v.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::looks_like_a_secret;

    #[test]
    fn env_var_names_pass_and_real_keys_are_refused() {
        for name in ["KIMI_API_KEY", "OPENAI_API_KEY", "MY_KEY_2", "A"] {
            assert!(!looks_like_a_secret(name), "{name} 是合法的环境变量名");
        }
        for key in [
            "sk-abc123",
            "ghp_aaaaaaaaaaaaaaaaaaaa",
            "sk-proj-0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            "eyJhbGciOiJIUzI1NiJ9.aaaa.bbbb",
        ] {
            assert!(looks_like_a_secret(key), "{key} 像一把真钥匙,该拦");
        }
    }

    /// 空值是"这条通道不需要密钥"(如本机 Ollama),不是可疑值。
    #[test]
    fn empty_is_not_suspicious() {
        assert!(!looks_like_a_secret(""));
    }
}

/// 下发的形状必须能被 `muster_provider::RegistryConfig` 直接吃下。
///
/// 中间加一层翻译是会漂的,而漂了的表现是"服务端说 X、节点解析出 Y"——
/// 在密级这种地方,那意味着一条云通道可能被解析成本地。
///
/// muster-provider 只在 **dev-dependencies** 里:服务端运行时不该链进
/// 模型客户端,它一个 provider 都不调。但形状的一致性必须有东西守着。
#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn catalog_deserialises_into_the_registry_config() {
        let mut providers = std::collections::HashMap::new();
        providers.insert(
            "kimi".to_string(),
            CatalogEntry {
                kind: "openai_compat".into(),
                base_url: "https://api.kimi.com/coding/v1".into(),
                model: "kimi-k3".into(),
                locality: "cloud".into(),
                display_name: Some("云端·Kimi K3".into()),
                api_key_env: Some("KIMI_API_KEY".into()),
                timeout_secs: 300,
            },
        );
        providers.insert(
            "local-ollama".to_string(),
            CatalogEntry {
                kind: "openai_compat".into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen3:8b".into(),
                locality: "local".into(),
                display_name: None,
                api_key_env: None, // 本地通道不需要密钥
                timeout_secs: 120,
            },
        );
        let json = serde_json::to_string(&Catalog {
            default: Some("kimi".into()),
            providers,
        })
        .unwrap();

        let cfg: muster_provider::RegistryConfig = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("下发的形状 RegistryConfig 吃不下:{e}\n{json}"));
        assert_eq!(cfg.default.as_deref(), Some("kimi"));
        assert_eq!(cfg.providers.len(), 2);
    }
}
