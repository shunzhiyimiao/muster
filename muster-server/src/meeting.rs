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
//!
//! ## 字幕、翻译、总结(阶段 2,对标 Teams)
//!
//! - **字幕**就是纪要行的另一种显示:同一条推送,会议室页叠在画面上。没有第二条转写链路。
//! - **翻译**由会议 Agent 做(模型调用必须在能跑 `muster_route` 的进程里,服务端不代劳):
//!   每个参会者给自己选字幕语言([`set_caption_lang`]),Agent 读 [`caption_langs`] 决定翻成
//!   哪些语言,翻译行以 `lang` + `origin_id` 落回 [`add_transcript`]。没人要的语言不翻。
//! - **总结**同样由 Agent 生成后落到 [`add_summary`];正文在库里,链上只有哈希。
//! - **录制**见 `recording.rs`。

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
    /// 是否正在录制。晚进来的人也必须看见"正在录制"——被录而不知情是会议产品最不能犯的错。
    #[serde(default)]
    pub recording_active: bool,
}

/// 列表查询共用的列:录制状态由 meeting_recording 派生,不另存一份会漂的字段。
const MEETING_COLS: &str = "id, channel_id, title, level, room, started_ms, ended_ms, wants_agent,
    EXISTS(SELECT 1 FROM meeting_recording r WHERE r.meeting_id = meeting.id
           AND r.status IN ('starting','active','stopping')) AS recording_active";

#[derive(Deserialize)]
pub struct NewMeeting {
    pub title: String,
}

#[derive(Deserialize)]
pub struct NewTranscript {
    pub speaker_id: String,
    pub text: String,
    /// 这行的语言;缺省按原文语言 `zh`。
    #[serde(default)]
    pub lang: Option<String>,
    /// 翻译行指回原文行;原文行为空。
    #[serde(default)]
    pub origin_id: Option<i64>,
}

/// 原文语言:Agent 的转写提示语言。字幕"跟原文"时就看这个语言的行。
pub const SOURCE_LANG: &str = "zh";
/// 散会后还收纪要的宽限(毫秒):在途的转写与翻译要落地。
pub const TRANSCRIPT_GRACE_MS: i64 = 90_000;

#[derive(Deserialize)]
pub struct CaptionPref {
    /// 字幕语言代码(`en` / `ja` …);`source` 或 `off` = 不要翻译,看原文。
    pub lang: String,
}

#[derive(Serialize)]
pub struct CaptionLangs {
    pub source: &'static str,
    /// 在场的人要的翻译语言(去重,不含原文语言)。Agent 只翻这些。
    pub langs: Vec<String>,
}

#[derive(Deserialize)]
pub struct NewSummary {
    /// `recap`(散会回顾)/ `interim`(会中小结)。
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct SummaryOut {
    pub id: i64,
    pub meeting_id: Uuid,
    pub kind: String,
    pub text: String,
    pub author_id: String,
    pub model: Option<String>,
    pub created_ms: i64,
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
pub(crate) struct MeetingRow {
    pub channel_id: String,
    pub title: String,
    pub level: String,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub wants_agent: bool,
    pub room: String,
}

pub(crate) async fn load(db: &Db, mid: Uuid) -> Result<MeetingRow> {
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
        recording_active: false,
    }))
}

pub async fn list(
    State((db, _hub, _audit)): State<MeetingState>,
    id: Identity,
    Path(cid): Path<String>,
) -> Result<Json<Vec<MeetingOut>>> {
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;
    let rows = sqlx::query_as::<_, MeetingOut>(&format!(
        "SELECT {MEETING_COLS} FROM meeting WHERE channel_id = $1 ORDER BY started_ms DESC LIMIT 50"
    ))
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
        id: 0,
        lang: SOURCE_LANG.into(),
        origin_id: None,
    });
    push_meeting(&hub, &row.channel_id, mid, "level", &row.title, level_str(want), row.wants_agent);
    Ok(Json(serde_json::json!({ "level": level_str(want) })))
}

