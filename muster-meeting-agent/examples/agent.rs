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

use muster_meeting_agent::{
    room, Answer, Answerer, ChunkConfig, Context, EnergyGate, HttpSink, MentionRules, Pipeline,
};
use muster_provider::{ProviderRegistry, SpeechCompatProvider, SpeechConfig, SpeechProvider};
use muster_route::{OrgPolicy, Router, Sensitivity, SpeechRouter};
use tokio::sync::Mutex;

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

    let sink = Arc::new(HttpSink::new(&server, &token, &meeting));
    let mut pipeline = Pipeline::new(Arc::new(SpeechRouter::new(vec![stt])), sink.clone())
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

    // ---- 被叫到时作答(B1)。没配对话 provider 就只转写不作答,
    // 而不是启动失败——听会本身已经有价值。
    let level = match join["level"].as_str() {
        Some("restricted") => Sensitivity::Restricted,
        Some("open") => Sensitivity::Open,
        _ => Sensitivity::Internal,
    };
    let answerer = match std::env::var("MUSTER_PROVIDER_CONFIG") {
        Ok(path) => {
            let reg = ProviderRegistry::from_toml_str(&std::fs::read_to_string(&path)?)?;
            let providers: Vec<_> =
                reg.ids().iter().filter_map(|id| reg.get(id)).collect();
            if providers.is_empty() {
                return Err("provider 配置里一个都没有".into());
            }
            let router = Arc::new(Router::new(providers, policy.clone()));
            let aliases: Vec<String> = std::env::var("MUSTER_AGENT_ALIASES")
                .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_else(|_| MentionRules::default().aliases);
            println!("作答已启用 · 别名 {aliases:?}");
            Some(Arc::new(Answerer::new(
                router,
                MentionRules { aliases, ..Default::default() },
                level,
                &meeting,
            )))
        }
        Err(_) => {
            println!("未配 MUSTER_PROVIDER_CONFIG:只转写,不作答");
            None
        }
    };
    let agent_name = std::env::var("MUSTER_AGENT_NAME").unwrap_or_else(|_| "小七".into());
    let ctx = Arc::new(Mutex::new(Context::default()));

    println!("进房间,开始听…\n");
    let pipeline = Arc::new(pipeline);
    room::run(lk_url, lk_token, ChunkConfig::default(), EnergyGate::default(), move |u| {
        let (p, pol, ctx, ans, sink, name) = (
            pipeline.clone(),
            policy.clone(),
            ctx.clone(),
            answerer.clone(),
            sink.clone(),
            agent_name.clone(),
        );
        async move {
            // 先转写落库,再判断要不要作答——**顺序不能反**:
            // 人问的那句本身也是会议记录的一部分,答了却没记下问题,
            // 事后看纪要就成了 Agent 自说自话。
            let (speaker, started) = (u.speaker.clone(), u.started_ms);
            let text = p.handle(u, &pol).await;
            let Some(text) = text else { return };
            ctx.lock().await.push(&speaker, &text);

            let Some(ans) = ans else { return };
            let Some(q) = ans.question_in(&text).map(str::to_owned) else { return };
            tracing::info!(speaker = %speaker, at_ms = started, question = %q, "被叫到了");
            let snapshot = { ctx.lock().await.transcript() };
            let mut c = Context::new(64);
            for line in snapshot.lines() {
                if let Some((s, t)) = line.split_once(':') {
                    c.push(s, t);
                }
            }
            match ans.answer(&q, &c).await {
                Answer::Text(t) => {
                    println!("💬 {name}:{t}");
                    sink.say(&name, &t).await;
                    ctx.lock().await.push(&name, &t);
                }
                // 答不了要说出来:沉默会被当成"它没听见",于是有人再喊一遍、
                // 再等一次,把一次治理拒绝变成一分钟冷场
                Answer::Unavailable(why) => {
                    println!("⛔ 无法作答:{why}");
                    sink.say(&name, &format!("[我这次没法回答:{why}]")).await;
                }
            }
        }
    })
    .await?;
    println!("会议结束");
    Ok(())
}
