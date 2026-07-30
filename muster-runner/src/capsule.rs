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

/// 锻造时把正文写入存储侧的便利封装:审计存哈希、存储存正文,两者由
/// [`CapsuleStore::load`] 的哈希校验绑定。
pub fn forge_and_store(
    audit: &Arc<Mutex<AuditStore>>,
    store: &CapsuleStore,
    badge: &str,
    policy_version: &str,
    source_run_id: &str,
    spec: CapsuleSpec,
    visibility: &str,
    event_scope: Scope,
) -> Result<ForgeOutcome, CapsuleError> {
    let out = forge(audit, badge, policy_version, source_run_id, spec, visibility, event_scope)?;
    store.save(&out.capsule_id, &out.spec)?;
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum CapsuleError {
    #[error("不可锻造:{0}")]
    NotForgeable(String),
    #[error("审计读写失败:{0}")]
    Audit(String),
    #[error("run.start 的 payload 无法解析重放引用:{0}")]
    BadReplay(String),
    #[error("Capsule 存储失败:{0}")]
    Storage(String),
    #[error("Capsule {capsule_id} 的定义正文与审计哈希不符(期望 {expect},实际 {actual})——已被篡改,拒绝使用")]
    Tampered { capsule_id: String, expect: String, actual: String },
    #[error("找不到 Capsule {0} 的锻造记录")]
    NotFound(String),
    /// **不是验真失败,是没法验真**——两者绝不可混为一谈(见 verify 文档)。
    #[error("无法验真:{0}")]
    Unverifiable(String),
    #[error("重放执行失败:{0}")]
    Replay(String),
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

// ---------------------------------------------------------------- 正文存储

/// Capsule 定义正文的存储侧。
///
/// **为什么必须有它**:铁律 3 规定审计只存哈希不存正文,可重放又需要正文
/// (goal、判据)。于是正文落在这里,审计里的 `content_hash` 成为它的
/// **防篡改校验和**——[`CapsuleStore::load`] 每次都比对哈希,对不上就拒绝加载。
/// 这正是"只存哈希"这条铁律的价值兑现处,而不是它的代价。
pub struct CapsuleStore {
    dir: std::path::PathBuf,
}

impl CapsuleStore {
    pub fn open(dir: impl Into<std::path::PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, capsule_id: &str) -> std::path::PathBuf {
        self.dir.join(format!("{capsule_id}.json"))
    }

    pub fn save(&self, capsule_id: &str, spec: &CapsuleSpec) -> Result<(), CapsuleError> {
        let json = serde_json::to_string_pretty(spec)
            .map_err(|e| CapsuleError::Storage(e.to_string()))?;
        std::fs::write(self.path(capsule_id), json).map_err(|e| CapsuleError::Storage(e.to_string()))
    }

    /// 加载并**用审计里的哈希验真**:正文被改过就拒绝加载,不静默用脏数据重放。
    pub fn load(&self, capsule_id: &str, expect: &ContentHash) -> Result<CapsuleSpec, CapsuleError> {
        let raw = std::fs::read_to_string(self.path(capsule_id))
            .map_err(|e| CapsuleError::Storage(format!("读取 {capsule_id} 失败:{e}")))?;
        let spec: CapsuleSpec =
            serde_json::from_str(&raw).map_err(|e| CapsuleError::Storage(e.to_string()))?;
        let actual = spec.content_hash();
        if &actual != expect {
            return Err(CapsuleError::Tampered {
                capsule_id: capsule_id.to_owned(),
                expect: expect.0.clone(),
                actual: actual.0,
            });
        }
        Ok(spec)
    }
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

// ---------------------------------------------------------------- 影子重放验真

/// 一次影子重放的结果。
#[derive(Debug, Clone, Serialize)]
pub struct VerifyOutcome {
    pub capsule_id: String,
    pub version: String,
    /// 本次重放的运行号。
    pub run_id: String,
    pub passed: bool,
    /// 判定说明(人读)。
    pub detail: String,
    /// 源运行的产出哈希(期望)。
    pub expected_hash: String,
    /// 本次重放的产出哈希(实际)。
    pub actual_hash: String,
}

/// 从审计取回锻造记录:(ReplayRefs, 内容哈希, 版本, 源运行号)。
fn forge_record(
    audit: &Arc<Mutex<AuditStore>>,
    capsule_id: &str,
) -> Result<(ReplayRefs, ContentHash, String, String), CapsuleError> {
    let store = audit.lock().unwrap();
    let evs = muster_audit::recent_events_of(store.conn(), "capsule.forge", 200)
        .map_err(|e| CapsuleError::Audit(e.to_string()))?;
    let ev = evs
        .into_iter()
        .find(|e| e.payload["capsule_id"].as_str() == Some(capsule_id))
        .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_owned()))?;
    let replay: ReplayRefs = serde_json::from_value(ev.payload["replay"].clone())
        .map_err(|e| CapsuleError::BadReplay(e.to_string()))?;
    let hash = ContentHash(ev.payload["content_hash"].as_str().unwrap_or_default().to_owned());
    let version = ev.payload["version"].as_str().unwrap_or("1.0.0").to_owned();
    let source_run = ev.payload["source_run_id"].as_str().unwrap_or_default().to_owned();
    Ok((replay, hash, version, source_run))
}

/// 源运行的产出哈希(比对基准)。
fn source_output_hash(
    audit: &Arc<Mutex<AuditStore>>,
    source_run: &str,
) -> Result<String, CapsuleError> {
    let store = audit.lock().unwrap();
    let chain =
        muster_audit::run_chain(store.conn(), source_run).map_err(|e| CapsuleError::Audit(e.to_string()))?;
    chain
        .iter()
        .find(|e| e.payload["event_type"] == "run.finish")
        .and_then(|e| e.payload["output_hash"].as_str().map(str::to_owned))
        .ok_or_else(|| CapsuleError::Unverifiable("源运行没有产出哈希,无从比对".into()))
}

/// **影子重放验真**:在与锻造时相同的环境条件下重跑一次,比对产出。
///
/// ## 三种结局,不是两种
///
/// | 结局 | 含义 | 落库 |
/// |---|---|---|
/// | `passed = true` | 重放产出与源运行一致 | `capsule.verify` |
/// | `passed = false` | 重放产出不一致——能力不可靠,或环境有隐含依赖 | `capsule.verify` |
/// | `Err(Unverifiable)` | **没法验真**(环境已漂移等) | **不落库** |
///
/// 第三种最容易被做错。若环境漂移时也记一条 `passed=false`,验真率的分母就被
/// 污染了——那是在用"我们没条件验"冒充"它验失败了"。所以此处宁可报错也不落库。
///
/// ## 诚实边界
///
/// - **单次重放不足以下结论**:模型有采样以外的非确定性,一次不符不等于能力坏了。
///   累计口径由 [`muster_audit::CapsuleRow::verified_rate`] 给出,看的是多次的比例。
/// - **哈希相同 ⇒ 通过,不同 ⇒ 存疑而非确凿失败**:两段语义等价的 diff 哈希不同
///   是常态。本函数不做语义等价判定(那需要跑测试或人来看),只如实记录差异,
///   把"是否等价"留给人与后续的判据(spec.verification)。
/// - **⚠️ 使用前提:必须在与锻造时同一基线的工作区上重放**。
///
///   实测发现的陷阱:典型流程是「任务 → 审批合入 → 锻造」,合入那一刻主仓
///   HEAD 就变了,于是**紧接着在主仓上验真必然报漂移**。这不是 bug,是本函数
///   在如实拒绝一次无意义的比较——但它意味着正确用法是:在合入**之前**验真,
///   或把仓库检出到锻造时的那个 commit 的副本上再验。
///
///   根因值得记一笔:[`ReplayRefs::repo_snapshot`] 存的是 `sha256("git-head:<commit>")`,
///   **哈希无法反推 commit**,所以本函数没法自动把工作区切到正确基线。
///   commit hash 本身已是内容寻址,再套一层 sha256 属于信息损失。改进方向是让
///   重放引用保留可用于检出的原值——那要动 A9 的核心类型与既有哈希链,
///   留待专门一轮处理,不在此处顺手改。
pub async fn verify(
    router: &muster_route::Router,
    audit: &Arc<Mutex<AuditStore>>,
    store: &CapsuleStore,
    cfg: &crate::runner::RunnerConfig,
    capsule_id: &str,
    workspace: &std::path::Path,
    workspace_root: &std::path::Path,
    mut on_event: impl FnMut(crate::runner::RunnerEvent) + Send,
) -> Result<VerifyOutcome, CapsuleError> {
    let (replay, content_hash, version, source_run) = forge_record(audit, capsule_id)?;

    // 前置检查按"最根本且最便宜"排序:
    // ① 环境漂移——环境都不是同一个了,后面一切比对都没有意义,先挡在这里
    let now_snapshot = crate::runner::repo_snapshot_of(workspace);
    if now_snapshot != replay.repo_snapshot {
        return Err(CapsuleError::Unverifiable(format!(
            "仓库快照已漂移(锻造时 {} / 现在 {}),同一份能力在不同代码基线上的产出本就不可比",
            &replay.repo_snapshot.0[..replay.repo_snapshot.0.len().min(19)],
            &now_snapshot.0[..now_snapshot.0.len().min(19)]
        )));
    }
    // ② 正文完整性——被改过就拒绝重放(审计哈希是它的校验和)
    let spec = store.load(capsule_id, &content_hash)?;
    // ③ 比对基准存在
    let expected_hash = source_output_hash(audit, &source_run)?;

    // 重放:同一 goal、同一工作区、隔离 worktree(影子=不碰主仓)
    let run_id = format!("RUN-VERIFY-{capsule_id}-{}", std::process::id());
    let spec_prompt = spec.goal.clone();
    let summary = crate::runner::run_task(
        router,
        audit,
        cfg,
        crate::runner::TaskSpec {
            run_id: run_id.clone(),
            session_id: Some(format!("verify:{capsule_id}")),
            team: None,
            channel: None,
            sources: vec![],
            requested_provider: None,
            default_provider: Some(replay.model.provider_id.clone()),
            prompt: spec_prompt,
            workspace: workspace.to_path_buf(),
            workspace_root: Some(workspace_root.to_path_buf()),
        },
        |e| on_event(e),
    )
    .await
    .map_err(|e| CapsuleError::Replay(e.to_string()))?;

    let actual_hash = summary
        .diff
        .as_ref()
        .filter(|d| !d.is_empty())
        .map(|d| ContentHash::sha256(d.patch.as_bytes()).0)
        .unwrap_or_else(|| ContentHash::sha256(summary.final_text.as_bytes()).0);

    let passed = actual_hash == expected_hash;
    let detail = if passed {
        "重放产出与源运行逐字节一致".to_string()
    } else {
        "重放产出与源运行不一致——可能是等价的另一种写法,也可能是能力不稳定;需人工判读".to_string()
    };

    // 证据:比对了什么(两侧哈希 + 判据),正文不入表
    let evidence = serde_json::json!({
        "expected": expected_hash, "actual": actual_hash,
        "verification": spec.verification, "source_run": source_run,
    })
    .to_string();

    audit
        .lock()
        .unwrap()
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::agent(&cfg.badge),
            scope: Scope::default(),
            run_id: Some(run_id.clone()),
            session_id: None,
            policy_version: Some(cfg.policy_version.clone()),
            label: None,
            locality: None,
            body: EventBody::CapsuleVerify {
                capsule_id: capsule_id.to_owned(),
                version: version.clone(),
                run_id: run_id.clone(),
                passed,
                evidence_hash: ContentHash::sha256(evidence.as_bytes()),
            },
        })
        .map_err(|e| CapsuleError::Audit(e.to_string()))?;

    Ok(VerifyOutcome {
        capsule_id: capsule_id.to_owned(),
        version,
        run_id,
        passed,
        detail,
        expected_hash,
        actual_hash,
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

    /// 存储侧正文被改 ⇒ 哈希对不上 ⇒ **拒绝加载**,不静默用脏数据重放。
    /// 这是"审计只存哈希"这条铁律的价值兑现处。
    #[test]
    fn tampered_spec_is_refused_by_hash_check() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapsuleStore::open(dir.path()).unwrap();
        let audit = store_with_run("RUN-T", true);
        let out = forge_and_store(
            &audit, &store, "A-007", "policy-v1", "RUN-T", spec(), "team", Scope::default(),
        )
        .unwrap();

        // 原样加载:通过
        let loaded = store.load(&out.capsule_id, &ContentHash(out.content_hash.clone())).unwrap();
        assert_eq!(loaded.goal, spec().goal);

        // 有人偷偷改了正文(把判据删掉)
        let mut evil = spec();
        evil.verification.clear();
        store.save(&out.capsule_id, &evil).unwrap();

        let err = store
            .load(&out.capsule_id, &ContentHash(out.content_hash.clone()))
            .unwrap_err();
        assert!(matches!(err, CapsuleError::Tampered { .. }), "{err}");
        assert!(err.to_string().contains("已被篡改"), "错误必须说清是篡改:{err}");
    }

    /// 环境漂移是"没法验真",**不是**"验真失败"——不能写进 capsule.verify,
    /// 否则验真率的分母被污染,等于用"我们没条件验"冒充"它验失败了"。
    #[tokio::test]
    async fn environment_drift_yields_unverifiable_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapsuleStore::open(dir.path()).unwrap();
        let audit = store_with_run("RUN-D", true);
        let out = forge_and_store(
            &audit, &store, "A-007", "policy-v1", "RUN-D", spec(), "team", Scope::default(),
        )
        .unwrap();

        // 重放目标是一个与锻造时完全不同的目录 ⇒ 仓库快照必然不符
        let elsewhere = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let router = muster_route::Router::new(
            vec![Arc::new(muster_provider::MockProvider::cloud("m").with_text("x"))
                as Arc<dyn muster_provider::ModelProvider>],
            muster_route::OrgPolicy::new(muster_route::Sensitivity::Internal).unwrap(),
        );

        let err = verify(
            &router,
            &audit,
            &store,
            &crate::runner::RunnerConfig::default(),
            &out.capsule_id,
            elsewhere.path(),
            root.path(),
            |_| {},
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CapsuleError::Unverifiable(_)), "{err}");
        assert!(err.to_string().contains("漂移"), "{err}");

        // **一条 capsule.verify 都不该写**——验真率的分母不能被污染
        let s = audit.lock().unwrap();
        let rows = capsules(s.conn()).unwrap();
        assert_eq!(rows[0].verify_total, 0, "没法验真时不得记账");
        assert_eq!(rows[0].verified_rate(), None);
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
