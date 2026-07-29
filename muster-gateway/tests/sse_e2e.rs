//! 端到端(Mock provider,零网络):真起 Axum 服务,用 HTTP 客户端读 SSE,
//! 断言事件序列与 Codex 解析器要求的形状一致。

use std::sync::Arc;

use futures::StreamExt;
use muster_gateway::{server::router, GatewayState};
#[allow(unused_imports)]
use muster_provider::StreamEvent;
use muster_provider::{MockProvider, ModelProvider};
use serde_json::{json, Value};

async fn spawn(provider: Arc<dyn ModelProvider>) -> String {
    spawn_with(GatewayState::new(provider)).await
}

async fn spawn_with(state: GatewayState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    format!("http://{addr}")
}

/// 读完整个 SSE 流,返回逐条 data 的 JSON。
async fn post_responses(base: &str, body: Value) -> Vec<Value> {
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/responses"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "HTTP {}", resp.status());
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    buf.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str::<Value>(d).expect("SSE data 必须是 JSON"))
        .collect()
}

fn kinds(evs: &[Value]) -> Vec<&str> {
    evs.iter().map(|e| e["type"].as_str().unwrap_or("?")).collect()
}

#[tokio::test]
async fn text_turn_produces_codex_shaped_events() {
    let mock = MockProvider::cloud("mock").with_text("你好,世界");
    let base = spawn(Arc::new(mock)).await;

    let evs = post_responses(
        &base,
        json!({
            "model": "m", "instructions": "你是 agent", "stream": true,
            "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "打招呼" }] }]
        }),
    )
    .await;

    let k = kinds(&evs);
    assert_eq!(k.first(), Some(&"response.created"));
    assert_eq!(k.last(), Some(&"response.completed"));
    assert!(k.contains(&"response.output_text.delta"), "{k:?}");
    // 正文经 output_item.done 交付(Codex 从这里取消息)
    let done = evs.iter().find(|e| e["type"] == "response.output_item.done").expect("必须有 item.done");
    assert_eq!(done["item"]["type"], "message");
    assert_eq!(done["item"]["content"][0]["text"], "你好,世界");
    // 增量拼起来 == 正文
    let joined: String = evs
        .iter()
        .filter(|e| e["type"] == "response.output_text.delta")
        .map(|e| e["delta"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(joined, "你好,世界");
}

#[tokio::test]
async fn tool_call_turn_maps_to_function_call_item() {
    let mock = MockProvider::cloud("mock").with_tool_call("list_dir", r#"{"path":"."}"#);
    let base = spawn(Arc::new(mock)).await;

    let evs = post_responses(
        &base,
        json!({
            "model": "m", "stream": true,
            "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "列目录" }] }],
            "tools": [{ "type": "function", "name": "list_dir", "description": "列目录",
                        "parameters": { "type": "object", "properties": { "path": { "type": "string" } } } }],
            "tool_choice": "auto"
        }),
    )
    .await;

    let fc = evs
        .iter()
        .find(|e| e["type"] == "response.output_item.done" && e["item"]["type"] == "function_call")
        .expect("必须产出 function_call item");
    assert_eq!(fc["item"]["name"], "list_dir");
    assert_eq!(fc["item"]["arguments"], r#"{"path":"."}"#);
    assert!(fc["item"]["call_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(kinds(&evs).last(), Some(&"response.completed"));
}

#[tokio::test]
async fn provider_failure_surfaces_as_response_failed() {
    let mock = MockProvider::cloud("mock").with_text("unused");
    mock.set_healthy(false);
    let base = spawn(Arc::new(mock)).await;

    let evs = post_responses(
        &base,
        json!({ "model": "m", "stream": true,
                "input": [{ "type": "message", "role": "user", "content": "x" }] }),
    )
    .await;

    let k = kinds(&evs);
    assert_eq!(k.first(), Some(&"response.created"));
    assert_eq!(k.last(), Some(&"response.failed"), "失败必须显式告知,不能静默截断:{k:?}");
    assert!(evs.last().unwrap()["response"]["error"]["message"].is_string());
}

/// 上游"已开流但不再吐字节"的挂起流:A2 的总超时管不住它(实测踩到 64 分钟
/// 无响应),网关的空闲看门狗必须在限期内显式失败,不能让客户端假死。
#[tokio::test]
async fn hung_upstream_trips_idle_watchdog() {
    let mock = MockProvider::cloud("mock").with_hang();
    let state = GatewayState::new(Arc::new(mock) as Arc<dyn ModelProvider>)
        .with_idle_timeout(std::time::Duration::from_millis(300));
    let base = spawn_with(state).await;

    let started = std::time::Instant::now();
    let evs = post_responses(
        &base,
        json!({ "model": "m", "stream": true,
                "input": [{ "type": "message", "role": "user", "content": "x" }] }),
    )
    .await;
    let elapsed = started.elapsed();

    assert!(elapsed < std::time::Duration::from_secs(5), "必须由看门狗掐断,而非无限等待:{elapsed:?}");
    let k = kinds(&evs);
    assert_eq!(k.last(), Some(&"response.failed"), "{k:?}");
    let msg = evs.last().unwrap()["response"]["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("空闲"), "失败原因要说清是挂起而非其它:{msg}");
}

#[tokio::test]
async fn models_endpoint_answers_health_probe() {
    let base = spawn(Arc::new(MockProvider::cloud("mock").with_text("x"))).await;
    let v: Value = reqwest::get(format!("{base}/v1/models")).await.unwrap().json().await.unwrap();
    assert_eq!(v["object"], "list");
    assert!(v["data"][0]["id"].as_str().is_some());
}
