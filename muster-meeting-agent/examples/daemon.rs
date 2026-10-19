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
//! ## 推送为主,轮询兜底(2026-10-08 起)
//!
//! 订着服务端的 SSE(`/events`):`meeting` 事件(建会 / 请 Agent / 结束)一到,
//! 主循环立刻去服务端对一次账——该进的进、该退的退。所以点了按钮通常一秒内它就进来。
//! SSE 断了、漏了,每 30 秒的兜底对账也会把事情捞回来;推送只是加速器,不是唯一通道。
//!
//! 对账是**双向**的:不再被请的会议(散会了、或有人请它离开)会被叫停——先退出房间,
//! **再做散会提炼**(行动项提案),然后任务结束。不等 LiveKit 房间自己散:Agent 自己就是
//! 参会者,它不走房间永远不空,以前的"散会提炼"实际上只有 Ctrl-C 才会触发。
//! 叫停 180 秒还没收尾完的任务才 abort(提炼 + 回顾是两次模型调用)。
//!
//! ## 诚实边界
//!
//! - 单进程:所有会议在同一个进程里跑(每会一个 tokio 任务),一场会 panic 不会连累别的会,
//!   但 CPU 与内存是共享的。
//! - 令牌 12 小时过期:对账遇到 401 会自动重登;**已经在会里的任务**用的是入会时的令牌,
//!   一场超过 12 小时的会,它回传纪要会开始失败。
//! - 连不上房间的会议会**退避重试**(30 秒起翻倍,上限 10 分钟),不会每轮对账都去撞;
//!   但它不会放弃——那场会仍然"请了 Agent",直到有人结束它或请它离开。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use muster_meeting_agent::{
    room, Answer, Answerer, ChunkConfig, Context, EnergyGate, ExtractOutcome, Extractor, HttpSink,
    MentionRules, Pipeline, Summarizer, SummaryOutcome, Translation, Translator,
};
use muster_provider::{ProviderRegistry, SpeechCompatProvider, SpeechConfig, SpeechProvider};
use muster_route::{OrgPolicy, Router, Sensitivity, SpeechRouter};
use tokio::sync::Mutex;

/// 兜底对账间隔。平时靠推送叫醒,这个只管"推送漏了"的情况。
const POLL: Duration = Duration::from_secs(30);
/// 查询失败后的重试间隔。
const RETRY: Duration = Duration::from_secs(5);
/// 叫停一场会后,给它退出房间 + 散会提炼 + 写回顾的时间(两次模型调用,慢的 provider
/// 一次就要半分钟);超过就 abort。
const STOP_GRACE: Duration = Duration::from_secs(180);

/// 正在服务的一场会。
struct Served {
    handle: tokio::task::JoinHandle<()>,
    /// 叫停信号:退出房间、做散会提炼、任务结束
    stop: Arc<tokio::sync::Notify>,
    stop_requested: Option<std::time::Instant>,
    started_at: std::time::Instant,
}

/// 一场会的任务起来不到这么久就结束了,算"没服务成"(多半是连不上房间),要退避。
const QUICK_EXIT: Duration = Duration::from_secs(60);
/// 退避上限。
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// 连不上的会议的退避记录:下次允许重试的时刻、连续失败次数。
///
/// 没有它的话,一场连不上房间的会(服务端下发的 LiveKit 地址这台机器到不了,是最常见的
/// 原因)会在每一轮对账里被重新认领:每次认领都是一次入会票、一条 meeting.join 审计——
/// 实测一场 8 月的旧会议 11 天刷了两千多条。退避不改变结论,只是别每分钟都去撞。
#[derive(Default)]
struct Cooldown {
    until: Option<std::time::Instant>,
    failures: u32,
}

struct Cfg {
    server: String,
    /// 可换:令牌 12 小时过期,对账遇到 401 就重登一次。
    token: tokio::sync::RwLock<String>,
    account: String,
    password: String,
    stt_url: String,
    stt_model: Option<String>,
    stt_prompt: Option<String>,
    provider_config: Option<String>,
    aliases: Vec<String>,
    agent_name: String,
    /// 原文语言:转写提示语言,也是纪要原文行的 lang。
    lang: String,
    /// 覆盖入会票里的 LiveKit 信令地址。入会票里那个是给外面的人的(公网 wss 经反代);
    /// 守护进程和 LiveKit 跑在同一个 compose 网络里时直接走容器名,不绕公网。
    livekit_url: Option<String>,
}

/// 翻译目标语言的缓存:在场的人各自选的字幕语言,10 秒内不重复问服务端。
struct LangCache {
    inner: Mutex<(Option<std::time::Instant>, Vec<String>)>,
}

