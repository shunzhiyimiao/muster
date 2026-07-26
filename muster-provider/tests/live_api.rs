//! Live integration tests — network + credentials required, therefore `#[ignore]`.
//!
//! Run explicitly:
//!   DEEPSEEK_API_KEY=sk-… cargo test --test live_api -- --ignored deepseek
//!   (with a local Ollama running)  cargo test --test live_api -- --ignored ollama
//!
//! The Ollama case doubles as the A6 weekly keep-alive smoke.

use muster_provider::{
    collect_stream, ChatMessage, ChatRequest, FinishReason, ModelProvider, OpenAiCompatConfig,
    OpenAiCompatProvider, ToolChoice, ToolSpec,
};

fn weather_tool() -> ToolSpec {
    ToolSpec {
        name: "get_weather".into(),
        description: "查询指定城市当前天气".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    }
}

fn tool_call_request() -> ChatRequest {
    ChatRequest {
        messages: vec![
            ChatMessage::system("你必须通过工具回答，不要直接回复文本。"),
            ChatMessage::user("上海现在天气怎么样？"),
        ],
        tools: vec![weather_tool()],
        tool_choice: Some(ToolChoice::Auto),
        temperature: Some(0.0),
        max_tokens: Some(256),
        run_id: Some("live-test".into()),
    }
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY and egress"]
async fn deepseek_streaming_tool_call() {
    let key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY");
    let p = OpenAiCompatProvider::new("deepseek", OpenAiCompatConfig::deepseek(key)).unwrap();

    p.health_check().await.expect("health");
    let stream = p.chat_stream(tool_call_request()).await.expect("stream open");
    let resp = collect_stream(stream, "deepseek-chat").await.expect("stream drain");

    assert_eq!(resp.finish_reason, FinishReason::ToolCalls, "model should call the tool");
    let call = &resp.message.tool_calls[0];
    assert_eq!(call.name, "get_weather");
    let args: serde_json::Value = serde_json::from_str(&call.arguments).expect("valid JSON args");
    assert!(args["city"].as_str().unwrap_or_default().contains("上海"));
}

#[tokio::test]
#[ignore = "requires a local Ollama daemon"]
async fn ollama_smoke_chat() {
    let p = OpenAiCompatProvider::new("ollama", OpenAiCompatConfig::ollama()).unwrap();
    p.health_check().await.expect("ollama up");
    let resp = p
        .chat(ChatRequest {
            messages: vec![ChatMessage::user("回复两个字：收到")],
            max_tokens: Some(16),
            ..Default::default()
        })
        .await
        .expect("chat");
    assert!(resp.message.content.unwrap_or_default().contains("收到"));
}