/// 落一行转写。
///
/// **本接口只收文本**——音频到文本那一步必须在调用方经 `muster_route` 完成,
/// 服务端不代劳,也就无从绕过密级路由。转写正文属正文存储侧:可导出、
/// 可按保留期删除,审计链里只有它的哈希(与桌面壳 state.db 同性质)。
/// 会议结束后还收 [`TRANSCRIPT_GRACE_MS`]:最后一句往往是结论,而转写(whisper 要几秒)
/// 与翻译(再一次模型调用)都在途;过了宽限才拒——记录不能在散会后无限期地长。
/// 宽限期内落的行在链上排在 `meeting.end` 之后,如实。
pub async fn add_transcript(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(t): Json<NewTranscript>,
) -> Result<Json<serde_json::Value>> {
    let row = load(&db, mid).await?;
    if let Some(ended) = row.ended_ms {
        if now_ms() - ended > TRANSCRIPT_GRACE_MS {
            return Err(ServerError::BadRequest("会议已结束,不再接收纪要".into()));
        }
    }
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id.clone())).await?;
    if t.text.trim().is_empty() {
        return Err(ServerError::BadRequest("纪要行不能为空".into()));
    }
    let lang = normalize_lang(t.lang.as_deref().unwrap_or(SOURCE_LANG))
        .ok_or_else(|| ServerError::BadRequest("语言代码不合法".into()))?;
    // 翻译行必须指回**同一场会**的原文行,否则一条翻译能把别的会的话"搬"进来
    if let Some(origin) = t.origin_id {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM meeting_transcript WHERE id = $1 AND meeting_id = $2 AND origin_id IS NULL",
        )
        .bind(origin)
        .bind(mid)
        .fetch_one(&db.pool)
        .await?;
        if n == 0 {
            return Err(ServerError::BadRequest("origin_id 不是这场会的原文行".into()));
        }
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
                lang: lang.clone(),
            },
        ))
        .await?;

    let ts = now_ms();
    let (line_id,): (i64,) = sqlx::query_as(
        "INSERT INTO meeting_transcript(meeting_id, speaker_id, text, ts_ms, lang, origin_id)
         VALUES($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(mid)
    .bind(&t.speaker_id)
    .bind(&t.text)
    .bind(ts)
    .bind(&lang)
    .bind(t.origin_id)
    .fetch_one(&db.pool)
    .await?;

    hub.push(&row.channel_id, &Push::Transcript {
        meeting_id: mid.to_string(),
        speaker_id: t.speaker_id,
        text: t.text,
        ts_ms: ts,
        id: line_id,
        lang,
        origin_id: t.origin_id,
    });
    Ok(Json(serde_json::json!({ "ok": true, "id": line_id, "ts_ms": ts })))
}

/// 语言代码只认 `[a-z]{2,3}(-[A-Za-z0-9]{2,8})?`:它会进 SQL、进推送、进审计,
/// 不能让任意字符串混进来。
pub fn normalize_lang(s: &str) -> Option<String> {
    let s = s.trim();
    let mut parts = s.splitn(2, '-');
    let main = parts.next()?;
    if !(2..=3).contains(&main.len()) || !main.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    match parts.next() {
        None => Some(main.to_string()),
        Some(sub) if (2..=8).contains(&sub.len()) && sub.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Some(format!("{main}-{sub}"))
        }
        Some(_) => None,
    }
}

#[derive(Serialize, sqlx::FromRow)]
pub struct TranscriptLine {
    pub id: i64,
    pub speaker_id: String,
    pub text: String,
    pub ts_ms: i64,
    pub lang: String,
    pub origin_id: Option<i64>,
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
        "SELECT id, speaker_id, text, ts_ms, lang, origin_id FROM meeting_transcript
         WHERE meeting_id = $1 ORDER BY ts_ms ASC, id ASC",
    )
    .bind(mid)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

/// 给自己选字幕语言。`source` / `off` = 看原文,不要翻译。
/// 推一条 `meeting` 事件(`captions`),Agent 据此调整翻译目标集合。
pub async fn set_caption_lang(
    State((db, hub, _audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(p): Json<CaptionPref>,
) -> Result<Json<CaptionLangs>> {
    let row = load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id.clone())).await?;
    let want = p.lang.trim();
    if want == "source" || want == "off" || want == SOURCE_LANG {
        sqlx::query("DELETE FROM meeting_caption_pref WHERE meeting_id = $1 AND account_id = $2")
            .bind(mid)
            .bind(&id.account_id)
            .execute(&db.pool)
            .await?;
    } else {
        let lang = normalize_lang(want).ok_or_else(|| ServerError::BadRequest("语言代码不合法".into()))?;
        sqlx::query(
            "INSERT INTO meeting_caption_pref(meeting_id, account_id, lang, updated_ms) VALUES($1,$2,$3,$4)
             ON CONFLICT (meeting_id, account_id) DO UPDATE SET lang = EXCLUDED.lang, updated_ms = EXCLUDED.updated_ms",
        )
        .bind(mid)
        .bind(&id.account_id)
        .bind(&lang)
        .bind(now_ms())
        .execute(&db.pool)
        .await?;
    }
    let langs = caption_lang_set(&db, mid).await?;
    push_meeting(&hub, &row.channel_id, mid, "captions", &row.title, &row.level, row.wants_agent);
    Ok(Json(CaptionLangs { source: SOURCE_LANG, langs }))
}