impl LangCache {
    fn new() -> Self {
        Self { inner: Mutex::new((None, vec![])) }
    }
    async fn current(&self, sink: &HttpSink) -> Vec<String> {
        let mut g = self.inner.lock().await;
        let stale = g.0.map(|t| t.elapsed() > Duration::from_secs(10)).unwrap_or(true);
        if stale {
            g.1 = sink.caption_langs().await;
            g.0 = Some(std::time::Instant::now());
        }
        g.1.clone()
    }
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
        token: tokio::sync::RwLock::new(login(&http, &server, &acct, &pw).await?),
        account: acct.clone(),
        password: pw,
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
        lang: std::env::var("MUSTER_STT_LANG").unwrap_or_else(|_| "zh".into()),
        livekit_url: std::env::var("MUSTER_LIVEKIT_URL").ok().filter(|s| !s.is_empty()),
    });

    println!("会议 Agent 常驻服务已启动");
    println!("  服务端 {} · 身份 {}", cfg.server, acct);
    println!("  转写落点 {}", cfg.stt_url);
    println!(
        "  作答 {}",
        if cfg.provider_config.is_some() { "已启用" } else { "未启用(只转写)" }
    );
    if let Some(u) = &cfg.livekit_url {
        println!("  LiveKit 信令走 {u}(覆盖入会票里的地址)");
    }
    println!("  订阅服务端推送,{}s 兜底对账一次\n", POLL.as_secs());

    // 推送一到就叫醒主循环;Notify 会存下一个许可,主循环正忙时也不会漏
    let wake = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(subscribe_pushes(http.clone(), cfg.clone(), wake.clone()));

    let mut joined: HashMap<String, Served> = HashMap::new();
    let mut cooldown: HashMap<String, Cooldown> = HashMap::new();
    loop {
        let wanted = match wanted_meetings(&http, &cfg).await {
            Ok(v) => v,
            Err(Fetch::Unauthorized) => {
                // 令牌过期就重登。守护进程活得比令牌久,这是常态不是异常。
                match login(&http, &cfg.server, &cfg.account, &cfg.password).await {
                    Ok(t) => {
                        *cfg.token.write().await = t;
                        tracing::info!("令牌已过期,重新登录成功");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "重新登录失败,稍后重试");
                        tokio::time::sleep(RETRY).await;
                    }
                }
                continue;
            }
            Err(Fetch::Other(e)) => {
                tracing::warn!(error = %e, "查询失败,稍后重试");
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        let wanted_ids: HashSet<String> =
            wanted.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect();

        // 不再被请的会议,退避记录也清掉
        cooldown.retain(|id, _| wanted_ids.contains(id));

        // 对账:自己退出的清句柄;不再被请的(散会 / 被请离)叫停——退出房间、做散会提炼;
        // 叫停太久还没收尾完的才 abort
        joined.retain(|id, s| {
            if s.handle.is_finished() {
                if s.started_at.elapsed() < QUICK_EXIT && s.stop_requested.is_none() {
                    // 起来就退 = 没服务成。退避,免得每轮对账都去撞一次
                    let c = cooldown.entry(id.clone()).or_default();
                    c.failures += 1;
                    let wait = (POLL * 2u32.saturating_pow(c.failures.min(8))).min(MAX_BACKOFF);
                    c.until = Some(std::time::Instant::now() + wait);
                    tracing::warn!(
                        meeting = %id, failures = c.failures, retry_in_s = wait.as_secs(),
                        "这场会起来就退了(多半是连不上房间,看上面的错误),先退避"
                    );
                } else {
                    cooldown.remove(id);
                    tracing::info!(meeting = %id, "已退出该会议");
                }
                return false;
            }
            if !wanted_ids.contains(id) {
                match s.stop_requested {
                    None => {
                        s.stop.notify_one();
                        s.stop_requested = Some(std::time::Instant::now());
                        tracing::info!(meeting = %id, "会议已结束或被请离,正在退出该会议");
                    }
                    Some(at) if at.elapsed() > STOP_GRACE => {
                        s.handle.abort();
                        tracing::warn!(meeting = %id, "收尾超时,强制退出该会议");
                        return false;
                    }
                    Some(_) => {}
                }
            }
            true
        });

        for m in wanted {
            let id = m["id"].as_str().unwrap_or_default().to_string();
            if id.is_empty() || joined.contains_key(&id) {
                continue;
            }
            if let Some(until) = cooldown.get(&id).and_then(|c| c.until) {
                if until > std::time::Instant::now() {
                    continue;
                }
            }
            let title = m["title"].as_str().unwrap_or("").to_string();
            let channel = m["channel_id"].as_str().unwrap_or("").to_string();
            tracing::info!(meeting = %id, %title, "被请去开会");
            let c = cfg.clone();
            let stop = Arc::new(tokio::sync::Notify::new());
            let (id2, stop2) = (id.clone(), stop.clone());
            joined.insert(
                id,
                Served {
                    handle: tokio::spawn(async move { serve_meeting(c, id2, channel, title, stop2).await }),
                    stop,
                    stop_requested: None,
                    started_at: std::time::Instant::now(),
                },
            );
        }

        // 等推送;POLL 内没动静就兜底对账一次
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(POLL) => {}
        }
    }
}

