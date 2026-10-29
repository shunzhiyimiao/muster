//! 会议行动项 = **派发单**:提案,不是任务。
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
//! ## 确认权归被派到的本人(块 3,docs/节点与工牌-公用私有值班.md 闸 1、2、6)
//!
//! 值班 Agent 提炼出"李负责把重试逻辑定下来",这条**派给李**;只有李能接受或驳回,
//! 主持人、管理员都不能替他接受——派活是请求,不是命令。`owner_hint`(会上听到的名字)
//! 永远只作提示,服务端把它解析成 `assignee`(解析规则保守,拿不准就空着留给主持人
//! 指派);解析错了的后果是"错的人收到一条可以驳回的提案",不是"错的人的电脑开始跑"。
//! 驳回必须带理由;72 小时没人处理 ⇒ 过期回收并提醒主持人,**不自动转派**。
//! 指派、接受 / 驳回、过期都进审计链;正文只留哈希。
//!
//! 状态机:proposed → accepted(self) → done;→ rejected;→ expired。
//! `accepted(private)`(交给私有 Agent)是设计里的第 3 步,**这里不实现**。
//!
//! ## 服务端只记号,不执行
//!
//! 接受之后,任务由**持有那份代码的节点**去跑(与审批合入由持有 worktree 的
//! 节点执行同理)。服务端记下 `run_id`,不碰源码。

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use muster_audit::{Actor, ContentHash, EventBody, NewEvent, Scope as AuditScope};
use muster_identity::{can, Action, OrgProhibitions, PrincipalKind, Scope as IdScope};

use crate::audit::Audit;
use crate::auth::Identity;
use crate::db::now_ms;
use crate::org::require;
use crate::ws::Hub;
use crate::{Db, Result, ServerError};

pub type ActionState = (Db, Hub, Audit);

/// 提出后多久没人处理算过期。
pub const EXPIRE_AFTER_MS: i64 = 72 * 3600 * 1000;

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ActionItemOut {
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub text: String,
    /// 会上听到的负责人名字。**只是提示**,不是授权依据。
    pub owner_hint: Option<String>,
    pub source_quote: Option<String>,
    /// proposed | accepted | rejected | expired | done(老行可能是 confirmed)
    pub status: String,
    pub decided_by: Option<String>,
    pub run_id: Option<String>,
    pub created_ms: i64,
    /// 被派到的人(账号 id)。空 = 还没指派,等主持人。
    pub assignee: Option<String>,
    /// self = 本人自己做;private 留给第 3 步
    pub accepted_mode: Option<String>,
    /// 来源的密级(会议密级)
    pub sensitivity: Option<String>,
    pub source_kind: String,
    pub reject_reason: Option<String>,
    pub expires_ms: Option<i64>,
}

const COLS: &str = "id, meeting_id, text, owner_hint, source_quote, status, decided_by, run_id, created_ms,
    assignee, accepted_mode, sensitivity, source_kind, reject_reason, expires_ms";

#[derive(Deserialize)]
pub struct NewActionItem {
    pub text: String,
    #[serde(default)]
    pub owner_hint: Option<String>,
    #[serde(default)]
    pub source_quote: Option<String>,
    /// 提案方已经知道是谁就直接给;否则由服务端从 owner_hint 解析
    #[serde(default)]
    pub assignee: Option<String>,
}

#[derive(Deserialize)]
pub struct Decision {
    /// true = 接受(自己做),false = 驳回
    pub confirm: bool,
    /// 驳回时必填:理由。接受时是备注
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize)]
pub struct AssignReq {
    pub assignee: String,
}

#[derive(sqlx::FromRow)]
struct MeetingInfo {
    channel_id: String,
    level: String,
    host: Option<String>,
    title: String,
}

async fn meeting_info(db: &Db, mid: Uuid) -> Result<MeetingInfo> {
    sqlx::query_as::<_, MeetingInfo>("SELECT channel_id, level, host, title FROM meeting WHERE id = $1")
        .bind(mid)
        .fetch_optional(&db.pool)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("会议 {mid}")))
}