/// 在场的人要的翻译语言。Agent 每句话翻之前问一次(有缓存),没人要就不翻。
pub async fn caption_langs(
    State((db, _hub, _audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<CaptionLangs>> {
    let row = load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id)).await?;
    Ok(Json(CaptionLangs { source: SOURCE_LANG, langs: caption_lang_set(&db, mid).await? }))
}

async fn caption_lang_set(db: &Db, mid: Uuid) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT lang FROM meeting_caption_pref WHERE meeting_id = $1 AND lang <> $2 ORDER BY lang",
    )
    .bind(mid)
    .bind(SOURCE_LANG)
    .fetch_all(&db.pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// 落一份总结(散会回顾 / 会中小结)。生成在 Agent 那边,服务端只收文本——
/// 与纪要同一条边界。散会后也收:回顾本来就是散会后才有的。
pub async fn add_summary(
    State((db, hub, audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(s): Json<NewSummary>,
) -> Result<Json<SummaryOut>> {
    let row = load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id.clone())).await?;
    let kind = match s.kind.as_str() {
        "recap" | "interim" => s.kind.clone(),
        other => return Err(ServerError::BadRequest(format!("未知总结类型 {other}"))),
    };
    let text = s.text.trim().to_string();
    if text.is_empty() {
        return Err(ServerError::BadRequest("总结不能为空".into()));
    }

    audit
        .append(envelope(
            &id,
            &row.channel_id,
            mid,
            parse_level(&row.level)?,
            EventBody::MeetingSummary {
                meeting_id: mid.to_string(),
                kind: kind.clone(),
                text_hash: ContentHash::sha256(text.as_bytes()),
            },
        ))
        .await?;

    let created = now_ms();
    let (sid,): (i64,) = sqlx::query_as(
        "INSERT INTO meeting_summary(meeting_id, kind, text, author_id, model, created_ms)
         VALUES($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(mid)
    .bind(&kind)
    .bind(&text)
    .bind(&id.account_id)
    .bind(&s.model)
    .bind(created)
    .fetch_one(&db.pool)
    .await?;

    push_meeting(&hub, &row.channel_id, mid, "summary", &row.title, &row.level, row.wants_agent);
    Ok(Json(SummaryOut {
        id: sid,
        meeting_id: mid,
        kind,
        text,
        author_id: id.account_id,
        model: s.model,
        created_ms: created,
    }))
}

pub async fn summaries(
    State((db, _hub, _audit)): State<MeetingState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<Vec<SummaryOut>>> {
    let row = load(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(row.channel_id)).await?;
    let rows = sqlx::query_as::<_, SummaryOut>(
        "SELECT id, meeting_id, kind, text, author_id, model, created_ms FROM meeting_summary
         WHERE meeting_id = $1 ORDER BY created_ms ASC, id ASC",
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
    let rows = sqlx::query_as::<_, MeetingOut>(&format!(
        "SELECT {MEETING_COLS} FROM meeting WHERE wants_agent AND ended_ms IS NULL ORDER BY started_ms"
    ))
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

    // 散会即停录:录着的会结束了,文件要收口。停不了只告警,散会本身不受影响。
    crate::recording::auto_stop(&db, &hub, &audit, mid, &row.channel_id, &row.level, &id).await;

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

    /// 语言代码进 SQL、进推送、进审计,只认规范形状。
    #[test]
    fn lang_codes_are_validated() {
        assert_eq!(normalize_lang("en").as_deref(), Some("en"));
        assert_eq!(normalize_lang(" zh-Hant ").as_deref(), Some("zh-Hant"));
        assert_eq!(normalize_lang("yue").as_deref(), Some("yue"));
        for bad in ["", "E", "english", "en_US", "en-", "zh-漢", "en;drop"] {
            assert!(normalize_lang(bad).is_none(), "{bad:?} 不该通过");
        }
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
