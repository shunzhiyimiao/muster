//! 节点:一台登记到组织的机器(docs/节点与工牌-公用私有值班.md,实施顺序 1)。
//!
//! ## 节点不是账号
//!
//! 入网时节点本地生成 ed25519 密钥,服务端只存公钥;会话靠 challenge / 签名换一张
//! 短期 JWT,claims 标明 node_id,不是任何人的账号。**节点会话只能代表它托管的工牌
//! 行事**:请求带 `X-Muster-Badge: <工牌>`,[`crate::auth::Identity`] 提取器核对这张
//! 工牌确实分配在这个节点上,才以工牌身份放行;别的工牌一律 403。于是会议 daemon
//! 不再需要任何口令——compose 里写死 A-007 口令那条路就此可以退场。
//!
//! ## 设备码入网
//!
//! 节点 `POST /nodes/enroll` 得到 user_code(形如 K7PX-4MQ2)与 poll_token;管理员拿着
//! user_code 批准;节点轮询 `GET /nodes/enroll/poll` 直到批准。入网批准、工牌分配、
//! 值班工牌变更都进服务端审计链。
//!
//! ## 心跳与熔断
//!
//! 心跳带进度(最近一次成功回传转写 / 纪要的时间)和熔断上报。90 秒无心跳的节点,
//! 其上的工牌在 `/accounts`、`/org/duty` 显示不可用。熔断由节点自己判(连续 3 次同类
//! 错误),服务端只记账、展示;**解除靠人**:再点一次「请 Agent」,
//! `meeting.agent_requested_ms` 更新,服务端清掉上报记录,节点据此重新认领。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use argon2::password_hash::rand_core::{OsRng, RngCore};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::Json;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use muster_audit::{Actor, ContentHash, EventBody, NewEvent, Scope as AuditScope};
use muster_identity::{Action, Scope};

use crate::audit::Audit;
use crate::auth::{decode_claims, issue_node_token, Identity};
use crate::db::now_ms;
use crate::org::{record_grant_change, require};
use crate::ratelimit::{client_ip, LoginLimiter, MAX_PER_IP};
use crate::ws::Hub;
use crate::{Db, Result, ServerError};

pub type NodeState = (Db, Hub, Audit, Challenges);

/// 多久没心跳算离线。daemon 每 30 秒一跳,漏两跳才算断。
pub const HEARTBEAT_TIMEOUT_MS: i64 = 90_000;
/// challenge 的有效期。
pub const CHALLENGE_TTL_MS: i64 = 60_000;
/// 组织设置里值班工牌那一项的键。
pub const DUTY_BADGE_KEY: &str = "duty_badge";

// ---------------------------------------------------------------- 纯函数

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 设备码:8 个字符,去掉易混的 0/O/1/I,形如 `K7PX-4MQ2`。念给管理员听也不会错。
pub fn new_user_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut raw = [0u8; 8];
    OsRng.fill_bytes(&mut raw);
    let s: String = raw.iter().map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char).collect();
    format!("{}-{}", &s[..4], &s[4..])
}

/// 被签名的内容:带域分隔,免得一张签名在别处也能当凭据用。
pub fn challenge_message(node_id: Uuid, nonce_hex: &str) -> Vec<u8> {
    format!("muster-node-session:v1:{node_id}:{nonce_hex}").into_bytes()
}

/// 十六进制公钥 → ed25519 验签钥。非法的一律拒,不猜。
pub fn parse_pubkey(hex: &str) -> Result<VerifyingKey> {
    let bytes = hex_decode(hex).ok_or_else(|| ServerError::BadRequest("公钥必须是 64 位十六进制".into()))?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| ServerError::BadRequest("公钥必须是 32 字节".into()))?;
    VerifyingKey::from_bytes(&arr).map_err(|_| ServerError::BadRequest("不是有效的 ed25519 公钥".into()))
}

pub fn verify_signature(pubkey: &[u8], msg: &[u8], sig_hex: &str) -> bool {
    let Ok(arr) = <[u8; 32]>::try_from(pubkey) else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&arr) else { return false };
    let Some(sig) = hex_decode(sig_hex) else { return false };
    let Ok(sig) = Signature::from_slice(&sig) else { return false };
    vk.verify(msg, &sig).is_ok()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// 待验的 challenge:每个节点一个,60 秒,用一次即作废。
