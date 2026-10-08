//! 会议录制:自托管的 LiveKit Egress 把房间合成一路音视频写到本地文件,
//! 这里管**谁能录、谁在录、录完的文件是什么、谁能看**。
//!
//! ## 边界
//!
//! - **restricted 的会录不了**:入会票上 `roomRecord=false`(见 `livekit.rs`),这里再拦一次。
//!   录像是长期留存、极易被搬走的正文,高密级会议连"能不能录"都不该是与会者的选择。
//! - **录制必须人人可见**:开始 / 停止都推 [`Push::Recording`],会议列表里也带 `recording_active`,
//!   晚进来的人同样看见。被录而不知情是会议产品最不能犯的错。
//! - **文件不经服务端生成**:Egress 直接写到录制目录(与服务端共享的卷),服务端只记元数据、
//!   算哈希、按 Range 回放。审计链上是文件哈希,不是文件。
//!
//! ## 为什么轮询 Egress 而不是接它的 webhook
//!
//! webhook 要 LiveKit 配一个回调地址、服务端多一个无鉴权入口再做签名校验;
//! 而一场录制的收口只发生一次,停止后每 2 秒问一次 `ListEgress`,最多两分钟,
//! 便宜且没有新攻击面。接 webhook 留作后续。
//!
//! ## Twirp,JSON 编码
//!
//! LiveKit 服务端的 Egress 接口是 Twirp(`/twirp/livekit.Egress/<方法>`),接受 JSON。
//! 不引入 `livekit-api` crate(它拖进 protobuf 代码生成那一整套),用 reqwest 直接打。
//! 响应里的字段名 protojson 默认是小驼峰、int64 是字符串——解析时两种形状都认。

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use muster_audit::{ContentHash, EventBody};
use muster_identity::{can, Action, OrgProhibitions, Scope as IdScope};
use muster_route::Sensitivity;

use crate::audit::Audit;
use crate::auth::Identity;
use crate::db::now_ms;
use crate::livekit::LiveKitConfig;
use crate::meeting::{envelope, parse_level, MeetingState};
use crate::org::{directory, require};
use crate::ws::{Hub, Push};
use crate::{Db, Result, ServerError};

// ---------------------------------------------------------------- Egress 客户端

/// LiveKit Egress 的最小客户端:开始房间合成录制、停止、查状态。
pub struct EgressClient {
    http: reqwest::Client,
    /// LiveKit 服务端的 HTTP 地址(`http://host:7880`)。由 `LIVEKIT_HTTP_URL` 指定,
    /// 缺省从 `LIVEKIT_URL` 的 ws/wss 换成 http/https。
    base: String,
    cfg: LiveKitConfig,
}

/// Egress 回来的状态(只取我们关心的字段)。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EgressInfo {
    pub egress_id: String,
    /// `EGRESS_STARTING` / `EGRESS_ACTIVE` / `EGRESS_ENDING` / `EGRESS_COMPLETE` /
    /// `EGRESS_FAILED` / `EGRESS_ABORTED` / `EGRESS_LIMIT_REACHED`
    pub status: String,
    pub error: String,
    pub files: Vec<EgressFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EgressFile {
    pub filename: String,
    pub duration_ms: u64,
    pub size: u64,
}

impl EgressInfo {
    pub fn finished(&self) -> bool {
        matches!(
            self.status.as_str(),
            "EGRESS_COMPLETE" | "EGRESS_FAILED" | "EGRESS_ABORTED" | "EGRESS_LIMIT_REACHED"
        )
    }
}

