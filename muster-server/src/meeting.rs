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
//!
//! ## 会议进审计链(2026-10-08,会议线阶段 1)
//!
//! 建会 / 入会 / 离会 / 结束 / 每行纪要 / 密级抬升 / 请 Agent,全部落服务端自己的链
//! (`crate::audit`)。正文一律只留哈希:标题、纪要文本、抬升理由都可能是敏感内容,
//! 链里只能回答"有没有、有没有被改",不能回答"说了什么"(铁律 3)。
//!
//! ## 先记账,再动库
//!
//! 链与 PostgreSQL 是两份存储,没有跨库事务,总有一边先写。两种失败方式不对称:
//!
//! - 链上多一条、库里没有 = **一次失败的尝试**,可解释(与 `command.run` 记被拒的命令同理);
//! - 库里多一条、链上没有 = **一段没有记录的历史**,不可解释。
//!
//! 所以这里一律**先 append 审计,再写库**;审计写不进去,操作就失败(fail-closed)。
//! 纪要行是写入最密的一类(每几秒一行),也照此办——"正文有、链上无"恰恰最不能
//! 发生在它身上。

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use muster_audit::{Actor, ContentHash, EventBody, NewEvent, Scope as AuditScope};
use muster_identity::{can, Action, OrgProhibitions, PrincipalKind, Scope as IdScope};
use muster_route::Sensitivity;

use crate::audit::Audit;
use crate::auth::Identity;
use crate::db::now_ms;
use crate::livekit::{self, JoinCaps, LiveKitConfig};
use crate::org::{directory, require};
use crate::ws::{Hub, Push};
use crate::{Db, Result, ServerError};

pub type MeetingState = (Db, Hub, Audit);

/// 与 action.rs 同一个策略版本号:会议事件和行动项裁决属同一套服务端策略。
const POLICY_VERSION: &str = "policy-v1";

