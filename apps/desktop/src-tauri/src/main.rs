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
    actor_first_seen, capsules, day_throughput, distinct_runs, downgrades_zh, drill_report,
    forgeable, pending_approval_list, pending_approvals, recent_events, recent_events_of, roster,
    Actor, AuditStore, ContentHash, EgressBytes, EventBody, NewEvent, Scope,
};
use muster_provider::{ChatMessage, ChatRequest, Locality, ProviderRegistry, StreamEvent};
use muster_route::{LabelOrigin, LabelSource, OrgPolicy, RoutePlan, RouteRequest, Router, Sensitivity};
use muster_runner::{run_task, RunnerConfig, RunnerError, RunnerEvent, TaskSpec};

const POLICY_VERSION: &str = "policy-v1";
const AGENT_BADGE: &str = "A-007";
const SYSTEM_PROMPT: &str =
    "你是 Muster 点将台的协作 Agent(工牌 A-007)。用中文回答,简洁、直接。";

// ---------------------------------------------------------------- 演示编制

#[derive(Clone, Serialize)]
struct ChannelInfo {
    id: String,
    /// 频道显示名(v4:# 前缀由前端加)。
    name: String,
    team_id: String,
    team: String,
    level: Sensitivity,
    /// 徽章悬浮:密级从哪来(与 label_sources 保持一致)。
    level_note: String,
    desc: String,
    /// 个人空间伪频道:不出现在团队树,只服务「我的工作台」。
    personal: bool,
}

fn ch(
    id: &str,
    name: &str,
    team_id: &str,
    team: &str,
    level: Sensitivity,
    level_note: &str,
    desc: &str,
) -> ChannelInfo {
    ChannelInfo {
        id: id.into(),
        name: name.into(),
        team_id: team_id.into(),
        team: team.into(),
        level,
        level_note: level_note.into(),
        desc: desc.into(),
        personal: false,
    }
}

/// v4 编制:三团队六频道 + 个人空间。密级与 label_sources 一一对应。
fn demo_channels() -> Vec<ChannelInfo> {
    let mut v = vec![
        ch("general", "general", "platform", "平台组", Sensitivity::Open,
            "未贴标签,默认 open(产品决策:未标注可走云端)", "公开讨论,允许云端模型"),
        ch("platform", "platform", "platform", "平台组", Sensitivity::Internal,
            "频道标签 internal(恰在 cloud_max 上限,仍可云端)", "平台组主频道"),
        ch("code-review", "code-review", "platform", "平台组", Sensitivity::Internal,
            "频道标签 internal", "评审与合入讨论"),
        ch("payments", "payments", "pay", "支付组", Sensitivity::Internal,
            "频道标签 internal", "支付业务频道"),
        ch("release-train", "release-train", "pay", "支付组", Sensitivity::Internal,
            "频道标签 internal", "发布列车"),
        ch("sec-ops", "sec-ops", "sec", "安全组", Sensitivity::Restricted,
            "频道标签 restricted——硬编码不变量:永不上云;本地不可用即拒绝", "安全运营(仅本地)"),
    ];
    v.push(ChannelInfo {
        id: "personal".into(),
        name: "私有会话".into(),
        team_id: "personal".into(),
        team: "个人".into(),
        level: Sensitivity::Open,
        level_note: "个人空间默认 open;引用 restricted 资源会被会话棘轮抬升(E3)".into(),
        desc: "与小七的私有会话,默认不进团队".into(),
        personal: true,
    });
    v
}

fn label_sources(channel_id: &str) -> Vec<LabelSource> {
    let channel_internal =
        |id: &str| vec![LabelSource::new(LabelOrigin::Channel, Sensitivity::Internal, format!("channel:{id}"))];
    match channel_id {
        "platform" | "code-review" | "payments" | "release-train" => channel_internal(channel_id),
        "sec-ops" => vec![LabelSource::new(
            LabelOrigin::Channel,
            Sensitivity::Restricted,
            "channel:sec-ops",
        )],
        _ => vec![],
    }
}