/// 把 Twirp 的 JSON 解析成 [`EgressInfo`]:字段名小驼峰 / 蛇形都认,int64 字符串 / 数字都认。
pub fn parse_egress_info(v: &serde_json::Value) -> Option<EgressInfo> {
    let get = |o: &serde_json::Value, camel: &str, snake: &str| -> serde_json::Value {
        o.get(camel).or_else(|| o.get(snake)).cloned().unwrap_or(serde_json::Value::Null)
    };
    let num = |x: &serde_json::Value| -> u64 {
        match x {
            serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
            serde_json::Value::String(s) => s.parse::<u64>().unwrap_or(0),
            _ => 0,
        }
    };
    let egress_id = get(v, "egressId", "egress_id").as_str()?.to_string();
    let status = get(v, "status", "status").as_str().unwrap_or("").to_string();
    let error = get(v, "error", "error").as_str().unwrap_or("").to_string();
    let files = get(v, "fileResults", "file_results")
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| EgressFile {
                    filename: get(f, "filename", "filename").as_str().unwrap_or("").to_string(),
                    // protobuf 里 duration 是纳秒
                    duration_ms: num(&get(f, "duration", "duration")) / 1_000_000,
                    size: num(&get(f, "size", "size")),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(EgressInfo { egress_id, status, error, files })
}

/// `ws://host:7880` → `http://host:7880`;`wss://` → `https://`。
pub fn http_url_from_ws(ws: &str) -> String {
    if let Some(rest) = ws.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = ws.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        ws.to_string()
    }
}

impl EgressClient {
    pub fn from_env() -> std::result::Result<Self, String> {
        let cfg = LiveKitConfig::from_env()?;
        let base = std::env::var("LIVEKIT_HTTP_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| http_url_from_ws(&cfg.url));
        Ok(Self { http: reqwest::Client::new(), base: base.trim_end_matches('/').to_string(), cfg })
    }

    /// 调 Egress 接口用的令牌:`roomRecord` 授权,60 秒有效。它不是给人的入会票。
    fn token(&self, room: &str) -> Result<String> {
        use jsonwebtoken::{encode, EncodingKey, Header};
        let now = now_ms() / 1000;
        let claims = serde_json::json!({
            "iss": self.cfg.api_key,
            "sub": "muster-server",
            "nbf": now - 10,
            "exp": now + 60,
            "video": { "roomRecord": true, "room": room, "roomAdmin": false }
        });
        encode(&Header::default(), &claims, &EncodingKey::from_secret(self.cfg.api_secret.as_bytes()))
            .map_err(|e| ServerError::Internal(format!("Egress 令牌签发失败:{e}")))
    }

    async fn call(&self, method: &str, room: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("{}/twirp/livekit.Egress/{method}", self.base);
        let r = self
            .http
            .post(&url)
            .bearer_auth(self.token(room)?)
            .json(&body)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| ServerError::Internal(format!("连不上 LiveKit Egress 接口({url}):{e}")))?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ServerError::Internal(format!("Egress {method} 失败:HTTP {status} {text}")));
        }
        serde_json::from_str(&text)
            .map_err(|e| ServerError::Internal(format!("Egress {method} 响应无法解析:{e}")))
    }

    /// 开始录整个房间(宫格布局);`audio_only` 只录声音(语音室省掉 Chrome 合成画面的开销)。
    pub async fn start_room_composite(&self, room: &str, audio_only: bool, filepath: &str) -> Result<EgressInfo> {
        let body = serde_json::json!({
            "room_name": room,
            "layout": "grid",
            "audio_only": audio_only,
            // disable_manifest:不要旁边那个 .json 清单,录制目录里只放录像本身
            "file_outputs": [{ "file_type": if audio_only { "OGG" } else { "MP4" }, "filepath": filepath, "disable_manifest": true }],
        });
        let v = self.call("StartRoomCompositeEgress", room, body).await?;
        parse_egress_info(&v).ok_or_else(|| ServerError::Internal(format!("Egress 返回里没有 egress_id:{v}")))
    }

    pub async fn stop(&self, room: &str, egress_id: &str) -> Result<EgressInfo> {
        let v = self.call("StopEgress", room, serde_json::json!({ "egress_id": egress_id })).await?;
        parse_egress_info(&v).ok_or_else(|| ServerError::Internal(format!("Egress 返回里没有 egress_id:{v}")))
    }

    pub async fn get(&self, room: &str, egress_id: &str) -> Result<Option<EgressInfo>> {
        let v = self.call("ListEgress", room, serde_json::json!({ "egress_id": egress_id })).await?;
        Ok(v.get("items").and_then(|a| a.as_array()).and_then(|a| a.iter().find_map(parse_egress_info)))
    }
}

// ---------------------------------------------------------------- 录制目录

