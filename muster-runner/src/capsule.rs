//! P4 Capsule 锻造:把**一次成功的运行**固化成可复用能力。
//!
//! ## 为什么锻造的对象是"运行"而不是"提示词"
//!
//! 一段提示词只是意图;一次成功运行留下的是**完整证据**:在什么仓库快照上、
//! 用什么模型与参数、有哪些工具可用、做了什么、结果是否被人认可。Capsule
//! 复制的是后者([`muster_audit::ReplayRefs`] 原样取自 `run.start`),
//! 所以"照着它再跑一次"的条件天然齐备——这就是影子重放验真的前提。
//!
//! ## 三条硬规则
//!
//! 1. **没有出处不许锻造**:`source_run_id` 必填,且该运行必须
//!    成功结束并留有 `run.start`([`muster_audit::forgeable`] 把关)。
//! 2. **一次运行只锻一次**:重复锻造被拒,避免同一份证据派生出多个"同源不同版"。
//! 3. **定义正文不进审计**:只存 [`ContentHash`];正文属 Capsule 存储侧。

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use muster_audit::{
    forgeable, run_start_of, Actor, AuditStore, ContentHash, EventBody, NewEvent, ReplayRefs, Scope,
};

#[derive(Debug, thiserror::Error)]
pub enum CapsuleError {
    #[error("不可锻造:{0}")]
    NotForgeable(String),
    #[error("审计读写失败:{0}")]
    Audit(String),
    #[error("run.start 的 payload 无法解析重放引用:{0}")]
    BadReplay(String),
}

/// 能力定义(正文)。**存 Capsule 存储侧,审计只留其哈希**。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapsuleSpec {
    pub name: String,
    /// 这个能力解决什么问题(锻造时取自原任务提示词)。
    pub goal: String,
    /// 可用工具清单(取自源运行的实际工具环境)。
    pub tools: Vec<String>,
    /// 完成判据——**能力必须能被机器验证**,否则谈不上"验真"。
    pub verification: Vec<String>,
    /// 源运行的模型引用(重放时用同一模型才有可比性)。
    pub model: String,
}

impl CapsuleSpec {
    /// 规范化 JSON 的哈希(BTreeMap 键序,与审计哈希链同一口径)。
    pub fn content_hash(&self) -> ContentHash {
        let v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        ContentHash::sha256(v.to_string().as_bytes())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ForgeOutcome {
    pub capsule_id: String,
    pub version: String,
    pub content_hash: String,
    pub spec: CapsuleSpec,
}

/// 从一次成功运行锻造 Capsule。
///
/// `scope` 是可见范围(private / team / org);`badge` 是锻造发起者。
pub fn forge(
    audit: &Arc<Mutex<AuditStore>>,
    badge: &str,
    policy_version: &str,
    source_run_id: &str,
    spec: CapsuleSpec,
    visibility: &str,
    event_scope: Scope,
) -> Result<ForgeOutcome, CapsuleError> {
    // 规则 1 & 2:出处校验(成功结束 + 有 run.start + 未锻造过)
    let (ok, why) = {
        let store = audit.lock().unwrap();
        forgeable(store.conn(), source_run_id).map_err(|e| CapsuleError::Audit(e.to_string()))?
    };
    if !ok {
        return Err(CapsuleError::NotForgeable(why));
    }

    // 重放引用**原样取自源运行**,不重新计算——重算会得到"锻造时刻"的环境,
    // 而我们要的是"那次成功发生时"的环境。
    let replay: ReplayRefs = {
        let store = audit.lock().unwrap();
        let ev = run_start_of(store.conn(), source_run_id)
            .map_err(|e| CapsuleError::Audit(e.to_string()))?
            .ok_or_else(|| CapsuleError::NotForgeable("找不到 run.start".into()))?;
        serde_json::from_value(ev.payload["replay"].clone())
            .map_err(|e| CapsuleError::BadReplay(e.to_string()))?
    };

    let capsule_id = format!("CAP-{source_run_id}");
    let version = "1.0.0".to_string();
    let content_hash = spec.content_hash();

    audit
        .lock()
        .unwrap()
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::agent(badge),
            scope: event_scope,
            run_id: Some(source_run_id.to_owned()),
            session_id: None,
            policy_version: Some(policy_version.to_owned()),
            label: None,
            locality: None,
            body: EventBody::CapsuleForge {
                capsule_id: capsule_id.clone(),
                name: spec.name.clone(),
                version: version.clone(),
                source_run_id: source_run_id.to_owned(),
                replay,
                content_hash: content_hash.clone(),
                scope: visibility.to_owned(),
            },
        })
        .map_err(|e| CapsuleError::Audit(e.to_string()))?;

    Ok(ForgeOutcome { capsule_id, version, content_hash: content_hash.0, spec })
}