// ---------------------------------------------------------------- 状态

// ---------------------------------------------------------------- C1 会话持久化
//
// 桌面本地状态库,与审计库**刻意分离**:审计是证据层(只存哈希,append-only,
// 防篡改);这里是会话正文的 run 存储侧,带自己的密级语义,可清除。

#[derive(Serialize)]
struct StoredMsg {
    channel_id: String,
    role: String,
    text: String,
    run_id: Option<String>,
    status: String,
    ts_ms: i64,
}

struct StateStore {
    conn: rusqlite::Connection,
}

impl StateStore {
    fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id TEXT NOT NULL,
                role TEXT NOT NULL,
                text TEXT NOT NULL,
                run_id TEXT,
                status TEXT NOT NULL,
                ts_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_msg_chan ON messages(channel_id, id);",
        )?;
        Ok(Self { conn })
    }

    /// 状态库是便利层,写失败降级为日志,不阻断任务(与审计的 fail-closed 相反,刻意)。
    fn insert(&self, channel_id: &str, role: &str, text: &str, run_id: Option<&str>, status: &str) {
        if let Err(e) = self.conn.execute(
            "INSERT INTO messages(channel_id, role, text, run_id, status, ts_ms) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![channel_id, role, text, run_id, status, now_ms() as i64],
        ) {
            eprintln!("state 持久化失败(忽略):{e}");
        }
    }

    fn bulk(&self, limit: u32) -> rusqlite::Result<Vec<StoredMsg>> {
        let mut stmt = self.conn.prepare(
            "SELECT channel_id, role, text, run_id, status, ts_ms FROM messages
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], |r| {
            Ok(StoredMsg {
                channel_id: r.get(0)?,
                role: r.get(1)?,
                text: r.get(2)?,
                run_id: r.get(3)?,
                status: r.get(4)?,
                ts_ms: r.get(5)?,
            })
        })?;
        let mut out: Vec<StoredMsg> = rows.collect::<Result<_, _>>()?;
        out.reverse(); // 倒查取最近 N 条,再转回时间正序
        Ok(out)
    }
}

/// 进行中的演习(E6):id + 起始时刻,结束时用 drill_report 聚合窗口。
struct DrillState {
    id: String,
    from_ms: u64,
}

