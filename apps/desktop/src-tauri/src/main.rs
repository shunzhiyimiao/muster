//! Muster 点将台桌面壳(P1-07/08/09 纵切):
//! 频道消息 → E2 路由决策(徽章数据)→ A2 流式模型调用 → A9 审计落库。
//!
//! 形态说明:B1 Runner / Worktree / Diff 落地后,`send_message` 将改为驱动
//! 完整任务;当前是"真实决策 + 真实流式 + 真实审计"的最小闭环,不含工具执行,
//! 因此审计只写 route.decide / model.call 两类**如实发生**的事件
//! (run.start 的 ReplayRefs 要求仓库快照等硬引用,聊天形态尚无,不伪造)。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::Serialize;
use tauri::{Emitter, State};

use muster_audit::{
    drill_report, recent_events, Actor, AuditStore, ContentHash, EgressBytes, EventBody, NewEvent,
    Scope,
};
use muster_provider::{ChatMessage, ChatRequest, Locality, ProviderRegistry, StreamEvent};
use muster_route::{LabelOrigin, LabelSource, OrgPolicy, RoutePlan, RouteRequest, Router, Sensitivity};

const POLICY_VERSION: &str = "policy-v1";
const AGENT_BADGE: &str = "A-007";
const SYSTEM_PROMPT: &str =
    "你是 Muster 点将台的协作 Agent(工牌 A-007)。用中文回答,简洁、直接。";

// ---------------------------------------------------------------- 演示编制

#[derive(Clone, Serialize)]
struct ChannelInfo {
    id: String,
    name: String,
    team: String,
    level: Sensitivity,
    /// 徽章悬浮:密级从哪来(与 label_sources 保持一致)。
    level_note: String,
    desc: String,
}

fn demo_channels() -> Vec<ChannelInfo> {
    vec![
        ChannelInfo {
            id: "general".into(),
            name: "平台组·大厅".into(),
            team: "平台组".into(),
            level: Sensitivity::Open,
            level_note: "未贴标签,默认 open(产品决策:未标注可走云端)".into(),
            desc: "日常讨论,允许云端模型".into(),
        },
        ChannelInfo {
            id: "platform-internal".into(),
            name: "平台组·内部".into(),
            team: "平台组".into(),
            level: Sensitivity::Internal,
            level_note: "频道标签 internal(cloud_max=internal,恰在云端许可上限)".into(),
            desc: "内部事项,internal ≤ cloud_max,仍可云端".into(),
        },
        ChannelInfo {
            id: "pay-core".into(),
            name: "支付组·核心库".into(),
            team: "支付组".into(),
            level: Sensitivity::Restricted,
            level_note: "仓库标签 restricted(repo:pay-core)——硬编码不变量:永不上云".into(),
            desc: "restricted:仅本地执行;本地不可用即拒绝(fail-closed)".into(),
        },
    ]
}

fn label_sources(channel_id: &str) -> Vec<LabelSource> {
    match channel_id {
        "platform-internal" => vec![LabelSource::new(
            LabelOrigin::Channel,
            Sensitivity::Internal,
            "channel:平台组·内部",
        )],
        "pay-core" => vec![LabelSource::new(
            LabelOrigin::Repo,
            Sensitivity::Restricted,
            "repo:pay-core",
        )],
        _ => vec![],
    }
}

// ---------------------------------------------------------------- 状态

/// 进行中的演习(E6):id + 起始时刻,结束时用 drill_report 聚合窗口。
struct DrillState {
    id: String,
    from_ms: u64,
}

struct Backend {
    router: Arc<Router>,
    audit: Arc<Mutex<AuditStore>>,
    run_seq: Arc<AtomicU64>,
    drill: Arc<Mutex<Option<DrillState>>>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("时钟早于 epoch").as_millis() as u64
}

#[derive(Default)]
struct AppState(Mutex<Option<Backend>>);

// ---------------------------------------------------------------- 载荷

#[derive(Serialize, Clone)]
struct ProviderCard {
    id: String,
    display_name: String,
    model: String,
    locality: String,
}

#[derive(Serialize)]
struct BootstrapInfo {
    channels: Vec<ChannelInfo>,
    providers: Vec<ProviderCard>,
    policy_cloud_max: Sensitivity,
    audit_db: String,
    egress_locked: bool,
}