async fn load_item(db: &Db, aid: Uuid) -> Result<ActionItemOut> {
    sqlx::query_as::<_, ActionItemOut>(&format!("SELECT {COLS} FROM meeting_action_item WHERE id = $1"))
        .bind(aid)
        .fetch_optional(&db.pool)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("行动项 {aid}")))
}

/// 把会上听到的名字解析成账号。**保守**:候选是未停用的人类账号;命中 = 账号 id 不分大小写相等、
/// 显示名相等、或显示名与提示互为前缀(「李」→「李四」,「Bob Li」→「Bob」);恰好一个候选才算,
/// 多个或没有就空着留给主持人指派——解析错的代价已经被闸 1 压到"一次驳回",但也别主动制造。
pub fn pick_assignee(hint: &str, candidates: &[(String, String)]) -> Option<String> {
    let h = hint.trim();
    if h.chars().count() < 1 {
        return None;
    }
    let hl = h.to_lowercase();
    let exact: Vec<&String> = candidates
        .iter()
        .filter(|(id, name)| id.to_lowercase() == hl || name == h)
        .map(|(id, _)| id)
        .collect();
    if exact.len() == 1 {
        return Some(exact[0].clone());
    }
    if exact.len() > 1 {
        return None;
    }
    let fuzzy: Vec<&String> = candidates
        .iter()
        .filter(|(_, name)| {
            let n = name.to_lowercase();
            !n.is_empty() && (n.starts_with(&hl) || hl.starts_with(&n))
        })
        .map(|(id, _)| id)
        .collect();
    if fuzzy.len() == 1 { Some(fuzzy[0].clone()) } else { None }
}

async fn human_accounts(db: &Db) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT id, display_name FROM account WHERE kind = 'human' AND disabled = FALSE",
    )
    .fetch_all(&db.pool)
    .await?)
}

/// 派发单的候选负责人 = 能在这个频道里说话的真人。
///
/// 解析到看不见频道的人没有意义:他的收件箱会露出频道内容(密级跟着任务走,不能跨可见范围),
/// 而他点裁决又会因为没有频道权限被拒。判定复用 `can()`,不另写一份。组织不大,逐人判定够用。
async fn channel_humans(db: &Db, channel_id: &str) -> Result<Vec<(String, String)>> {
    let dir = crate::org::directory(db).await?;
    let scope = IdScope::Channel(channel_id.to_string());
    let mut out = Vec::new();
    for (aid, name) in human_accounts(db).await? {
        let who = Identity { account_id: aid.clone(), display_name: name.clone(), kind: PrincipalKind::Human };
        let p = who.principal(db).await?;
        if can(&p, &Action::SendMessage, &scope, &OrgProhibitions::default(), &dir).allowed() {
            out.push((aid, name));
        }
    }
    Ok(out)
}

/// Agent 落一条提案。只需要"能在这个频道说话"的权限——**提议不是授权**。
pub async fn propose(
    State((db, hub, _a)): State<ActionState>,
    id: Identity,
    Path(mid): Path<Uuid>,
    Json(item): Json<NewActionItem>,
) -> Result<Json<ActionItemOut>> {
    let m = meeting_info(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(m.channel_id.clone())).await?;
    if item.text.trim().is_empty() {
        return Err(ServerError::BadRequest("行动项不能为空".into()));
    }
    // 负责人:提案方直接给的要是这个频道里的真人;否则从会上听到的名字解析。拿不准就空着,留给人指派
    let candidates = channel_humans(&db, &m.channel_id).await?;
    let assignee = match item.assignee.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => candidates.iter().find(|(id, _)| id == a).map(|(id, _)| id.clone()),
        None => match item.owner_hint.as_deref() {
            Some(h) if !h.trim().is_empty() => pick_assignee(h, &candidates),
            _ => None,
        },
    };

    let aid = Uuid::new_v4();
    let created = now_ms();
    sqlx::query(
        "INSERT INTO meeting_action_item(id, meeting_id, text, owner_hint, source_quote, created_ms,
                                         assignee, sensitivity, source_kind, expires_ms)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,'meeting',$9)",
    )
    .bind(aid)
    .bind(mid)
    .bind(item.text.trim())
    .bind(&item.owner_hint)
    .bind(&item.source_quote)
    .bind(created)
    .bind(&assignee)
    .bind(&m.level)
    .bind(created + EXPIRE_AFTER_MS)
    .execute(&db.pool)
    .await?;

    let out = load_item(&db, aid).await?;
    // 广播:不推的话,会上说完那句话界面上什么都不会发生
    hub.push(&m.channel_id, &crate::ws::Push::ActionItem(out.clone()));
    Ok(Json(out))
}

