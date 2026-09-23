//! 验收查询层:**演示里的每个数字都从这里的 SQL 出**。
//!
//! 对照关系(完整表见 README「8 幕 → SQL」):
//! 第 7 幕拨杆文案 = [`downgrades_zh`];第 8 幕演习报告 = [`drill_report`];
//! 工牌页「待审批」= [`pending_approvals`];Capsule 锻造取料 = [`run_chain`]。

use muster_route::DowngradeReason;
use rusqlite::{params, Connection};

use crate::event::AuditEvent;
use crate::store::{row_to_event, StoreError};

/// 第 8 幕演习报告。`ok` 是 fail-closed 的:窗口内任何一次 `Unmetered`
/// 调用都判不达标——字节数不明按违规记,不按 0 记。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrillReport {
    pub model_calls: u64,
    /// 模型通道 + 工具通道(`tool.egress`)的实测外发字节之和。
    pub egress_bytes: u64,
    /// 两条通道里测不到字节数的调用次数——按违规计。
    pub unmetered_calls: u64,
    pub local_calls: u64,
    pub cloud_calls: u64,
    /// 工具侧外发次数(含被拒的:被拒也是一次外发企图)。
    pub tool_egress_calls: u64,
}

impl DrillReport {
    pub fn ok(&self) -> bool {
        self.unmetered_calls == 0 && self.egress_bytes == 0
    }
}

/// 演习窗口聚合(闭区间毫秒时间戳)。
pub const SQL_DRILL_REPORT: &str = r#"
SELECT
  COALESCE(SUM(CASE WHEN event_type = 'model.call' THEN 1 ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN json_type(payload,'$.bytes_out') = 'object'
                    THEN json_extract(payload,'$.bytes_out.measured') ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN json_extract(payload,'$.bytes_out') = 'unmetered' THEN 1 ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN event_type = 'model.call' AND locality = 'local' THEN 1 ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN event_type = 'model.call' AND locality = 'cloud' THEN 1 ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN event_type = 'tool.egress' THEN 1 ELSE 0 END), 0)
FROM audit_event
WHERE event_type IN ('model.call', 'tool.egress') AND ts_ms BETWEEN ?1 AND ?2
"#;

pub fn drill_report(conn: &Connection, from_ms: u64, to_ms: u64) -> Result<DrillReport, StoreError> {
    let r = conn.query_row(SQL_DRILL_REPORT, params![from_ms as i64, to_ms as i64], |r| {
        Ok(DrillReport {
            model_calls: r.get::<_, i64>(0)? as u64,
            egress_bytes: r.get::<_, i64>(1)? as u64,
            unmetered_calls: r.get::<_, i64>(2)? as u64,
            local_calls: r.get::<_, i64>(3)? as u64,
            cloud_calls: r.get::<_, i64>(4)? as u64,
            tool_egress_calls: r.get::<_, i64>(5)? as u64,
        })
    })?;
    Ok(r)
}

// ---------------------------------------------------------------- 工作流

/// 工作流的一次运行:触发事件 + (若有)收尾事件,按 run_id 对上。
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRunRow {
    pub run_id: String,
    pub ts_ms: u64,
    pub trigger: String,
    pub scheduled_for_ms: Option<u64>,
    /// 没有收尾事件 ⇒ `None`(还在跑,或进程中途没了——两者都该显示为"未收尾")。
    pub outcome: Option<String>,
    /// (kind, content_hash)。
    pub artifacts: Vec<(String, String)>,
    pub merged_commit: Option<String>,
}

pub const SQL_WORKFLOW_LAST_TRIGGER: &str = r#"
SELECT MAX(ts_ms) FROM audit_event
WHERE event_type = 'workflow.trigger' AND json_extract(payload, '$.workflow') = ?1
"#;

/// 最近一次触发的时刻(定时器据此算"下一次");从未触发 ⇒ `None`。
pub fn workflow_last_trigger(conn: &Connection, workflow: &str) -> Result<Option<u64>, StoreError> {
    let v: Option<i64> = conn.query_row(SQL_WORKFLOW_LAST_TRIGGER, params![workflow], |r| r.get(0))?;
    Ok(v.map(|x| x as u64))
}

pub const SQL_WORKFLOW_RUNS: &str = r#"
SELECT t.run_id, t.ts_ms,
       json_extract(t.payload, '$.trigger'),
       json_extract(t.payload, '$.scheduled_for_ms'),
       (SELECT o.payload FROM audit_event o
         WHERE o.event_type = 'workflow.output' AND o.run_id = t.run_id
         ORDER BY o.event_id DESC LIMIT 1)
FROM audit_event t
WHERE t.event_type = 'workflow.trigger' AND json_extract(t.payload, '$.workflow') = ?1
ORDER BY t.ts_ms DESC
LIMIT ?2
"#;

