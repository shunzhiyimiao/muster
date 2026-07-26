//! 哈希链:`hash = sha256(prev_hash || canonical_json(envelope+payload))`。
//!
//! 规范化依赖 serde_json 默认的 BTreeMap 键序(**禁止**开启 `preserve_order`
//! feature,否则历史哈希全部失效——已在 README「不做清单」注明)。
//! 一列哈希的成本,换「审计不可篡改」的尽调叙事;签名与外部锚定留给 v1.x。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::event::{AuditEvent, NewEvent};

/// 创世 prev_hash:64 个 0。
pub const GENESIS: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// 参与哈希的规范化视图(字段名即哈希契约的一部分,只增不改)。
fn canonical_view(
    event_id: &str,
    ts_ms: u64,
    e: &NewEvent,
    payload: &Value,
    schema_version: u32,
) -> Value {
    json!({
        "event_id": event_id,
        "ts_ms": ts_ms,
        "actor": e.actor,
        "scope": e.scope,
        "run_id": e.run_id,
        "session_id": e.session_id,
        "policy_version": e.policy_version,
        "label": e.label,
        "locality": e.locality,
        "schema_version": schema_version,
        "payload": payload,
    })
}

pub fn chain_hash(
    prev_hash: &str,
    event_id: &str,
    ts_ms: u64,
    e: &NewEvent,
    payload: &Value,
    schema_version: u32,
) -> String {
    let canonical = serde_json::to_vec(&canonical_view(event_id, ts_ms, e, payload, schema_version))
        .expect("canonical serialization cannot fail");
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(&canonical);
    format!("{:x}", h.finalize())
}

/// 校验用:从已落库行重建哈希(与 [`chain_hash`] 必须逐字节同构)。
pub fn recompute(prev_hash: &str, row: &AuditEvent) -> String {
    let e = NewEvent {
        ts_ms: Some(row.ts_ms),
        actor: row.actor.clone(),
        scope: row.scope.clone(),
        run_id: row.run_id.clone(),
        session_id: row.session_id.clone(),
        policy_version: row.policy_version.clone(),
        label: row.label,
        locality: row.locality,
        // body 不参与重建(payload 已是落库 JSON),放任意占位。
        body: crate::event::EventBody::DrillStart { drill_id: String::new() },
    };
    chain_hash(prev_hash, &row.event_id, row.ts_ms, &e, &row.payload, row.schema_version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, EventBody};

    #[test]
    fn hash_is_deterministic_and_prev_sensitive() {
        let e = NewEvent::new(Actor::system("router"), EventBody::DrillStart { drill_id: "D".into() });
        let p = serde_json::to_value(&e.body).unwrap();
        let a = chain_hash(GENESIS, "01AAA", 1000, &e, &p, 1);
        let b = chain_hash(GENESIS, "01AAA", 1000, &e, &p, 1);
        let c = chain_hash(&a, "01AAA", 1000, &e, &p, 1);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
    }
}