#[derive(Serialize, sqlx::FromRow)]
pub struct MeetingOut {
    pub id: Uuid,
    pub channel_id: String,
    pub title: String,
    pub level: String,
    pub room: String,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    /// 这场会是否请了 Agent。**只是意愿**——它到没到看参会者列表。
    #[serde(default)]
    pub wants_agent: bool,
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

pub fn parse_level(s: &str) -> Result<Sensitivity> {
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

/// 会议的一行基本信息。各 handler 先查它:不存在 → 404,再谈权限。
#[derive(sqlx::FromRow)]
struct MeetingRow {
    channel_id: String,
    title: String,
    level: String,
    started_ms: i64,
    ended_ms: Option<i64>,
    wants_agent: bool,
    room: String,
}

async fn load(db: &Db, mid: Uuid) -> Result<MeetingRow> {
    sqlx::query_as::<_, MeetingRow>(
        "SELECT channel_id, title, level, started_ms, ended_ms, wants_agent, room
         FROM meeting WHERE id = $1",
    )
    .bind(mid)
    .fetch_optional(&db.pool)
    .await?
    .ok_or_else(|| ServerError::NotFound(format!("会议 {mid}")))
}

/// 会议事件的审计信封:actor 是操作者(人 / Agent / 服务),scope 是频道,
/// session 是会议(`meeting:<id>`,与 action.rs 的裁决同一口径,一句 SQL 能把一场会串起来),
/// label 是**当时**的会议密级。
pub fn envelope(id: &Identity, cid: &str, mid: Uuid, level: Sensitivity, body: EventBody) -> NewEvent {
    let actor = match id.kind {
        PrincipalKind::Agent => Actor::agent(&id.account_id),
        PrincipalKind::Service => Actor::system(&id.account_id),
        _ => Actor::human(&id.account_id),
    };
    NewEvent {
        ts_ms: None,
        actor,
        scope: AuditScope { team: None, channel: Some(cid.to_string()) },
        run_id: None,
        session_id: Some(format!("meeting:{mid}")),
        policy_version: Some(POLICY_VERSION.into()),
        label: Some(level),
        locality: None,
        body,
    }
}

fn push_meeting(hub: &Hub, cid: &str, mid: Uuid, event: &str, title: &str, level: &str, wants_agent: bool) {
    hub.push(
        cid,
        &Push::Meeting {
            meeting_id: mid.to_string(),
            channel_id: cid.to_string(),
            event: event.to_string(),
            title: title.to_string(),
            level: level.to_string(),
            wants_agent,
        },
    );
}

pub async fn start(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(cid): Path<String>,
    Json(m): Json<NewMeeting>,
) -> Result<Json<MeetingOut>> {
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;
    let title = m.title.trim().to_string();
    if title.is_empty() {
        return Err(ServerError::BadRequest("会议要有个主题".into()));
    }
    // 会议起始密级 = 频道密级。会议不能比它所在的频道更开放,
    // 否则"把话题挪进会议"就成了绕过密级的办法。
    let level = crate::org::channel_level(&db, &cid).await?;
    let lv = parse_level(&level)?;
    let mid = Uuid::new_v4();
    let room = format!("muster-{mid}");
    let started = now_ms();

    audit
        .append(envelope(
            &id,
            &cid,
            mid,
            lv,
            EventBody::MeetingStart {
                meeting_id: mid.to_string(),
                channel_id: cid.clone(),
                title_hash: ContentHash::sha256(title.as_bytes()),
                level: lv,
            },
        ))
        .await?;

    sqlx::query(
        "INSERT INTO meeting(id, channel_id, title, level, started_ms, room) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(mid)
    .bind(&cid)
    .bind(&title)
    .bind(&level)
    .bind(started)
    .bind(&room)
    .execute(&db.pool)
    .await?;

    push_meeting(&hub, &cid, mid, "started", &title, &level, false);
    Ok(Json(MeetingOut {
        id: mid,
        channel_id: cid,
        title,
        level,
        room,
        started_ms: started,
        ended_ms: None,
        wants_agent: false,
    }))
}

pub async fn list(
    State((db, _hub, _audit)): State<MeetingState>,
    id: Identity,
    Path(cid): Path<String>,
) -> Result<Json<Vec<MeetingOut>>> {
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;
    let rows = sqlx::query_as::<_, MeetingOut>(
        "SELECT id, channel_id, title, level, room, started_ms, ended_ms, wants_agent
         FROM meeting WHERE channel_id = $1 ORDER BY started_ms DESC LIMIT 50",
    )
    .bind(&cid)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

/// 密级棘轮:**只升不降**。传一个更低的密级不是"设置",是错误。
pub async fn raise_level(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(r): Json<RaiseLevel>,
) -> Result<Json<serde_json::Value>> {
    let row = load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id.clone())).await?;

    let cur = parse_level(&row.level)?;
    let want = parse_level(&r.level)?;
    if want <= cur {
        return Err(ServerError::BadRequest(format!(
            "会议密级只升不降:当前 {},不能设为 {}",
            level_str(cur),
            level_str(want)
        )));
    }

    audit
        .append(envelope(
            &id,
            &row.channel_id,
            mid,
            want,
            EventBody::MeetingLevelRaise {
                meeting_id: mid.to_string(),
                from_level: cur,
                to_level: want,
                cause_hash: ContentHash::sha256(r.cause.as_bytes()),
            },
        ))
        .await?;

    sqlx::query("UPDATE meeting SET level = $1 WHERE id = $2")
        .bind(level_str(want))
        .bind(mid)
        .execute(&db.pool)
        .await?;

    hub.push(&row.channel_id, &Push::Transcript {
        meeting_id: mid.to_string(),
        speaker_id: "系统".into(),
        text: format!("会议密级已抬升至 {}({})", level_str(want), r.cause),
        ts_ms: now_ms(),
    });
    push_meeting(&hub, &row.channel_id, mid, "level", &row.title, level_str(want), row.wants_agent);
    Ok(Json(serde_json::json!({ "level": level_str(want) })))
}

/// 落一行转写。
///
/// **本接口只收文本**——音频到文本那一步必须在调用方经 `muster_route` 完成,
/// 服务端不代劳,也就无从绕过密级路由。转写正文属正文存储侧:可导出、
/// 可按保留期删除,审计链里只有它的哈希(与桌面壳 state.db 同性质)。
/// 会议结束后不再收:结束就是结束,记录不能在散会后继续长。
pub async fn add_transcript(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(t): Json<NewTranscript>,
) -> Result<Json<serde_json::Value>> {
    let row = load(&db, mid).await?;
    if row.ended_ms.is_some() {
        return Err(ServerError::BadRequest("会议已结束,不再接收纪要".into()));
    }
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id.clone())).await?;
    if t.text.trim().is_empty() {
        return Err(ServerError::BadRequest("纪要行不能为空".into()));
    }

    audit
        .append(envelope(
            &id,
            &row.channel_id,
            mid,
            parse_level(&row.level)?,
            EventBody::MeetingTranscript {
                meeting_id: mid.to_string(),
                speaker_id: t.speaker_id.clone(),
                text_hash: ContentHash::sha256(t.text.as_bytes()),
            },
        ))
        .await?;

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