/// 某工作流最近 `limit` 次运行(新在前),每次带上产物与合入结果。
pub fn workflow_runs(conn: &Connection, workflow: &str, limit: u32) -> Result<Vec<WorkflowRunRow>, StoreError> {
    let mut stmt = conn.prepare(SQL_WORKFLOW_RUNS)?;
    let rows = stmt.query_map(params![workflow, limit as i64], |r| {
        let output: Option<String> = r.get(4)?;
        let parsed = output.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let outcome = parsed.as_ref().and_then(|v| v["outcome"].as_str().map(str::to_owned));
        let merged_commit = parsed.as_ref().and_then(|v| v["merged_commit"].as_str().map(str::to_owned));
        let artifacts = parsed
            .as_ref()
            .and_then(|v| v["artifacts"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|a| {
                Some((a["kind"].as_str()?.to_owned(), a["content_hash"].as_str()?.to_owned()))
            })
            .collect();
        Ok(WorkflowRunRow {
            run_id: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            ts_ms: r.get::<_, i64>(1)? as u64,
            trigger: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            scheduled_for_ms: r.get::<_, Option<i64>>(3)?.map(|x| x as u64),
            outcome,
            artifacts,
            merged_commit,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// ---------------------------------------------------------------- 编制:按 actor 的统计

pub const SQL_ACTOR_RUNS: &str =
    "SELECT COUNT(DISTINCT run_id) FROM audit_event WHERE actor_id = ?1 AND run_id IS NOT NULL";

/// 某个 actor(工牌)参与过的 run 数。
pub fn actor_runs(conn: &Connection, actor_id: &str) -> Result<u64, StoreError> {
    let n: i64 = conn.query_row(SQL_ACTOR_RUNS, params![actor_id], |r| r.get(0))?;
    Ok(n as u64)
}

pub const SQL_ACTOR_EGRESS: &str = r#"
SELECT COALESCE(SUM(CASE WHEN json_type(payload,'$.bytes_out') = 'object'
                         THEN json_extract(payload,'$.bytes_out.measured') ELSE 0 END), 0)
FROM audit_event
WHERE actor_id = ?1 AND event_type IN ('model.call', 'tool.egress')
"#;

/// 某个 actor 的实测外发字节(模型 + 工具两条通道)。
pub fn actor_egress_bytes(conn: &Connection, actor_id: &str) -> Result<u64, StoreError> {
    let n: i64 = conn.query_row(SQL_ACTOR_EGRESS, params![actor_id], |r| r.get(0))?;
    Ok(n as u64)
}

pub const SQL_LAST_BADGE_UPDATE: &str = r#"
SELECT payload FROM audit_event
WHERE event_type = 'badge.update' AND actor_id = ?1
ORDER BY event_id DESC LIMIT 1
"#;

/// 某工牌最近一次 `badge.update`:(能力哈希, 版本号)。从未记录 ⇒ `None`。
pub fn last_badge_update(conn: &Connection, badge: &str) -> Result<Option<(String, u32)>, StoreError> {
    use rusqlite::OptionalExtension;
    let payload: Option<String> = conn
        .query_row(SQL_LAST_BADGE_UPDATE, params![badge], |r| r.get(0))
        .optional()?;
    Ok(payload.and_then(|p| {
        let v: serde_json::Value = serde_json::from_str(&p).ok()?;
        Some((v["capabilities_hash"].as_str()?.to_owned(), v["badge_version"].as_u64()? as u32))
    }))
}

/// 第 7 幕降级流(时间窗内所有带 downgrade 的路由决策)。
pub const SQL_DOWNGRADES: &str = r#"
SELECT ts_ms, run_id, json_extract(payload, '$.downgrade')
FROM audit_event
WHERE event_type = 'route.decide'
  AND json_extract(payload, '$.downgrade') IS NOT NULL
  AND ts_ms BETWEEN ?1 AND ?2
ORDER BY ts_ms ASC
"#;

/// 返回 (ts_ms, run_id, 中文文案)——文案由 [`DowngradeReason::text_zh`] 供给,
/// 前端不自己拼理由字符串(单一来源)。
pub fn downgrades_zh(
    conn: &Connection,
    from_ms: u64,
    to_ms: u64,
) -> Result<Vec<(u64, Option<String>, &'static str)>, StoreError> {
    let mut stmt = conn.prepare(SQL_DOWNGRADES)?;
    let rows = stmt.query_map(params![from_ms as i64, to_ms as i64], |r| {
        Ok((
            r.get::<_, i64>(0)? as u64,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (ts, run, reason_str) = row?;
        // json_extract 对字符串值返回裸串(无引号),按 JSON string 解析。
        let reason: DowngradeReason =
            serde_json::from_value(serde_json::Value::String(reason_str))?;
        out.push((ts, run, reason.text_zh()));
    }
    Ok(out)
}

/// 工牌页「待审批」数字:该 Agent 已申请、且尚无任何裁决事件的 approval_id 计数。
pub const SQL_PENDING_APPROVALS: &str = r#"
SELECT COUNT(*)
FROM audit_event req
WHERE req.event_type = 'approval.request'
  AND req.actor_id = ?1
  AND NOT EXISTS (
    SELECT 1 FROM audit_event dec
    WHERE dec.event_type = 'approval.decision'
      AND json_extract(dec.payload, '$.approval_id')
        = json_extract(req.payload, '$.approval_id')
  )
"#;

pub fn pending_approvals(conn: &Connection, badge: &str) -> Result<u64, StoreError> {
    let n: i64 = conn.query_row(SQL_PENDING_APPROVALS, params![badge], |r| r.get(0))?;
    Ok(n as u64)
}

/// 待裁决的一条审批(P5 审批页直接消费)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub approval_id: String,
    pub ts_ms: u64,
    /// 申请者(Agent 工牌)。
    pub actor_id: String,
    pub run_id: Option<String>,
    pub team: Option<String>,
    pub channel: Option<String>,
    /// 申请的能力(如 `merge_to_main`)。
    pub requested_capability: String,
    pub reason: String,
    /// 申请内容的哈希(diff 正文的 ContentHash;正文在 run 存储侧)。
    pub command_hash: String,
}

/// 全部未决审批,新的在前。`badge` 为 `None` 时不限申请者。
pub const SQL_PENDING_LIST: &str = r#"
SELECT json_extract(req.payload, '$.approval_id'), req.ts_ms, req.actor_id, req.run_id,
       req.team, req.channel,
       json_extract(req.payload, '$.requested_capability'),
       json_extract(req.payload, '$.reason'),
       json_extract(req.payload, '$.command_hash')
FROM audit_event req
WHERE req.event_type = 'approval.request'
  AND (?1 IS NULL OR req.actor_id = ?1)
  AND NOT EXISTS (
    SELECT 1 FROM audit_event dec
    WHERE dec.event_type = 'approval.decision'
      AND json_extract(dec.payload, '$.approval_id')
        = json_extract(req.payload, '$.approval_id')
  )
ORDER BY req.event_id DESC
"#;

pub fn pending_approval_list(
    conn: &Connection,
    badge: Option<&str>,
) -> Result<Vec<PendingApproval>, StoreError> {
    let mut stmt = conn.prepare(SQL_PENDING_LIST)?;
    let rows = stmt.query_map(params![badge], |r| {
        Ok(PendingApproval {
            approval_id: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            ts_ms: r.get::<_, i64>(1)? as u64,
            actor_id: r.get(2)?,
            run_id: r.get(3)?,
            team: r.get(4)?,
            channel: r.get(5)?,
            requested_capability: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            reason: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
            command_hash: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 某审批是否已有裁决(防重复裁决;审批是 append-only 事件流,不能靠删行去重)。
pub const SQL_DECISION_OF: &str =
    "SELECT json_extract(payload,'$.granted') FROM audit_event
     WHERE event_type='approval.decision' AND json_extract(payload,'$.approval_id') = ?1
     ORDER BY event_id ASC LIMIT 1";

pub fn decision_of(conn: &Connection, approval_id: &str) -> Result<Option<bool>, StoreError> {
    let mut stmt = conn.prepare(SQL_DECISION_OF)?;
    let mut rows = stmt.query(params![approval_id])?;
    match rows.next()? {
        // json_extract 对 bool 返回 0/1
        Some(row) => Ok(Some(row.get::<_, i64>(0)? != 0)),
        None => Ok(None),
    }
}

/// E3 第 3 幕:会话当前锁定状态 = 该 session 最近一次 lock.raise。
/// 返回 (锁定级, 肇因来源, ts_ms);None = 会话从未被抬升。
pub const SQL_SESSION_LOCK: &str = r#"
SELECT json_extract(payload,'$.to_level'), json_extract(payload,'$.cause'), ts_ms
FROM audit_event
WHERE event_type = 'session.lock.raise' AND session_id = ?1
ORDER BY event_id DESC LIMIT 1
"#;

pub fn session_lock(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<(muster_route::Sensitivity, muster_route::LabelSource, u64)>, StoreError> {
    use rusqlite::OptionalExtension;
    let row = conn
        .query_row(SQL_SESSION_LOCK, params![session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u64,
            ))
        })
        .optional()?;
    match row {
        None => Ok(None),
        Some((lv, cause_json, ts)) => {
            let level = serde_json::from_value(serde_json::Value::String(lv))?;
            let cause: muster_route::LabelSource = serde_json::from_str(&cause_json)?;
            Ok(Some((level, cause, ts)))
        }
    }
}

/// Capsule 锻造取料:一个 run 的完整事件链,按时间(=id 字典序)升序。
pub const SQL_RUN_CHAIN: &str =
    "SELECT event_id, ts_ms, actor_kind, actor_id, run_id, session_id, team, channel,
            label, locality, policy_version, schema_version, payload, prev_hash, hash
     FROM audit_event WHERE run_id = ?1 ORDER BY event_id ASC";

pub fn run_chain(conn: &Connection, run_id: &str) -> Result<Vec<AuditEvent>, StoreError> {
    let mut stmt = conn.prepare(SQL_RUN_CHAIN)?;
    let rows = stmt.query_map(params![run_id], row_to_event)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 审计中心侧栏:最近 N 条事件,倒序(event_id 为 ULID,字典序即时间序)。
pub const SQL_RECENT: &str =
    "SELECT event_id, ts_ms, actor_kind, actor_id, run_id, session_id, team, channel,
            label, locality, policy_version, schema_version, payload, prev_hash, hash
     FROM audit_event ORDER BY event_id DESC LIMIT ?1";

pub fn recent_events(conn: &Connection, limit: u64) -> Result<Vec<AuditEvent>, StoreError> {
    let mut stmt = conn.prepare(SQL_RECENT)?;
    let rows = stmt.query_map(params![limit], row_to_event)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 按事件类型取最近 N 条(倒序)——首页「最近任务」「最近演习」用。
pub const SQL_RECENT_OF: &str =
    "SELECT event_id, ts_ms, actor_kind, actor_id, run_id, session_id, team, channel,
            label, locality, policy_version, schema_version, payload, prev_hash, hash
     FROM audit_event WHERE event_type = ?1 ORDER BY event_id DESC LIMIT ?2";

pub fn recent_events_of(
    conn: &Connection,
    event_type: &str,
    limit: u64,
) -> Result<Vec<AuditEvent>, StoreError> {
    let mut stmt = conn.prepare(SQL_RECENT_OF)?;
    let rows = stmt.query_map(params![event_type, limit], row_to_event)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// P4 能力库的一行。**验真率由事件算出,不另存**——两处存储必然有一处会失真。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleRow {
    pub capsule_id: String,
    pub name: String,
    pub version: String,
    pub scope: String,
    /// 锻造它的那次运行(出处;没有出处的能力不该存在)。
    pub source_run_id: String,
    pub forged_ms: u64,
    pub forged_by: String,
    /// 密级(继承自源运行;跨团队引入时随包迁移,不可降密)。
    pub label: Option<String>,
    /// 所属团队(锻造事件的 scope.team)。
    pub owner_team: Option<String>,
    /// 影子重放:通过次数 / 总次数。
    pub verify_passed: u64,
    pub verify_total: u64,
    /// 被跨团队引入的次数。
    pub adopted: u64,
}

impl CapsuleRow {
    /// 验真率。**没跑过验真就是 None,不是 0% 也不是 100%**——
    /// "未验证"与"验证失败"是两回事,UI 必须能区分。
    pub fn verified_rate(&self) -> Option<f64> {
        (self.verify_total > 0).then(|| self.verify_passed as f64 / self.verify_total as f64)
    }
}

/// 能力库:每个 capsule 取其最近一次锻造事件,并聚合验真与引入次数。
pub const SQL_CAPSULES: &str = r#"
WITH forge AS (
  SELECT json_extract(payload,'$.capsule_id') AS cid,
         json_extract(payload,'$.name')       AS name,
         json_extract(payload,'$.version')    AS ver,
         json_extract(payload,'$.scope')      AS scope,
         json_extract(payload,'$.source_run_id') AS src,
         ts_ms, actor_id, event_id, label, team,
         ROW_NUMBER() OVER (PARTITION BY json_extract(payload,'$.capsule_id')
                            ORDER BY event_id DESC) AS rn
  FROM audit_event WHERE event_type = 'capsule.forge'
)
SELECT f.cid, f.name, f.ver, f.scope, f.src, f.ts_ms, f.actor_id, f.label, f.team,
  COALESCE((SELECT SUM(CASE WHEN json_extract(v.payload,'$.passed') THEN 1 ELSE 0 END)
            FROM audit_event v WHERE v.event_type='capsule.verify'
              AND json_extract(v.payload,'$.capsule_id') = f.cid), 0),
  COALESCE((SELECT COUNT(*) FROM audit_event v WHERE v.event_type='capsule.verify'
              AND json_extract(v.payload,'$.capsule_id') = f.cid), 0),
  COALESCE((SELECT COUNT(*) FROM audit_event a WHERE a.event_type='capsule.adopt'
              AND json_extract(a.payload,'$.capsule_id') = f.cid), 0)
FROM forge f WHERE f.rn = 1
ORDER BY f.event_id DESC
"#;

pub fn capsules(conn: &Connection) -> Result<Vec<CapsuleRow>, StoreError> {
    let mut stmt = conn.prepare(SQL_CAPSULES)?;
    let rows = stmt.query_map([], |r| {
        Ok(CapsuleRow {
            capsule_id: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            version: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            scope: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            source_run_id: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            forged_ms: r.get::<_, i64>(5)? as u64,
            forged_by: r.get(6)?,
            label: r.get(7)?,
            owner_team: r.get(8)?,
            verify_passed: r.get::<_, i64>(9)? as u64,
            verify_total: r.get::<_, i64>(10)? as u64,
            adopted: r.get::<_, i64>(11)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 一次运行是否**够格被锻造**。四项缺一不可:
///
/// 1. 有 `run.start`(才有 ReplayRefs,才谈得上重放);
/// 2. 成功结束(不锻造半途而废的过程);
/// 3. **产出已被人认可**——有变更就必须有获批的 `approval.decision`。
///    能力是要被反复复用的东西;若人否决过(或尚未过目)的做法也能固化,
///    等于把被拒绝的方案偷偷塞回流程。**无变更的纯查询运行不需要审批**,
///    因为它本就没有待认可的产出。
/// 4. 未锻造过(避免同源派生多个版本)。
pub const SQL_FORGEABLE: &str = r#"
SELECT
  (SELECT COUNT(*) FROM audit_event WHERE run_id = ?1 AND event_type = 'run.start'),
  (SELECT COUNT(*) FROM audit_event WHERE run_id = ?1 AND event_type = 'run.finish'
     AND json_extract(payload,'$.outcome') = 'success'),
  (SELECT COUNT(*) FROM audit_event WHERE event_type = 'capsule.forge'
     AND json_extract(payload,'$.source_run_id') = ?1),
  (SELECT COUNT(*) FROM audit_event WHERE run_id = ?1 AND event_type = 'approval.request'),
  (SELECT COUNT(*) FROM audit_event WHERE run_id = ?1 AND event_type = 'approval.decision'
     AND json_extract(payload,'$.granted') = 1)
"#;

/// 返回 `(可锻造, 原因)`。
pub fn forgeable(conn: &Connection, run_id: &str) -> Result<(bool, String), StoreError> {
    let (starts, oks, forged, requested, granted): (i64, i64, i64, i64, i64) = conn
        .query_row(SQL_FORGEABLE, params![run_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
    Ok(match (starts > 0, oks > 0, forged > 0, requested > 0, granted > 0) {
        (false, ..) => (false, "该运行没有 run.start,缺少重放引用,无法锻造".into()),
        (_, false, ..) => (false, "该运行未成功结束,不锻造半途而废的过程".into()),
        (_, _, true, ..) => (false, "该运行已锻造过 Capsule".into()),
        // 有产出但未获批:人还没认可(或已否决)的做法不该被固化成可复用能力
        (_, _, _, true, false) => (
            false,
            "该运行的产出尚未获批准——先在「待我审批」里裁决,批准后方可锻造".into(),
        ),
        _ => (true, "可锻造".into()),
    })
}

/// 取某次运行的 `run.start` payload(锻造时复制其 ReplayRefs)。
pub fn run_start_of(conn: &Connection, run_id: &str) -> Result<Option<AuditEvent>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT event_id, ts_ms, actor_kind, actor_id, run_id, session_id, team, channel,
                label, locality, policy_version, schema_version, payload, prev_hash, hash
         FROM audit_event WHERE run_id = ?1 AND event_type = 'run.start'
         ORDER BY event_id ASC LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![run_id], row_to_event)?;
    Ok(rows.next().transpose()?)
}

/// D6 编制页的一行:**编制 = 审计链里真实干过活的 actor**。
/// 花名册不是配置出来的,是干出来的——谁执行过任务,谁就在编制里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterRow {
    pub actor_kind: String,
    pub actor_id: String,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// 参与过的不同 run 数。
    pub runs: u64,
    /// 模型调用去向(仅 `model.call` 计数)。
    pub local_calls: u64,
    pub cloud_calls: u64,
    /// 该 actor 名下被拒绝的路由次数(fail-closed 拦下的)。
    pub refusals: u64,
    /// 事件总数(活跃度)。
    pub events: u64,
}

/// 按最近活跃倒序。团队/频道维度可选过滤(`None` = 全组织)。
pub const SQL_ROSTER: &str = r#"
SELECT actor_kind, actor_id,
       MIN(ts_ms), MAX(ts_ms),
       COUNT(DISTINCT run_id),
       COALESCE(SUM(CASE WHEN event_type='model.call' AND locality='local' THEN 1 ELSE 0 END), 0),
       COALESCE(SUM(CASE WHEN event_type='model.call' AND locality='cloud' THEN 1 ELSE 0 END), 0),
       COALESCE(SUM(CASE WHEN event_type='route.refuse' THEN 1 ELSE 0 END), 0),
       COUNT(*)
FROM audit_event
WHERE (?1 IS NULL OR team = ?1)
GROUP BY actor_kind, actor_id
ORDER BY MAX(ts_ms) DESC
"#;

pub fn roster(conn: &Connection, team: Option<&str>) -> Result<Vec<RosterRow>, StoreError> {
    let mut stmt = conn.prepare(SQL_ROSTER)?;
    let rows = stmt.query_map(params![team], |r| {
        Ok(RosterRow {
            actor_kind: r.get(0)?,
            actor_id: r.get(1)?,
            first_seen_ms: r.get::<_, i64>(2)? as u64,
            last_seen_ms: r.get::<_, i64>(3)? as u64,
            runs: r.get::<_, i64>(4)? as u64,
            local_calls: r.get::<_, i64>(5)? as u64,
            cloud_calls: r.get::<_, i64>(6)? as u64,
            refusals: r.get::<_, i64>(7)? as u64,
            events: r.get::<_, i64>(8)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 各团队的在册人数(侧栏「N人·M AI」)。
///
/// 与 [`roster`] 同一口径——**编制 = 在审计链里真实干过活的 actor**,
/// 而不是某处配置里写了几个人。`system` 类不算人手。
/// 一个 actor 在多个团队干过活就在各团队各算一次:这不是重复计数,
/// 「谁参与了这个团队」本来就该按团队问。
pub const SQL_ROSTER_COUNTS: &str = r#"
SELECT team,
       COUNT(DISTINCT CASE WHEN actor_kind='human' THEN actor_id END),
       COUNT(DISTINCT CASE WHEN actor_kind='agent' THEN actor_id END)
FROM audit_event
WHERE team IS NOT NULL AND actor_kind <> 'system'
GROUP BY team
"#;

/// `(团队, 人数, Agent 数)`。
pub fn roster_counts(conn: &Connection) -> Result<Vec<(String, u64, u64)>, StoreError> {
    let mut stmt = conn.prepare(SQL_ROSTER_COUNTS)?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Agent 档案页「入职时间」:该 actor 的首条审计事件时刻(ULID 即时序)。
pub const SQL_ACTOR_FIRST_SEEN: &str =
    "SELECT MIN(ts_ms) FROM audit_event WHERE actor_id = ?1";

pub fn actor_first_seen(conn: &Connection, actor_id: &str) -> Result<Option<u64>, StoreError> {
    let n: Option<i64> = conn.query_row(SQL_ACTOR_FIRST_SEEN, params![actor_id], |r| r.get(0))?;
    Ok(n.map(|v| v as u64))
}

/// 首页 KPI:窗口内出现过 `model.call` 的不同 run 数。
pub const SQL_DISTINCT_RUNS: &str =
    "SELECT COUNT(DISTINCT run_id) FROM audit_event
     WHERE event_type = 'model.call' AND run_id IS NOT NULL AND ts_ms BETWEEN ?1 AND ?2";

pub fn distinct_runs(conn: &Connection, from_ms: u64, to_ms: u64) -> Result<u64, StoreError> {
    let n: i64 =
        conn.query_row(SQL_DISTINCT_RUNS, params![from_ms as i64, to_ms as i64], |r| r.get(0))?;
    Ok(n as u64)
}

/// 首页吞吐图的一根柱:某个自然日(本地时区)的本地/云端 `model.call` 数。
/// `weekday` 为 SQLite `strftime('%w')`:0=周日 … 6=周六。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayThroughput {
    pub date: String,
    pub weekday: String,
    pub local: u64,
    pub cloud: u64,
}

/// 最近 `?1` 个自然日逐日聚合,无调用的日子补零,按日期升序。
pub const SQL_DAY_THROUGHPUT: &str = r#"
WITH RECURSIVE seq(n) AS (
  SELECT 0 UNION ALL SELECT n + 1 FROM seq WHERE n < ?1 - 1
),
days AS (
  SELECT date('now', 'localtime', '-' || n || ' days') AS d,
         strftime('%w', date('now', 'localtime', '-' || n || ' days')) AS w
  FROM seq
)
SELECT days.d, days.w,
       COALESCE(SUM(CASE WHEN a.locality = 'local' THEN 1 ELSE 0 END), 0),
       COALESCE(SUM(CASE WHEN a.locality = 'cloud' THEN 1 ELSE 0 END), 0)
FROM days
LEFT JOIN audit_event a
  ON a.event_type = 'model.call'
 AND date(a.ts_ms / 1000, 'unixepoch', 'localtime') = days.d
GROUP BY days.d
ORDER BY days.d ASC
"#;

pub fn day_throughput(conn: &Connection, days: u32) -> Result<Vec<DayThroughput>, StoreError> {
    let mut stmt = conn.prepare(SQL_DAY_THROUGHPUT)?;
    let rows = stmt.query_map(params![days], |r| {
        Ok(DayThroughput {
            date: r.get(0)?,
            weekday: r.get(1)?,
            local: r.get::<_, i64>(2)? as u64,
            cloud: r.get::<_, i64>(3)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod home_query_tests {
    use super::*;
    use crate::event::{
        Actor, ContentHash, EgressBytes, EventBody, ModelRef, NewEvent, ReplayRefs, RunOutcome,
        Scope,
    };
    use crate::store::AuditStore;
    use muster_provider::Locality;
    use muster_route::Sensitivity;

    fn call_event(run: &str, locality: Locality) -> NewEvent {
        NewEvent {
            ts_ms: None,
            actor: Actor::agent("A-007"),
            scope: Scope::default(),
            run_id: Some(run.into()),
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: Some(Sensitivity::Open),
            locality: Some(locality),
            body: EventBody::ModelCall {
                provider_id: "p".into(),
                model: "m".into(),
                locality,
                label: Sensitivity::Open,
                tokens_in: None,
                tokens_out: None,
                bytes_in: 0,
                bytes_out: EgressBytes::Measured(0),
                latency_ms: 1,
                request_hash: ContentHash::sha256(b"req"),
            },
        }
    }

    fn approval_req(approval_id: &str, run: &str) -> NewEvent {
        NewEvent {
            ts_ms: None,
            actor: Actor::agent("A-007"),
            scope: Scope::default(),
            run_id: Some(run.into()),
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body: EventBody::ApprovalRequest {
                approval_id: approval_id.into(),
                requested_capability: "merge_to_main".into(),
                badge_capabilities_hash: ContentHash::sha256(b"caps"),
                command_hash: ContentHash::sha256(b"patch"),
                reason: "申请合入".into(),
            },
        }
    }

    fn replay_refs() -> ReplayRefs {
        ReplayRefs {
            repo_snapshot: ContentHash::sha256(b"git-head:abc"),
            repo_ref: Some("abc".into()),
            deps_lock: ContentHash::sha256(b"lock"),
            model: ModelRef {
                provider_id: "kimi".into(),
                model: "kimi-k3".into(),
                params_hash: ContentHash::sha256(b"params"),
            },
            tool_env: ContentHash::sha256(b"tools"),
        }
    }

    fn ev(run: &str, body: EventBody) -> NewEvent {
        NewEvent {
            ts_ms: None,
            actor: Actor::agent("A-007"),
            scope: Scope::default(),
            run_id: Some(run.into()),
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body,
        }
    }

    /// P4:能力必须有出处——只有"成功结束且有 run.start"的运行够格锻造,
    /// 且同一次运行不能锻造两遍。
    #[test]
    fn only_successful_runs_with_replay_refs_are_forgeable() {
        let mut store = AuditStore::open_in_memory().unwrap();

        // RUN-OK:有 start,成功结束 ⇒ 可锻造
        store
            .append(ev("RUN-OK", EventBody::RunStart {
                task_kind: "chat.tools.v0".into(),
                replay: replay_refs(),
                label: Sensitivity::Open,
                locality_planned: Locality::Cloud,
            }))
            .unwrap();
        store
            .append(ev("RUN-OK", EventBody::RunFinish {
                outcome: RunOutcome::Success,
                duration_ms: 100,
                output_hash: None,
            }))
            .unwrap();
        assert!(forgeable(store.conn(), "RUN-OK").unwrap().0);

        // RUN-FAIL:失败结束 ⇒ 不锻造半途而废的过程
        store
            .append(ev("RUN-FAIL", EventBody::RunStart {
                task_kind: "chat.tools.v0".into(),
                replay: replay_refs(),
                label: Sensitivity::Open,
                locality_planned: Locality::Cloud,
            }))
            .unwrap();
        store
            .append(ev("RUN-FAIL", EventBody::RunFinish {
                outcome: RunOutcome::Failed { class: "stream".into() },
                duration_ms: 1,
                output_hash: None,
            }))
            .unwrap();
        let (ok, why) = forgeable(store.conn(), "RUN-FAIL").unwrap();
        assert!(!ok && why.contains("未成功"), "{why}");

        // 无 run.start ⇒ 缺重放引用
        let (ok, why) = forgeable(store.conn(), "RUN-GHOST").unwrap();
        assert!(!ok && why.contains("重放引用"), "{why}");

        // 锻造后不可再锻造(RUN-OK 无 approval.request,属"纯查询运行",无需审批)
        store
            .append(ev("RUN-OK", EventBody::CapsuleForge {
                capsule_id: "CAP-1".into(),
                name: "修复减法 bug".into(),
                version: "1.0.0".into(),
                source_run_id: "RUN-OK".into(),
                replay: replay_refs(),
                content_hash: ContentHash::sha256(b"def"),
                scope: "team".into(),
            }))
            .unwrap();
        let (ok, why) = forgeable(store.conn(), "RUN-OK").unwrap();
        assert!(!ok && why.contains("已锻造过"), "{why}");
    }

    /// **产出未获批的运行不许锻造**。
    ///
    /// 这条曾经只写在 UI 文案里("成功完成**且经审批**")而没写进代码——
    /// 于是真机跑通时,未批准的运行照样被锻造成了能力。能力要被反复复用,
    /// 若人否决过(或尚未过目)的做法也能固化,等于把被拒方案偷偷塞回流程。
    #[test]
    fn runs_with_unapproved_output_cannot_be_forged() {
        let mut store = AuditStore::open_in_memory().unwrap();
        let start_finish = |s: &mut AuditStore, run: &str| {
            s.append(ev(run, EventBody::RunStart {
                task_kind: "chat.tools.v0".into(),
                replay: replay_refs(),
                label: Sensitivity::Open,
                locality_planned: Locality::Cloud,
            }))
            .unwrap();
            s.append(ev(run, EventBody::RunFinish {
                outcome: RunOutcome::Success,
                duration_ms: 10,
                output_hash: None,
            }))
            .unwrap();
        };
        let request = |s: &mut AuditStore, run: &str| {
            s.append(ev(run, EventBody::ApprovalRequest {
                approval_id: format!("APR-{run}"),
                requested_capability: "merge_to_main".into(),
                badge_capabilities_hash: ContentHash::sha256(b"caps"),
                command_hash: ContentHash::sha256(b"patch"),
                reason: "申请合入".into(),
            }))
            .unwrap();
        };
        let decide = |s: &mut AuditStore, run: &str, granted: bool| {
            s.append(ev(run, EventBody::ApprovalDecision {
                approval_id: format!("APR-{run}"),
                granted,
                note_hash: None,
            }))
            .unwrap();
        };

        // ① 有产出、已申请、未裁决 ⇒ 不可锻造
        start_finish(&mut store, "RUN-P1");
        request(&mut store, "RUN-P1");
        let (ok, why) = forgeable(store.conn(), "RUN-P1").unwrap();
        assert!(!ok && why.contains("尚未获批准"), "{why}");

        // ② 有产出、被拒绝 ⇒ 仍不可锻造(被否决的做法不该固化)
        start_finish(&mut store, "RUN-P2");
        request(&mut store, "RUN-P2");
        decide(&mut store, "RUN-P2", false);
        let (ok, why) = forgeable(store.conn(), "RUN-P2").unwrap();
        assert!(!ok && why.contains("尚未获批准"), "被拒绝的运行也不许锻造:{why}");

        // ③ 有产出、已批准 ⇒ 可锻造
        start_finish(&mut store, "RUN-P3");
        request(&mut store, "RUN-P3");
        decide(&mut store, "RUN-P3", true);
        assert!(forgeable(store.conn(), "RUN-P3").unwrap().0);

        // ④ 无产出的纯查询运行 ⇒ 本就无待认可的东西,不需要审批
        start_finish(&mut store, "RUN-P4");
        assert!(forgeable(store.conn(), "RUN-P4").unwrap().0, "纯查询运行不该被审批门槛卡住");
    }

    /// 能力库的数字全部由事件算出:验真率不另存,未验真 ≠ 0%。
    #[test]
    fn capsule_library_numbers_come_from_events() {
        let mut store = AuditStore::open_in_memory().unwrap();
        store
            .append(ev("RUN-1", EventBody::CapsuleForge {
                capsule_id: "CAP-1".into(),
                name: "Release Checklist".into(),
                version: "1.0.0".into(),
                source_run_id: "RUN-1".into(),
                replay: replay_refs(),
                content_hash: ContentHash::sha256(b"c1"),
                scope: "team".into(),
            }))
            .unwrap();

        // 刚锻造:未验真 —— 必须是 None,不能显示成 0%(否则看着像"验证失败")
        let rows = capsules(store.conn()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_run_id, "RUN-1");
        assert_eq!(rows[0].verified_rate(), None, "未验真必须与验真失败可区分");
        assert_eq!(rows[0].adopted, 0);

        // 三次影子重放,过两次
        for (i, passed) in [true, false, true].into_iter().enumerate() {
            store
                .append(ev(&format!("RUN-V{i}"), EventBody::CapsuleVerify {
                    capsule_id: "CAP-1".into(),
                    version: "1.0.0".into(),
                    run_id: format!("RUN-V{i}"),
                    passed,
                    evidence_hash: ContentHash::sha256(b"ev"),
                }))
                .unwrap();
        }
        // 跨团队引入一次
        store
            .append(ev("RUN-1", EventBody::CapsuleAdopt {
                capsule_id: "CAP-1".into(),
                version: "1.0.0".into(),
                from_team: "支付组".into(),
                to_team: "平台组".into(),
                label: Sensitivity::Internal,
            }))
            .unwrap();

        let rows = capsules(store.conn()).unwrap();
        assert_eq!((rows[0].verify_passed, rows[0].verify_total), (2, 3));
        assert!((rows[0].verified_rate().unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(rows[0].adopted, 1);
    }

    /// P5:未决 = 有请求且无裁决;裁决后立即出列,且裁决结果可查(防重复裁决)。
    #[test]
    fn approvals_move_out_of_pending_once_decided() {
        let mut store = AuditStore::open_in_memory().unwrap();
        store.append(approval_req("APR-1", "RUN-1")).unwrap();
        store.append(approval_req("APR-2", "RUN-2")).unwrap();

        let pending = pending_approval_list(store.conn(), None).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].approval_id, "APR-2", "新的在前");
        assert_eq!(pending[0].run_id.as_deref(), Some("RUN-2"));
        assert_eq!(pending[0].requested_capability, "merge_to_main");
        assert_eq!(pending_approvals(store.conn(), "A-007").unwrap(), 2);
        assert_eq!(decision_of(store.conn(), "APR-1").unwrap(), None, "尚无裁决");

        // 裁决 APR-1(批准)
        store
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::human("owner"),
                scope: Scope::default(),
                run_id: Some("RUN-1".into()),
                session_id: None,
                policy_version: Some("policy-v1".into()),
                label: None,
                locality: None,
                body: EventBody::ApprovalDecision {
                    approval_id: "APR-1".into(),
                    granted: true,
                    note_hash: None,
                },
            })
            .unwrap();

        let pending = pending_approval_list(store.conn(), None).unwrap();
        assert_eq!(pending.len(), 1, "已裁决的必须出列");
        assert_eq!(pending[0].approval_id, "APR-2");
        assert_eq!(pending_approvals(store.conn(), "A-007").unwrap(), 1);
        assert_eq!(decision_of(store.conn(), "APR-1").unwrap(), Some(true), "裁决结果可查");
        assert_eq!(decision_of(store.conn(), "APR-2").unwrap(), None);
    }

    /// D6:编制由审计链生成——没干过活的不在册,干过的统计必须准。
    #[test]
    fn roster_is_derived_from_who_actually_worked() {
        let mut store = AuditStore::open_in_memory().unwrap();
        // A-007:两个 run,一云一本地
        store.append(call_event("RUN-A", Locality::Cloud)).unwrap();
        store.append(call_event("RUN-B", Locality::Local)).unwrap();
        // A-021:一个 run,云端
        let mut e = call_event("RUN-C", Locality::Cloud);
        e.actor = Actor::agent("A-021");
        store.append(e).unwrap();

        let rows = roster(store.conn(), None).unwrap();
        assert_eq!(rows.len(), 2, "只有真干过活的两个 agent 在册:{rows:?}");
        let a7 = rows.iter().find(|r| r.actor_id == "A-007").unwrap();
        assert_eq!((a7.runs, a7.cloud_calls, a7.local_calls), (2, 1, 1));
        assert_eq!(a7.actor_kind, "agent");
        assert!(a7.last_seen_ms >= a7.first_seen_ms);
        let a21 = rows.iter().find(|r| r.actor_id == "A-021").unwrap();
        assert_eq!((a21.runs, a21.cloud_calls), (1, 1));

        // 团队过滤:事件的 scope.team 为空,按团队查应当查不到
        assert!(roster(store.conn(), Some("平台组")).unwrap().is_empty());
        // 无团队的事件不进分团队计数(侧栏不该凭空多出一个组)
        assert!(roster_counts(store.conn()).unwrap().is_empty());
    }

    /// 侧栏「N人·M AI」:与在册编制同一口径,按团队分开数,人与 Agent 分开数。
    #[test]
    fn roster_counts_split_by_team_and_kind() {
        let mut store = AuditStore::open_in_memory().unwrap();
        let mut mk = |team: &str, actor: Actor| {
            let mut e = call_event("RUN-X", Locality::Local);
            e.actor = actor;
            e.scope = Scope { team: Some(team.into()), channel: None };
            store.append(e).unwrap();
        };
        mk("平台组", Actor::agent("A-007"));
        mk("平台组", Actor::agent("A-007")); // 同一人多次活动只算一个
        mk("平台组", Actor::human("alice"));
        mk("支付组", Actor::agent("A-012"));
        // 跨团队干活的人在两边各算一次——「谁参与了这个团队」本就该按团队问
        mk("支付组", Actor::human("alice"));
        // system 不算人手(与在册编制口径一致)
        mk("平台组", Actor::system("scheduler"));

        let mut got = roster_counts(store.conn()).unwrap();
        got.sort();
        assert_eq!(
            got,
            vec![("平台组".to_string(), 1, 1), ("支付组".to_string(), 1, 1)],
            "平台组 1人1AI(system 不计)· 支付组 1人1AI"
        );
    }

    #[test]
    fn day_throughput_fills_all_days_and_counts_today() {
        let mut store = AuditStore::open_in_memory().unwrap();
        store.append(call_event("RUN-A", Locality::Cloud)).unwrap();
        store.append(call_event("RUN-A", Locality::Cloud)).unwrap();
        store.append(call_event("RUN-B", Locality::Local)).unwrap();

        let days = day_throughput(store.conn(), 7).unwrap();
        assert_eq!(days.len(), 7, "无调用的日子必须补零");
        let today = days.last().unwrap();
        assert_eq!((today.cloud, today.local), (2, 1));
        assert!(days.iter().take(6).all(|d| d.cloud == 0 && d.local == 0));

        let now = today_now_ms();
        assert_eq!(distinct_runs(store.conn(), now - 86_400_000, now).unwrap(), 2);
        assert_eq!(recent_events_of(store.conn(), "model.call", 10).unwrap().len(), 3);
        assert!(recent_events_of(store.conn(), "run.finish", 10).unwrap().is_empty());
    }

    fn today_now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    fn at(ts_ms: u64, locality: Option<Locality>, run: &str, body: EventBody) -> NewEvent {
        let mut e = ev(run, body);
        e.ts_ms = Some(ts_ms);
        e.locality = locality;
        e
    }

    /// 工具侧外发与模型调用同进演习报告:字节相加、Unmetered 按违规计、
    /// 模型调用计数不被工具调用污染。
    #[test]
    fn drill_report_counts_tool_egress_alongside_model_calls() {
        use crate::event::{ContentHash, EgressBytes};
        let mut store = AuditStore::open_in_memory().unwrap();
        let egress = |bytes_out: EgressBytes, refused: Option<&str>| EventBody::ToolEgress {
            tool: "http_get".into(),
            host: "example.test".into(),
            url_hash: ContentHash::sha256(b"u"),
            bytes_out,
            bytes_in: 30,
            status: Some(200),
            latency_ms: 3,
            refused: refused.map(str::to_owned),
        };
        store
            .append(at(1_000, Some(Locality::Cloud), "RUN-D", EventBody::ModelCall {
                provider_id: "p".into(),
                model: "m".into(),
                locality: Locality::Cloud,
                label: Sensitivity::Open,
                tokens_in: None,
                tokens_out: None,
                bytes_in: 10,
                bytes_out: EgressBytes::Measured(100),
                latency_ms: 5,
                request_hash: ContentHash::sha256(b"r"),
            }))
            .unwrap();
        store.append(at(1_100, Some(Locality::Cloud), "RUN-D", egress(EgressBytes::Measured(40), None))).unwrap();
        store.append(at(1_200, Some(Locality::Cloud), "RUN-D", egress(EgressBytes::Unmetered, None))).unwrap();
        store.append(at(1_300, Some(Locality::Cloud), "RUN-D", egress(EgressBytes::Measured(0), Some("host")))).unwrap();

        let r = drill_report(store.conn(), 0, 2_000).unwrap();
        assert_eq!(r.model_calls, 1, "工具调用不算模型调用");
        assert_eq!(r.cloud_calls, 1);
        assert_eq!(r.tool_egress_calls, 3, "被拒的也是一次外发企图");
        assert_eq!(r.egress_bytes, 140, "两条通道的实测字节相加");
        assert_eq!(r.unmetered_calls, 1);
        assert!(!r.ok());
    }

    /// 工作流:触发与收尾按 run_id 配对;没收尾的显示为未收尾;最近触发时刻可查。
    #[test]
    fn workflow_runs_pair_triggers_with_outputs() {
        use crate::event::{ArtifactRef, ContentHash};
        let mut store = AuditStore::open_in_memory().unwrap();
        let trigger = |t: &str| EventBody::WorkflowTrigger {
            workflow: "daily".into(),
            definition_hash: ContentHash::sha256(b"def"),
            trigger: t.into(),
            scheduled_for_ms: Some(5),
        };
        store.append(at(10, None, "WF-1", trigger("scheduled"))).unwrap();
        store
            .append(at(15, None, "WF-1", EventBody::WorkflowOutput {
                workflow: "daily".into(),
                outcome: "success".into(),
                artifacts: vec![
                    ArtifactRef { kind: "html".into(), content_hash: ContentHash::sha256(b"h") },
                    ArtifactRef { kind: "pdf".into(), content_hash: ContentHash::sha256(b"p") },
                ],
                merged_commit: Some("abc123".into()),
            }))
            .unwrap();
        store.append(at(20, None, "WF-2", trigger("manual"))).unwrap();

        let runs = workflow_runs(store.conn(), "daily", 10).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, "WF-2");
        assert_eq!(runs[0].trigger, "manual");
        assert!(runs[0].outcome.is_none(), "没有收尾事件就是未收尾,不能猜成成功");
        assert_eq!(runs[1].run_id, "WF-1");
        assert_eq!(runs[1].outcome.as_deref(), Some("success"));
        assert_eq!(runs[1].artifacts.len(), 2);
        assert_eq!(runs[1].artifacts[1].0, "pdf");
        assert_eq!(runs[1].merged_commit.as_deref(), Some("abc123"));
        assert_eq!(workflow_last_trigger(store.conn(), "daily").unwrap(), Some(20));
        assert_eq!(workflow_last_trigger(store.conn(), "other").unwrap(), None);
        assert!(workflow_runs(store.conn(), "other", 10).unwrap().is_empty());
    }

    /// 编制:按 actor 的 run 数与外发字节;工牌能力变更取最近一次。
    #[test]
    fn actor_stats_and_last_badge_update() {
        use crate::event::{ContentHash, EgressBytes};
        let mut store = AuditStore::open_in_memory().unwrap();
        let call = |actor: &str, run: &str, bytes: u64| NewEvent {
            ts_ms: Some(1_000),
            actor: Actor::agent(actor),
            scope: Scope::default(),
            run_id: Some(run.into()),
            session_id: None,
            policy_version: None,
            label: None,
            locality: Some(Locality::Cloud),
            body: EventBody::ModelCall {
                provider_id: "p".into(),
                model: "m".into(),
                locality: Locality::Cloud,
                label: Sensitivity::Open,
                tokens_in: None,
                tokens_out: None,
                bytes_in: 1,
                bytes_out: EgressBytes::Measured(bytes),
                latency_ms: 1,
                request_hash: ContentHash::sha256(b"r"),
            },
        };
        store.append(call("A-007", "RUN-1", 100)).unwrap();
        store.append(call("A-007", "RUN-1", 50)).unwrap();
        store.append(call("A-007", "RUN-2", 10)).unwrap();
        store.append(call("A-012", "RUN-3", 7)).unwrap();
        assert_eq!(actor_runs(store.conn(), "A-007").unwrap(), 2);
        assert_eq!(actor_egress_bytes(store.conn(), "A-007").unwrap(), 160);
        assert_eq!(actor_runs(store.conn(), "A-012").unwrap(), 1);
        assert_eq!(actor_runs(store.conn(), "A-099").unwrap(), 0);

        assert_eq!(last_badge_update(store.conn(), "A-007").unwrap(), None);
        for (v, h) in [(1u32, "sha256:aa"), (2, "sha256:bb")] {
            store
                .append(NewEvent {
                    ts_ms: Some(2_000 + v as u64),
                    actor: Actor::agent("A-007"),
                    scope: Scope::default(),
                    run_id: None,
                    session_id: None,
                    policy_version: None,
                    label: None,
                    locality: None,
                    body: EventBody::BadgeUpdate {
                        changed_by: Actor::system("roster"),
                        capabilities_hash: ContentHash(h.into()),
                        badge_version: v,
                    },
                })
                .unwrap();
        }
        assert_eq!(last_badge_update(store.conn(), "A-007").unwrap(), Some(("sha256:bb".into(), 2)));
    }
}
