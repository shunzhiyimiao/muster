//! 会议行动项:**提案,不是任务**。
//!
//! ## 为什么中间必须站一个人
//!
//! 会上一句话不能直接变成在别人机器上跑的代码改动:
//!
//! - **转写会出错**。实测把"幂等键"转成"蜜等键";一个转错的行动项直接开跑,
//!   跑出来的东西没人看得懂为什么。
//! - **会议发言是低保真输入**。人在会上说"这个回头改一下"跟在任务框里写下
//!   一条明确的需求,是两种不同强度的意图。
//! - **Runner 在开发者机器上**(架构文档边界一)。服务端的 Agent 本来也跑不了任务,
//!   它只能提议。
//!
//! 所以确认那一步不是流程负担,是**授权边界**:提案由 Agent 落,
//! 确认必须由**有 CreateTask 权限的人**做,且**确认这件事本身进审计链**。
//!
//! ## 服务端只记号,不执行
//!
//! 确认之后,任务由**持有那份代码的节点**去跑(与审批合入由持有 worktree 的
//! 节点执行同理)。服务端记下 `run_id`,不碰源码。

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use muster_audit::{Actor, ContentHash, EventBody, NewEvent, Scope as AuditScope};
use muster_identity::{Action, Scope as IdScope};

use crate::audit::Audit;
use crate::auth::Identity;
use crate::db::now_ms;
use crate::org::require;
use crate::ws::Hub;
use crate::{Db, Result, ServerError};

pub type ActionState = (Db, Hub, Audit);

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ActionItemOut {
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub text: String,
    pub owner_hint: Option<String>,
    pub source_quote: Option<String>,
    pub status: String,
    pub decided_by: Option<String>,
    pub run_id: Option<String>,
    pub created_ms: i64,
}

#[derive(Deserialize)]
pub struct NewActionItem {
    pub text: String,
    #[serde(default)]
    pub owner_hint: Option<String>,
    #[serde(default)]
    pub source_quote: Option<String>,
}

#[derive(Deserialize)]
pub struct Decision {
    /// true = 确认成任务,false = 驳回
    pub confirm: bool,
    #[serde(default)]
    pub note: Option<String>,
}

async fn meeting_channel(db: &Db, mid: Uuid) -> Result<String> {
    sqlx::query_as::<_, (String,)>("SELECT channel_id FROM meeting WHERE id = $1")
        .bind(mid)
        .fetch_optional(&db.pool)
        .await?
        .map(|r| r.0)
        .ok_or_else(|| ServerError::NotFound(format!("会议 {mid}")))
}