#[derive(Clone, Default)]
pub struct Challenges {
    inner: Arc<Mutex<HashMap<Uuid, (String, i64)>>>,
}

impl Challenges {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn issue(&self, node: Uuid, now: i64) -> String {
        let nonce = random_hex(32);
        let mut m = self.inner.lock().unwrap();
        m.retain(|_, (_, exp)| *exp > now);
        m.insert(node, (nonce.clone(), now + CHALLENGE_TTL_MS));
        nonce
    }
    pub fn take(&self, node: Uuid, now: i64) -> Option<String> {
        let (nonce, exp) = self.inner.lock().unwrap().remove(&node)?;
        (exp > now).then_some(nonce)
    }
}

// ---------------------------------------------------------------- 节点会话提取器

/// 节点会话(不是账号)。只有节点自己的接口(desired / heartbeat)用它;
/// 要以工牌身份调业务接口,带 `X-Muster-Badge` 走 [`Identity`]。
pub struct NodeSession {
    pub node_id: Uuid,
    pub name: String,
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for NodeSession {
    type Rejection = ServerError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> std::result::Result<Self, Self::Rejection> {
        let raw = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| ServerError::Unauthenticated("缺少 Bearer 令牌".into()))?;
        let claims = decode_claims(raw)?;
        if claims.kind != "node" {
            return Err(ServerError::Unauthenticated("这个接口只认节点会话".into()));
        }
        let node_id = claims
            .sub
            .strip_prefix("node:")
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| ServerError::Unauthenticated("节点会话无效".into()))?;
        Ok(NodeSession { node_id, name: claims.name })
    }
}

/// 节点会话以工牌身份行事时的核对(给 `Identity` 提取器用):
/// 工牌必须是 kind=agent、未停用、分配在这个节点上,节点必须已批准。
pub async fn badge_hosted_by(db: &Db, badge: &str, node_id: Uuid) -> Result<Option<(String, String)>> {
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT a.id, a.display_name FROM account a JOIN node n ON n.id = a.node_id
         WHERE a.id = $1 AND a.kind = 'agent' AND a.disabled = FALSE AND n.id = $2 AND n.status = 'approved'",
    )
    .bind(badge)
    .bind(node_id)
    .fetch_optional(&db.pool)
    .await?)
}

// ---------------------------------------------------------------- 入网

#[derive(Deserialize)]
pub struct EnrollReq {
    pub name: String,
    /// ed25519 公钥,64 位十六进制
    pub pubkey: String,
    #[serde(default)]
    pub capabilities: serde_json::Value,
}