pub async fn list(
    State((db, _hub, _a)): State<ActionState>,
    id: Identity,
    Path(mid): Path<Uuid>,
) -> Result<Json<Vec<ActionItemOut>>> {
    let m = meeting_info(&db, mid).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(m.channel_id)).await?;
    let rows = sqlx::query_as::<_, ActionItemOut>(&format!(
        "SELECT {COLS} FROM meeting_action_item WHERE meeting_id = $1 ORDER BY created_ms"
    ))
    .bind(mid)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

/// 指派(或改派)负责人。主持人或组织管理员才行;值班 Agent 不行。指派本身进审计链。
pub async fn assign(
    State((db, hub, audit)): State<ActionState>,
    id: Identity,
    Path(aid): Path<Uuid>,
    Json(req): Json<AssignReq>,
) -> Result<Json<ActionItemOut>> {
    let it = load_item(&db, aid).await?;
    if it.status != "proposed" {
        return Err(ServerError::BadRequest(format!("该行动项已是 {},不能再指派", it.status)));
    }
    let m = meeting_info(&db, it.meeting_id).await?;
    if id.kind != PrincipalKind::Human {
        return Err(ServerError::Forbidden("指派必须由人做".into()));
    }
    let is_host = m.host.as_deref() == Some(id.account_id.as_str());
    if !is_host {
        require(&db, &id, &Action::ManageMembers, &IdScope::Org)
            .await
            .map_err(|_| ServerError::Forbidden("只有会议主持人或组织管理员能指派".into()))?;
    }
    let assignee = req.assignee.trim().to_string();
    if !channel_humans(&db, &m.channel_id).await?.iter().any(|(a, _)| *a == assignee) {
        return Err(ServerError::BadRequest(format!("{assignee} 不是能在这个频道里行动的人员账号")));
    }

    audit
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::human(&id.account_id),
            scope: AuditScope { team: None, channel: Some(m.channel_id.clone()) },
            run_id: None,
            session_id: Some(format!("meeting:{}", it.meeting_id)),
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body: EventBody::DispatchAssign {
                item_id: format!("ACT-{aid}"),
                assignee: assignee.clone(),
                assigned_by: Actor::human(&id.account_id),
            },
        })
        .await?;
    sqlx::query("UPDATE meeting_action_item SET assignee = $1, assigned_by = $2, assigned_ms = $3 WHERE id = $4")
        .bind(&assignee)
        .bind(&id.account_id)
        .bind(now_ms())
        .bind(aid)
        .execute(&db.pool)
        .await?;
    let out = load_item(&db, aid).await?;
    hub.push(&m.channel_id, &crate::ws::Push::ActionItem(out.clone()));
    Ok(Json(out))
}

