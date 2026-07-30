//! SQLite 存储:单表 append-only,信封列固定、payload JSON、哈希链。
//!
//! **没有 UPDATE/DELETE API**——这不是疏忽,是接口即政策。
//!
//! 那保留/删除怎么办?答案不在这一层:**删正文,不删证据**。链里从来只有
//! 哈希,所以正文存储侧整个删光,`verify_chain()` 仍然通过
//! (见 `deleting_the_plaintext_store_does_not_break_the_chain`)。
//! 换成"审计表里存全文"的设计,此刻就只能在"破坏证据链"和"留着不该留的
//! 东西"之间二选一。
//!
//! 仍未做的是**加密擦除**(正文加密、按需销毁密钥),以及正文侧的保留期与
//! 导出——那属于桌面壳的 state.db,不属于本 crate。

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
        // WAL + FULL:审计库是全系统**唯一不能丢一条**的东西,持久化姿态
        // 必须比别的库更紧,而不是更松。
        //
        // - `journal_mode=WAL`:rollback journal 模式下读会被写阻塞——UI 查
        //   审计中心的同时后台在写 command.run,就会撞上;且崩溃恢复窗口更大。
        //   内存库不支持 WAL,失败按普通错误吞掉(`query_row` 会返回新模式名)。
        // - `synchronous=FULL`:每次提交都落盘。审计写失败即任务失败
        //   (fail-closed),那就不能允许"以为写了其实在 OS 缓存里"。
        let _: Result<String, _> = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0));
        conn.execute_batch("PRAGMA synchronous=FULL;")?;
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
    use crate::event::{Actor, ContentHash, EventBody};

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

    /// 落盘的库开着 WAL。审计库是唯一不能丢一条的东西,持久化姿态必须比
    /// 别的库更紧:rollback journal 下读写互阻,且崩溃恢复窗口更大。
    #[test]
    fn on_disk_store_uses_wal_and_full_sync() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("a.db");
        let s = AuditStore::open(path.to_str().unwrap()).unwrap();
        let mode: String = s.conn().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "审计库必须走 WAL");
        let sync: i64 = s.conn().query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
        assert_eq!(sync, 2, "synchronous 必须是 FULL——审计写失败即任务失败,不能停在 OS 缓存里");
    }

    /// **正文可删,证据不坏**——铁律三真正买到的东西。
    ///
    /// 密钥被粘进对话、或有人要求删除某段记录时,删掉正文存储侧那几行,
    /// 审计链**照样验得过**,因为链里从来没装过正文。换成"审计表存全文"
    /// 的设计,此刻就只能在"破坏证据链"和"留着不该留的东西"之间二选一。
    #[test]
    fn deleting_the_plaintext_store_does_not_break_the_chain() {
        let d = tempfile::tempdir().unwrap();
        let audit_path = d.path().join("audit.db");
        let plaintext_path = d.path().join("state.db");

        let mut audit = AuditStore::open(audit_path.to_str().unwrap()).unwrap();
        // 正文存储侧:与桌面壳的 state.db 同形态(对话原文明文落这里)
        let plain = Connection::open(&plaintext_path).unwrap();
        plain.execute_batch("CREATE TABLE messages(id INTEGER PRIMARY KEY, text TEXT)").unwrap();

        for i in 0..3 {
            let body = crate::event::EventBody::ModelCall {
                provider_id: "p".into(),
                model: "m".into(),
                locality: muster_provider::Locality::Local,
                label: muster_route::Sensitivity::Open,
                tokens_in: None,
                tokens_out: None,
                bytes_in: 1,
                bytes_out: crate::event::EgressBytes::Measured(1),
                latency_ms: 1,
                // 审计侧只有正文的哈希
                request_hash: ContentHash::sha256(format!("绝密内容 {i}").as_bytes()),
            };
            audit.append(NewEvent::new(Actor::agent("A-007"), body).at(1000 + i)).unwrap();
            plain
                .execute("INSERT INTO messages(text) VALUES(?1)", params![format!("绝密内容 {i}")])
                .unwrap();
        }
        assert_eq!(audit.verify_chain().unwrap(), Ok(3));

        // 审计库里搜不到任何一句正文(铁律三:只存哈希)
        let leaked: i64 = audit
            .conn()
            .query_row("SELECT COUNT(*) FROM audit_event WHERE payload LIKE '%绝密内容%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(leaked, 0, "正文不得出现在审计表里");

        // 把正文连库带文件一起销毁
        drop(plain);
        std::fs::remove_file(&plaintext_path).unwrap();

        // 链依旧完整:说得清"发生过什么、有没有被动过",只是读不到"说了什么"
        let reopened = AuditStore::open(audit_path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.verify_chain().unwrap(), Ok(3), "删正文不该动摇证据链");
    }
}