/// 节点申请入网。**不要鉴权**(它还没有任何身份),所以按来源 IP 限流,
/// 且只会留下一行 pending——批准前它什么都做不了。
pub async fn enroll(
    State((db, _hub, _audit, _ch)): State<NodeState>,
    axum::Extension(limiter): axum::Extension<Arc<LoginLimiter>>,
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<EnrollReq>,
) -> Result<Json<serde_json::Value>> {
    let ip = client_ip(&headers, peer.map(|c| c.0));
    if let Err(wait) = limiter.check(&format!("enroll:{ip}"), MAX_PER_IP) {
        return Err(ServerError::TooManyRequests(format!("入网申请过多,请 {} 秒后再试", wait.as_secs() + 1)));
    }
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ServerError::BadRequest("节点名 1–64 字".into()));
    }
    let vk = parse_pubkey(&req.pubkey)?;
    if !req.capabilities.is_object() && !req.capabilities.is_null() {
        return Err(ServerError::BadRequest("capabilities 必须是对象".into()));
    }
    let caps = if req.capabilities.is_null() { serde_json::json!({}) } else { req.capabilities };

    let id = Uuid::new_v4();
    let poll_token = random_hex(32);
    // 设备码可能撞(32^8 里撞的概率极小,但撞了就是一次失败的入网),撞了换一个再试
    for _ in 0..5 {
        let code = new_user_code();
        let r = sqlx::query(
            "INSERT INTO node(id, name, pubkey, capabilities, status, user_code, poll_token_hash, created_ms)
             VALUES($1,$2,$3,$4,'pending',$5,$6,$7)",
        )
        .bind(id)
        .bind(name)
        .bind(vk.as_bytes().as_slice())
        .bind(&caps)
        .bind(&code)
        .bind(sha256_hex(poll_token.as_bytes()))
        .bind(now_ms())
        .execute(&db.pool)
        .await;
        match r {
            Ok(_) => {
                return Ok(Json(serde_json::json!({ "node_id": id, "user_code": code, "poll_token": poll_token })))
            }
            Err(e) if is_unique_violation(&e) => {
                // 同一把公钥重复入网:告诉它已经登记过,别再生成新节点
                let dup: Option<(Uuid, String)> =
                    sqlx::query_as("SELECT id, status FROM node WHERE pubkey = $1")
                        .bind(vk.as_bytes().as_slice())
                        .fetch_optional(&db.pool)
                        .await?;
                if let Some((nid, status)) = dup {
                    return Err(ServerError::BadRequest(format!("这把公钥已登记为节点 {nid}(状态 {status})")));
                }
                continue; // 是设备码撞了,换一个
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(ServerError::Internal("设备码连续撞车,稍后再试".into()))
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error().and_then(|d| d.code()).map(|c| c == "23505").unwrap_or(false)
}

#[derive(Deserialize)]
pub struct PollQuery {
    pub node_id: Uuid,
    pub poll_token: String,
}

/// 节点轮询自己批准了没有。poll_token 就是凭据——没有它,谁都能探任意节点的状态。
pub async fn enroll_poll(
    State((db, _hub, _audit, _ch)): State<NodeState>,
    Query(q): Query<PollQuery>,
) -> Result<Json<serde_json::Value>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT status, name FROM node WHERE id = $1 AND poll_token_hash = $2",
    )
    .bind(q.node_id)
    .bind(sha256_hex(q.poll_token.as_bytes()))
    .fetch_optional(&db.pool)
    .await?;
    let (status, name) = row.ok_or_else(|| ServerError::NotFound("没有这个待入网的节点".into()))?;
    Ok(Json(serde_json::json!({ "node_id": q.node_id, "name": name, "status": status })))
}

/// 管理员批准:拿设备码,不拿 node_id——设备码是节点屏幕上那串,管理员照着念就行。
pub async fn enroll_approve(
    State((db, _hub, audit, _ch)): State<NodeState>,
    id: Identity,
    Path(user_code): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let code = user_code.trim().to_uppercase();
    let row: Option<(Uuid, String, Vec<u8>)> =
        sqlx::query_as("SELECT id, name, pubkey FROM node WHERE user_code = $1 AND status = 'pending'")
            .bind(&code)
            .fetch_optional(&db.pool)
            .await?;
    let (nid, name, pubkey) = row.ok_or_else(|| ServerError::NotFound(format!("没有待批准的设备码 {code}")))?;

    // 先记账再动库(与会议接口同一姿态)
    audit
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::system(format!("node:{nid}")),
            scope: AuditScope::default(),
            run_id: None,
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body: EventBody::NodeEnroll {
                node_id: nid.to_string(),
                name: name.clone(),
                pubkey_hash: ContentHash::sha256(&pubkey),
                approved_by: Actor::human(&id.account_id),
            },
        })
        .await?;
    sqlx::query(
        "UPDATE node SET status = 'approved', approved_by = $1, approved_ms = $2, user_code = NULL WHERE id = $3",
    )
    .bind(&id.account_id)
    .bind(now_ms())
    .bind(nid)
    .execute(&db.pool)
    .await?;
    Ok(Json(serde_json::json!({ "node_id": nid, "name": name, "status": "approved" })))
}