/// Egress 写在**它的容器里**的路径(如 `/out/<会议>/<录制>.mp4`),服务端从宿主机的
/// 挂载点读。两个前缀由环境给:`MUSTER_RECORDINGS_MOUNT`(默认 `/out`)和
/// `MUSTER_RECORDINGS_DIR`(默认 `./deploy/recordings`)。
pub struct RecordingStore {
    pub host_dir: PathBuf,
    pub mount: String,
}

impl RecordingStore {
    pub fn from_env() -> Self {
        Self {
            host_dir: PathBuf::from(
                std::env::var("MUSTER_RECORDINGS_DIR").unwrap_or_else(|_| "./deploy/recordings".into()),
            ),
            mount: std::env::var("MUSTER_RECORDINGS_MOUNT").unwrap_or_else(|_| "/out".into()),
        }
    }

    /// 给 Egress 的文件路径(容器内)。
    pub fn container_path(&self, meeting: Uuid, recording: Uuid, audio_only: bool) -> String {
        let ext = if audio_only { "ogg" } else { "mp4" };
        format!("{}/{meeting}/{recording}.{ext}", self.mount.trim_end_matches('/'))
    }

    /// 容器路径 → 宿主机路径。只认挂载点之下、不含 `..` 的路径——这个值来自数据库,
    /// 库被改过也不能把服务端引到录制目录之外去读文件。
    pub fn host_path(&self, container_path: &str) -> Option<PathBuf> {
        let rel = container_path.strip_prefix(&self.mount)?.trim_start_matches('/');
        if rel.is_empty() || rel.split('/').any(|seg| seg == ".." || seg.is_empty()) {
            return None;
        }
        Some(self.host_dir.join(rel))
    }
}

// ---------------------------------------------------------------- 接口

#[derive(Deserialize, Default)]
pub struct StartRecording {
    /// 只录声音。语音室用它,省掉 Chrome 合成画面的开销。
    #[serde(default)]
    pub audio_only: bool,
}

#[derive(Serialize, sqlx::FromRow, Clone)]
pub struct RecordingOut {
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub started_by: String,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    /// starting / active / stopping / ready / failed
    pub status: String,
    pub audio_only: bool,
    pub size_bytes: Option<i64>,
    pub duration_ms: Option<i64>,
    pub content_hash: Option<String>,
    pub error: Option<String>,
}

const REC_COLS: &str =
    "id, meeting_id, started_by, started_ms, ended_ms, status, audio_only, size_bytes, duration_ms, content_hash, error";

#[derive(sqlx::FromRow)]
struct RecRow {
    id: Uuid,
    meeting_id: Uuid,
    egress_id: Option<String>,
    status: String,
    audio_only: bool,
    filepath: Option<String>,
}

/// 能不能录 = 会议密级 < restricted **且** 权限内核允许(与入会票上 `can_record` 同一条规则)。
async fn record_allowed(db: &Db, id: &Identity, cid: &str, level: Sensitivity) -> Result<()> {
    if level >= Sensitivity::Restricted {
        return Err(ServerError::Forbidden("高密级会议禁止录制".into()));
    }
    let p = id.principal(db).await?;
    let dir = directory(db).await?;
    let target = IdScope::Channel(cid.to_string());
    if !can(&p, &Action::ViewAudit, &target, &OrgProhibitions::default(), &dir).allowed() {
        return Err(ServerError::Forbidden("你没有录制这场会的权限".into()));
    }
    Ok(())
}

fn push_rec(hub: &Hub, cid: &str, mid: Uuid, rid: Uuid, state: &str, by: &str) {
    hub.push(
        cid,
        &Push::Recording {
            meeting_id: mid.to_string(),
            channel_id: cid.to_string(),
            recording_id: rid.to_string(),
            state: state.to_string(),
            by: by.to_string(),
        },
    );
}

