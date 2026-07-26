//! SQLite 存储:单表 append-only,信封列固定、payload JSON、哈希链。
//!
//! **没有 UPDATE/DELETE API**——这不是疏忽,是接口即政策。保留/删除策略与
//! append-only 的冲突是已登记的 open question(方案方向:正文侧加密擦除,
//! 审计只留哈希),MVP 不解。

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::event::{
    Actor, ActorKind, AuditEvent, NewEvent, Scope, SCHEMA_VERSION,
};
use crate::hash::{chain_hash, recompute, GENESIS};
use crate::id::UlidGen;

/// 迁移脚本 v1(嵌入常量,服务器侧 C1 直接复用)。
/// 索引由 8 幕反推:run_id(事件链/Capsule 锻造)、ts_ms(演习窗口)、
/// actor_id(工牌页三宫格数字)、event_type+ts_ms(审批监控/降级流)。
pub const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS audit_event (
    event_id       TEXT PRIMARY KEY,
    ts_ms          INTEGER NOT NULL,
    actor_kind     TEXT NOT NULL,
    actor_id       TEXT NOT NULL,
    event_type     TEXT NOT NULL,
    run_id         TEXT,
    session_id     TEXT,
    team           TEXT,
    channel        TEXT,
    label          TEXT,
    locality       TEXT,
    policy_version TEXT,
    schema_version INTEGER NOT NULL,
    payload        TEXT NOT NULL,
    prev_hash      TEXT NOT NULL,
    hash           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_run        ON audit_event(run_id);
CREATE INDEX IF NOT EXISTS idx_audit_ts         ON audit_event(ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_actor      ON audit_event(actor_id, ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_type_ts    ON audit_event(event_type, ts_ms);
"#;

#[derive(Debug)]
pub enum StoreError {
    Db(rusqlite::Error),
    Json(serde_json::Error),
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Db(e) => write!(f, "sqlite: {e}"),
            StoreError::Json(e) => write!(f, "json: {e}"),
        }
    }
}
impl std::error::Error for StoreError {}

/// 链校验失败:第 `index` 行(0 起)的 `hash` 或 `prev_hash` 对不上。
#[derive(Debug, PartialEq, Eq)]
pub struct ChainError {
    pub index: u64,
    pub event_id: String,
}

pub struct AuditStore {
    conn: Connection,
    ids: UlidGen,
}

impl AuditStore {
    pub fn open(path: &str) -> Result<Self, StoreError> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(MIGRATION_V1)?;
        Ok(Self { conn, ids: UlidGen::default() })
    }

    /// 追加一条事件,返回落库形态(含生成的 id 与链哈希)。
    pub fn append(&mut self, e: NewEvent) -> Result<AuditEvent, StoreError> {
        let ts_ms = e.ts_ms.unwrap_or_else(now_ms);
        let event_id = self.ids.next(Some(ts_ms));
        let payload: Value = serde_json::to_value(&e.body)?;
        let prev_hash: String = self
            .conn
            .query_row(
                "SELECT hash FROM audit_event ORDER BY event_id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| GENESIS.to_string());
        let hash = chain_hash(&prev_hash, &event_id, ts_ms, &e, &payload, SCHEMA_VERSION);

        self.conn.execute(
            "INSERT INTO audit_event (event_id, ts_ms, actor_kind, actor_id, event_type,
                run_id, session_id, team, channel, label, locality, policy_version,
                schema_version, payload, prev_hash, hash)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                event_id,
                ts_ms as i64,
                enum_str(&e.actor.kind)?,
                e.actor.id,
                e.body.event_type(),
                e.run_id,
                e.session_id,
                e.scope.team,
                e.scope.channel,
                opt_enum_str(&e.label)?,
                opt_enum_str(&e.locality)?,
                e.policy_version,
                SCHEMA_VERSION,
                payload.to_string(),
                prev_hash,
                hash,
            ],
        )?;

        Ok(AuditEvent {
            event_id,
            ts_ms,
            actor: e.actor,
            scope: e.scope,
            run_id: e.run_id,
            session_id: e.session_id,
            policy_version: e.policy_version,
            label: e.label,
            locality: e.locality,
            schema_version: SCHEMA_VERSION,
            payload,
            prev_hash,
            hash,
        })
    }

    /// 全链校验:逐行重算哈希。返回校验过的行数;任何一行对不上即返回其位置。
    pub fn verify_chain(&self) -> Result<Result<u64, ChainError>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, ts_ms, actor_kind, actor_id, run_id, session_id, team, channel,
                    label, locality, policy_version, schema_version, payload, prev_hash, hash
             FROM audit_event ORDER BY event_id ASC",
        )?;
        let rows = stmt.query_map([], row_to_event)?;
        let mut prev = GENESIS.to_string();
        let mut n: u64 = 0;
        for row in rows {
            let ev = row?;
            if ev.prev_hash != prev || recompute(&prev, &ev) != ev.hash {
                return Ok(Err(ChainError { index: n, event_id: ev.event_id }));
            }
            prev = ev.hash.clone();
            n += 1;
        }
        Ok(Ok(n))
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 仅测试用:直接改 payload 模拟篡改(生产接口没有任何 UPDATE 路径)。
    #[doc(hidden)]
    pub fn tamper_for_test(&self, event_id: &str, payload: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE audit_event SET payload = ?1 WHERE event_id = ?2",
            params![payload, event_id],
        )?;
        Ok(())
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// snake_case 枚举 → 存储字符串(与 serde rename 保持同一来源)。
fn enum_str<T: serde::Serialize>(v: &T) -> Result<String, StoreError> {
    Ok(serde_json::to_value(v)?
        .as_str()
        .expect("enum must serialize to string")
        .to_string())
}