#[derive(Serialize)]
pub struct NodeOut {
    pub id: Uuid,
    pub name: String,
    pub owner_account: Option<String>,
    pub status: String,
    /// 只在 pending 时有:管理员要念的那串
    pub user_code: Option<String>,
    pub capabilities: serde_json::Value,
    pub version: Option<String>,
    pub last_seen_ms: Option<i64>,
    pub online: bool,
    pub last_progress_ms: Option<i64>,
    pub last_progress_kind: Option<String>,
    /// 托管的工牌
    pub badges: Vec<String>,
    /// 节点上报的熔断
    pub breakers: Vec<BreakerOut>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct BreakerOut {
    pub meeting_id: Uuid,
    pub error_kind: String,
    pub failures: i32,
    pub reported_ms: i64,
}

#[derive(sqlx::FromRow)]
struct NodeRow {
    id: Uuid,
    name: String,
    owner_account: Option<String>,
    status: String,
    user_code: Option<String>,
    capabilities: serde_json::Value,
    version: Option<String>,
    last_seen_ms: Option<i64>,
    last_progress_ms: Option<i64>,
    last_progress_kind: Option<String>,
}

pub fn is_online(last_seen_ms: Option<i64>, now: i64) -> bool {
    last_seen_ms.map(|t| now - t <= HEARTBEAT_TIMEOUT_MS).unwrap_or(false)
}

/// 列节点(管理员)。待批准的带设备码,好对着节点屏幕核对。
pub async fn list(State((db, _hub, _audit, _ch)): State<NodeState>, id: Identity) -> Result<Json<Vec<NodeOut>>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let rows = sqlx::query_as::<_, NodeRow>(
        "SELECT id, name, owner_account, status, user_code, capabilities, version, last_seen_ms,
                last_progress_ms, last_progress_kind FROM node ORDER BY created_ms",
    )
    .fetch_all(&db.pool)
    .await?;
    let now = now_ms();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let badges: Vec<(String,)> = sqlx::query_as("SELECT id FROM account WHERE node_id = $1 ORDER BY id")
            .bind(r.id)
            .fetch_all(&db.pool)
            .await?;
        let breakers = sqlx::query_as::<_, BreakerOut>(
            "SELECT meeting_id, error_kind, failures, reported_ms FROM node_breaker WHERE node_id = $1 ORDER BY reported_ms",
        )
        .bind(r.id)
        .fetch_all(&db.pool)
        .await?;
        out.push(NodeOut {
            id: r.id,
            name: r.name,
            owner_account: r.owner_account,
            online: r.status == "approved" && is_online(r.last_seen_ms, now),
            status: r.status,
            user_code: r.user_code,
            capabilities: r.capabilities,
            version: r.version,
            last_seen_ms: r.last_seen_ms,
            last_progress_ms: r.last_progress_ms,
            last_progress_kind: r.last_progress_kind,
            badges: badges.into_iter().map(|b| b.0).collect(),
            breakers,
        });
    }
    Ok(Json(out))
}

// ---------------------------------------------------------------- 会话

