//! 会议(P3 之上)。**媒体面不在这里**——音视频交给自托管的 LiveKit,
//! 本模块只管 Muster 关心的那一半:谁在场、密级、转写、产出了哪些任务。
//!
//! ## 为什么不自建 SFU
//!
//! 回声消除、抖动缓冲、带宽自适应、NAT 穿透是一门专业手艺,做完也只是追平
//! 一个已被解决透的问题。LiveKit 是 Apache-2.0 且可自托管,**部署在内网就
//! 不破坏主权前提**。这块是集成,不是设计。
//!
//! ## 转写必须走本地 provider(这条不许"先凑合")
//!
//! 会议音频是全系统密级最高的数据流。转写就是一次模型调用,若走云端 STT:
//! - 铁律 2「绝不静默升云」当场破;
//! - 铁律 4 的外发记账破——**演习报告会说「零外发」,而整场会议音频都出去了**。
//!
//! 所以 STT 必须注册成 [`muster_provider::Locality::Local`] 的 provider,经
//! `muster_route` 决策后调用。做法上并不更麻烦:whisper.cpp 起一个 OpenAI
//! 兼容端点,把 base_url 指到本机即可。演习模式下云端 STT 会被 fail-closed
//! 直接拒掉——现有骨架免费给的,不用新造治理。
//!
//! ## 会议密级只升不降
//!
//! 会中有人共享了 restricted 资源 ⇒ 整场会议棘轮抬升(与 E3 同一语义),
//! 此后这场会的转写与派生任务都锁在本地。[`raise_level`] 没有降级对应物,
//! 和 `SessionRatchet` 一样是**类型上不给降**。

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use muster_identity::{can, Action, OrgProhibitions, Scope as IdScope};
use muster_route::Sensitivity;

use crate::auth::Identity;
use crate::db::now_ms;
use crate::livekit::{self, JoinCaps, LiveKitConfig};
use crate::org::{directory, require};
use crate::ws::{Hub, Push};
use crate::{Db, Result, ServerError};

#[derive(Serialize, sqlx::FromRow)]
pub struct MeetingOut {
    pub id: Uuid,
    pub channel_id: String,
    pub title: String,
    pub level: String,
    pub room: String,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
}

#[derive(Deserialize)]
pub struct NewMeeting {
    pub title: String,
}

#[derive(Deserialize)]
pub struct NewTranscript {
    pub speaker_id: String,
    pub text: String,
}

#[derive(Deserialize)]
pub struct RaiseLevel {
    /// 目标密级;低于当前值会被拒绝(只升不降)。
    pub level: String,
    /// 为什么要升(共享了哪个资源)——进理由,不进正文。
    pub cause: String,
}

fn parse_level(s: &str) -> Result<Sensitivity> {
    match s {
        "open" => Ok(Sensitivity::Open),
        "internal" => Ok(Sensitivity::Internal),
        "restricted" => Ok(Sensitivity::Restricted),
        other => Err(ServerError::BadRequest(format!("未知密级 {other}"))),
    }
}

fn level_str(s: Sensitivity) -> &'static str {
    match s {
        Sensitivity::Open => "open",
        Sensitivity::Internal => "internal",
        Sensitivity::Restricted => "restricted",
    }
}

