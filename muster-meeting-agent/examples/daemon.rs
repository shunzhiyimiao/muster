//! 会议 Agent 常驻服务:轮询服务端,谁请就去谁那儿。
//!
//! ## 为什么是常驻服务,而不是按钮直接起进程
//!
//! 让桌面壳自己 spawn 一个 Agent 的话,**两个人开会就有两个 Agent 在同一个
//! 房间里各转各的**——同一句话转两遍,纪要里出现重复行,模型调用也翻倍。
//! 而且 Agent 该跑在服务器上(架构文档的部署拓扑),不该在每个人的笔记本上。
//!
//! 所以按钮只在服务端记一个**意愿**(`meeting.wants_agent`),由这个进程认领。
//! 好处不止是去重:
//!
//! - Agent 崩了、机器重启了,下一轮轮询自动回到会里;
//! - 会议结束或有人请它离开,它自己退出那场会;
//! - 谁都不必知道会议 id——按钮点一下就行。
//!
//! ```bash
//! MUSTER_SERVER=http://localhost:8787 \
//! MUSTER_ACCOUNT=A-007 MUSTER_PASSWORD=… \
//! MUSTER_STT_URL=http://localhost:9000/v1 \
//! cargo run -p muster-meeting-agent --features livekit --example daemon
//! ```
//!
//! ## 诚实边界
//!
//! - **轮询,不是推送**:间隔 5 秒,所以点了按钮最多等 5 秒它才进来。
//!   服务端已有 SSE,换成订阅是后续的事;当前这样简单且不会漏。
//! - 单进程:所有会议在同一个进程里跑,一场会拖垮进程会连累别的会。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use muster_meeting_agent::{
    room, Answer, Answerer, ChunkConfig, Context, EnergyGate, ExtractOutcome, Extractor, HttpSink,
    MentionRules, Pipeline,
};
use muster_provider::{ProviderRegistry, SpeechCompatProvider, SpeechConfig, SpeechProvider};
use muster_route::{OrgPolicy, Router, Sensitivity, SpeechRouter};
use tokio::sync::Mutex;

const POLL: Duration = Duration::from_secs(5);

struct Cfg {
    server: String,
    token: String,
    stt_url: String,
    stt_model: Option<String>,
    stt_prompt: Option<String>,
    provider_config: Option<String>,
    aliases: Vec<String>,
    agent_name: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "muster_meeting_agent=info,daemon=info".into()),
        )
        .init();

    let server = std::env::var("MUSTER_SERVER").unwrap_or_else(|_| "http://localhost:8787".into());
    let http = reqwest::Client::new();

    // 当场登录,不吃预签的令牌:守护进程一跑就是几天,令牌 12 小时过期
    let (acct, pw) = (
        std::env::var("MUSTER_ACCOUNT").map_err(|_| "需要 MUSTER_ACCOUNT")?,
        std::env::var("MUSTER_PASSWORD").map_err(|_| "需要 MUSTER_PASSWORD")?,
    );
    let cfg = Arc::new(Cfg {
        token: login(&http, &server, &acct, &pw).await?,
        server: server.clone(),
        stt_url: std::env::var("MUSTER_STT_URL")
            .unwrap_or_else(|_| "http://localhost:9000/v1".into()),
        stt_model: std::env::var("MUSTER_STT_MODEL").ok(),
        stt_prompt: std::env::var("MUSTER_STT_PROMPT").ok(),
        provider_config: std::env::var("MUSTER_PROVIDER_CONFIG").ok(),
        aliases: std::env::var("MUSTER_AGENT_ALIASES")
            .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_else(|_| MentionRules::default().aliases),
        agent_name: std::env::var("MUSTER_AGENT_NAME").unwrap_or_else(|_| "小七".into()),
    });

    println!("会议 Agent 常驻服务已启动");
    println!("  服务端 {} · 身份 {}", cfg.server, acct);
    println!("  转写落点 {}", cfg.stt_url);
    println!(
        "  作答 {}",
        if cfg.provider_config.is_some() { "已启用" } else { "未启用(只转写)" }
    );
    println!("  每 {}s 查一次谁请了 Agent\n", POLL.as_secs());

    let mut joined: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    loop {
        // 令牌过期就重登。守护进程活得比令牌久,这是常态不是异常。
        let wanted = match wanted_meetings(&http, &cfg.server, &cfg.token).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "查询失败,稍后重试");
                tokio::time::sleep(POLL).await;
                continue;
            }
        };

        // 已结束或被请离的:任务自己会退出,这里清掉句柄
        joined.retain(|id, h| {
            if h.is_finished() {
                tracing::info!(meeting = %id, "已退出该会议");
                false
            } else {
                true
            }
        });

        for m in wanted {
            let id = m["id"].as_str().unwrap_or_default().to_string();
            if id.is_empty() || joined.contains_key(&id) {
                continue;
            }
            let title = m["title"].as_str().unwrap_or("").to_string();
            tracing::info!(meeting = %id, %title, "被请去开会");
            let c = cfg.clone();
            joined.insert(id.clone(), tokio::spawn(async move { serve_meeting(c, id).await }));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// 向服务端要 provider 目录。
///
/// 拿不到就返回 `None`,由调用方回落到本地 TOML **并告警**——
/// 会议 Agent 是常驻服务,不该因为服务端抖一下就整个停摆;但"此刻用的是
/// 本机声明的 locality"这件事必须说出来,不能悄悄发生。
///
/// (桌面壳那边更严:拿不到就不算连上。区别在于桌面壳前面坐着一个人,
/// 他能立刻看见并决定重试;守护进程没有。)
async fn fetch_catalog(http: &reqwest::Client, cfg: &Cfg) -> Option<serde_json::Value> {
    let r = http
        .get(format!("{}/providers/catalog", cfg.server))
        .bearer_auth(&cfg.token)
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        tracing::warn!(status = %r.status(), "拿不到 provider 目录,回落到本地配置");
        return None;
    }
    let v: serde_json::Value = r.json().await.ok()?;
    let n = v.get("providers").and_then(|p| p.as_object()).map(|o| o.len()).unwrap_or(0);
    if n == 0 {
        tracing::warn!("服务端的 provider 目录是空的,回落到本地配置");
        return None;
    }
    tracing::info!(n, "已采用服务端下发的 provider 目录");
    Some(v)
}

