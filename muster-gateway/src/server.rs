//! Axum 服务:`POST /v1/responses`(SSE)+ `GET /v1/models` + `GET /health`。

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio_stream::wrappers::UnboundedReceiverStream;

use muster_audit::AuditStore;
use muster_provider::{ModelProvider, StreamEvent, ToolCallAccumulator};
use muster_route::{OrgPolicy, Router as RouteRouter, Sensitivity};

use crate::govern::{Governance, SENSITIVITY_HEADER};

use crate::translate::{
    ev_completed, ev_created, ev_failed, ev_function_call_added, ev_function_call_done,
    ev_message_added, ev_message_done, ev_text_delta, to_chat, ResponsesRequest, Usage,
};

/// 流式空闲看门狗默认值。A2 的总超时管不住"已开流但不再吐字节"的挂起流
/// (README 早已登记该边界);网关是必须补上它的地方——否则一个挂死的上游
/// 会永久占住 Codex 的一轮,agent 直接僵死。实测踩到过 64 分钟无响应。
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 120;

#[derive(Clone)]
pub struct GatewayState {
    /// 治理层:路由 + 审计 + 棘轮(见 [`crate::govern`])。
    pub gov: Arc<Governance>,
    pub idle_timeout: std::time::Duration,
    seq: Arc<AtomicU64>,
}

impl GatewayState {
    /// 单 provider、内存审计链、底线 internal 的最简形态——给测试与本机试用;
    /// 正式启动走 [`GatewayState::governed`]。
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        let default_id = provider.metadata().id.clone();
        let router = Arc::new(RouteRouter::new(vec![provider], OrgPolicy::new(Sensitivity::Internal).expect("internal 是合法的 cloud_max")));
        let audit = Arc::new(Mutex::new(AuditStore::open_in_memory().expect("内存审计库")));
        let gov = Governance::new(router, Some(default_id), audit, Sensitivity::Internal, "codex", "gateway:local", None);
        Self::governed(gov)
    }

    pub fn governed(gov: Governance) -> Self {
        Self { gov: Arc::new(gov), idle_timeout: std::time::Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS), seq: Arc::new(AtomicU64::new(0)) }
    }

    pub fn with_idle_timeout(mut self, d: std::time::Duration) -> Self {
        self.idle_timeout = d;
        self
    }
    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}_{:08}", self.seq.fetch_add(1, Ordering::SeqCst))
    }
}

pub fn router(state: GatewayState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses))
        .with_state(state)
}

pub async fn serve(state: GatewayState, port: u16) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let cands: Vec<String> = state.gov.router.candidates().iter().map(|m| format!("{} ({}, {:?})", m.id, m.model, m.locality)).collect();
    tracing::info!(
        "muster-gateway 监听 http://127.0.0.1:{port}/v1 · 底线 {:?} · 默认 {} · 候选 [{}]",
        state.gov.floor_now(),
        state.gov.default_provider.as_deref().unwrap_or("-"),
        cands.join(", ")
    );
    axum::serve(listener, router(state)).await
}

async fn models(State(st): State<GatewayState>) -> impl IntoResponse {
    // 默认 provider 排第一,其余候选跟在后面
    let mut cands = st.gov.router.candidates();
    if let Some(d) = &st.gov.default_provider {
        cands.sort_by_key(|m| if &m.id == d { 0 } else { 1 });
    }
    let data: Vec<Value> = cands.iter().map(|m| json!({ "id": m.model, "object": "model", "owned_by": m.id })).collect();
    Json(json!({ "object": "list", "data": data }))
}