/// **接受或驳回。这是授权动作,只有被派到的本人能做。**
///
/// - **必须由人**:让 Agent 自己确认自己提的行动项,中间那个人就没有了;
/// - **只认 assignee 本人**:主持人、管理员都不能替他接受——派活是请求,不是命令;
/// - 驳回必须带理由;接受与驳回**都进审计链**:驳回不是"什么都没发生"。
pub async fn decide(
    State((db, hub, audit)): State<ActionState>,
    id: Identity,
    Path(aid): Path<Uuid>,
    Json(d): Json<Decision>,
) -> Result<Json<ActionItemOut>> {
    let it = load_item(&db, aid).await?;
    // append-only 的姿态:已裁决的不再裁决(与 P5 审批同一条规矩)
    if it.status != "proposed" {
        return Err(ServerError::BadRequest(format!("该行动项已是 {},不可重复裁决", it.status)));
    }
    let m = meeting_info(&db, it.meeting_id).await?;
    require(&db, &id, &Action::SendMessage, &IdScope::Channel(m.channel_id.clone())).await?;
    if id.kind != PrincipalKind::Human {
        return Err(ServerError::Forbidden(
            "行动项必须由人确认:让 Agent 自己确认自己提的事,中间那个人就没有了".into(),
        ));
    }
    let Some(assignee) = it.assignee.as_deref() else {
        return Err(ServerError::BadRequest("这条还没指派给任何人,先由主持人指派".into()));
    };
    if assignee != id.account_id {
        return Err(ServerError::Forbidden(format!(
            "只有被派到的本人({assignee})能接受或驳回——派活是请求,不是命令"
        )));
    }
    let note = d.note.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if !d.confirm && note.is_none() {
        return Err(ServerError::BadRequest("驳回要写理由:它会回到提出者的视野,没理由就没法改".into()));
    }

    let new_status = if d.confirm { "accepted" } else { "rejected" };
    // 先记账再动库:确认与驳回都留痕。用 approval.decision:语义就是"人对一项申请做出了裁决",
    // 正文(行动项文本与意见)只留哈希,与铁律三一致。
    audit
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::human(&id.account_id),
            scope: AuditScope { team: None, channel: Some(m.channel_id.clone()) },
            run_id: None,
            session_id: Some(format!("meeting:{}", it.meeting_id)),
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body: EventBody::ApprovalDecision {
                approval_id: format!("ACT-{aid}"),
                granted: d.confirm,
                note_hash: Some(ContentHash::sha256(format!("{}|{}", it.text, note.unwrap_or("")).as_bytes())),
            },
        })
        .await?;
    sqlx::query(
        "UPDATE meeting_action_item SET status = $1, decided_by = $2, decided_ms = $3,
                accepted_mode = $4, reject_reason = $5 WHERE id = $6",
    )
    .bind(new_status)
    .bind(&id.account_id)
    .bind(now_ms())
    .bind(if d.confirm { Some("self") } else { None })
    .bind(if d.confirm { None } else { note })
    .bind(aid)
    .execute(&db.pool)
    .await?;

    let out = load_item(&db, aid).await?;
    // 裁决也要广播:同一场会里别人正看着这条待批项
    hub.push(&m.channel_id, &crate::ws::Push::ActionItem(out.clone()));
    Ok(Json(out))
}

/// 收件箱里的一条:派发单 + 它来自哪场会。
#[derive(Serialize, sqlx::FromRow)]
pub struct InboxItem {
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub meeting_title: String,
    pub channel_id: String,
    pub text: String,
    pub owner_hint: Option<String>,
    pub source_quote: Option<String>,
    pub sensitivity: Option<String>,
    pub created_ms: i64,
    pub expires_ms: Option<i64>,
}

/// 本人的待处理派发单。只有本人能看自己的收件箱——它就是"派到我这儿的"。
pub async fn inbox(State((db, _hub, _a)): State<ActionState>, id: Identity) -> Result<Json<Vec<InboxItem>>> {
    let rows = sqlx::query_as::<_, InboxItem>(
        "SELECT a.id, a.meeting_id, m.title AS meeting_title, m.channel_id, a.text, a.owner_hint,
                a.source_quote, a.sensitivity, a.created_ms, a.expires_ms
         FROM meeting_action_item a JOIN meeting m ON m.id = a.meeting_id
         WHERE a.assignee = $1 AND a.status = 'proposed' ORDER BY a.created_ms",
    )
    .bind(&id.account_id)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
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
    let it = load_item(&db, aid).await?;
    // 只有接受过的才能开跑——否则"人确认"这道闸形同虚设(老行的 confirmed 也认)
    if it.status != "accepted" && it.status != "confirmed" {
        return Err(ServerError::BadRequest(format!("只有已接受的行动项才能关联运行,当前是 {}", it.status)));
    }
    let m = meeting_info(&db, it.meeting_id).await?;
    require(&db, &id, &Action::CreateTask, &IdScope::Channel(m.channel_id)).await?;
    sqlx::query("UPDATE meeting_action_item SET run_id=$1, status='done' WHERE id=$2")
        .bind(&l.run_id)
        .bind(aid)
        .execute(&db.pool)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true, "run_id": l.run_id })))
}