async fn login(
    http: &reqwest::Client,
    server: &str,
    id: &str,
    pw: &str,
) -> Result<String, String> {
    let v: serde_json::Value = http
        .post(format!("{server}/auth/login"))
        .json(&serde_json::json!({ "id": id, "password": pw }))
        .send()
        .await
        .map_err(|e| format!("连不上服务端:{e}"))?
        .json()
        .await
        .map_err(|e| format!("登录响应无法解析:{e}"))?;
    v["token"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| format!("登录失败:{}", v["error"].as_str().unwrap_or("未知原因")))
}

async fn wanted_meetings(
    http: &reqwest::Client,
    server: &str,
    token: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let r = http
        .get(format!("{server}/meetings/agent-wanted"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status()));
    }
    r.json().await.map_err(|e| e.to_string())
}

/// 在一场会里干活,直到房间断开或被请离。
async fn serve_meeting(cfg: Arc<Cfg>, meeting: String) {
    if let Err(e) = serve_inner(cfg, &meeting).await {
        tracing::warn!(meeting = %meeting, error = %e, "这场会没能服务");
    }
}

async fn serve_inner(cfg: Arc<Cfg>, meeting: &str) -> Result<(), Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    let join: serde_json::Value = http
        .post(format!("{}/meetings/{meeting}/join", cfg.server))
        .bearer_auth(&cfg.token)
        .send()
        .await?
        .json()
        .await?;
    if let Some(e) = join.get("error") {
        return Err(format!("入会被拒:{e}").into());
    }
    let (lk_url, lk_token) = (
        join["url"].as_str().ok_or("缺 url")?.to_string(),
        join["token"].as_str().ok_or("缺 token")?.to_string(),
    );
    let level = match join["level"].as_str() {
        Some("restricted") => Sensitivity::Restricted,
        Some("open") => Sensitivity::Open,
        _ => Sensitivity::Internal,
    };

    let mut scfg = SpeechConfig::local_whisper(&cfg.stt_url);
    if let Some(m) = &cfg.stt_model {
        scfg.model = m.clone();
    }
    let stt: Arc<dyn SpeechProvider> = Arc::new(SpeechCompatProvider::new("whisper", scfg)?);
    let sink = Arc::new(HttpSink::new(&cfg.server, &cfg.token, meeting));
    let mut pipeline =
        Pipeline::new(Arc::new(SpeechRouter::new(vec![stt])), sink.clone()).with_language("zh");
    if let Some(p) = &cfg.stt_prompt {
        pipeline = pipeline.with_prompt(p.clone());
    }

    let mut policy = OrgPolicy::new(Sensitivity::Internal)?;
    if std::env::var("MUSTER_DRILL").is_ok() {
        policy.set_egress_locked(true);
    }

    // Provider 目录**优先向服务端要**。
    //
    // 本地那份 TOML 只在服务端没有目录时兜底,而且会告警:决定 restricted
    // 内容能不能出门的 `locality`,不该由跑模型的这台机器自己声明。
    let toml_from_server = fetch_catalog(&http, &cfg).await;
    let (answerer, extractor) = match (&toml_from_server, &cfg.provider_config) {
        (Some(_), _) | (None, Some(_)) => {
            let reg = match &toml_from_server {
                Some(json) => ProviderRegistry::from_config(serde_json::from_value(json.clone())?)?,
                None => ProviderRegistry::from_toml_str(&std::fs::read_to_string(
                    cfg.provider_config.as_ref().expect("上面的 match 保证了它是 Some"),
                )?)?,
            };
            let providers: Vec<_> = reg.ids().iter().filter_map(|id| reg.get(id)).collect();
            let router = Arc::new(Router::new(providers, policy.clone()));
            (
                Some(Arc::new(Answerer::new(
                    router.clone(),
                    MentionRules { aliases: cfg.aliases.clone(), ..Default::default() },
                    level,
                    meeting,
                ))),
                Some(Arc::new(Extractor::new(router, level, meeting))),
            )
        }
        (None, None) => (None, None),
    };

    // 中途入会补上下文——晚到、重连、崩溃重启都是常态
    let ctx = Arc::new(Mutex::new(Context::default()));
    if let Ok(r) = http
        .get(format!("{}/meetings/{meeting}/transcript", cfg.server))
        .bearer_auth(&cfg.token)
        .send()
        .await
    {
        let rows: Vec<serde_json::Value> = r.json().await.unwrap_or_default();
        let n = rows.len();
        let mut c = ctx.lock().await;
        for row in rows {
            if let (Some(sp), Some(tx)) = (row["speaker_id"].as_str(), row["text"].as_str()) {
                c.push(sp, tx);
            }
        }
        if n > 0 {
            tracing::info!(meeting = %meeting, n, "已补回既有纪要");
        }
    }

    let pipeline = Arc::new(pipeline);
    let (ctx2, sink2, name2) = (ctx.clone(), sink.clone(), cfg.agent_name.clone());
    let name = cfg.agent_name.clone();
    let ex_live = extractor.clone();

    room::run(&lk_url, &lk_token, ChunkConfig::default(), EnergyGate::default(), move |u| {
        let (p, pol, ctx, ans, sink, name, ex) = (
            pipeline.clone(),
            policy.clone(),
            ctx.clone(),
            answerer.clone(),
            sink.clone(),
            name.clone(),
            ex_live.clone(),
        );
        async move {
            let speaker = u.speaker.clone();
            let Some(text) = p.handle(u, &pol).await else { return };
            ctx.lock().await.push(&speaker, &text);

            let Some(ans) = ans else { return };

            // 会中即时派活:叫了它、并且是在派活 ⇒ 提一条待批任务,而不是回答。
            //
            // **提出来仍然只是提案**:确认必须由人做,而且服务端不许 Agent
            // 确认自己提的(见 muster-server/src/action.rs)。
            if let (Some(_), Some(ex)) = (ans.task_request_in(&text), ex.as_ref()) {
                match ex.from_utterance(&speaker, &text).await {
                    ExtractOutcome::Items(items) if !items.is_empty() => {
                        for it in &items {
                            sink.propose_action(it).await;
                        }
                        // **必须说出来。** 不吭声的话人不知道它记没记,
                        // 会再说一遍,于是多出一条重复的待批项。
                        let line =
                            format!("已记下一条待批任务:{}(请在任务面板批准)", items[0].text);
                        sink.say(&name, &line).await;
                        ctx.lock().await.push(&name, &line);
                    }
                    ExtractOutcome::Items(_) => {
                        // 听出是在派活,但没提炼出内容——说清楚,别装作没听见
                        let line = "我听出你在派活,但没听清具体要做什么,能再说一遍吗?".to_string();
                        sink.say(&name, &line).await;
                        ctx.lock().await.push(&name, &line);
                    }
                    ExtractOutcome::Unavailable(why) => {
                        sink.say(&name, &format!("[这条任务没能记下:{why}]")).await;
                    }
                }
                return;
            }

            let Some(q) = ans.question_in(&text).map(str::to_owned) else { return };
            let snapshot = { ctx.lock().await.transcript() };
            let mut c = Context::new(64);
            for line in snapshot.lines() {
                if let Some((s, t)) = line.split_once(':') {
                    c.push(s, t);
                }
            }
            match ans.answer(&q, &c).await {
                Answer::Text(t) => {
                    sink.say(&name, &t).await;
                    ctx.lock().await.push(&name, &t);
                }
                Answer::Unavailable(why) => {
                    sink.say(&name, &format!("[我这次没法回答:{why}]")).await;
                }
            }
        }
    })
    .await?;

    // 散会提炼:提案,确认归人
    if let Some(ex) = extractor {
        let transcript = ctx2.lock().await.transcript();
        match ex.extract(&transcript).await {
            ExtractOutcome::Items(items) if !items.is_empty() => {
                for it in &items {
                    sink2.propose_action(it).await;
                }
                tracing::info!(meeting = %meeting, n = items.len(), "已提出行动项(待人确认)");
            }
            ExtractOutcome::Items(_) => tracing::info!(meeting = %meeting, "没有明确的行动项"),
            ExtractOutcome::Unavailable(why) => {
                sink2.say(&name2, &format!("[散会提炼未能完成:{why}]")).await;
            }
        }
    }
    Ok(())
}
