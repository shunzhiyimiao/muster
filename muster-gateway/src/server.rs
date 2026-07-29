//! Axum 服务:`POST /v1/responses`(SSE)+ `GET /v1/models` + `GET /health`。

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio_stream::wrappers::UnboundedReceiverStream;

use muster_provider::{ModelProvider, StreamEvent, ToolCallAccumulator};

use crate::translate::{
    ev_completed, ev_created, ev_failed, ev_function_call_done, ev_message_done, ev_text_delta,
    to_chat, ResponsesRequest, Usage,
};

#[derive(Clone)]
pub struct GatewayState {
    pub provider: Arc<dyn ModelProvider>,
    seq: Arc<AtomicU64>,
}

impl GatewayState {
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        Self { provider, seq: Arc::new(AtomicU64::new(0)) }
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
    let meta = state.provider.metadata().clone();
    tracing::info!(
        "muster-gateway 监听 http://127.0.0.1:{port}/v1 → {} ({}, {:?})",
        meta.id,
        meta.model,
        meta.locality
    );
    axum::serve(listener, router(state)).await
}

async fn models(State(st): State<GatewayState>) -> impl IntoResponse {
    let m = st.provider.metadata().clone();
    Json(json!({
        "object": "list",
        "data": [{ "id": m.model, "object": "model", "owned_by": m.id }]
    }))
}

async fn responses(State(st): State<GatewayState>, body: Json<Value>) -> impl IntoResponse {
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
    let model = st.provider.metadata().model.clone();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let provider = st.provider.clone();
    let rid = response_id.clone();
    let id_base = st.next_id("item");

    tokio::spawn(async move {
        let _ = tx.send(ev_created(&rid, &model));
        let mut stream = match provider.chat_stream(translated.request).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(ev_failed(&format!("开流失败:{e}")));
                return;
            }
        };

        let mut text = String::new();
        let mut acc = ToolCallAccumulator::new();
        let mut usage: Option<Usage> = None;

        while let Some(ev) = stream.next().await {
            match ev {
                Ok(StreamEvent::TextDelta(d)) => {
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
                    let _ = tx.send(ev_failed(&e.to_string()));
                    return;
                }
            }
        }

        // 成型顺序:文本 item 先于工具调用 item(与 OpenAI 一致)。
        if !text.is_empty() {
            let _ = tx.send(ev_message_done(&format!("{id_base}_msg"), &text));
        }
        for (i, call) in acc.finish().into_iter().enumerate() {
            let _ = tx.send(ev_function_call_done(&format!("{id_base}_fc{i}"), &call));
        }
        let _ = tx.send(ev_completed(&rid, usage));
    });

    let stream = UnboundedReceiverStream::new(rx)
        .map(|v| Ok::<Event, Infallible>(Event::default().data(v.to_string())));
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}