/// 要一个 challenge。只有已批准的节点有资格——批准前连 nonce 都不发。
pub async fn challenge(
    State((db, _hub, _audit, ch)): State<NodeState>,
    Path(node_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let status: Option<(String,)> = sqlx::query_as("SELECT status FROM node WHERE id = $1")
        .bind(node_id)
        .fetch_optional(&db.pool)
        .await?;
    match status.map(|s| s.0).as_deref() {
        Some("approved") => {}
        Some("pending") => return Err(ServerError::Forbidden("节点尚未批准".into())),
        Some(_) => return Err(ServerError::Forbidden("节点已吊销".into())),
        None => return Err(ServerError::NotFound(format!("节点 {node_id}"))),
    }
    let nonce = ch.issue(node_id, now_ms());
    Ok(Json(serde_json::json!({ "nonce": nonce, "ttl_ms": CHALLENGE_TTL_MS })))
}

#[derive(Deserialize)]
pub struct SessionReq {
    /// 对 challenge_message 的 ed25519 签名,128 位十六进制
    pub sig: String,
}

/// 验签换会话。nonce 用一次即作废:签名被截获也重放不了。
pub async fn session(
    State((db, _hub, _audit, ch)): State<NodeState>,
    Path(node_id): Path<Uuid>,
    Json(req): Json<SessionReq>,
) -> Result<Json<serde_json::Value>> {
    let nonce = ch
        .take(node_id, now_ms())
        .ok_or_else(|| ServerError::Unauthenticated("没有有效的 challenge,先 POST /nodes/:id/challenge".into()))?;
    let row: Option<(String, Vec<u8>, String)> =
        sqlx::query_as("SELECT name, pubkey, status FROM node WHERE id = $1")
            .bind(node_id)
            .fetch_optional(&db.pool)
            .await?;
    let (name, pubkey, status) = row.ok_or_else(|| ServerError::NotFound(format!("节点 {node_id}")))?;
    if status != "approved" {
        return Err(ServerError::Forbidden("节点尚未批准".into()));
    }
    if !verify_signature(&pubkey, &challenge_message(node_id, &nonce), &req.sig) {
        return Err(ServerError::Unauthenticated("签名验不过".into()));
    }
    let token = issue_node_token(node_id, &name)?;
    sqlx::query("UPDATE node SET last_seen_ms = $1 WHERE id = $2")
        .bind(now_ms())
        .bind(node_id)
        .execute(&db.pool)
        .await?;
    Ok(Json(serde_json::json!({
        "token": token,
        "node_id": node_id,
        "expires_in": crate::auth::NODE_TOKEN_TTL_SECS,
    })))
}

// ---------------------------------------------------------------- 设定值与心跳

#[derive(Serialize)]
pub struct DesiredBadge {
    pub id: String,
    pub display_name: String,
    pub duty: bool,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct DesiredMeeting {
    pub id: Uuid,
    pub channel_id: String,
    pub title: String,
    pub level: String,
    pub room: String,
    pub started_ms: i64,
    pub wants_agent: bool,
    /// 最近一次「请 Agent」的时刻。节点的熔断只认比它晚的请求。
    pub agent_requested_ms: Option<i64>,
}

#[derive(Serialize)]
pub struct Desired {
    pub node_id: Uuid,
    pub badges: Vec<DesiredBadge>,
    /// 当前值班工牌(全组织一个);不一定在这个节点上
    pub duty_badge: Option<String>,
    /// 要进的会:只有值班工牌住在这个节点上时才非空
    pub meetings: Vec<DesiredMeeting>,
    pub server_ms: i64,
}

pub async fn duty_badge(db: &Db) -> Result<Option<String>> {
    Ok(sqlx::query_as::<_, (String,)>("SELECT value FROM org_setting WHERE key = $1")
        .bind(DUTY_BADGE_KEY)
        .fetch_optional(&db.pool)
        .await?
        .map(|r| r.0))
}

/// 这个节点该干什么:托管哪些工牌,其中值班工牌该进哪些会。
pub async fn desired(
    State((db, _hub, _audit, _ch)): State<NodeState>,
    sess: NodeSession,
    Path(node_id): Path<Uuid>,
) -> Result<Json<Desired>> {
    if sess.node_id != node_id {
        return Err(ServerError::Forbidden("只能看自己的设定值".into()));
    }
    let duty = duty_badge(&db).await?;
    let badges: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, display_name FROM account WHERE node_id = $1 AND kind = 'agent' AND disabled = FALSE ORDER BY id",
    )
    .bind(node_id)
    .fetch_all(&db.pool)
    .await?;
    let hosts_duty = duty.as_ref().map(|d| badges.iter().any(|(id, _)| id == d)).unwrap_or(false);
    let meetings = if hosts_duty {
        sqlx::query_as::<_, DesiredMeeting>(
            "SELECT id, channel_id, title, level, room, started_ms, wants_agent, agent_requested_ms
             FROM meeting WHERE wants_agent AND ended_ms IS NULL ORDER BY started_ms",
        )
        .fetch_all(&db.pool)
        .await?
    } else {
        vec![]
    };
    Ok(Json(Desired {
        node_id,
        badges: badges
            .into_iter()
            .map(|(id, display_name)| DesiredBadge { duty: duty.as_deref() == Some(id.as_str()), id, display_name })
            .collect(),
        duty_badge: duty,
        meetings,
        server_ms: now_ms(),
    }))
}

#[derive(Deserialize)]
pub struct Progress {
    pub kind: String,
    pub at_ms: i64,
}

#[derive(Deserialize)]
pub struct BreakerReport {
    pub meeting_id: Uuid,
    pub error_kind: String,
    pub failures: i32,
}

#[derive(Deserialize)]
pub struct Heartbeat {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub capabilities: Option<serde_json::Value>,
    /// 最近一次**成功干活**的时间:活着和在干活是两回事
    #[serde(default)]
    pub progress: Option<Progress>,
    #[serde(default)]
    pub breakers: Vec<BreakerReport>,
}

/// 心跳:更新 last_seen / 进度 / 能力,登记熔断上报(新出现或次数变了才记账,
/// 否则每 30 秒一条审计就是噪音)。
pub async fn heartbeat(
    State((db, _hub, audit, _ch)): State<NodeState>,
    sess: NodeSession,
    Path(node_id): Path<Uuid>,
    Json(hb): Json<Heartbeat>,
) -> Result<Json<serde_json::Value>> {
    if sess.node_id != node_id {
        return Err(ServerError::Forbidden("只能报自己的心跳".into()));
    }
    let now = now_ms();
    sqlx::query(
        "UPDATE node SET last_seen_ms = $1,
                version = COALESCE($2, version),
                capabilities = COALESCE($3, capabilities),
                last_progress_ms = COALESCE($4, last_progress_ms),
                last_progress_kind = COALESCE($5, last_progress_kind)
         WHERE id = $6 AND status = 'approved'",
    )
    .bind(now)
    .bind(&hb.version)
    .bind(&hb.capabilities)
    .bind(hb.progress.as_ref().map(|p| p.at_ms))
    .bind(hb.progress.as_ref().map(|p| p.kind.clone()))
    .bind(node_id)
    .execute(&db.pool)
    .await?;

    for b in &hb.breakers {
        let existing: Option<(String, i32)> =
            sqlx::query_as("SELECT error_kind, failures FROM node_breaker WHERE node_id = $1 AND meeting_id = $2")
                .bind(node_id)
                .bind(b.meeting_id)
                .fetch_optional(&db.pool)
                .await?;
        if existing.as_ref().map(|(k, n)| k == &b.error_kind && *n == b.failures).unwrap_or(false) {
            continue; // 老消息,不重复记账
        }
        let cid: Option<(String,)> = sqlx::query_as("SELECT channel_id FROM meeting WHERE id = $1")
            .bind(b.meeting_id)
            .fetch_optional(&db.pool)
            .await?;
        let Some((cid,)) = cid else {
            tracing::warn!(node = %node_id, meeting = %b.meeting_id, "熔断上报指向不存在的会议,忽略");
            continue;
        };
        audit
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::system(format!("node:{node_id}")),
                scope: AuditScope { team: None, channel: Some(cid) },
                run_id: None,
                session_id: Some(format!("meeting:{}", b.meeting_id)),
                policy_version: Some("policy-v1".into()),
                label: None,
                locality: None,
                body: EventBody::NodeBreaker {
                    node_id: node_id.to_string(),
                    meeting_id: b.meeting_id.to_string(),
                    error_kind: b.error_kind.clone(),
                    failures: b.failures.max(0) as u32,
                },
            })
            .await?;
        sqlx::query(
            "INSERT INTO node_breaker(node_id, meeting_id, error_kind, failures, reported_ms) VALUES($1,$2,$3,$4,$5)
             ON CONFLICT (node_id, meeting_id) DO UPDATE SET error_kind = EXCLUDED.error_kind,
             failures = EXCLUDED.failures, reported_ms = EXCLUDED.reported_ms",
        )
        .bind(node_id)
        .bind(b.meeting_id)
        .bind(&b.error_kind)
        .bind(b.failures)
        .bind(now)
        .execute(&db.pool)
        .await?;
    }
    Ok(Json(serde_json::json!({ "ok": true, "server_ms": now })))
}