pub async fn start(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    body: Option<Json<StartRecording>>,
) -> Result<Json<RecordingOut>> {
    let Json(req) = body.unwrap_or_default();
    let m = crate::meeting::load(&db, mid).await?;
    if m.ended_ms.is_some() {
        return Err(ServerError::BadRequest("会议已结束".into()));
    }
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(m.channel_id.clone())).await?;
    let level = parse_level(&m.level)?;
    record_allowed(&db, &id, &m.channel_id, level).await?;

    let (active,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM meeting_recording WHERE meeting_id = $1 AND status IN ('starting','active','stopping')",
    )
    .bind(mid)
    .fetch_one(&db.pool)
    .await?;
    if active > 0 {
        return Err(ServerError::BadRequest("这场会已在录制中".into()));
    }

    let egress = EgressClient::from_env().map_err(|e| {
        ServerError::Internal(format!("录制服务未配置:{e}(需要 LIVEKIT_URL/LIVEKIT_HTTP_URL 与 Egress 容器)"))
    })?;
    let store = RecordingStore::from_env();
    let rid = Uuid::new_v4();
    let filepath = store.container_path(mid, rid, req.audio_only);
    let started = now_ms();

    // 先记账:谁按下了录制。Egress 没起来也留下"有人想录"这条
    audit
        .append(envelope(
            &id,
            &m.channel_id,
            mid,
            level,
            EventBody::MeetingRecordStart {
                meeting_id: mid.to_string(),
                recording_id: rid.to_string(),
                audio_only: req.audio_only,
            },
        ))
        .await?;
    sqlx::query(
        "INSERT INTO meeting_recording(id, meeting_id, started_by, started_ms, status, audio_only, filepath)
         VALUES($1,$2,$3,$4,'starting',$5,$6)",
    )
    .bind(rid)
    .bind(mid)
    .bind(&id.account_id)
    .bind(started)
    .bind(req.audio_only)
    .bind(&filepath)
    .execute(&db.pool)
    .await?;

    match egress.start_room_composite(&m.room, req.audio_only, &filepath).await {
        Ok(info) => {
            sqlx::query("UPDATE meeting_recording SET egress_id = $1, status = 'active' WHERE id = $2")
                .bind(&info.egress_id)
                .bind(rid)
                .execute(&db.pool)
                .await?;
            push_rec(&hub, &m.channel_id, mid, rid, "started", &id.account_id);
            Ok(Json(load_rec(&db, rid).await?))
        }
        Err(e) => {
            let why = e.to_string();
            sqlx::query("UPDATE meeting_recording SET status = 'failed', error = $1, ended_ms = $2 WHERE id = $3")
                .bind(&why)
                .bind(now_ms())
                .bind(rid)
                .execute(&db.pool)
                .await?;
            // 开始失败也要收口:链上不能只有 start 没有 stop
            let _ = audit
                .append(envelope(
                    &id,
                    &m.channel_id,
                    mid,
                    level,
                    EventBody::MeetingRecordStop {
                        meeting_id: mid.to_string(),
                        recording_id: rid.to_string(),
                        outcome: "failed:egress_start".into(),
                        duration_ms: 0,
                        size_bytes: 0,
                        content_hash: None,
                    },
                ))
                .await;
            push_rec(&hub, &m.channel_id, mid, rid, "failed", &id.account_id);
            Err(e)
        }
    }
}

pub async fn stop(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<RecordingOut>> {
    let m = crate::meeting::load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(m.channel_id.clone())).await?;
    let level = parse_level(&m.level)?;
    record_allowed(&db, &id, &m.channel_id, level).await?;

    let rec = sqlx::query_as::<_, RecRow>(
        "SELECT id, meeting_id, egress_id, status, audio_only, filepath
         FROM meeting_recording WHERE meeting_id = $1 AND status IN ('starting','active')
         ORDER BY started_ms DESC LIMIT 1",
    )
    .bind(mid)
    .fetch_optional(&db.pool)
    .await?
    .ok_or_else(|| ServerError::BadRequest("这场会没有在录".into()))?;

    begin_stop(&db, &hub, &audit, &m.channel_id, level, &id, rec).await?;
    Ok(Json(load_rec(&db, mid_latest(&db, mid).await?).await?))
}

async fn mid_latest(db: &Db, mid: Uuid) -> Result<Uuid> {
    let (rid,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM meeting_recording WHERE meeting_id = $1 ORDER BY started_ms DESC LIMIT 1",
    )
    .bind(mid)
    .fetch_one(&db.pool)
    .await?;
    Ok(rid)
}

