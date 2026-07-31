//! 会议 Agent:进房间、听、转写、回传。
//!
//! ```bash
//! MUSTER_SERVER=http://localhost:8787 \
//! MUSTER_TOKEN=<账号令牌> \
//! MUSTER_STT_URL=http://localhost:9000/v1 \
//! cargo run -p muster-meeting-agent --features livekit --example agent -- <会议id>
//! ```
//!
//! 它自己去 `/meetings/:id/join` 换入会令牌——**和人走同一个接口**,
//! 所以"Agent 能不能进这个会、能不能开麦"同样由权限内核决定,
//! 不存在给 Agent 开后门这回事。

use std::sync::Arc;

use muster_meeting_agent::{room, ChunkConfig, EnergyGate, HttpSink, Pipeline};
use muster_provider::{SpeechCompatProvider, SpeechConfig, SpeechProvider};
use muster_route::{OrgPolicy, Sensitivity, SpeechRouter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "muster_meeting_agent=info,agent=info".into()),
        )
        .init();

    let meeting = std::env::args().nth(1).ok_or("用法:agent <会议id>")?;
    let server = std::env::var("MUSTER_SERVER").unwrap_or_else(|_| "http://localhost:8787".into());
    let token = std::env::var("MUSTER_TOKEN").map_err(|_| "需要 MUSTER_TOKEN")?;
    let stt_url = std::env::var("MUSTER_STT_URL").unwrap_or_else(|_| "http://localhost:9000/v1".into());

    // 换入会令牌:和人走同一个接口,同一套权限判定
    let join: serde_json::Value = reqwest::Client::new()
        .post(format!("{server}/meetings/{meeting}/join"))
        .bearer_auth(&token)
        .send()
        .await?
        .json()
        .await?;
    if let Some(e) = join.get("error") {
        return Err(format!("入会被拒:{e}").into());
    }
    let lk_url = join["url"].as_str().ok_or("响应缺 url")?;
    let lk_token = join["token"].as_str().ok_or("响应缺 token")?;
    println!("会议密级 {} · 房间 {}", join["level"], join["room"]);

    let mut cfg = SpeechConfig::local_whisper(&stt_url);
    if let Ok(m) = std::env::var("MUSTER_STT_MODEL") {
        cfg.model = m;
    }
    let stt: Arc<dyn SpeechProvider> = Arc::new(SpeechCompatProvider::new("whisper", cfg)?);
    println!("转写落点 {stt_url}(locality={:?})", stt.metadata().locality);

    let mut pipeline = Pipeline::new(
        Arc::new(SpeechRouter::new(vec![stt])),
        Arc::new(HttpSink::new(&server, &token, &meeting)),
    )
    .with_language("zh");
    // 领域词表:中文默认出繁体,且技术术语容易转成同音别字。
    // 这段每次调用都原样发给后端 ⇒ **绝不能放机密内容**。
    if let Ok(p) = std::env::var("MUSTER_STT_PROMPT") {
        pipeline = pipeline.with_prompt(p);
    }

    // 演习状态本应从服务端读(组织策略在那边);当前先从环境取。
    let mut policy = OrgPolicy::new(Sensitivity::Internal)?;
    if std::env::var("MUSTER_DRILL").is_ok() {
        policy.set_egress_locked(true);
        println!("⚑ 演习模式");
    }

    println!("进房间,开始听…\n");
    room::run_with_pipeline(
        lk_url,
        lk_token,
        ChunkConfig::default(),
        EnergyGate::default(),
        Arc::new(pipeline),
        policy,
    )
    .await?;
    println!("会议结束");
    Ok(())
}