// ---------------------------------------------------------------- 工牌分配与值班

#[derive(Deserialize)]
pub struct AssignReq {
    /// 空 = 从节点上摘下来
    #[serde(default)]
    pub node_id: Option<Uuid>,
}

/// 把一张工牌(kind=agent 的账号)分配到某个节点。
pub async fn assign_badge(
    State((db, _hub, audit, _ch)): State<NodeState>,
    id: Identity,
    Path(badge): Path<String>,
    Json(req): Json<AssignReq>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let kind: Option<(String,)> = sqlx::query_as("SELECT kind FROM account WHERE id = $1")
        .bind(&badge)
        .fetch_optional(&db.pool)
        .await?;
    match kind.map(|k| k.0).as_deref() {
        Some("agent") => {}
        Some(k) => return Err(ServerError::BadRequest(format!("{badge} 是 {k} 账号,只有工牌(agent)能分配到节点"))),
        None => return Err(ServerError::NotFound(format!("账号 {badge}"))),
    }
    if let Some(nid) = req.node_id {
        let st: Option<(String,)> = sqlx::query_as("SELECT status FROM node WHERE id = $1")
            .bind(nid)
            .fetch_optional(&db.pool)
            .await?;
        match st.map(|s| s.0).as_deref() {
            Some("approved") => {}
            Some(s) => return Err(ServerError::BadRequest(format!("节点 {nid} 状态是 {s},先批准"))),
            None => return Err(ServerError::NotFound(format!("节点 {nid}"))),
        }
    }
    record_grant_change(
        &audit,
        &id,
        &badge,
        &match req.node_id {
            Some(n) => format!("assign node={n}"),
            None => "unassign node".into(),
        },
    )
    .await?;
    sqlx::query("UPDATE account SET node_id = $1 WHERE id = $2")
        .bind(req.node_id)
        .bind(&badge)
        .execute(&db.pool)
        .await?;
    Ok(Json(serde_json::json!({ "badge": badge, "node_id": req.node_id })))
}