/// 停一条录制:告诉 Egress 停,标 stopping,起收口任务。
async fn begin_stop(
    db: &Db,
    hub: &Hub,
    audit: &Audit,
    cid: &str,
    level: Sensitivity,
    by: &Identity,
    rec: RecRow,
) -> Result<()> {
    let egress = EgressClient::from_env().map_err(ServerError::Internal)?;
    let room = crate::meeting::load(db, rec.meeting_id).await?.room;
    if let Some(eid) = &rec.egress_id {
        if let Err(e) = egress.stop(&room, eid).await {
            // 停不下来也要收口:可能 Egress 已经自己结束了(房间空了),让收口任务去查
            tracing::warn!(recording = %rec.id, error = %e, "StopEgress 失败,交给收口任务核对");
        }
    }
    sqlx::query("UPDATE meeting_recording SET status = 'stopping' WHERE id = $1")
        .bind(rec.id)
        .execute(&db.pool)
        .await?;
    push_rec(hub, cid, rec.meeting_id, rec.id, "stopping", &by.account_id);

    let (db2, hub2, audit2, by2, cid2) = (db.clone(), hub.clone(), audit.clone(), by.clone(), cid.to_string());
    tokio::spawn(async move {
        finish(db2, hub2, audit2, Arc::new(egress), room, cid2, level, by2, rec).await;
    });
    Ok(())
}

/// 散会时停掉还在录的(由 `meeting::end` 调)。失败只告警,散会不受影响。
pub async fn auto_stop(db: &Db, hub: &Hub, audit: &Audit, mid: Uuid, cid: &str, level: &str, by: &Identity) {
    let Ok(level) = parse_level(level) else { return };
    let rows = match sqlx::query_as::<_, RecRow>(
        "SELECT id, meeting_id, egress_id, status, audio_only, filepath
         FROM meeting_recording WHERE meeting_id = $1 AND status IN ('starting','active')",
    )
    .bind(mid)
    .fetch_all(&db.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(meeting = %mid, error = %e, "查不到录制状态,散会时无法停录");
            return;
        }
    };
    for rec in rows {
        if let Err(e) = begin_stop(db, hub, audit, cid, level, by, rec).await {
            tracing::warn!(meeting = %mid, error = %e, "散会时停录失败");
        }
    }
}