struct Backend {
    router: Arc<Router>,
    audit: Arc<Mutex<AuditStore>>,
    state: Arc<Mutex<StateStore>>,
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
struct DiffPayload {
    run_id: String,
    branch: String,
    files_changed: usize,
    insertions: u32,
    deletions: u32,
    files: Vec<muster_runner::FileChange>,
    patch: String,
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

// ---------------------------------------------------------------- 首页数据

#[derive(Serialize)]
struct DayBar {
    date: String,
    weekday: String,
    local: u64,
    cloud: u64,
}

#[derive(Serialize)]
struct DrillLast {
    ts_ms: u64,
    drill_id: String,
    egress_bytes: u64,
    unmetered_calls: u64,
    ok: bool,
}

#[derive(Serialize)]
struct DowngradeItem {
    ts_ms: u64,
    run_id: Option<String>,
    text: String,
}

#[derive(Serialize)]
struct RunItem {
    ts_ms: u64,
    run_id: String,
    outcome: String,
    duration_ms: u64,
}

/// 首页全部数字——每一项都由 muster-audit::queries 的一条 SQL 产出(G1 口径)。
#[derive(Serialize)]
struct HomeStats {
    runs_week: u64,
    runs_prev_week: u64,
    egress_week_bytes: u64,
    egress_prev_week_bytes: u64,
    unmetered_week: u64,
    cloud_calls_week: u64,
    local_calls_week: u64,
    pending_approvals: u64,
    drill_last: Option<DrillLast>,
    throughput: Vec<DayBar>,
    downgrades: Vec<DowngradeItem>,
    recent_runs: Vec<RunItem>,
}

const WEEK_MS: u64 = 7 * 24 * 3600 * 1000;

#[tauri::command]
fn home_stats(state: State<'_, AppState>) -> Result<HomeStats, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let conn = store.conn();
    let now = now_ms();
    let week_ago = now.saturating_sub(WEEK_MS);
    let two_weeks_ago = now.saturating_sub(2 * WEEK_MS);

    let week = drill_report(conn, week_ago, now).map_err(|e| e.to_string())?;
    let prev = drill_report(conn, two_weeks_ago, week_ago).map_err(|e| e.to_string())?;

    let drill_last = recent_events_of(conn, "drill.end", 1)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .map(|e| {
            let egress = e.payload["egress_bytes_snapshot"].as_u64().unwrap_or(0);
            let unmetered = e.payload["unmetered_calls_snapshot"].as_u64().unwrap_or(0);
            DrillLast {
                ts_ms: e.ts_ms,
                drill_id: e.payload["drill_id"].as_str().unwrap_or("?").to_string(),
                egress_bytes: egress,
                unmetered_calls: unmetered,
                ok: egress == 0 && unmetered == 0,
            }
        });

    let recent_runs = recent_events_of(conn, "run.finish", 8)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|e| {
            let outcome = match &e.payload["outcome"] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Object(o) => o
                    .get("failed")
                    .and_then(|f| f["class"].as_str())
                    .map(|c| format!("failed:{c}"))
                    .unwrap_or_else(|| "?".into()),
                _ => "?".into(),
            };
            RunItem {
                ts_ms: e.ts_ms,
                run_id: e.run_id.unwrap_or_else(|| "—".into()),
                outcome,
                duration_ms: e.payload["duration_ms"].as_u64().unwrap_or(0),
            }
        })
        .collect();