async fn responses(State(st): State<GatewayState>, headers: HeaderMap, body: Json<Value>) -> impl IntoResponse {
    let req: ResponsesRequest = match serde_json::from_value(body.0) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": format!("请求不可解析:{e}") } })),
            )
                .into_response()
        }
    };

    let response_id = st.next_id("resp");
    let translated = to_chat(&req, Some(response_id.clone()));
    if !translated.dropped.is_empty() {
        tracing::warn!(dropped = ?translated.dropped, "Responses 请求中的不支持项已丢弃");
    }
    tracing::info!(
        %response_id,
        input_items = req.input.len(),
        messages = translated.request.messages.len(),
        tools = translated.request.tools.len(),
        "收到 Responses 请求"
    );
    // ---- 治理:先路由再开流;拒绝即 403,绝不静默升云;落不进审计链也拒绝
    let header = headers.get(SENSITIVITY_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let resolution = match st.gov.decide(header.as_deref(), &response_id).await {
        Ok(r) => r,
        Err(e) => {
            // 拒绝也走 SSE 的 created → failed:这是 Codex 认得的失败形状(e2e 测试钉死的契约),
            // 与"失败必须显式告知,不能静默截断"同一条规矩;原因已落 route.refuse
            tracing::warn!(%response_id, error = %e, "请求被治理层拒绝");
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
            let _ = tx.send(ev_created(&response_id, &req.model));
            let _ = tx.send(ev_failed(&e.to_string()));
            drop(tx);
            let stream = UnboundedReceiverStream::new(rx)
                .map(|v| Ok::<Event, Infallible>(Event::default().data(v.to_string())));
            return Sse::new(stream).keep_alive(KeepAlive::default()).into_response();
        }
    };
    let provider = resolution.provider.clone();
    let meta = provider.metadata().clone();
    let label = resolution.plan.effective;
    let model = meta.model.clone();
    let names = translated.names;
    // 外发按请求载荷实测(与 Runner 同口径)
    let request_repr = json!({ "model": model, "messages": &translated.request.messages, "tools": translated.request.tools.len() }).to_string();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let gov = st.gov.clone();
    let rid = response_id.clone();
    let id_base = st.next_id("item");
    let idle = st.idle_timeout;

    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let _ = tx.send(ev_created(&rid, &model));
        let mut stream = match provider.chat_stream(translated.request).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(%rid, error = %e, "开流失败");
                gov.record_call(&rid, &meta, label, &request_repr, 0, None, started.elapsed().as_millis() as u64);
                let _ = tx.send(ev_failed(&format!("开流失败:{e}")));
                return;
            }
        };
        tracing::info!(%rid, elapsed_ms = started.elapsed().as_millis(), "上游已开流");

        let mut text = String::new();
        let mut acc = ToolCallAccumulator::new();
        let mut usage: Option<Usage> = None;
        let mut first_token_logged = false;
        let msg_item_id = format!("{id_base}_msg");
        let mut msg_item_opened = false;

        loop {
            // 空闲看门狗:上游静默超过 idle 即判定挂起并显式失败,
            // 绝不让 Codex 无限等待(fail-fast 优于假死)。
            let ev = match tokio::time::timeout(idle, stream.next()).await {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(_) => {
                    let secs = idle.as_secs();
                    tracing::error!(%rid, idle_secs = secs, "上游流空闲超时,判定挂起");
                    gov.record_call(&rid, &meta, label, &request_repr, text.len() as u64, None, started.elapsed().as_millis() as u64);
                    let _ = tx.send(ev_failed(&format!(
                        "上游流空闲超过 {secs}s 未发送数据,判定挂起(网关空闲看门狗)"
                    )));
                    return;
                }
            };
            if !first_token_logged {
                first_token_logged = true;
                tracing::info!(%rid, elapsed_ms = started.elapsed().as_millis(), "上游首个事件");
            }
            match ev {
                Ok(StreamEvent::TextDelta(d)) => {
                    // 协议状态机:added → delta* → done。漏掉 added 会让 Codex
                    // panic(OutputTextDelta without active item),实测踩过。
                    if !msg_item_opened {
                        msg_item_opened = true;
                        if tx.send(ev_message_added(&msg_item_id)).is_err() {
                            return;
                        }
                    }
                    text.push_str(&d);
                    if tx.send(ev_text_delta(&d)).is_err() {
                        return; // 客户端断开
                    }
                }
                Ok(ev @ StreamEvent::ToolCallDelta { .. }) => acc.push_event(&ev),
                Ok(StreamEvent::Usage(u)) => {
                    usage = Some(Usage {
                        input_tokens: u.prompt_tokens as u64,
                        output_tokens: u.completion_tokens as u64,
                        total_tokens: u.total_tokens as u64,
                    })
                }
                Ok(StreamEvent::Finish(_)) => {}
                Err(e) => {
                    tracing::error!(%rid, error = %e, "上游流错误");
                    gov.record_call(&rid, &meta, label, &request_repr, text.len() as u64, None, started.elapsed().as_millis() as u64);
                    let _ = tx.send(ev_failed(&e.to_string()));
                    return;
                }
            }
        }

        // 成型顺序:文本 item 先于工具调用 item(与 OpenAI 一致)。
        // 每个 item 都是 added → (delta*) → done 的完整三段,不留半开状态。
        if !text.is_empty() {
            if !msg_item_opened {
                let _ = tx.send(ev_message_added(&msg_item_id));
            }
            let _ = tx.send(ev_message_done(&msg_item_id, &text));
        }
        let calls = acc.finish();
        let bytes_in = text.len() as u64 + calls.iter().map(|c| c.arguments.len() as u64).sum::<u64>();
        gov.record_call(&rid, &meta, label, &request_repr, bytes_in, usage.as_ref().map(|u| (u.input_tokens, u.output_tokens)), started.elapsed().as_millis() as u64);
        let call_names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        for (i, call) in calls.iter().enumerate() {
            let item_id = format!("{id_base}_fc{i}");
            let _ = tx.send(ev_function_call_added(&item_id, call, &names));
            let _ = tx.send(ev_function_call_done(&item_id, call, &names));
        }
        tracing::info!(
            %rid,
            elapsed_ms = started.elapsed().as_millis(),
            text_len = text.chars().count(),
            tool_calls = ?call_names,
            "本轮完成"
        );
        let _ = tx.send(ev_completed(&rid, usage));
    });

    let stream = UnboundedReceiverStream::new(rx)
        .map(|v| Ok::<Event, Infallible>(Event::default().data(v.to_string())));
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}
