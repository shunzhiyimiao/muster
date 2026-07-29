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
    pub egress_bytes: u64,
    pub unmetered_calls: u64,
    pub local_calls: u64,
    pub cloud_calls: u64,
}

impl DrillReport {
    pub fn ok(&self) -> bool {
        self.unmetered_calls == 0 && self.egress_bytes == 0
    }
}

/// 演习窗口聚合(闭区间毫秒时间戳)。
pub const SQL_DRILL_REPORT: &str = r#"
SELECT
  COUNT(*),
  COALESCE(SUM(CASE WHEN json_type(payload,'$.bytes_out') = 'object'
                    THEN json_extract(payload,'$.bytes_out.measured') ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN json_extract(payload,'$.bytes_out') = 'unmetered' THEN 1 ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN locality = 'local' THEN 1 ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN locality = 'cloud' THEN 1 ELSE 0 END), 0)
FROM audit_event
WHERE event_type = 'model.call' AND ts_ms BETWEEN ?1 AND ?2
"#;

pub fn drill_report(conn: &Connection, from_ms: u64, to_ms: u64) -> Result<DrillReport, StoreError> {
    let r = conn.query_row(SQL_DRILL_REPORT, params![from_ms as i64, to_ms as i64], |r| {
        Ok(DrillReport {
            model_calls: r.get::<_, i64>(0)? as u64,
            egress_bytes: r.get::<_, i64>(1)? as u64,
            unmetered_calls: r.get::<_, i64>(2)? as u64,
            local_calls: r.get::<_, i64>(3)? as u64,
            cloud_calls: r.get::<_, i64>(4)? as u64,
        })
    })?;
    Ok(r)
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
    use crate::event::{Actor, ContentHash, EgressBytes, EventBody, NewEvent, Scope};
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
}