    Ok(HomeStats {
        runs_week: distinct_runs(conn, week_ago, now).map_err(|e| e.to_string())?,
        runs_prev_week: distinct_runs(conn, two_weeks_ago, week_ago).map_err(|e| e.to_string())?,
        egress_week_bytes: week.egress_bytes,
        egress_prev_week_bytes: prev.egress_bytes,
        unmetered_week: week.unmetered_calls,
        cloud_calls_week: week.cloud_calls,
        local_calls_week: week.local_calls,
        pending_approvals: pending_approvals(conn, AGENT_BADGE).map_err(|e| e.to_string())?,
        drill_last,
        throughput: day_throughput(conn, 7)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|d| DayBar { date: d.date, weekday: d.weekday, local: d.local, cloud: d.cloud })
            .collect(),
        downgrades: downgrades_zh(conn, week_ago, now)
            .map_err(|e| e.to_string())?
            .into_iter()
            .rev()
            .take(6)
            .map(|(ts_ms, run_id, text)| DowngradeItem { ts_ms, run_id, text: text.to_string() })
            .collect(),
        recent_runs,
    })
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
        let state_db = format!("{dir}/desktop-state.db");
        let state = StateStore::open(&state_db).map_err(|e| format!("状态库打开失败:{e}"))?;

        *guard = Some(Backend {
            router,
            audit: Arc::new(Mutex::new(audit)),
            state: Arc::new(Mutex::new(state)),
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
    let (router, audit, store, run_seq) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化(先调用 bootstrap)")?;
        (b.router.clone(), b.audit.clone(), b.state.clone(), b.run_seq.clone())
    };
    let channel = demo_channels()
        .into_iter()
        .find(|c| c.id == channel_id)
        .ok_or_else(|| format!("未知频道 {channel_id}"))?;
    let sources = label_sources(&channel.id);
    let run_id = format!("RUN-{}", 2231 + run_seq.fetch_add(1, Ordering::SeqCst));
    let session_id = format!("session:{}", channel.id);
    store.lock().unwrap().insert(&channel.id, "user", &text, None, "done");

    // ---- E2 路由决策(含探活,fail-closed)
    let route_req = RouteRequest {
        sources: &sources,
        requested_provider: None,
        default_provider: Some("kimi"),
    };
    let resolution = match router.resolve(&route_req).await {
        Ok(r) => r,
        Err(e) => {
            // 拒绝也是证据:route.refuse 落审计(E4),写失败按命令失败处理。
            let (body, label, locality) = EventBody::route_refuse(&e, POLICY_VERSION);
            audit
                .lock()
                .unwrap()
                .append(NewEvent {
                    ts_ms: None,
                    actor: Actor::agent(AGENT_BADGE),
                    scope: Scope { team: Some(channel.team.clone()), channel: Some(channel.id.clone()) },
                    run_id: Some(run_id.clone()),
                    session_id: Some(session_id.clone()),
                    policy_version: Some(POLICY_VERSION.into()),
                    label,
                    locality,
                    body,
                })
                .map_err(|er| format!("审计写入失败:{er}"))?;
            let msg = e.to_string();
            store.lock().unwrap().insert(
                &channel.id,
                "agent",
                &format!("⛔ 路由拒绝(fail-closed,绝不静默升云)\n{msg}"),
                Some(&run_id),
                "refused",
            );
            app.emit(
                "task-refused",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id.clone(), message: msg },
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
            store.lock().unwrap().insert(&channel.id, "agent", &format!("⚠️ 开流失败:{e}"), Some(&run_id), "failed");
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
            store.lock().unwrap().insert(
                &channel.id,
                "agent",
                &format!("{full}\n\n⚠️ 中流失败:{msg}"),
                Some(&run_id),
                "failed",
            );
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
            store.lock().unwrap().insert(&channel.id, "agent", &full, Some(&run_id), "done");
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

/// B1 任务模式:在真实工作区上跑工具循环(muster-runner),事件转发给
/// 与聊天模式相同的前端通道——工具活动以文本行形式流进任务卡。
#[tauri::command]
async fn run_workspace_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    channel_id: String,
    text: String,
) -> Result<String, String> {
    let (router, audit, store, run_seq) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化(先调用 bootstrap)")?;
        (b.router.clone(), b.audit.clone(), b.state.clone(), b.run_seq.clone())
    };
    let channel = demo_channels()
        .into_iter()
        .find(|c| c.id == channel_id)
        .ok_or_else(|| format!("未知频道 {channel_id}"))?;
    let run_id = format!("RUN-{}", 2231 + run_seq.fetch_add(1, Ordering::SeqCst));
    let home = std::env::var("HOME").map_err(|_| "HOME 未设置".to_string())?;
    let workspace = std::env::var("MUSTER_WORKSPACE").unwrap_or_else(|_| format!("{home}/muster"));
    store.lock().unwrap().insert(&channel.id, "user", &format!("▶ 任务:{text}"), None, "done");
    // 持久化的任务轨迹 = 前端看到的一切(文本增量 + 工具行 + 通告)。
    let transcript = Arc::new(Mutex::new(String::new()));

    // P1-04:每 run 一个隔离 worktree(WORKSPACE_ROOT 可配),写工具因此可用;
    // 非 git 仓时 runner 会如实降级只读并发 Notice。
    let workspace_root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));
    let spec = TaskSpec {
        run_id: run_id.clone(),
        session_id: Some(format!("session:{}", channel.id)),
        team: Some(channel.team.clone()),
        channel: Some(channel.id.clone()),
        sources: label_sources(&channel.id),
        requested_provider: None,
        default_provider: Some("kimi".into()),
        prompt: text,
        workspace: workspace.into(),
        workspace_root: Some(workspace_root.into()),
    };
    let cfg = RunnerConfig { policy_version: POLICY_VERSION.into(), ..Default::default() };

    let a = app.clone();
    let rid = run_id.clone();
    let chan = channel.id.clone();
    let tr = transcript.clone();
    let result = run_task(&router, &audit, &cfg, spec, move |ev| match ev {
        RunnerEvent::Planned { plan, provider_id, provider_name, model, locality, attempts, .. } => {
            a.emit(
                "task-start",
                StartPayload {
                    run_id: rid.clone(),
                    channel_id: chan.clone(),
                    plan,
                    provider: ProviderCard {
                        id: provider_id,
                        display_name: provider_name,
                        model,
                        locality,
                    },
                    attempts,
                },
            )
            .ok();
        }
        RunnerEvent::TextDelta { text } => {
            tr.lock().unwrap().push_str(&text);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text }).ok();
        }
        RunnerEvent::ToolCall { name, arguments, .. } => {
            let line = format!("\n🔧 {name} {arguments}\n");
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
        }
        RunnerEvent::ToolResult { summary, .. } => {
            let line = format!("   ↳ {summary}\n");
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
        }
        RunnerEvent::Notice { text } => {
            let line = format!("\n⚠️ {text}\n");
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
        }
        RunnerEvent::ApprovalRequested { approval_id, branch, .. } => {
            let line = format!(
                "\n⏳ 已申请合入(审批号 {approval_id},分支 {branch})——需人工裁决,Agent 不自行合入\n"
            );
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
            a.emit("approvals-changed", ()).ok();
        }
        RunnerEvent::WorkspaceReady { branch, .. } => {
            let line = format!("🌿 隔离工作区就绪 · 分支 {branch}\n");
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
        }
        RunnerEvent::Diff { diff, branch } => {
            let line = if diff.is_empty() {
                "\n📄 本次运行没有产生代码改动\n".to_string()
            } else {
                format!(
                    "\n📄 代码变更:{} 个文件 +{} −{}(分支 {branch})\n",
                    diff.files_changed, diff.insertions, diff.deletions
                )
            };
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
            a.emit(
                "task-diff",
                DiffPayload {
                    run_id: rid.clone(),
                    branch,
                    files_changed: diff.files_changed,
                    insertions: diff.insertions,
                    deletions: diff.deletions,
                    files: diff.files,
                    patch: diff.patch,
                },
            )
            .ok();
        }
        RunnerEvent::Finished { outcome, latency_ms, turns, prompt_tokens, completion_tokens } => {
            if outcome == "success" {
                a.emit(
                    "task-done",
                    DonePayload {
                        run_id: rid.clone(),
                        latency_ms,
                        finish: format!("{turns} 回合"),
                        prompt_tokens: Some(prompt_tokens),
                        completion_tokens: Some(completion_tokens),
                        chars: 0,
                    },
                )
                .ok();
            } else {
                a.emit(
                    "task-failed",
                    FailPayload {
                        run_id: rid.clone(),
                        channel_id: chan.clone(),
                        message: format!("任务未完成:{outcome}"),
                    },
                )
                .ok();
            }
        }
    })
    .await;

    let saved = transcript.lock().unwrap().clone();
    match result {
        Ok(s) => {
            let status = if s.outcome == "success" { "done" } else { "failed" };
            let text = if saved.is_empty() { s.final_text.clone() } else { saved };
            store.lock().unwrap().insert(&channel.id, "agent", &text, Some(&run_id), status);
            Ok(s.run_id)
        }
        // Model 错误的 UI 反馈已由 Finished(failed:stream) 事件完成,不重复发。
        Err(RunnerError::Model(msg)) => {
            store.lock().unwrap().insert(
                &channel.id,
                "agent",
                &format!("{saved}\n\n⚠️ 中流失败(已重试一次):{msg}"),
                Some(&run_id),
                "failed",
            );
            Ok(run_id)
        }
        Err(RunnerError::Refused(msg)) => {
            store.lock().unwrap().insert(
                &channel.id,
                "agent",
                &format!("⛔ 路由拒绝(fail-closed,绝不静默升云)\n{msg}"),
                Some(&run_id),
                "refused",
            );
            app.emit(
                "task-refused",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id, message: msg },
            )
            .ok();
            Ok(run_id)
        }
        Err(e) => {
            store.lock().unwrap().insert(&channel.id, "agent", &format!("⚠️ {e}"), Some(&run_id), "failed");
            app.emit(
                "task-failed",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id, message: e.to_string() },
            )
            .ok();
            Ok(run_id)
        }
    }
}