/// Agent 落一条提案。只需要"能在这个频道说话"的权限——**提议不是授权**。
pub async fn propose(
    State((db, hub, _a)): State<ActionState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(item): Json<NewActionItem>,
) -> Result<Json<ActionItemOut>> {
    let cid = meeting_channel(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;
    if item.text.trim().is_empty() {
        return Err(ServerError::BadRequest("行动项不能为空".into()));
    }

    let aid = Uuid::new_v4();
    let created = now_ms();
    sqlx::query(
        "INSERT INTO meeting_action_item(id, meeting_id, text, owner_hint, source_quote, created_ms)
         VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(aid)
    .bind(mid)
    .bind(item.text.trim())
    .bind(&item.owner_hint)
    .bind(&item.source_quote)
    .bind(created)
    .execute(&db.pool)
    .await?;

    let out = ActionItemOut {
        id: aid,
        meeting_id: mid,
        text: item.text.trim().to_string(),
        owner_hint: item.owner_hint,
        source_quote: item.source_quote,
        status: "proposed".into(),
        decided_by: None,
        run_id: None,
        created_ms: created,
    };
    // 广播:不推的话,会上说完那句话界面上什么都不会发生
    hub.push(&cid, &crate::ws::Push::ActionItem(out.clone()));
    Ok(Json(out))
}

pub async fn list(
    State((db, _hub, _a)): State<ActionState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<Vec<ActionItemOut>>> {
    let cid = meeting_channel(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(cid.clone())).await?;
    let rows = sqlx::query_as::<_, ActionItemOut>(
        "SELECT id, meeting_id, text, owner_hint, source_quote, status, decided_by, run_id, created_ms
         FROM meeting_action_item WHERE meeting_id = $1 ORDER BY created_ms",
    )
    .bind(mid)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

/// **确认或驳回。这是授权动作。**
///
/// - 需要 [`Action::CreateTask`]——把会上一句话变成会改代码的任务,
///   本就是"发起任务"这件事;
/// - **必须由人**:身份内核已保证 Agent 不能自批合入,这里同理——
///   让 Agent 自己确认自己提的行动项,等于中间那个人没有了;
/// - 确认与驳回**都进审计链**:驳回不是"什么都没发生"。
pub async fn decide(
    State((db, hub, audit)): State<ActionState>,
    id: Identity,
    Path(aid): Path<Uuid>,
    Json(d): Json<Decision>,
) -> Result<Json<ActionItemOut>> {
    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT meeting_id, text, status FROM meeting_action_item WHERE id = $1",
    )
    .bind(aid)
    .fetch_optional(&db.pool)
    .await?
    .ok_or_else(|| ServerError::NotFound(format!("行动项 {aid}")))?;
    let (mid, text, status) = row;

    // append-only 的姿态:已裁决的不再裁决(与 P5 审批同一条规矩)
    if status != "proposed" {
        return Err(ServerError::BadRequest(format!("该行动项已是 {status},不可重复裁决")));
    }

    let cid = meeting_channel(&db, mid).await?;
    require(&db, &id, &Action::CreateTask, &IdScope::Channel(cid.clone())).await?;

    // Agent 不得确认自己提的行动项——中间那个人就是为此存在的
    if id.kind != muster_identity::PrincipalKind::Human {
        return Err(ServerError::Forbidden(
            "行动项必须由人确认:让 Agent 自己确认自己提的事,中间那个人就没有了".into(),
        ));
    }

    let new_status = if d.confirm { "confirmed" } else { "rejected" };
    let now = now_ms();
    sqlx::query(
        "UPDATE meeting_action_item SET status=$1, decided_by=$2, decided_ms=$3 WHERE id=$4",
    )
    .bind(new_status)
    .bind(&id.account_id)
    .bind(now)
    .bind(aid)
    .execute(&db.pool)
    .await?;

    // 确认与驳回都留痕。用 approval.decision:语义就是"人对一项申请做出了裁决",
    // 正文(行动项文本与意见)只留哈希,与铁律三一致。
    audit
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::human(&id.account_id),
            scope: AuditScope { team: None, channel: Some(cid.clone()) },
            run_id: None,
            session_id: Some(format!("meeting:{mid}")),
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body: EventBody::ApprovalDecision {
                approval_id: format!("ACT-{aid}"),
                granted: d.confirm,
                note_hash: Some(ContentHash::sha256(
                    format!("{}|{}", text, d.note.as_deref().unwrap_or("")).as_bytes(),
                )),
            },
        })
        .await?;

    let out = ActionItemOut {
        id: aid,
        meeting_id: mid,
        text,
        owner_hint: None,
        source_quote: None,
        status: new_status.into(),
        decided_by: Some(id.account_id),
        run_id: None,
        created_ms: now,
    };
    // 裁决也要广播:同一场会里别人正看着这条待批项
    hub.push(&cid, &crate::ws::Push::ActionItem(out.clone()));
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct LinkRun {
    pub run_id: String,
}

/// 节点跑起来之后回填 run_id。**服务端只记号,不执行**——
/// 代码在开发者机器上,任务也在那里跑。
pub async fn link_run(
    State((db, _hub, _a)): State<ActionState>,
    id: Identity,
    Path(aid): Path<Uuid>,
    Json(l): Json<LinkRun>,
) -> Result<Json<serde_json::Value>> {
    let (mid, status): (Uuid, String) =
        sqlx::query_as("SELECT meeting_id, status FROM meeting_action_item WHERE id = $1")
            .bind(aid)
            .fetch_optional(&db.pool)
            .await?
            .ok_or_else(|| ServerError::NotFound(format!("行动项 {aid}")))?;
    // 只有确认过的才能开跑——否则"人确认"这道闸形同虚设
    if status != "confirmed" {
        return Err(ServerError::BadRequest(format!(
            "只有已确认的行动项才能关联运行,当前是 {status}"
        )));
    }
    let cid = meeting_channel(&db, mid).await?;
    require(&db, &id, &Action::CreateTask, &IdScope::Channel(cid)).await?;

    sqlx::query("UPDATE meeting_action_item SET run_id=$1, status='done' WHERE id=$2")
        .bind(&l.run_id)
        .bind(aid)
        .execute(&db.pool)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true, "run_id": l.run_id })))
}