/// 订服务端的 SSE,`meeting` 事件一到就叫醒主循环。断了退避重连;
/// 这条流只是加速器——漏一条也有兜底对账,所以这里不做任何状态判断,只负责叫醒。
async fn subscribe_pushes(http: reqwest::Client, cfg: Arc<Cfg>, wake: Arc<tokio::sync::Notify>) {
    use futures::StreamExt;
    let mut backoff = Duration::from_secs(1);
    loop {
        let url = format!("{}/events?token={}", cfg.server, cfg.token.read().await);
        let resp = match http.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "订阅推送被拒,{}s 后重试", backoff.as_secs());
                if r.status() == reqwest::StatusCode::UNAUTHORIZED {
                    // 令牌过期:叫醒主循环去重登,下一轮再用新令牌订
                    wake.notify_one();
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, "订阅推送失败,{}s 后重试", backoff.as_secs());
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        tracing::info!("已订阅服务端推送");
        backoff = Duration::from_secs(1);

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find("\n\n") {
                let frame: String = buf.drain(..pos + 2).collect();
                let data: Vec<&str> =
                    frame.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).collect();
                if data.is_empty() {
                    continue; // keep-alive 注释行
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data.join("\n")) {
                    if v["type"] == "meeting" {
                        tracing::debug!(event = %v["event"], meeting = %v["meeting_id"], "收到会议推送");
                        wake.notify_one();
                    }
                }
            }
        }
        tracing::warn!("推送流断开,重连");
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
async fn fetch_catalog(http: &reqwest::Client, cfg: &Cfg, token: &str) -> Option<serde_json::Value> {
    let r = http
        .get(format!("{}/providers/catalog", cfg.server))
        .bearer_auth(token)
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

/// 查询失败的两类:令牌不认了(要重登)/ 别的(稍后重试)。
enum Fetch {
    Unauthorized,
    Other(String),
}

async fn wanted_meetings(http: &reqwest::Client, cfg: &Cfg) -> Result<Vec<serde_json::Value>, Fetch> {
    let token = cfg.token.read().await.clone();
    let r = http
        .get(format!("{}/meetings/agent-wanted", cfg.server))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| Fetch::Other(e.to_string()))?;
    if r.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Fetch::Unauthorized);
    }
    if !r.status().is_success() {
        return Err(Fetch::Other(format!("HTTP {}", r.status())));
    }
    r.json().await.map_err(|e| Fetch::Other(e.to_string()))
}

/// 在一场会里干活,直到房间断开或被叫停(散会 / 被请离)。
async fn serve_meeting(
    cfg: Arc<Cfg>,
    meeting: String,
    channel: String,
    title: String,
    stop: Arc<tokio::sync::Notify>,
) {
    if let Err(e) = serve_inner(cfg, &meeting, &channel, &title, stop).await {
        tracing::warn!(meeting = %meeting, error = %e, "这场会没能服务");
    }
}