/// C1:启动时取回全部频道的历史消息(时间正序,总量截断)。
#[tauri::command]
fn history_bulk(state: State<'_, AppState>, limit: u32) -> Result<Vec<StoredMsg>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.state.lock().unwrap();
    store.bulk(limit).map_err(|e| e.to_string())
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

// ---------------------------------------------------------------- P4 能力库

#[derive(Serialize)]
struct CapsuleOut {
    capsule_id: String,
    name: String,
    version: String,
    scope: String,
    source_run_id: String,
    forged_ms: u64,
    forged_by: String,
    verify_passed: u64,
    verify_total: u64,
    /// **未验真是 None,不是 0%**——"没验过"与"验证失败"必须能区分。
    verified_rate: Option<f64>,
    adopted: u64,
}

#[tauri::command]
fn capsules_list(state: State<'_, AppState>) -> Result<Vec<CapsuleOut>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    Ok(capsules(store.conn())
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|c| CapsuleOut {
            verified_rate: c.verified_rate(),
            capsule_id: c.capsule_id,
            name: c.name,
            version: c.version,
            scope: c.scope,
            source_run_id: c.source_run_id,
            forged_ms: c.forged_ms,
            forged_by: c.forged_by,
            verify_passed: c.verify_passed,
            verify_total: c.verify_total,
            adopted: c.adopted,
        })
        .collect())
}