    hub.push(&row.channel_id, &Push::Transcript {
        meeting_id: mid.to_string(),
        speaker_id: t.speaker_id,
        text: t.text,
        ts_ms: ts,
    });
    Ok(Json(serde_json::json!({ "ok": true, "ts_ms": ts })))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct TranscriptLine {
    pub speaker_id: String,
    pub text: String,
    pub ts_ms: i64,
}

/// 取会议纪要。**中途入会要靠它补上下文**——人和 Agent 都一样:
/// 晚到、重连、崩溃重启都是常态,而上下文只在内存里的话,
/// 重启一次就等于失忆:会上明明说过的事,它答"记录里没有"。
pub async fn transcript(
    State((db, _hub, _audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<Vec<TranscriptLine>>> {
    let row = load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id)).await?;
    let rows = sqlx::query_as::<_, TranscriptLine>(
        "SELECT speaker_id, text, ts_ms FROM meeting_transcript
         WHERE meeting_id = $1 ORDER BY ts_ms ASC, id ASC",
    )
    .bind(mid)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct WantAgent {
    pub want: bool,
}

/// 请 Agent 来这场会 / 请它离开。
///
/// **只记意愿,不起进程。** 认领由服务器上常驻的 agent-daemon 完成——
/// 让桌面壳自己 spawn 一个的话,两个人开会就有两个 Agent 各转各的。
/// 意愿变化会推 `meeting` 事件,daemon 订着 SSE 立刻认领;轮询只是它的兜底。
pub async fn set_wants_agent(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(w): Json<WantAgent>,
) -> Result<Json<serde_json::Value>> {
    let row = load(&db, mid).await?;
    if row.ended_ms.is_some() {
        return Err(ServerError::BadRequest("会议已结束".into()));
    }
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id.clone())).await?;

    audit
        .append(envelope(
            &id,
            &row.channel_id,
            mid,
            parse_level(&row.level)?,
            EventBody::MeetingAgent { meeting_id: mid.to_string(), wanted: w.want },
        ))
        .await?;

    sqlx::query("UPDATE meeting SET wants_agent = $1 WHERE id = $2 AND ended_ms IS NULL")
        .bind(w.want)
        .bind(mid)
        .execute(&db.pool)
        .await?;

    push_meeting(&hub, &row.channel_id, mid, "agent", &row.title, &row.level, w.want);
    Ok(Json(serde_json::json!({ "wants_agent": w.want })))
}

/// agent-daemon 用:哪些**进行中**的会议请了 Agent。
pub async fn agent_wanted(
    State((db, _hub, _audit)): State<MeetingState>,
    _id: Identity,
) -> Result<Json<Vec<MeetingOut>>> {
    let rows = sqlx::query_as::<_, MeetingOut>(
        "SELECT id, channel_id, title, level, room, started_ms, ended_ms, wants_agent
         FROM meeting WHERE wants_agent AND ended_ms IS NULL ORDER BY started_ms",
    )
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

/// 结束会议。幂等:已结束的再结束一次只回 `already: true`,不再记账、不再广播。
/// 结束时把所有还没离开的参会记录补上 left_ms——散会了,人就不在会里了。
pub async fn end(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let row = load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id.clone())).await?;
    if row.ended_ms.is_some() {
        return Ok(Json(serde_json::json!({ "ok": true, "already": true })));
    }