/// 从一次运行的事件链推导能力定义草稿。
///
/// **这是建议而非结论**:锻造是人的决定,草稿只是把散在事件里的事实
/// (任务目标、用过的工具、模型)聚拢起来,由人确认或改写后再落库。
pub fn draft_spec(
    audit: &Arc<Mutex<AuditStore>>,
    run_id: &str,
    goal: &str,
    tools: Vec<String>,
) -> Result<CapsuleSpec, CapsuleError> {
    let store = audit.lock().unwrap();
    let ev = run_start_of(store.conn(), run_id)
        .map_err(|e| CapsuleError::Audit(e.to_string()))?
        .ok_or_else(|| CapsuleError::NotForgeable("找不到 run.start".into()))?;
    let model = ev.payload["replay"]["model"]["model"].as_str().unwrap_or("?").to_string();
    let name: String = goal.chars().take(24).collect();
    Ok(CapsuleSpec {
        name: if name.is_empty() { format!("能力 {run_id}") } else { name },
        goal: goal.to_owned(),
        tools,
        // 默认判据:任务成功结束且产出了可复核的变更。人可在确认时改写。
        verification: vec![
            "run.finish 的 outcome 为 success".into(),
            "产出的 diff 经人工审批通过".into(),
        ],
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use muster_audit::{capsules, ModelRef, RunOutcome};

    fn store_with_run(run_id: &str, success: bool) -> Arc<Mutex<AuditStore>> {
        let mut s = AuditStore::open_in_memory().unwrap();
        let replay = ReplayRefs {
            repo_snapshot: ContentHash::sha256(b"git-head:deadbeef"),
            deps_lock: ContentHash::sha256(b"lock"),
            model: ModelRef {
                provider_id: "kimi".into(),
                model: "kimi-k3".into(),
                params_hash: ContentHash::sha256(b"p"),
            },
            tool_env: ContentHash::sha256(b"t"),
        };
        let mk = |body| NewEvent {
            ts_ms: None,
            actor: Actor::agent("A-007"),
            scope: Scope::default(),
            run_id: Some(run_id.to_owned()),
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body,
        };
        s.append(mk(EventBody::RunStart {
            task_kind: "chat.tools.v0".into(),
            replay,
            label: muster_route::Sensitivity::Open,
            locality_planned: muster_provider::Locality::Cloud,
        }))
        .unwrap();
        s.append(mk(EventBody::RunFinish {
            outcome: if success {
                RunOutcome::Success
            } else {
                RunOutcome::Failed { class: "stream".into() }
            },
            duration_ms: 10,
            output_hash: None,
        }))
        .unwrap();
        Arc::new(Mutex::new(s))
    }

    fn spec() -> CapsuleSpec {
        CapsuleSpec {
            name: "修复算术 bug".into(),
            goal: "把 add 的减法改成加法".into(),
            tools: vec!["read_file".into(), "replace_in_file".into()],
            verification: vec!["run.finish=success".into()],
            model: "kimi-k3".into(),
        }
    }

    /// 锻造的 ReplayRefs 必须**原样取自源运行**——重算会得到锻造时刻的环境,
    /// 而重放需要的是那次成功发生时的环境。
    #[test]
    fn forged_capsule_carries_source_run_replay_refs() {
        let audit = store_with_run("RUN-1", true);
        let out = forge(&audit, "A-007", "policy-v1", "RUN-1", spec(), "team", Scope::default())
            .unwrap();
        assert_eq!(out.capsule_id, "CAP-RUN-1");

        let store = audit.lock().unwrap();
        let rows = capsules(store.conn()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_run_id, "RUN-1", "能力必须带着出处");
        assert_eq!(rows[0].verified_rate(), None, "刚锻造未验真");

        // 事件里的 replay 与源运行 run.start 的 replay 逐字节一致
        let chain = muster_audit::run_chain(store.conn(), "RUN-1").unwrap();
        let start = chain.iter().find(|e| e.payload["event_type"] == "run.start").unwrap();
        let forge_ev = chain.iter().find(|e| e.payload["event_type"] == "capsule.forge").unwrap();
        assert_eq!(start.payload["replay"], forge_ev.payload["replay"]);
        // 定义正文不入表,只有哈希
        assert!(forge_ev.payload.get("goal").is_none());
        assert_eq!(forge_ev.payload["content_hash"], out.content_hash);
        assert!(store.verify_chain().unwrap().is_ok());
    }

    #[test]
    fn failed_run_and_double_forge_are_refused() {
        let bad = store_with_run("RUN-F", false);
        let e = forge(&bad, "A-007", "policy-v1", "RUN-F", spec(), "team", Scope::default())
            .unwrap_err();
        assert!(matches!(e, CapsuleError::NotForgeable(ref w) if w.contains("未成功")), "{e}");

        let ok = store_with_run("RUN-2", true);
        forge(&ok, "A-007", "policy-v1", "RUN-2", spec(), "team", Scope::default()).unwrap();
        let e = forge(&ok, "A-007", "policy-v1", "RUN-2", spec(), "team", Scope::default())
            .unwrap_err();
        assert!(matches!(e, CapsuleError::NotForgeable(ref w) if w.contains("已锻造过")), "{e}");
    }

    #[test]
    fn draft_pulls_facts_from_the_run() {
        let audit = store_with_run("RUN-3", true);
        let d = draft_spec(&audit, "RUN-3", "把 add 的减法改成加法", vec!["read_file".into()])
            .unwrap();
        assert_eq!(d.model, "kimi-k3", "模型取自源运行的重放引用");
        assert!(d.goal.contains("减法"));
        assert!(!d.verification.is_empty(), "必须有机器可判的完成判据");
    }
}