pub async fn start(
    State((db, _hub)): State<(Db, Hub)>,
    id: Identity,
    Path(cid): Path<String>,
    Json(m): Json<NewMeeting>,
) -> Result<Json<MeetingOut>> {
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;
    // 会议起始密级 = 频道密级。会议不能比它所在的频道更开放,
    // 否则"把话题挪进会议"就成了绕过密级的办法。
    let level = crate::org::channel_level(&db, &cid).await?;
    let mid = Uuid::new_v4();
    let room = format!("muster-{mid}");
    let started = now_ms();
    sqlx::query(
        "INSERT INTO meeting(id, channel_id, title, level, started_ms, room) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(mid)
    .bind(&cid)
    .bind(&m.title)
    .bind(&level)
    .bind(started)
    .bind(&room)
    .execute(&db.pool)
    .await?;

    Ok(Json(MeetingOut {
        id: mid,
        channel_id: cid,
        title: m.title,
        level,
        room,
        started_ms: started,
        ended_ms: None,
    }))
}

pub async fn list(
    State((db, _hub)): State<(Db, Hub)>,
    id: Identity,
    Path(cid): Path<String>,
) -> Result<Json<Vec<MeetingOut>>> {
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;
    let rows = sqlx::query_as::<_, MeetingOut>(
        "SELECT id, channel_id, title, level, room, started_ms, ended_ms
         FROM meeting WHERE channel_id = $1 ORDER BY started_ms DESC LIMIT 50",
    )
    .bind(&cid)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

/// 密级棘轮:**只升不降**。传一个更低的密级不是"设置",是错误。
pub async fn raise_level(
    State((db, hub)): State<(Db, Hub)>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(r): Json<RaiseLevel>,
) -> Result<Json<serde_json::Value>> {
    let (cid, cur): (String, String) =
        sqlx::query_as("SELECT channel_id, level FROM meeting WHERE id = $1")
            .bind(mid)
            .fetch_optional(&db.pool)
            .await?
            .ok_or_else(|| ServerError::NotFound(format!("会议 {mid}")))?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;

    let cur = parse_level(&cur)?;
    let want = parse_level(&r.level)?;
    if want <= cur {
        return Err(ServerError::BadRequest(format!(
            "会议密级只升不降:当前 {},不能设为 {}",
            level_str(cur),
            level_str(want)
        )));
    }
    sqlx::query("UPDATE meeting SET level = $1 WHERE id = $2")
        .bind(level_str(want))
        .bind(mid)
        .execute(&db.pool)
        .await?;

    hub.push(&cid, &Push::Transcript {
        meeting_id: mid.to_string(),
        speaker_id: "系统".into(),
        text: format!("会议密级已抬升至 {}({})", level_str(want), r.cause),
        ts_ms: now_ms(),
    });
    Ok(Json(serde_json::json!({ "level": level_str(want) })))
}

/// 落一行转写。
///
/// **本接口只收文本**——音频到文本那一步必须在调用方经 `muster_route` 完成,
/// 服务端不代劳,也就无从绕过密级路由。转写正文属正文存储侧:可导出、
/// 可按保留期删除,审计链里只有它的哈希(与桌面壳 state.db 同性质)。
pub async fn add_transcript(
    State((db, hub)): State<(Db, Hub)>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(t): Json<NewTranscript>,
) -> Result<Json<serde_json::Value>> {
    let (cid,): (String,) = sqlx::query_as("SELECT channel_id FROM meeting WHERE id = $1")
        .bind(mid)
        .fetch_optional(&db.pool)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("会议 {mid}")))?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;

    let ts = now_ms();
    sqlx::query(
        "INSERT INTO meeting_transcript(meeting_id, speaker_id, text, ts_ms) VALUES($1,$2,$3,$4)",
    )
    .bind(mid)
    .bind(&t.speaker_id)
    .bind(&t.text)
    .bind(ts)
    .execute(&db.pool)
    .await?;

    hub.push(&cid, &Push::Transcript {
        meeting_id: mid.to_string(),
        speaker_id: t.speaker_id,
        text: t.text,
        ts_ms: ts,
    });
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct TranscriptLine {
    pub speaker_id: String,
    pub text: String,
    pub ts_ms: i64,
}

/// 取会议纪要。**Agent 中途入会要靠它补上下文**——
/// 晚到、重连、崩溃重启都是常态,而上下文只存在进程内存里的话,
/// 重启一次就等于失忆:会上明明说过的事,它答"记录里没有"。
pub async fn transcript(
    State((db, _hub)): State<(Db, Hub)>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<Vec<TranscriptLine>>> {
    let (cid,): (String,) = sqlx::query_as("SELECT channel_id FROM meeting WHERE id = $1")
        .bind(mid)
        .fetch_optional(&db.pool)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("会议 {mid}")))?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid)).await?;
    let rows = sqlx::query_as::<_, TranscriptLine>(
        "SELECT speaker_id, text, ts_ms FROM meeting_transcript
         WHERE meeting_id = $1 ORDER BY ts_ms ASC",
    )
    .bind(mid)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