/// 过期回收:proposed 超过 72 小时没人处理 ⇒ expired,记账,广播,并在会议所在频道提醒主持人。
/// **不自动转派**:转派会让"派给了谁"这件事不再是人的决定。
pub async fn expire_due(db: &Db, hub: &Hub, audit: &Audit, now: i64) -> Result<Vec<ActionItemOut>> {
    let due = sqlx::query_as::<_, ActionItemOut>(&format!(
        "SELECT {COLS} FROM meeting_action_item
         WHERE status = 'proposed' AND COALESCE(expires_ms, created_ms + $1) < $2 ORDER BY created_ms"
    ))
    .bind(EXPIRE_AFTER_MS)
    .bind(now)
    .fetch_all(&db.pool)
    .await?;
    let mut out = Vec::with_capacity(due.len());
    for it in due {
        let m = meeting_info(db, it.meeting_id).await?;
        audit
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::system("dispatch-expiry"),
                scope: AuditScope { team: None, channel: Some(m.channel_id.clone()) },
                run_id: None,
                session_id: Some(format!("meeting:{}", it.meeting_id)),
                policy_version: Some("policy-v1".into()),
                label: None,
                locality: None,
                body: EventBody::DispatchExpire { item_id: format!("ACT-{}", it.id), assignee: it.assignee.clone() },
            })
            .await?;
        sqlx::query("UPDATE meeting_action_item SET status = 'expired', decided_ms = $1 WHERE id = $2 AND status = 'proposed'")
            .bind(now)
            .bind(it.id)
            .execute(&db.pool)
            .await?;
        let updated = load_item(db, it.id).await?;
        hub.push(&m.channel_id, &crate::ws::Push::ActionItem(updated.clone()));
        // 提醒主持人:回到他的视野,由人决定重新指派还是作罢
        let who = updated.assignee.as_deref().map(|a| format!("派给 {a} 的")).unwrap_or_else(|| "尚未指派的".into());
        let body = format!(
            "⏰ 会议《{}》{}行动项「{}」72 小时无人处理,已过期回收。主持人{}可重新指派。",
            m.title,
            who,
            updated.text,
            m.host.as_deref().map(|h| format!(" {h}")).unwrap_or_default()
        );
        match crate::message::insert(db, &m.channel_id, "系统", "system", &body, None).await {
            Ok(msg) => hub.broadcast(&m.channel_id, &msg),
            Err(e) => tracing::warn!(item = %it.id, error = %e, "过期提醒没发出去"),
        }
        out.push(updated);
    }
    Ok(out)
}

/// 每 10 分钟扫一次过期的派发单。扫描本身不写审计,过期的那条写。
pub fn spawn_expiry_sweeper(db: Db, hub: Hub, audit: Audit) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            match expire_due(&db, &hub, &audit, now_ms()).await {
                Ok(v) if !v.is_empty() => tracing::info!(n = v.len(), "派发单过期回收"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "派发单过期扫描失败"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    /// 解析规则:精确命中优先;前缀只在唯一时算;拿不准一律空着。
    #[test]
    fn assignee_resolution_is_conservative() {
        let people = c(&[("li", "李四"), ("bob", "Bob Li"), ("alice", "Alice"), ("wang", "王五")]);
        assert_eq!(pick_assignee("bob", &people).as_deref(), Some("bob"), "账号 id 不分大小写");
        assert_eq!(pick_assignee("Bob", &people).as_deref(), Some("bob"));
        assert_eq!(pick_assignee("李四", &people).as_deref(), Some("li"), "显示名精确");
        assert_eq!(pick_assignee("李", &people).as_deref(), Some("li"), "「李」是「李四」的前缀,且唯一");
        assert_eq!(pick_assignee("Alice Chen", &people).as_deref(), Some("alice"), "提示比显示名长也算前缀");
        assert_eq!(pick_assignee("小七", &people), None, "没有这个人");
        assert_eq!(pick_assignee("", &people), None);
        let two = c(&[("li1", "李四"), ("li2", "李三")]);
        assert_eq!(pick_assignee("李", &two), None, "两个姓李的:拿不准就空着");
        assert_eq!(pick_assignee("李四", &two).as_deref(), Some("li1"), "但说全名就确定");
    }
}