#[derive(Deserialize)]
pub struct DutyReq {
    /// 空 = 取消值班
    #[serde(default)]
    pub badge: Option<String>,
}

/// 当前值班工牌及其节点在不在线。谁都能看:会议室页要据此解释"Agent 为什么没来"。
pub async fn duty_get(State((db, _hub, _audit, _ch)): State<NodeState>, _id: Identity) -> Result<Json<serde_json::Value>> {
    let badge = duty_badge(&db).await?;
    let Some(badge) = badge else {
        return Ok(Json(serde_json::json!({ "badge": null })));
    };
    let node: Option<(Option<Uuid>,)> = sqlx::query_as("SELECT node_id FROM account WHERE id = $1")
        .bind(&badge)
        .fetch_optional(&db.pool)
        .await?;
    let node_id = node.and_then(|n| n.0);
    let (name, last_seen) = match node_id {
        Some(nid) => sqlx::query_as::<_, (String, Option<i64>)>("SELECT name, last_seen_ms FROM node WHERE id = $1")
            .bind(nid)
            .fetch_optional(&db.pool)
            .await?
            .map(|(n, l)| (Some(n), l))
            .unwrap_or((None, None)),
        None => (None, None),
    };
    Ok(Json(serde_json::json!({
        "badge": badge,
        "node_id": node_id,
        "node_name": name,
        "node_online": is_online(last_seen, now_ms()),
        "node_last_seen_ms": last_seen,
    })))
}

