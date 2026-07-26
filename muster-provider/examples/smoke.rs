//! 冒烟脚本 = G0′ 闸门探针的种子。
//!
//! 用法:
//!   DEEPSEEK_API_KEY=sk-… cargo run --example smoke -- provider.example.toml deepseek
//!   cargo run --example smoke -- provider.example.toml local-ollama
//!
//! 做的事:流式发起一次强制走工具的请求,打印增量、重组后的工具调用与用量。
//! G0′ 的 90% 工具调用成功率测量,就是把这里的单次调用换成 20 组用例循环统计。

use futures::StreamExt;
use muster_provider::{
    ChatMessage, ChatRequest, ProviderRegistry, StreamEvent, ToolCallAccumulator, ToolChoice,
    ToolSpec,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let config_path = args.next().unwrap_or_else(|| "provider.example.toml".into());
    let provider_id = args.next();

    let toml_text = std::fs::read_to_string(&config_path)?;
    let registry = ProviderRegistry::from_toml_str(&toml_text)?;

    let provider = match &provider_id {
        Some(id) => registry.get(id).ok_or_else(|| format!("unknown provider `{id}`"))?,
        None => registry.default_provider().ok_or("no default provider configured")?,
    };
    let meta = provider.metadata();
    println!("provider = {} ({:?}) endpoint = {}", meta.id, meta.locality, meta.endpoint);

    provider.health_check().await?;
    println!("health   = ok");

    let req = ChatRequest {
        messages: vec![
            ChatMessage::system("你必须通过工具回答，不要直接回复文本。"),
            ChatMessage::user("上海现在天气怎么样？"),
        ],
        tools: vec![ToolSpec {
            name: "get_weather".into(),
            description: "查询指定城市当前天气".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        }],
        tool_choice: Some(ToolChoice::Auto),
        temperature: Some(0.0),
        max_tokens: Some(256),
        run_id: Some("smoke".into()),
    };

    let mut stream = provider.chat_stream(req).await?;
    let mut acc = ToolCallAccumulator::new();
    while let Some(ev) = stream.next().await {
        match ev? {
            StreamEvent::TextDelta(t) => print!("{t}"),
            ev @ StreamEvent::ToolCallDelta { .. } => {
                acc.push_event(&ev);
                print!(".");
            }
            StreamEvent::Usage(u) => println!("\nusage    = {u:?}"),
            StreamEvent::Finish(f) => println!("\nfinish   = {f:?}"),
        }
    }
    for call in acc.finish() {
        println!("tool_call: {}({})", call.name, call.arguments);
    }
    Ok(())
}