/// 可锻造的运行(成功结束、有 run.start、尚未锻造过)。
#[derive(Serialize)]
struct ForgeableRun {
    run_id: String,
    ts_ms: u64,
    /// 该运行的产出摘要(取 run.finish 的 output_hash 前缀,仅作标识)。
    output_hash: String,
    duration_ms: u64,
}

#[tauri::command]
fn forgeable_runs(state: State<'_, AppState>) -> Result<Vec<ForgeableRun>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let conn = store.conn();
    let mut out = Vec::new();
    for e in recent_events_of(conn, "run.finish", 40).map_err(|e| e.to_string())? {
        let Some(run_id) = e.run_id.clone() else { continue };
        if e.payload["outcome"] != "success" {
            continue;
        }
        if !forgeable(conn, &run_id).map_err(|e| e.to_string())?.0 {
            continue;
        }
        out.push(ForgeableRun {
            run_id,
            ts_ms: e.ts_ms,
            output_hash: e.payload["output_hash"].as_str().unwrap_or("").to_string(),
            duration_ms: e.payload["duration_ms"].as_u64().unwrap_or(0),
        });
    }
    Ok(out)
}

/// 锻造:先取草稿(把散在事件里的事实聚拢),再落 capsule.forge。
/// 草稿是建议不是结论——`goal` 由调用方给定,允许人改写。
#[tauri::command]
fn capsule_forge(
    state: State<'_, AppState>,
    run_id: String,
    goal: String,
    visibility: String,
) -> Result<String, String> {
    let audit = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.audit.clone()
    };
    let tools = vec![
        "list_dir".to_string(),
        "read_file".to_string(),
        "grep".to_string(),
        "write_file".to_string(),
        "replace_in_file".to_string(),
    ];
    let spec = muster_runner::draft_spec(&audit, &run_id, &goal, tools).map_err(|e| e.to_string())?;
    let store = capsule_store()?;
    // 审计存哈希、存储存正文,两者由 CapsuleStore::load 的哈希校验绑定
    let out = muster_runner::forge_and_store(
        &audit,
        &store,
        AGENT_BADGE,
        POLICY_VERSION,
        &run_id,
        spec,
        &visibility,
        Scope::default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(format!("已锻造 {} · {}(源运行 {run_id})", out.spec.name, out.capsule_id))
}