/// 收口:等 Egress 结束,算文件哈希,落元数据,记账,广播。最多等两分钟。
#[allow(clippy::too_many_arguments)]
async fn finish(
    db: Db,
    hub: Hub,
    audit: Audit,
    egress: Arc<EgressClient>,
    room: String,
    cid: String,
    level: Sensitivity,
    by: Identity,
    rec: RecRow,
) {
    let store = RecordingStore::from_env();
    let mut outcome: Option<(String, Option<EgressFile>)> = None;
    for _ in 0..60 {
        let Some(eid) = rec.egress_id.as_deref() else {
            outcome = Some(("failed:no_egress".into(), None));
            break;
        };
        match egress.get(&room, eid).await {
            Ok(Some(info)) if info.finished() => {
                let ok = info.status == "EGRESS_COMPLETE";
                outcome = Some((
                    if ok { "ready".into() } else { format!("failed:{}", info.status.to_lowercase()) },
                    info.files.into_iter().next(),
                ));
                break;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(recording = %rec.id, error = %e, "查询 Egress 状态失败,稍后再试"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let (outcome, file) = outcome.unwrap_or_else(|| ("failed:timeout".into(), None));

    // 文件哈希:链上记的是文件本身,事后能验"这份录像有没有被改过"
    let (hash, size) = match (&outcome[..], rec.filepath.as_deref().and_then(|p| store.host_path(p))) {
        ("ready", Some(path)) => match hash_file(&path).await {
            Ok((h, n)) => (Some(h), n),
            Err(e) => {
                tracing::warn!(recording = %rec.id, path = %path.display(), error = %e, "录像文件读不到");
                (None, 0)
            }
        },
        _ => (None, 0),
    };
    let outcome = if outcome == "ready" && hash.is_none() { "failed:file_missing".to_string() } else { outcome };
    let duration_ms = file.as_ref().map(|f| f.duration_ms as i64).unwrap_or(0);
    let size = if size > 0 { size } else { file.as_ref().map(|f| f.size).unwrap_or(0) };
    let ended = now_ms();

    if let Err(e) = audit
        .append(envelope(
            &by,
            &cid,
            rec.meeting_id,
            level,
            EventBody::MeetingRecordStop {
                meeting_id: rec.meeting_id.to_string(),
                recording_id: rec.id.to_string(),
                outcome: outcome.clone(),
                duration_ms: duration_ms.max(0) as u64,
                size_bytes: size,
                content_hash: hash.as_ref().map(|h| ContentHash(h.clone())),
            },
        ))
        .await
    {
        tracing::error!(recording = %rec.id, error = %e, "录制收口没能记账");
    }
    let status = if outcome == "ready" { "ready" } else { "failed" };
    if let Err(e) = sqlx::query(
        "UPDATE meeting_recording SET status = $1, ended_ms = $2, size_bytes = $3, duration_ms = $4,
         content_hash = $5, error = $6 WHERE id = $7",
    )
    .bind(status)
    .bind(ended)
    .bind(size as i64)
    .bind(duration_ms)
    .bind(&hash)
    .bind(if status == "ready" { None } else { Some(outcome.clone()) })
    .bind(rec.id)
    .execute(&db.pool)
    .await
    {
        tracing::error!(recording = %rec.id, error = %e, "录制元数据没写进去");
    }
    push_rec(&hub, &cid, rec.meeting_id, rec.id, status, &by.account_id);
    tracing::info!(recording = %rec.id, %outcome, size, duration_ms, "录制已收口");
}

/// 流式算 SHA-256,返回 `sha256:<hex>` 与字节数。录像可能上 GB,不整个读进内存。
async fn hash_file(path: &FsPath) -> std::io::Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((format!("sha256:{:x}", hasher.finalize()), total))
}

async fn load_rec(db: &Db, rid: Uuid) -> Result<RecordingOut> {
    sqlx::query_as::<_, RecordingOut>(&format!("SELECT {REC_COLS} FROM meeting_recording WHERE id = $1"))
        .bind(rid)
        .fetch_optional(&db.pool)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("录制 {rid}")))
}

pub async fn list(
    State((db, _hub, _audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<Vec<RecordingOut>>> {
    let m = crate::meeting::load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(m.channel_id)).await?;
    let rows = sqlx::query_as::<_, RecordingOut>(&format!(
        "SELECT {REC_COLS} FROM meeting_recording WHERE meeting_id = $1 ORDER BY started_ms ASC"
    ))
    .bind(mid)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct FileQuery {
    /// `<video src>` 带不了 Authorization 头,令牌走查询参数——与 `/events` 同一取舍,
    /// 同样会进访问日志,上线前一并换成一次性 ticket。
    pub token: String,
}

/// 回放录像。只有 `ready` 的能看;支持单段 Range,否则 `<video>` 拖不了进度条。
pub async fn file(
    State((db, _hub, _audit)): State<MeetingState>,
    Path(rid): Path<Uuid>,
    Query(q): Query<FileQuery>,
    headers: HeaderMap,
) -> Result<Response> {
    let id = Identity::from_token(&q.token)?;
    let rec = sqlx::query_as::<_, RecRow>(
        "SELECT id, meeting_id, egress_id, status, audio_only, filepath
         FROM meeting_recording WHERE id = $1",
    )
    .bind(rid)
    .fetch_optional(&db.pool)
    .await?
    .ok_or_else(|| ServerError::NotFound(format!("录制 {rid}")))?;
    let m = crate::meeting::load(&db, rec.meeting_id).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(m.channel_id)).await?;
    if rec.status != "ready" {
        return Err(ServerError::BadRequest(format!("录像还不可用(状态 {})", rec.status)));
    }
    let store = RecordingStore::from_env();
    let path = rec
        .filepath
        .as_deref()
        .and_then(|p| store.host_path(p))
        .ok_or_else(|| ServerError::Internal("录像路径不在录制目录内".into()))?;
    let meta = tokio::fs::metadata(&path).await.map_err(|e| ServerError::Internal(format!("录像文件读不到:{e}")))?;
    let total = meta.len();
    let mime = if rec.audio_only { "audio/ogg" } else { "video/mp4" };

    // 单段 Range:bytes=start-end / bytes=start- / bytes=-suffix
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|r| parse_range(r, total));
    let (start, end) = range.unwrap_or((0, total.saturating_sub(1)));
    if total == 0 || start > end || end >= total {
        return Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{total}"))],
        )
            .into_response());
    }
    let len = end - start + 1;

    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = tokio::fs::File::open(&path).await.map_err(|e| ServerError::Internal(format!("打不开录像:{e}")))?;
    f.seek(std::io::SeekFrom::Start(start)).await.map_err(|e| ServerError::Internal(e.to_string()))?;
    let stream = tokio_util::io::ReaderStream::new(f.take(len));
    let body = axum::body::Body::from_stream(stream);

    let mut resp = Response::new(body);
    *resp.status_mut() = if range.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, mime.parse().unwrap());
    h.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    h.insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    h.insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    if range.is_some() {
        h.insert(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}").parse().unwrap());
    }
    Ok(resp)
}