#[derive(Serialize)]
struct DrillReportOut {
    model_calls: u64,
    egress_bytes: u64,
    unmetered_calls: u64,
    local_calls: u64,
    cloud_calls: u64,
    ok: bool,
}

#[derive(Serialize)]
struct DrillStatus {
    on: bool,
    drill_id: Option<String>,
    report: Option<DrillReportOut>,
}

#[derive(Serialize, Clone)]
struct StartPayload {
    run_id: String,
    channel_id: String,
    plan: RoutePlan,
    provider: ProviderCard,
    /// 落到最终 provider 前失败的尝试(fail-closed 轨迹,展示用)。
    attempts: Vec<String>,
}

#[derive(Serialize, Clone)]
struct DeltaPayload {
    run_id: String,
    text: String,
}

#[derive(Serialize, Clone)]
struct DonePayload {
    run_id: String,
    latency_ms: u64,
    finish: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    chars: usize,
}

#[derive(Serialize, Clone)]
struct FailPayload {
    run_id: String,
    channel_id: String,
    message: String,
}

#[derive(Serialize)]
struct AuditRow {
    event_id: String,
    ts_ms: u64,
    event_type: String,
    actor: String,
    run_id: Option<String>,
    channel: Option<String>,
    label: Option<String>,
    locality: Option<String>,
}

#[derive(Serialize)]
struct ChainStatus {
    ok: bool,
    rows: u64,
    detail: String,
}

// ---------------------------------------------------------------- 命令

#[tauri::command]
fn bootstrap(state: State<'_, AppState>) -> Result<BootstrapInfo, String> {
    let mut guard = state.0.lock().unwrap();
    if guard.is_none() {
        let registry = ProviderRegistry::from_toml_str(include_str!("../../providers.toml"))
            .map_err(|e| format!("provider 注册表加载失败:{e}"))?;
        let policy = OrgPolicy::new(Sensitivity::Internal).map_err(|e| format!("组织策略非法:{e:?}"))?;
        let router = Arc::new(Router::from_registry(&registry, policy));

        let home = std::env::var("HOME").map_err(|_| "HOME 未设置".to_string())?;
        let dir = format!("{home}/.muster");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {dir} 失败:{e}"))?;
        let db = format!("{dir}/desktop-audit.db");
        let audit = AuditStore::open(&db).map_err(|e| format!("审计库打开失败:{e}"))?;

        *guard = Some(Backend {
            router,
            audit: Arc::new(Mutex::new(audit)),
            run_seq: Arc::new(AtomicU64::new(0)),
            drill: Arc::new(Mutex::new(None)),
        });
    }
    let backend = guard.as_ref().unwrap();
    let providers = backend
        .router
        .candidates()
        .into_iter()
        .map(|m| ProviderCard {
            id: m.id,
            display_name: m.display_name,
            model: m.model,
            locality: format!("{:?}", m.locality).to_lowercase(),
        })
        .collect();
    let home = std::env::var("HOME").unwrap_or_default();
    Ok(BootstrapInfo {
        channels: demo_channels(),
        providers,
        policy_cloud_max: backend.router.policy_snapshot().cloud_max(),
        audit_db: format!("{home}/.muster/desktop-audit.db"),
        egress_locked: backend.router.egress_locked(),
    })
}