fn capsule_store() -> Result<muster_runner::CapsuleStore, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME 未设置".to_string())?;
    muster_runner::CapsuleStore::open(format!("{home}/.muster/capsules"))
        .map_err(|e| format!("Capsule 存储打开失败:{e}"))
}

/// 影子重放验真。**三种结局**:通过 / 不通过 / 没法验真(后者不落库,
/// 否则验真率的分母被"我们没条件验"污染)。
#[tauri::command]
async fn capsule_verify(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    capsule_id: String,
) -> Result<String, String> {
    let (router, audit) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化")?;
        (b.router.clone(), b.audit.clone())
    };
    let home = std::env::var("HOME").map_err(|_| "HOME 未设置".to_string())?;
    let workspace = std::env::var("MUSTER_WORKSPACE").unwrap_or_else(|_| format!("{home}/muster"));
    let root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));
    let store = capsule_store()?;
    let cfg = RunnerConfig { policy_version: POLICY_VERSION.into(), ..Default::default() };

    let a = app.clone();
    let out = muster_runner::verify(
        &router,
        &audit,
        &store,
        &cfg,
        &capsule_id,
        std::path::Path::new(&workspace),
        std::path::Path::new(&root),
        move |ev| {
            if let RunnerEvent::Notice { text } = ev {
                a.emit("capsule-verify-progress", text).ok();
            }
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    app.emit("approvals-changed", ()).ok();
    Ok(format!(
        "{} · {}",
        if out.passed { "✓ 验真通过" } else { "✗ 产出不一致" },
        out.detail
    ))
}

// ---------------------------------------------------------------- P5 审批

/// 未决审批(等人裁决的合入申请)。
#[derive(Serialize)]
struct PendingApprovalOut {
    approval_id: String,
    ts_ms: u64,
    actor_id: String,
    run_id: Option<String>,
    channel: Option<String>,
    requested_capability: String,
    reason: String,
    command_hash: String,
    /// 该 run 的隔离分支与 worktree 路径(裁决时用)。
    branch: String,
    worktree_path: String,
    /// worktree 是否仍在(被保留策略回收过就没了,此时只能拒绝)。
    worktree_exists: bool,
}

#[tauri::command]
fn approvals_pending(state: State<'_, AppState>) -> Result<Vec<PendingApprovalOut>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let home = std::env::var("HOME").unwrap_or_default();
    let root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));

    Ok(pending_approval_list(store.conn(), None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| {
            let run_id = p.run_id.clone().unwrap_or_default();
            let slug: String = run_id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let wt = format!("{root}/run-{slug}");
            PendingApprovalOut {
                worktree_exists: std::path::Path::new(&wt).exists(),
                branch: muster_runner::approval::branch_for(&run_id),
                worktree_path: wt,
                approval_id: p.approval_id,
                ts_ms: p.ts_ms,
                actor_id: p.actor_id,
                run_id: p.run_id,
                channel: p.channel,
                requested_capability: p.requested_capability,
                reason: p.reason,
                command_hash: p.command_hash,
            }
        })
        .collect())
}

/// 人的裁决:批准则合入主仓,拒绝则丢弃;两者都写审计,处置后回收 worktree。
#[tauri::command]
fn approvals_decide(
    state: State<'_, AppState>,
    run_id: String,
    granted: bool,
    note: Option<String>,
) -> Result<String, String> {
    let (audit, channel_scope) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化")?;
        (b.audit.clone(), Scope::default())
    };
    let home = std::env::var("HOME").map_err(|_| "HOME 未设置".to_string())?;
    let base = std::env::var("MUSTER_WORKSPACE").unwrap_or_else(|_| format!("{home}/muster"));
    let root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));
    let slug: String =
        run_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let wt_path = std::path::PathBuf::from(format!("{root}/run-{slug}"));

    muster_runner::decide(
        &audit,
        "owner",
        POLICY_VERSION,
        &run_id,
        channel_scope,
        std::path::Path::new(&base),
        wt_path.exists().then_some(wt_path.as_path()),
        granted,
        note.as_deref(),
    )
    .map(|o| o.detail)
    .map_err(|e| e.to_string())
}