    let (lines,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM meeting_transcript WHERE meeting_id = $1")
            .bind(mid)
            .fetch_one(&db.pool)
            .await?;
    let ended = now_ms();

    audit
        .append(envelope(
            &id,
            &row.channel_id,
            mid,
            parse_level(&row.level)?,
            EventBody::MeetingEnd {
                meeting_id: mid.to_string(),
                duration_ms: (ended - row.started_ms).max(0) as u64,
                transcript_lines: lines.max(0) as u64,
            },
        ))
        .await?;

    let changed = sqlx::query("UPDATE meeting SET ended_ms = $1 WHERE id = $2 AND ended_ms IS NULL")
        .bind(ended)
        .bind(mid)
        .execute(&db.pool)
        .await?
        .rows_affected();
    if changed == 0 {
        // 和另一个结束请求撞上了:对方已经记过账、广播过,这边照幂等处理
        return Ok(Json(serde_json::json!({ "ok": true, "already": true })));
    }
    sqlx::query("UPDATE meeting_participant SET left_ms = $1 WHERE meeting_id = $2 AND left_ms IS NULL")
        .bind(ended)
        .bind(mid)
        .execute(&db.pool)
        .await?;

    push_meeting(&hub, &row.channel_id, mid, "ended", &row.title, &row.level, row.wants_agent);
    Ok(Json(serde_json::json!({ "ok": true, "ended_ms": ended, "room": row.room })))
}

#[derive(Serialize)]
pub struct JoinInfo {
    /// LiveKit 服务地址(客户端直连,媒体不经 muster-server)
    pub url: String,
    pub token: String,
    pub room: String,
    pub level: String,
    /// 能不能开麦 —— 由权限内核判定,前端照着显示
    pub can_publish: bool,
    /// 能不能录 —— 密级 + 权限共同判定;restricted 一律 false
    pub can_record: bool,
}

/// 入会:换一张 LiveKit 票。票上写了能不能开麦、能不能录,两者都不是前端决定的。
pub async fn join(
    State((db, _hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<JoinInfo>> {
    let row = load(&db, mid).await?;
    if row.ended_ms.is_some() {
        return Err(ServerError::BadRequest("会议已结束".into()));
    }
    let cid = row.channel_id.clone();

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
    let lv = parse_level(&row.level)?;
    let record = lv < Sensitivity::Restricted
        && can(&p, &Action::ViewAudit, &target, &OrgProhibitions::default(), &dir).allowed();

    let cfg = LiveKitConfig::from_env().map_err(ServerError::Internal)?;
    let token = livekit::mint(
        &cfg,
        &row.room,
        &id.account_id,
        &id.display_name,
        JoinCaps { publish, record },
        3600,
    )?;

    // 票签好了才记账:记的是"发出去的票说了什么"
    audit
        .append(envelope(
            &id,
            &cid,
            mid,
            lv,
            EventBody::MeetingJoin { meeting_id: mid.to_string(), can_publish: publish, can_record: record },
        ))
        .await?;

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
        room: row.room,
        level: row.level,
        can_publish: publish,
        can_record: record,
    }))
}

/// 离会(自己退出,不结束会议)。把自己所有还开着的参会记录补上 left_ms。
/// 没有开着的记录(没入过会、或已被结束会议时统一补过)就什么也不记——
/// 没有进过,就谈不上离开。
pub async fn leave(
    State((db, _hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let row = load(&db, mid).await?;
    let (open,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM meeting_participant
         WHERE meeting_id = $1 AND account_id = $2 AND left_ms IS NULL",
    )
    .bind(mid)
    .bind(&id.account_id)
    .fetch_one(&db.pool)
    .await?;
    if open == 0 {
        return Ok(Json(serde_json::json!({ "ok": true, "was_in": false })));
    }

    audit
        .append(envelope(
            &id,
            &row.channel_id,
            mid,
            parse_level(&row.level)?,
            EventBody::MeetingLeave { meeting_id: mid.to_string() },
        ))
        .await?;

    sqlx::query(
        "UPDATE meeting_participant SET left_ms = $1
         WHERE meeting_id = $2 AND account_id = $3 AND left_ms IS NULL",
    )
    .bind(now_ms())
    .bind(mid)
    .bind(&id.account_id)
    .execute(&db.pool)
    .await?;
    Ok(Json(serde_json::json!({ "ok": true, "was_in": true })))
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

    /// 会议事件的信封:会议即 session、频道即 scope、密级即 label;
    /// 标题这类正文**不进 payload**,只有哈希。
    #[test]
    fn meeting_envelope_carries_meeting_as_session_and_no_plaintext() {
        let id = Identity {
            account_id: "alice".into(),
            display_name: "Alice".into(),
            kind: PrincipalKind::Human,
        };
        let mid = Uuid::new_v4();
        let title = "季度预算(机密)";
        let ev = envelope(
            &id,
            "finance",
            mid,
            Sensitivity::Restricted,
            EventBody::MeetingStart {
                meeting_id: mid.to_string(),
                channel_id: "finance".into(),
                title_hash: ContentHash::sha256(title.as_bytes()),
                level: Sensitivity::Restricted,
            },
        );
        assert_eq!(ev.session_id.as_deref(), Some(format!("meeting:{mid}").as_str()));
        assert_eq!(ev.scope.channel.as_deref(), Some("finance"));
        assert_eq!(ev.label, Some(Sensitivity::Restricted));
        assert_eq!(ev.actor, Actor::human("alice"));
        assert_eq!(ev.body.event_type(), "meeting.start");
        let payload = serde_json::to_string(&ev.body).unwrap();
        assert!(!payload.contains(title), "标题正文不得进审计 payload:{payload}");
        assert!(payload.contains("sha256:"), "只留哈希");

        // Agent 身份 → Agent actor(工牌号即 actor id)
        let agent = Identity { account_id: "A-007".into(), display_name: "小七".into(), kind: PrincipalKind::Agent };
        let ev = envelope(&agent, "c", mid, Sensitivity::Open, EventBody::MeetingLeave { meeting_id: mid.to_string() });
        assert_eq!(ev.actor, Actor::agent("A-007"));
    }
}