#[tauri::command]
async fn send_message(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    channel_id: String,
    text: String,
) -> Result<String, String> {
    let (router, audit, run_seq) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化(先调用 bootstrap)")?;
        (b.router.clone(), b.audit.clone(), b.run_seq.clone())
    };
    let channel = demo_channels()
        .into_iter()
        .find(|c| c.id == channel_id)
        .ok_or_else(|| format!("未知频道 {channel_id}"))?;
    let sources = label_sources(&channel.id);
    let run_id = format!("RUN-{}", 2231 + run_seq.fetch_add(1, Ordering::SeqCst));
    let session_id = format!("session:{}", channel.id);

    // ---- E2 路由决策(含探活,fail-closed)
    let route_req = RouteRequest {
        sources: &sources,
        requested_provider: None,
        default_provider: Some("kimi"),
    };
    let resolution = match router.resolve(&route_req).await {
        Ok(r) => r,
        Err(e) => {
            app.emit(
                "task-refused",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id.clone(), message: e.to_string() },
            )
            .ok();
            return Ok(run_id);
        }
    };
    let plan = resolution.plan.clone();
    let meta = resolution.provider.metadata().clone();
    let provider_card = ProviderCard {
        id: meta.id.clone(),
        display_name: meta.display_name.clone(),
        model: meta.model.clone(),
        locality: format!("{:?}", meta.locality).to_lowercase(),
    };

    // ---- A9 审计:route.decide(证据层写失败按命令失败处理,fail-closed)
    {
        let mut store = audit.lock().unwrap();
        store
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::agent(AGENT_BADGE),
                scope: Scope { team: Some(channel.team.clone()), channel: Some(channel.id.clone()) },
                run_id: Some(run_id.clone()),
                session_id: Some(session_id.clone()),
                policy_version: Some(POLICY_VERSION.into()),
                label: Some(plan.effective),
                locality: Some(plan.primary_locality),
                body: EventBody::RouteDecide {
                    effective_label: plan.effective,
                    deciders: plan.deciders.clone(),
                    policy_version: POLICY_VERSION.into(),
                    locality: plan.primary_locality,
                    provider_id: plan.primary.clone(),
                    fallbacks: plan.fallbacks.clone(),
                    downgrade: plan.downgraded.as_ref().map(|d| d.reason),
                },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
    }

    app.emit(
        "task-start",
        StartPayload {
            run_id: run_id.clone(),
            channel_id: channel.id.clone(),
            plan: plan.clone(),
            provider: provider_card,
            attempts: resolution.attempts.iter().map(|a| format!("{a:?}")).collect(),
        },
    )
    .ok();

    // ---- A2 流式调用(请求载荷的规范化表示,供哈希与字节记账)
    let request_repr = serde_json::json!({
        "provider": meta.id,
        "model": meta.model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": text },
        ],
    })
    .to_string();
    let request_bytes = request_repr.len() as u64;

    let req = ChatRequest {
        messages: vec![ChatMessage::system(SYSTEM_PROMPT), ChatMessage::user(text.clone())],
        run_id: Some(run_id.clone()),
        ..Default::default()
    };

    let started = Instant::now();
    let mut stream = match resolution.provider.chat_stream(req).await {
        Ok(s) => s,
        Err(e) => {
            app.emit(
                "task-failed",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id.clone(), message: format!("开流失败:{e}") },
            )
            .ok();
            return Ok(run_id);
        }
    };

    let mut full = String::new();
    let mut prompt_tokens = None;
    let mut completion_tokens = None;
    let mut finish = String::new();
    let mut stream_err: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(StreamEvent::TextDelta(t)) => {
                full.push_str(&t);
                app.emit("task-delta", DeltaPayload { run_id: run_id.clone(), text: t }).ok();
            }
            // v1 未声明工具,正常情况下不会出现;出现即忽略(不伪装执行)。
            Ok(StreamEvent::ToolCallDelta { .. }) => {}
            Ok(StreamEvent::Usage(u)) => {
                prompt_tokens = Some(u.prompt_tokens as u64);
                completion_tokens = Some(u.completion_tokens as u64);
            }
            Ok(StreamEvent::Finish(f)) => finish = format!("{f:?}"),
            Err(e) => {
                stream_err = Some(e.to_string());
                break;
            }
        }
    }
    let latency_ms = started.elapsed().as_millis() as u64;

    // ---- A9 审计:model.call(外发记账唯一来源;字节为载荷近似,wire 级计量属 A2 后续)
    {
        let mut store = audit.lock().unwrap();
        store
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::agent(AGENT_BADGE),
                scope: Scope { team: Some(channel.team.clone()), channel: Some(channel.id.clone()) },
                run_id: Some(run_id.clone()),
                session_id: Some(session_id),
                policy_version: Some(POLICY_VERSION.into()),
                label: Some(plan.effective),
                locality: Some(plan.primary_locality),
                body: EventBody::ModelCall {
                    provider_id: meta.id.clone(),
                    model: meta.model.clone(),
                    locality: plan.primary_locality,
                    label: plan.effective,
                    tokens_in: prompt_tokens,
                    tokens_out: completion_tokens,
                    bytes_in: full.len() as u64,
                    // 外发口径 = 公网出口字节:本地回环不算外发,记 Measured(0)
                    //(演习报告对所有 Measured 求和,本地若记载荷字节会误报违规)。
                    bytes_out: if plan.primary_locality == Locality::Cloud {
                        EgressBytes::Measured(request_bytes)
                    } else {
                        EgressBytes::Measured(0)
                    },
                    latency_ms,
                    request_hash: ContentHash::sha256(request_repr.as_bytes()),
                },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
    }

    match stream_err {
        Some(msg) => {
            app.emit(
                "task-failed",
                FailPayload {
                    run_id: run_id.clone(),
                    channel_id: channel.id.clone(),
                    message: format!("中流失败(v1 不重试,重试策略属 B1 Runner):{msg}"),
                },
            )
            .ok();
        }
        None => {
            app.emit(
                "task-done",
                DonePayload {
                    run_id: run_id.clone(),
                    latency_ms,
                    finish,
                    prompt_tokens,
                    completion_tokens,
                    chars: full.chars().count(),
                },
            )
            .ok();
        }
    }
    Ok(run_id)
}