/// 指定值班工牌:必须是已分配到某个**已批准、有本地转写能力**节点上的工牌(边界五)。
pub async fn duty_set(
    State((db, _hub, audit, _ch)): State<NodeState>,
    id: Identity,
    Json(req): Json<DutyReq>,
) -> Result<Json<serde_json::Value>> {
    require(&db, &id, &Action::ManageMembers, &Scope::Org).await?;
    let Some(badge) = req.badge.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        record_grant_change(&audit, &id, "-", "duty cleared").await?;
        sqlx::query("DELETE FROM org_setting WHERE key = $1").bind(DUTY_BADGE_KEY).execute(&db.pool).await?;
        return Ok(Json(serde_json::json!({ "badge": null })));
    };
    let row: Option<(String, Option<Uuid>)> =
        sqlx::query_as("SELECT kind, node_id FROM account WHERE id = $1 AND disabled = FALSE")
            .bind(badge)
            .fetch_optional(&db.pool)
            .await?;
    let (kind, node_id) = row.ok_or_else(|| ServerError::NotFound(format!("账号 {badge}")))?;
    if kind != "agent" {
        return Err(ServerError::BadRequest(format!("{badge} 不是工牌(kind={kind})")));
    }
    let Some(nid) = node_id else {
        return Err(ServerError::BadRequest(format!("{badge} 还没分配到任何节点,先 badge-assign")));
    };
    let node: Option<(String, serde_json::Value)> =
        sqlx::query_as("SELECT status, capabilities FROM node WHERE id = $1")
            .bind(nid)
            .fetch_optional(&db.pool)
            .await?;
    let (status, caps) = node.ok_or_else(|| ServerError::NotFound(format!("节点 {nid}")))?;
    if status != "approved" {
        return Err(ServerError::BadRequest(format!("节点 {nid} 状态是 {status}")));
    }
    if caps.get("stt_local").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(ServerError::BadRequest(
            "值班工牌所在的节点必须有本地转写能力(capabilities.stt_local = true)——会议音频不出内网是边界五".into(),
        ));
    }
    record_grant_change(&audit, &id, badge, "duty set").await?;
    sqlx::query(
        "INSERT INTO org_setting(key, value, updated_by, updated_ms) VALUES($1,$2,$3,$4)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by, updated_ms = EXCLUDED.updated_ms",
    )
    .bind(DUTY_BADGE_KEY)
    .bind(badge)
    .bind(&id.account_id)
    .bind(now_ms())
    .execute(&db.pool)
    .await?;
    Ok(Json(serde_json::json!({ "badge": badge, "node_id": nid })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn keypair() -> (SigningKey, String) {
        let seed: [u8; 32] = *b"muster-node-test-seed-0123456789";
        let sk = SigningKey::from_bytes(&seed);
        let pk_hex: String = sk.verifying_key().as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        (sk, pk_hex)
    }

    /// 设备码:8 个不易混淆的字符,中间一个连字符——念出来不会错。
    #[test]
    fn user_code_is_readable_and_unique_enough() {
        let a = new_user_code();
        let b = new_user_code();
        assert_eq!(a.len(), 9);
        assert_eq!(&a[4..5], "-");
        assert!(a.chars().filter(|c| *c != '-').all(|c| "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(c)));
        assert!(!a.contains(['0', 'O', '1', 'I']));
        assert_ne!(a, b);
    }

    /// 签名回路:正确的密钥签正确的消息才过;换消息、换节点、坏签名都不过。
    #[test]
    fn challenge_signature_roundtrip() {
        let (sk, pk_hex) = keypair();
        let vk = parse_pubkey(&pk_hex).unwrap();
        let node = Uuid::new_v4();
        let nonce = random_hex(32);
        let msg = challenge_message(node, &nonce);
        let sig: String = sk.sign(&msg).to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert!(verify_signature(vk.as_bytes(), &msg, &sig));
        assert!(!verify_signature(vk.as_bytes(), &challenge_message(Uuid::new_v4(), &nonce), &sig), "换节点 id 就不过");
        assert!(!verify_signature(vk.as_bytes(), &challenge_message(node, "deadbeef"), &sig), "换 nonce 就不过");
        assert!(!verify_signature(vk.as_bytes(), &msg, "zz"), "坏签名");
        assert!(parse_pubkey("abcd").is_err(), "短公钥");
        assert!(parse_pubkey(&"g".repeat(64)).is_err(), "非十六进制");
    }

    /// challenge 一次性且会过期。
    #[test]
    fn challenge_is_single_use_and_expires() {
        let ch = Challenges::new();
        let node = Uuid::new_v4();
        let n = ch.issue(node, 1_000);
        assert_eq!(ch.take(node, 2_000).as_deref(), Some(n.as_str()));
        assert!(ch.take(node, 2_000).is_none(), "用过即作废");
        let n2 = ch.issue(node, 10_000);
        assert!(ch.take(node, 10_000 + CHALLENGE_TTL_MS + 1).is_none(), "过期:{n2}");
    }

    #[test]
    fn online_is_decided_by_heartbeat_age() {
        assert!(is_online(Some(1_000_000), 1_000_000 + HEARTBEAT_TIMEOUT_MS));
        assert!(!is_online(Some(1_000_000), 1_000_000 + HEARTBEAT_TIMEOUT_MS + 1));
        assert!(!is_online(None, 1));
    }
}