pub async fn end(
    State((db, _hub)): State<(Db, Hub)>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let (cid,): (String,) = sqlx::query_as("SELECT channel_id FROM meeting WHERE id = $1")
        .bind(mid)
        .fetch_optional(&db.pool)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("会议 {mid}")))?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid)).await?;
    sqlx::query("UPDATE meeting SET ended_ms = $1 WHERE id = $2 AND ended_ms IS NULL")
        .bind(now_ms())
        .bind(mid)
        .execute(&db.pool)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct JoinInfo {
    /// LiveKit 服务地址(客户端直连,媒体不经 muster-server)
    pub url: String,
    pub token: String,
    pub room: String,
    pub level: String,
    /// 前端据此决定要不要显示"开麦"按钮,而不是点了才被 LiveKit 拒
    pub can_publish: bool,
    pub can_record: bool,
}

/// 拿入会令牌。**能不能开麦由权限内核决定,不是前端说了算。**
pub async fn join(
    State((db, _hub)): State<(Db, Hub)>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<JoinInfo>> {
    let (cid, level, room, ended): (String, String, String, Option<i64>) =
        sqlx::query_as("SELECT channel_id, level, room, ended_ms FROM meeting WHERE id = $1")
            .bind(mid)
            .fetch_optional(&db.pool)
            .await?
            .ok_or_else(|| ServerError::NotFound(format!("会议 {mid}")))?;
    if ended.is_some() {
        return Err(ServerError::BadRequest("会议已结束".into()));
    }

    // 准入:先判能不能进(在签令牌之前——签了再拒等于已经把钥匙给出去了)
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;

    // 能不能开麦 = 能不能在这个频道发言。写死成 true 等于绕过权限内核。
    let p = id.principal(&db).await?;
    let dir = directory(&db).await?;
    let target = IdScope::Channel(cid.clone());
    let publish =
        can(&p, &Action::SendMessage, &target, &OrgProhibitions::default(), &dir).allowed();
    // 高密级会议一律禁录:录像是长期留存、极易被搬走的正文,
    // 这不该是与会者的选择
    let record = parse_level(&level)? < Sensitivity::Restricted
        && can(&p, &Action::ViewAudit, &target, &OrgProhibitions::default(), &dir).allowed();

    let cfg = LiveKitConfig::from_env().map_err(ServerError::Internal)?;
    let token = livekit::mint(
        &cfg,
        &room,
        &id.account_id,
        &id.display_name,
        JoinCaps { publish, record },
        3600,
    )?;

    sqlx::query(
        "INSERT INTO meeting_participant(meeting_id, account_id, joined_ms) VALUES($1,$2,$3)
         ON CONFLICT DO NOTHING",
    )
    .bind(mid)
    .bind(&id.account_id)
    .bind(now_ms())
    .execute(&db.pool)
    .await?;

    Ok(Json(JoinInfo {
        url: cfg.url,
        token,
        room,
        level,
        can_publish: publish,
        can_record: record,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 密级只升不降——服务端这一侧的把关,和 E3 棘轮同一语义。
    #[test]
    fn level_ordering_allows_only_raises() {
        let open = parse_level("open").unwrap();
        let internal = parse_level("internal").unwrap();
        let restricted = parse_level("restricted").unwrap();
        assert!(internal > open && restricted > internal, "密级比较即楼层");
        assert!(parse_level("绝密").is_err(), "未知密级必须拒绝,不得兜底");
        assert_eq!(level_str(restricted), "restricted");
    }
}