/// 解析 `Range` 头的第一段。解析不了就当没给(整文件回),不报错——浏览器会重试。
pub fn parse_range(h: &str, total: u64) -> Option<(u64, u64)> {
    let spec = h.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    if total == 0 {
        return None;
    }
    match (a.trim(), b.trim()) {
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            if n == 0 {
                return None;
            }
            Some((total.saturating_sub(n), total - 1))
        }
        (start, "") => {
            let s: u64 = start.parse().ok()?;
            (s < total).then_some((s, total - 1))
        }
        (start, end) => {
            let s: u64 = start.parse().ok()?;
            let e: u64 = end.parse().ok()?;
            (s <= e && s < total).then_some((s, e.min(total - 1)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twirp 的 JSON 两种形状都要认:小驼峰(protojson 默认)与蛇形;int64 是字符串。
    #[test]
    fn egress_info_parses_both_json_shapes() {
        let camel = serde_json::json!({
            "egressId": "EG_1", "roomName": "r", "status": "EGRESS_COMPLETE",
            "fileResults": [{ "filename": "/out/m/r.mp4", "duration": "65000000000", "size": "12345" }]
        });
        let i = parse_egress_info(&camel).unwrap();
        assert_eq!(i.egress_id, "EG_1");
        assert!(i.finished());
        assert_eq!(i.files[0].duration_ms, 65_000);
        assert_eq!(i.files[0].size, 12_345);

        let snake = serde_json::json!({
            "egress_id": "EG_2", "status": "EGRESS_ACTIVE",
            "file_results": [{ "filename": "x.ogg", "duration": 1500000000u64, "size": 7 }]
        });
        let i = parse_egress_info(&snake).unwrap();
        assert_eq!((i.egress_id.as_str(), i.finished()), ("EG_2", false));
        assert_eq!(i.files[0].duration_ms, 1_500);
        assert!(parse_egress_info(&serde_json::json!({ "status": "x" })).is_none(), "没有 egress_id 不算");
    }

    /// 容器路径 → 宿主机路径:只认挂载点之下,`..` 一律拒——这个值来自数据库。
    #[test]
    fn host_path_stays_inside_the_recordings_dir() {
        let s = RecordingStore { host_dir: PathBuf::from("/srv/rec"), mount: "/out".into() };
        assert_eq!(s.host_path("/out/m1/r1.mp4"), Some(PathBuf::from("/srv/rec/m1/r1.mp4")));
        assert_eq!(s.host_path("/out/../etc/passwd"), None);
        assert_eq!(s.host_path("/out/m1/../../x"), None);
        assert_eq!(s.host_path("/elsewhere/x.mp4"), None);
        assert_eq!(s.host_path("/out/"), None);
        let m = Uuid::nil();
        assert_eq!(s.container_path(m, m, true), format!("/out/{m}/{m}.ogg"));
        assert!(s.container_path(m, m, false).ends_with(".mp4"));
    }

    #[test]
    fn ws_urls_become_http() {
        assert_eq!(http_url_from_ws("ws://10.0.0.2:7880"), "http://10.0.0.2:7880");
        assert_eq!(http_url_from_ws("wss://livekit.example.com"), "https://livekit.example.com");
        assert_eq!(http_url_from_ws("http://x"), "http://x");
    }

    #[test]
    fn range_header_is_parsed_defensively() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=0-5000", 1000), Some((0, 999)), "越界的末尾裁到文件末");
        assert_eq!(parse_range("bytes=1000-", 1000), None, "起点越界不可满足");
        assert_eq!(parse_range("bytes=5-2", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
    }
}