async fn serve_inner(
    cfg: Arc<Cfg>,
    meeting: &str,
    channel: &str,
    title: &str,
    stop: Arc<tokio::sync::Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    // 这场会全程用入会这一刻的令牌(12 小时有效,够一场会)
    let tok = cfg.token.read().await.clone();
    let join: serde_json::Value = http
        .post(format!("{}/meetings/{meeting}/join", cfg.server))
        .bearer_auth(&tok)
        .send()
        .await?
        .json()
        .await?;
    if let Some(e) = join.get("error") {
        return Err(format!("入会被拒:{e}").into());
    }
    let (lk_url, lk_token) = (
        match &cfg.livekit_url {
            Some(u) => u.clone(),
            None => join["url"].as_str().ok_or("缺 url")?.to_string(),
        },
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
    let sink = Arc::new(HttpSink::new(&cfg.server, &tok, meeting).with_lang(&cfg.lang));
    let mut pipeline = Pipeline::new(Arc::new(SpeechRouter::new(vec![stt])), sink.clone())
        .with_language(cfg.lang.clone());
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
    let toml_from_server = fetch_catalog(&http, &cfg, &tok).await;
    let (answerer, extractor, translator, summarizer) = match (&toml_from_server, &cfg.provider_config) {
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
                Some(Arc::new(Extractor::new(router.clone(), level, meeting))),
                Some(Arc::new(Translator::new(router.clone(), level, meeting))),
                Some(Arc::new(Summarizer::new(router, level, meeting))),
            )
        }
        (None, None) => (None, None, None, None),
    };

    // 中途入会补上下文——晚到、重连、崩溃重启都是常态
    let ctx = Arc::new(Mutex::new(Context::default()));
    if let Ok(r) = http
        .get(format!("{}/meetings/{meeting}/transcript", cfg.server))
        .bearer_auth(&tok)
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
    let langs = Arc::new(LangCache::new());
    let src_lang = cfg.lang.clone();

    let in_room = room::run(&lk_url, &lk_token, ChunkConfig::default(), EnergyGate::default(), move |u| {
        let (p, pol, ctx, ans, sink, name, ex, tr, langs, src) = (
            pipeline.clone(),
            policy.clone(),
            ctx.clone(),
            answerer.clone(),
            sink.clone(),
            name.clone(),
            ex_live.clone(),
            translator.clone(),
            langs.clone(),
            src_lang.clone(),
        );
        async move {
            let speaker = u.speaker.clone();
            let Some(transcribed) = p.handle(u, &pol).await else { return };
            let text = transcribed.text;
            ctx.lock().await.push(&speaker, &text);

            // 字幕翻译:在场的人要哪些语言就翻哪些,各语言并行,不挡作答
            if let Some(tr) = tr {
                let targets = langs.current(&sink).await;
                if !targets.is_empty() {
                    let (sink, speaker, text, line_id) = (sink.clone(), speaker.clone(), text.clone(), transcribed.line_id);
                    tokio::spawn(async move {
                        let jobs = targets.into_iter().map(|to| {
                            let (tr, sink, speaker, text, src) = (tr.clone(), sink.clone(), speaker.clone(), text.clone(), src.clone());
                            async move {
                                match tr.translate(&text, &src, &to).await {
                                    Translation::Text(t) => {
                                        sink.say_translation(&speaker, &t, &to, line_id).await;
                                    }
                                    Translation::Unavailable(why) => {
                                        tracing::warn!(lang = %to, error = %why, "这句没能翻译");
                                    }
                                }
                            }
                        });
                        futures::future::join_all(jobs).await;
                    });
                }
            }

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
    });
    // 被叫停就放下房间(drop 即断开),然后照常做散会提炼——结论往往在最后
    tokio::select! {
        r = in_room => r?,
        _ = stop.notified() => {
            tracing::info!(meeting = %meeting, "被叫停,退出房间");
        }
    }

    // 散会提炼:提案,确认归人。散会后纪要不再收,说明走频道消息。
    let transcript = ctx2.lock().await.transcript();
    if let Some(ex) = extractor {
        match ex.extract(&transcript).await {
            ExtractOutcome::Items(items) if !items.is_empty() => {
                for it in &items {
                    sink2.propose_action(it).await;
                }
                tracing::info!(meeting = %meeting, n = items.len(), "已提出行动项(待人确认)");
            }
            ExtractOutcome::Items(_) => tracing::info!(meeting = %meeting, "没有明确的行动项"),
            ExtractOutcome::Unavailable(why) => {
                sink2.post_channel_message(channel, &format!("会议《{title}》的行动项提炼没能完成:{why}")).await;
            }
        }
    }

    // 散会回顾:落到服务端(链上只有哈希),再发一条频道消息让没开会的人也看得到
    if let Some(sm) = summarizer {
        if transcript.trim().is_empty() {
            tracing::info!(meeting = %meeting, "这场会没有转写,不写回顾");
        } else {
            let t0 = std::time::Instant::now();
            tracing::info!(meeting = %meeting, lines = transcript.lines().count(), "开始写回顾");
            match sm.summarize(title, &transcript).await {
                SummaryOutcome::Text { text, model } => {
                    if sink2.post_summary("recap", &text, Some(&model)).await {
                        tracing::info!(meeting = %meeting, %model, elapsed_s = t0.elapsed().as_secs(), "回顾已落库");
                        sink2.post_channel_message(channel, &format!("📝 会议回顾《{title}》(由 {name2} 整理,依据会议转写)\n\n{text}")).await;
                    }
                }
                SummaryOutcome::Unavailable(why) => {
                    sink2.post_channel_message(channel, &format!("会议《{title}》的回顾没能生成:{why}")).await;
                }
            }
        }
    }
    Ok(())
}