#[tauri::command]
fn audit_tail(state: State<'_, AppState>, limit: u64) -> Result<Vec<AuditRow>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let events = recent_events(store.conn(), limit).map_err(|e| e.to_string())?;
    Ok(events
        .into_iter()
        .map(|e| AuditRow {
            event_type: e
                .payload
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            event_id: e.event_id,
            ts_ms: e.ts_ms,
            actor: format!("{:?}", e.actor),
            run_id: e.run_id,
            channel: e.scope.channel,
            label: e.label.map(|l| format!("{l:?}").to_lowercase()),
            locality: e.locality.map(|l| format!("{l:?}").to_lowercase()),
        })
        .collect())
}

/// E6 主权演习开关。演习不是特殊逻辑,是策略的一个取值:复用同一条
/// fail-closed 路由路径;结束时以 SQL 聚合窗口出第 8 幕报告。
#[tauri::command]
fn toggle_drill(state: State<'_, AppState>, on: bool) -> Result<DrillStatus, String> {
    let (router, audit, drill) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化")?;
        (b.router.clone(), b.audit.clone(), b.drill.clone())
    };
    let mut d = drill.lock().unwrap();
    if on {
        if let Some(cur) = d.as_ref() {
            return Ok(DrillStatus { on: true, drill_id: Some(cur.id.clone()), report: None });
        }
        let from_ms = now_ms();
        let id = format!("DRILL-{from_ms}");
        router.set_egress_locked(true);
        audit
            .lock()
            .unwrap()
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::human("owner"),
                scope: Scope::default(),
                run_id: None,
                session_id: None,
                policy_version: Some(POLICY_VERSION.into()),
                label: None,
                locality: None,
                body: EventBody::DrillStart { drill_id: id.clone() },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
        *d = Some(DrillState { id: id.clone(), from_ms });
        Ok(DrillStatus { on: true, drill_id: Some(id), report: None })
    } else {
        let Some(cur) = d.take() else {
            return Ok(DrillStatus { on: false, drill_id: None, report: None });
        };
        router.set_egress_locked(false);
        let to_ms = now_ms();
        let mut store = audit.lock().unwrap();
        let report = drill_report(store.conn(), cur.from_ms, to_ms).map_err(|e| e.to_string())?;
        store
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::human("owner"),
                scope: Scope::default(),
                run_id: None,
                session_id: None,
                policy_version: Some(POLICY_VERSION.into()),
                label: None,
                locality: None,
                body: EventBody::DrillEnd {
                    drill_id: cur.id.clone(),
                    egress_bytes_snapshot: report.egress_bytes,
                    unmetered_calls_snapshot: report.unmetered_calls,
                },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
        Ok(DrillStatus {
            on: false,
            drill_id: Some(cur.id),
            report: Some(DrillReportOut {
                model_calls: report.model_calls,
                egress_bytes: report.egress_bytes,
                unmetered_calls: report.unmetered_calls,
                local_calls: report.local_calls,
                cloud_calls: report.cloud_calls,
                ok: report.ok(),
            }),
        })
    }
}

#[tauri::command]
fn verify_chain(state: State<'_, AppState>) -> Result<ChainStatus, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    match store.verify_chain().map_err(|e| e.to_string())? {
        Ok(rows) => Ok(ChainStatus { ok: true, rows, detail: format!("{rows} 行哈希链完整") }),
        Err(e) => Ok(ChainStatus { ok: false, rows: 0, detail: format!("{e:?}") }),
    }
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            send_message,
            audit_tail,
            verify_chain,
            toggle_drill
        ])
        .run(tauri::generate_context!())
        .expect("muster-desktop 启动失败");
}