/// D6 编制页:一行 = 审计链里真实干过活的 actor。
#[derive(Serialize)]
struct RosterEntryOut {
    actor_kind: String,
    actor_id: String,
    /// 展示名:Agent 用工牌人格名,人类用 id 本身。
    display_name: String,
    role: String,
    first_seen_ms: u64,
    last_seen_ms: u64,
    runs: u64,
    local_calls: u64,
    cloud_calls: u64,
    refusals: u64,
    events: u64,
    pending_approvals: u64,
    /// 当前路由倾向:最近一次 model.call 的落点。
    last_locality: Option<String>,
}

#[tauri::command]
fn roster_stats(state: State<'_, AppState>, team: Option<String>) -> Result<Vec<RosterEntryOut>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let conn = store.conn();
    let rows = roster(conn, team.as_deref()).map_err(|e| e.to_string())?;
    let recent = recent_events_of(conn, "model.call", 200).map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let last_locality = recent
                .iter()
                .find(|e| e.actor.id == r.actor_id)
                .and_then(|e| e.locality.map(|l| format!("{l:?}").to_lowercase()));
            let (display_name, role) = match (r.actor_kind.as_str(), r.actor_id.as_str()) {
                ("agent", AGENT_BADGE) => ("小七".to_string(), "代码评审员".to_string()),
                ("agent", id) => (id.to_string(), "Agent".to_string()),
                ("human", id) => (id.to_string(), "成员".to_string()),
                (_, id) => (id.to_string(), "系统".to_string()),
            };
            RosterEntryOut {
                pending_approvals: pending_approvals(conn, &r.actor_id).unwrap_or(0),
                actor_kind: r.actor_kind,
                actor_id: r.actor_id,
                display_name,
                role,
                first_seen_ms: r.first_seen_ms,
                last_seen_ms: r.last_seen_ms,
                runs: r.runs,
                local_calls: r.local_calls,
                cloud_calls: r.cloud_calls,
                refusals: r.refusals,
                events: r.events,
                last_locality,
            }
        })
        .collect())
}

/// Agent 档案页统计:入职(首条审计事件)/ 累计 Runs / 累计外发 / 活动热力。
#[derive(Serialize)]
struct AgentStats {
    badge: String,
    first_seen_ms: Option<u64>,
    hired_days: u64,
    total_runs: u64,
    total_egress_bytes: u64,
    /// 近 48 周逐日活动(model.call 数),供贡献热力图。
    heat: Vec<DayBar>,
}

#[tauri::command]
fn agent_stats(state: State<'_, AppState>) -> Result<AgentStats, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let conn = store.conn();
    let now = now_ms();
    let first = actor_first_seen(conn, AGENT_BADGE).map_err(|e| e.to_string())?;
    Ok(AgentStats {
        badge: AGENT_BADGE.into(),
        first_seen_ms: first,
        hired_days: first.map(|f| (now.saturating_sub(f)) / 86_400_000 + 1).unwrap_or(0),
        total_runs: distinct_runs(conn, 0, now).map_err(|e| e.to_string())?,
        total_egress_bytes: drill_report(conn, 0, now).map_err(|e| e.to_string())?.egress_bytes,
        heat: day_throughput(conn, 336)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|d| DayBar { date: d.date, weekday: d.weekday, local: d.local, cloud: d.cloud })
            .collect(),
    })
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
            run_workspace_task,
            audit_tail,
            verify_chain,
            toggle_drill,
            home_stats,
            agent_stats,
            history_bulk,
            roster_stats,
            approvals_pending,
            approvals_decide,
            capsules_list,
            forgeable_runs,
            capsule_forge,
            capsule_verify
        ])
        .run(tauri::generate_context!())
        .expect("muster-desktop 启动失败");
}