fn opt_enum_str<T: serde::Serialize>(v: &Option<T>) -> Result<Option<String>, StoreError> {
    v.as_ref().map(|x| enum_str(x)).transpose()
}

pub(crate) fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEvent> {
    let actor_kind: String = r.get(2)?;
    let label: Option<String> = r.get(8)?;
    let locality: Option<String> = r.get(9)?;
    let payload_str: String = r.get(12)?;
    Ok(AuditEvent {
        event_id: r.get(0)?,
        ts_ms: r.get::<_, i64>(1)? as u64,
        actor: Actor {
            kind: parse_enum::<ActorKind>(&actor_kind),
            id: r.get(3)?,
        },
        run_id: r.get(4)?,
        session_id: r.get(5)?,
        scope: Scope { team: r.get(6)?, channel: r.get(7)? },
        label: label.map(|s| parse_enum(&s)),
        locality: locality.map(|s| parse_enum(&s)),
        policy_version: r.get(10)?,
        schema_version: r.get::<_, i64>(11)? as u32,
        payload: serde_json::from_str(&payload_str).unwrap_or(Value::Null),
        prev_hash: r.get(13)?,
        hash: r.get(14)?,
    })
}

fn parse_enum<T: serde::de::DeserializeOwned>(s: &str) -> T {
    serde_json::from_value(Value::String(s.to_string())).expect("stored enum string must parse")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, EventBody};

    fn drill(id: &str) -> EventBody {
        EventBody::DrillStart { drill_id: id.into() }
    }

    #[test]
    fn append_builds_a_verifiable_chain() {
        let mut s = AuditStore::open_in_memory().unwrap();
        for i in 0..5 {
            s.append(NewEvent::new(Actor::system("router"), drill(&format!("D{i}"))).at(1000 + i))
                .unwrap();
        }
        assert_eq!(s.verify_chain().unwrap(), Ok(5));
    }

    #[test]
    fn tampering_is_detected_at_exact_row() {
        let mut s = AuditStore::open_in_memory().unwrap();
        let mut ids = vec![];
        for i in 0..4 {
            ids.push(
                s.append(NewEvent::new(Actor::system("x"), drill(&i.to_string())).at(2000 + i))
                    .unwrap()
                    .event_id,
            );
        }
        s.tamper_for_test(&ids[2], r#"{"event_type":"drill.start","drill_id":"EVIL"}"#).unwrap();
        match s.verify_chain().unwrap() {
            Err(ChainError { index, event_id }) => {
                assert_eq!(index, 2);
                assert_eq!(event_id, ids[2]);
            }
            Ok(n) => panic!("tamper undetected, chain reported {n} ok rows"),
        }
    }
}
