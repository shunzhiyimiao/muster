//! 会议 Agent 常驻服务:以**节点**身份登录服务端,托管值班工牌,谁请就去谁那儿。
//!
//! ## 为什么是常驻服务,而不是按钮直接起进程
//!
//! 让桌面壳自己 spawn 一个 Agent 的话,**两个人开会就有两个 Agent 在同一个
//! 房间里各转各的**——同一句话转两遍,纪要里出现重复行,模型调用也翻倍。
//! 而且 Agent 该跑在服务器上(架构文档的部署拓扑),不该在每个人的笔记本上。
//!
//! 所以按钮只在服务端记一个**意愿**(`meeting.wants_agent`),由这个进程认领。
//!
//! ## 节点身份(2026-10-29,块 2)
//!
//! 它不再拿工牌的口令登录。第一次运行生成一对 ed25519 密钥存在 `$MUSTER_NODE_HOME/node.key`,
//! 向服务端入网并打印设备码,等管理员批准;之后每次会话都是签服务端的 challenge 换一张
//! 短期 JWT,再以服务端分配到本节点的**值班工牌**行事(请求带 `X-Muster-Badge`)。
//! 服务器上落着的是节点的密钥,不是任何人的口令。
//!
//! ```bash
//! MUSTER_SERVER=http://localhost:8787 MUSTER_NODE_HOME=~/.muster/node \
//! MUSTER_STT_URL=http://localhost:9000/v1 \
//! cargo run -p muster-meeting-agent --features livekit --example daemon
//! # 第一次会打印:设备码 K7PX-4MQ2 —— 管理员:muster-admin node-approve K7PX-4MQ2
//! ```
//!
//! **过渡路径**:设了 `MUSTER_ACCOUNT` / `MUSTER_PASSWORD` 且没有节点密钥时仍按口令登录,
//! 启动时打 warn。muster.top 切到节点身份后删除(见 muster-server lib.rs 诚实边界)。
//!
//! ## 转写:流式为主,批处理为备(2026-10-30)
//!
//! `MUSTER_STT_URL` 的协议决定走哪条路:`ws://stt:9000/stream` 是 Muster 自己的 sherpa-onnx
//! 流式边车(`deploy/stt`),一个说话人一条流、引擎端点落句、有界队列背压、过载降级不熔断;
//! `http://…/v1` 仍走 whisper 兼容的批处理(每句一个请求)——2026-10-29 线上就是在这条路上
//! 被两个人说话压垮的,只留作兼容。`MUSTER_STT_HOTWORDS`(逗号分隔)是流式路径的热词,
//! Agent 名与别名自动算在内。
//!
//! ## 推送为主,轮询兜底
//!
//! 订着服务端的 SSE(`/events`):`meeting` 事件(建会 / 请 Agent / 结束)一到,
//! 主循环立刻去服务端对一次账——该进的进、该退的退。所以点了按钮通常一秒内它就进来。
//! SSE 断了、漏了,每 30 秒的兜底对账也会把事情捞回来;推送只是加速器,不是唯一通道。
//!
//! 对账是**双向**的:不再被请的会议(散会了、或有人请它离开)会被叫停——先退出房间,
//! **再做散会提炼与回顾**,然后任务结束。叫停 180 秒还没收尾完的任务才 abort。
//!
//! ## 退避与熔断
//!
//! 连不上房间的会议**退避重试**(30 秒起翻倍,上限 10 分钟)。同一场会连续 3 次以同一类
//! 错误失败(连不上、入会被拒、转写失败……)⇒ **熔断**:停止认领这场会,心跳里上报,
//! 等人再点一次「请 Agent」才解除。退避是别每分钟撞一次;熔断是停下来等人看。
//!
//! ## 诚实边界
//!
//! - 单进程:所有会议在同一个进程里跑(每会一个 tokio 任务),一场会 panic 不会连累别的会,
//!   但 CPU 与内存是共享的。
//! - 会话过期(节点 1 小时、口令 12 小时)对账遇到 401 会自动续;**已经在会里的任务**用的
//!   是入会时的令牌,一场超过令牌有效期的会,它回传纪要会开始失败。
//! - 口令过渡路径下服务端不下发 agent_requested_ms 之外的解除信号,熔断只能靠重启清除。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use muster_meeting_agent::{
    classify_error, node, room, Answer, Answerer, Breakers, ChunkConfig, Context, EnergyGate, ExtractOutcome,
    OnTranscribed, StreamPipeline, Transcribed,
    Extractor, HttpSink, MentionRules, NodeKey, NodeRecord, Pipeline, SinkEvent, Summarizer, SummaryOutcome,
    Translation, Translator,
};
use muster_provider::{
    ProviderRegistry, SherpaStreamProvider, SpeechCompatProvider, SpeechConfig, SpeechProvider, SpeechStreamConfig,
    SpeechStreamProvider,
};
use muster_route::{OrgPolicy, Router, Sensitivity, SpeechRouter, SpeechStreamRouter};
use tokio::sync::{Mutex, RwLock};

/// 兜底对账间隔。平时靠推送叫醒,这个只管"推送漏了"的情况。
const POLL: Duration = Duration::from_secs(30);
/// 查询失败后的重试间隔。
const RETRY: Duration = Duration::from_secs(5);
/// 叫停一场会后,给它退出房间 + 散会提炼 + 写回顾的时间(两次模型调用);超过就 abort。
const STOP_GRACE: Duration = Duration::from_secs(180);
/// 一场会的任务起来不到这么久就结束了,算"没服务成"(多半是连不上房间),要退避。
const QUICK_EXIT: Duration = Duration::from_secs(60);
/// 退避上限。
const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// 心跳间隔(服务端 90 秒没收到就判离线)。
const HEARTBEAT: Duration = Duration::from_secs(30);
/// 入网后轮询批准的间隔。
const ENROLL_POLL: Duration = Duration::from_secs(5);

/// 正在服务的一场会。
struct Served {
    handle: tokio::task::JoinHandle<Result<(), String>>,
    /// 叫停信号:退出房间、做散会提炼、任务结束
    stop: Arc<tokio::sync::Notify>,
    stop_requested: Option<std::time::Instant>,
    started_at: std::time::Instant,
    /// 认领时服务端的「请 Agent」时刻,熔断按它判断人有没有重试
    requested_ms: Option<i64>,
}

/// 连不上的会议的退避记录:下次允许重试的时刻、连续失败次数。
#[derive(Default)]
struct Cooldown {
    until: Option<std::time::Instant>,
    failures: u32,
}

/// 登录方式。
enum Mode {
    /// 过渡路径:拿工牌账号的口令登录。
    Password { account: String, password: String },
    /// 节点身份:签 challenge 换会话,以分配到本节点的值班工牌行事。
    Node { node_id: String, key: NodeKey },
}

struct Cfg {
    server: String,
    /// 当前会话令牌(节点会话或口令登录的令牌),401 时换新
    token: RwLock<String>,
    /// 节点模式下以哪张工牌行事;口令模式为空(令牌本身就是工牌账号)
    badge: RwLock<Option<String>>,
    mode: Mode,
    stt_url: String,
    stt_model: Option<String>,
    stt_prompt: Option<String>,
    /// 流式路径的热词(人名、领域词);Agent 名与别名另外自动加上。
    stt_hotwords: Vec<String>,
    provider_config: Option<String>,
    aliases: Vec<String>,
    agent_name: String,
    /// 原文语言:转写提示语言,也是纪要原文行的 lang。
    lang: String,
    /// 覆盖入会票里的 LiveKit 信令地址(同一个 compose 网络里直接走容器名)。
    livekit_url: Option<String>,
    /// 熔断状态(所有会议)。
    breakers: std::sync::Mutex<Breakers>,
    /// 最近一次成功回传转写 / 纪要的时间(心跳里的进度)。
    last_progress: AtomicI64,
}

impl Cfg {
    /// 给请求加凭据:Bearer + 节点模式下的工牌头。
    async fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let rb = rb.bearer_auth(self.token.read().await.as_str());
        match self.badge.read().await.as_deref() {
            Some(b) => rb.header("X-Muster-Badge", b),
            None => rb,
        }
    }

    fn node_id(&self) -> Option<&str> {
        match &self.mode {
            Mode::Node { node_id, .. } => Some(node_id),
            Mode::Password { .. } => None,
        }
    }
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

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// 服务端下发的、要进的一场会。
struct Wanted {
    id: String,
    title: String,
    channel: String,
    requested_ms: Option<i64>,
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
    let stt_url = std::env::var("MUSTER_STT_URL").unwrap_or_else(|_| "http://localhost:9000/v1".into());

    // 登录方式:有节点密钥(或显式给了 MUSTER_NODE_HOME)就是节点;否则有口令就走过渡路径
    let node_home: PathBuf = std::env::var("MUSTER_NODE_HOME").map(PathBuf::from).unwrap_or_else(|_| node::default_home());
    let explicit_home = std::env::var("MUSTER_NODE_HOME").is_ok();
    let has_password = std::env::var("MUSTER_ACCOUNT").is_ok() && std::env::var("MUSTER_PASSWORD").is_ok();
    let use_node = explicit_home || node_home.join("node.key").exists() || !has_password;

    let (mode, token) = if use_node {
        // 裸的 "Permission denied" 没人看得懂。说清楚是哪个目录、最可能为什么
        let (key, created) = NodeKey::load_or_create(&node_home).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "节点目录 {} 读写失败:{e}。容器里跑的话,多半是 MUSTER_NODE_HOME 挂的卷归 root 而进程是 agent 用户",
                    node_home.display()
                ),
            )
        })?;
        if created {
            println!("已生成节点密钥:{}(只把公钥交给服务端)", node_home.join("node.key").display());
        }
        let node_id = enroll_if_needed(&http, &server, &node_home, &key, &stt_url).await?;
        let token = node_session(&http, &server, &node_id, &key).await?;
        (Mode::Node { node_id, key }, token)
    } else {
        let (acct, pw) = (std::env::var("MUSTER_ACCOUNT")?, std::env::var("MUSTER_PASSWORD")?);
        tracing::warn!("以工牌口令登录(MUSTER_ACCOUNT)是过渡路径,服务器上不该放工牌的口令;请改用节点身份(MUSTER_NODE_HOME)");
        let token = login(&http, &server, &acct, &pw).await?;
        (Mode::Password { account: acct, password: pw }, token)
    };

    let cfg = Arc::new(Cfg {
        token: RwLock::new(token),
        badge: RwLock::new(None),
        mode,
        server: server.clone(),
        stt_url,
        stt_model: std::env::var("MUSTER_STT_MODEL").ok(),
        stt_prompt: std::env::var("MUSTER_STT_PROMPT").ok(),
        stt_hotwords: std::env::var("MUSTER_STT_HOTWORDS")
            .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default(),
        provider_config: std::env::var("MUSTER_PROVIDER_CONFIG").ok(),
        aliases: std::env::var("MUSTER_AGENT_ALIASES")
            .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_else(|_| MentionRules::default().aliases),
        agent_name: std::env::var("MUSTER_AGENT_NAME").unwrap_or_else(|_| "小七".into()),
        lang: std::env::var("MUSTER_STT_LANG").unwrap_or_else(|_| "zh".into()),
        livekit_url: std::env::var("MUSTER_LIVEKIT_URL").ok().filter(|s| !s.is_empty()),
        breakers: std::sync::Mutex::new(Breakers::new()),
        last_progress: AtomicI64::new(0),
    });

    println!("会议 Agent 常驻服务已启动");
    match &cfg.mode {
        Mode::Node { node_id, .. } => println!("  服务端 {} · 节点 {node_id}", cfg.server),
        Mode::Password { account, .. } => println!("  服务端 {} · 身份 {account}(口令过渡路径)", cfg.server),
    }
    println!(
        "  转写落点 {}({})",
        cfg.stt_url,
        if cfg.stt_url.starts_with("ws") { "流式,一人一条流" } else { "批处理,whisper 兼容;两人同时说话会跟不上" }
    );
    println!(
        "  作答 {}",
        if cfg.provider_config.is_some() { "已启用(本地配置)" } else { "看服务端 provider 目录" }
    );
    if let Some(u) = &cfg.livekit_url {
        println!("  LiveKit 信令走 {u}(覆盖入会票里的地址)");
    }
    println!("  订阅服务端推送,{}s 兜底对账一次,{}s 一次心跳\n", POLL.as_secs(), HEARTBEAT.as_secs());

    // 节点模式先等工牌分配到位:没有工牌,这个节点什么也代表不了
    if cfg.node_id().is_some() {
        wait_for_badge(&http, &cfg).await;
    }

    // 推送一到就叫醒主循环;Notify 会存下一个许可,主循环正忙时也不会漏
    let wake = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(subscribe_pushes(http.clone(), cfg.clone(), wake.clone()));
    if cfg.node_id().is_some() {
        tokio::spawn(heartbeat_loop(http.clone(), cfg.clone()));
    }

    let mut joined: HashMap<String, Served> = HashMap::new();
    let mut cooldown: HashMap<String, Cooldown> = HashMap::new();
    loop {
        let wanted = match fetch_wanted(&http, &cfg).await {
            Ok(v) => v,
            Err(Fetch::Unauthorized) => {
                // 会话过期就续。守护进程活得比令牌久,这是常态不是异常。
                match reauth(&http, &cfg).await {
                    Ok(()) => tracing::info!("会话已过期,重新登录成功"),
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
        let wanted_ids: HashSet<String> = wanted.iter().map(|m| m.id.clone()).collect();

        // 不再被请的会议,退避与熔断记录也清掉;人重试过的(请求时刻变新)熔断立刻解除——
        // 哪怕那场会的任务还在收尾,也别让下一次心跳把过期的熔断再报上去
        cooldown.retain(|id, _| wanted_ids.contains(id));
        {
            let mut b = cfg.breakers.lock().unwrap();
            let gone: Vec<String> = b.reports().into_iter().map(|(m, _, _, _)| m).filter(|m| !wanted_ids.contains(m)).collect();
            for m in gone {
                b.forget(&m);
            }
            for m in &wanted {
                b.is_tripped(&m.id, m.requested_ms);
            }
        }

        // 对账:自己退出的收结果;不再被请的(散会 / 被请离)叫停——退出房间、做散会提炼;
        // 叫停太久还没收尾完的才 abort
        let mut finished: Vec<(String, Served)> = Vec::new();
        joined.retain(|id, s| {
            if s.handle.is_finished() {
                return true; // 下面统一处理
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
        let done_ids: Vec<String> = joined.iter().filter(|(_, s)| s.handle.is_finished()).map(|(id, _)| id.clone()).collect();
        for id in done_ids {
            if let Some(s) = joined.remove(&id) {
                finished.push((id, s));
            }
        }
        for (id, s) in finished {
            let quick = s.started_at.elapsed() < QUICK_EXIT && s.stop_requested.is_none();
            let outcome = match s.handle.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(format!("任务异常退出:{e}")),
            };
            match outcome {
                Err(e) if quick => {
                    let kind = classify_error(&e);
                    let tripped = cfg.breakers.lock().unwrap().record_failure(&id, kind, s.requested_ms);
                    if tripped {
                        tracing::warn!(meeting = %id, kind, "连续 3 次同一类错误,熔断:不再认领这场会,等人再点一次「请 Agent」");
                        cooldown.remove(&id);
                    } else {
                        let c = cooldown.entry(id.clone()).or_default();
                        c.failures += 1;
                        let wait = (POLL * 2u32.saturating_pow(c.failures.min(8))).min(MAX_BACKOFF);
                        c.until = Some(std::time::Instant::now() + wait);
                        tracing::warn!(meeting = %id, kind, failures = c.failures, retry_in_s = wait.as_secs(), "这场会起来就退了,先退避");
                    }
                }
                Err(e) => tracing::warn!(meeting = %id, error = %e, "这场会以错误结束"),
                Ok(()) => {
                    cooldown.remove(&id);
                    cfg.breakers.lock().unwrap().record_success(&id);
                    tracing::info!(meeting = %id, "已退出该会议");
                }
            }
        }

        for m in wanted {
            if joined.contains_key(&m.id) {
                continue;
            }
            if cfg.breakers.lock().unwrap().is_tripped(&m.id, m.requested_ms) {
                continue;
            }
            if let Some(until) = cooldown.get(&m.id).and_then(|c| c.until) {
                if until > std::time::Instant::now() {
                    continue;
                }
            }
            tracing::info!(meeting = %m.id, title = %m.title, "被请去开会");
            let c = cfg.clone();
            let stop = Arc::new(tokio::sync::Notify::new());
            let (id2, stop2, req) = (m.id.clone(), stop.clone(), m.requested_ms);
            joined.insert(
                m.id,
                Served {
                    handle: tokio::spawn(async move { serve_meeting(c, id2, m.channel, m.title, req, stop2).await }),
                    stop,
                    stop_requested: None,
                    started_at: std::time::Instant::now(),
                    requested_ms: req,
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

// ---------------------------------------------------------------- 节点:入网与会话

/// 本机还没入网(或换了服务端)就入网:打印设备码,等管理员批准。
async fn enroll_if_needed(
    http: &reqwest::Client,
    server: &str,
    home: &std::path::Path,
    key: &NodeKey,
    stt_url: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(r) = node::load_record(home) {
        if r.server == server {
            return Ok(r.node_id);
        }
        tracing::warn!(old = %r.server, new = %server, "服务端地址变了,重新入网");
    }
    let name = std::env::var("MUSTER_NODE_NAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "node".into());
    let caps = serde_json::json!({
        "stt_local": true,
        "stt_url": stt_url,
        "version": env!("CARGO_PKG_VERSION"),
    });
    let v: serde_json::Value = http
        .post(format!("{server}/nodes/enroll"))
        .json(&serde_json::json!({ "name": name, "pubkey": key.pubkey_hex(), "capabilities": caps }))
        .send()
        .await
        .map_err(|e| format!("连不上服务端:{e}"))?
        .json()
        .await
        .map_err(|e| format!("入网响应无法解析:{e}"))?;
    let node_id = v["node_id"].as_str().ok_or_else(|| format!("入网失败:{}", v["error"].as_str().unwrap_or("未知原因")))?.to_string();
    let user_code = v["user_code"].as_str().unwrap_or("?").to_string();
    let poll_token = v["poll_token"].as_str().unwrap_or("").to_string();
    println!("\n==================================================");
    println!("  本机已申请入网,等管理员批准。设备码:{user_code}");
    println!("  管理员在服务器上执行:muster-admin node-approve {user_code}");
    println!("==================================================\n");
    let mut n = 0u32;
    loop {
        tokio::time::sleep(ENROLL_POLL).await;
        n += 1;
        let st: serde_json::Value = match http
            .get(format!("{server}/nodes/enroll/poll?node_id={node_id}&poll_token={poll_token}"))
            .send()
            .await
        {
            Ok(r) => r.json().await.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, "查询批准状态失败");
                continue;
            }
        };
        match st["status"].as_str() {
            Some("approved") => break,
            Some("revoked") => return Err("入网被拒绝".into()),
            _ => {
                if n % 6 == 0 {
                    println!("  仍在等管理员批准,设备码 {user_code}");
                }
            }
        }
    }
    node::save_record(home, &NodeRecord { node_id: node_id.clone(), server: server.to_string(), name })?;
    println!("  已批准,节点 {node_id}\n");
    Ok(node_id)
}

/// 签 challenge 换会话。
async fn node_session(http: &reqwest::Client, server: &str, node_id: &str, key: &NodeKey) -> Result<String, String> {
    let ch: serde_json::Value = http
        .post(format!("{server}/nodes/{node_id}/challenge"))
        .send()
        .await
        .map_err(|e| format!("连不上服务端:{e}"))?
        .json()
        .await
        .map_err(|e| format!("challenge 无法解析:{e}"))?;
    let nonce = ch["nonce"].as_str().ok_or_else(|| format!("拿不到 challenge:{}", ch["error"].as_str().unwrap_or("未知原因")))?;
    let sig = key.sign_challenge(node_id, nonce);
    let v: serde_json::Value = http
        .post(format!("{server}/nodes/{node_id}/session"))
        .json(&serde_json::json!({ "sig": sig }))
        .send()
        .await
        .map_err(|e| format!("连不上服务端:{e}"))?
        .json()
        .await
        .map_err(|e| format!("会话响应无法解析:{e}"))?;
    v["token"].as_str().map(String::from).ok_or_else(|| format!("换会话失败:{}", v["error"].as_str().unwrap_or("未知原因")))
}

/// 会话过期后按登录方式续。
async fn reauth(http: &reqwest::Client, cfg: &Cfg) -> Result<(), String> {
    let t = match &cfg.mode {
        Mode::Password { account, password } => login(http, &cfg.server, account, password).await?,
        Mode::Node { node_id, key } => node_session(http, &cfg.server, node_id, key).await?,
    };
    *cfg.token.write().await = t;
    Ok(())
}

/// 节点模式:等服务端把工牌分配到本节点(值班工牌优先)。
async fn wait_for_badge(http: &reqwest::Client, cfg: &Cfg) {
    let mut n = 0u32;
    loop {
        match fetch_wanted(http, cfg).await {
            Ok(_) if cfg.badge.read().await.is_some() => {
                println!("  以工牌 {} 行事\n", cfg.badge.read().await.as_deref().unwrap_or("?"));
                return;
            }
            Ok(_) => {
                n += 1;
                if n == 1 || n % 6 == 0 {
                    println!(
                        "  本节点还没有工牌。管理员:muster-admin badge-assign <工牌> {} && muster-admin duty-set <工牌>",
                        cfg.node_id().unwrap_or("?")
                    );
                }
            }
            Err(Fetch::Unauthorized) => {
                if let Err(e) = reauth(http, cfg).await {
                    tracing::warn!(error = %e, "重新登录失败");
                }
            }
            Err(Fetch::Other(e)) => tracing::warn!(error = %e, "查询设定值失败"),
        }
        tokio::time::sleep(ENROLL_POLL).await;
    }
}

/// 心跳:活着 + 最近一次成功干活的时间 + 熔断上报。
async fn heartbeat_loop(http: reqwest::Client, cfg: Arc<Cfg>) {
    let Some(node_id) = cfg.node_id().map(String::from) else { return };
    loop {
        tokio::time::sleep(HEARTBEAT).await;
        let reports: Vec<serde_json::Value> = cfg
            .breakers
            .lock()
            .unwrap()
            .reports()
            .into_iter()
            .map(|(m, k, n, req)| serde_json::json!({ "meeting_id": m, "error_kind": k, "failures": n, "requested_ms": req }))
            .collect();
        let last = cfg.last_progress.load(Ordering::SeqCst);
        let body = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "capabilities": { "stt_local": true, "stt_url": cfg.stt_url, "agent_name": cfg.agent_name },
            "progress": if last > 0 { serde_json::json!({ "kind": "transcript", "at_ms": last }) } else { serde_json::Value::Null },
            "breakers": reports,
        });
        match cfg.auth(http.post(format!("{}/nodes/{node_id}/heartbeat", cfg.server))).await.json(&body).send().await {
            Ok(r) if r.status().is_success() => {}
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
                if let Err(e) = reauth(&http, &cfg).await {
                    tracing::warn!(error = %e, "心跳时重新登录失败");
                }
            }
            Ok(r) => tracing::warn!(status = %r.status(), "心跳被拒"),
            Err(e) => tracing::warn!(error = %e, "心跳发送失败"),
        }
    }
}

// ---------------------------------------------------------------- 推送与对账

/// 订服务端的 SSE,`meeting` 事件一到就叫醒主循环。断了退避重连;
/// 这条流只是加速器——漏一条也有兜底对账,所以这里不做任何状态判断,只负责叫醒。
async fn subscribe_pushes(http: reqwest::Client, cfg: Arc<Cfg>, wake: Arc<tokio::sync::Notify>) {
    use futures::StreamExt;
    let mut backoff = Duration::from_secs(1);
    loop {
        // 先换一张一次性订阅票(块 1):登录令牌不进 URL
        let ticket = match cfg.auth(http.post(format!("{}/events/ticket", cfg.server))).await.send().await {
            Ok(r) if r.status().is_success() => r
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["ticket"].as_str().map(String::from)),
            Ok(r) => {
                if r.status() == reqwest::StatusCode::UNAUTHORIZED {
                    wake.notify_one(); // 会话过期:叫主循环去续
                }
                tracing::warn!(status = %r.status(), "换订阅票被拒,{}s 后重试", backoff.as_secs());
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "换订阅票失败,{}s 后重试", backoff.as_secs());
                None
            }
        };
        let Some(ticket) = ticket else {
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
            continue;
        };
        let url = format!("{}/events?ticket={ticket}", cfg.server);
        let resp = match http.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "订阅推送被拒,{}s 后重试", backoff.as_secs());
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
async fn fetch_catalog(http: &reqwest::Client, cfg: &Cfg) -> Option<serde_json::Value> {
    let r = cfg.auth(http.get(format!("{}/providers/catalog", cfg.server))).await.send().await.ok()?;
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

async fn login(http: &reqwest::Client, server: &str, id: &str, pw: &str) -> Result<String, String> {
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

/// 查询失败的两类:会话不认了(要续)/ 别的(稍后重试)。
enum Fetch {
    Unauthorized,
    Other(String),
}

/// 该进哪些会。节点模式看自己的设定值(顺带更新以哪张工牌行事);口令模式看 agent-wanted。
async fn fetch_wanted(http: &reqwest::Client, cfg: &Cfg) -> Result<Vec<Wanted>, Fetch> {
    let to_wanted = |m: &serde_json::Value| Wanted {
        id: m["id"].as_str().unwrap_or_default().to_string(),
        title: m["title"].as_str().unwrap_or("").to_string(),
        channel: m["channel_id"].as_str().unwrap_or("").to_string(),
        requested_ms: m["agent_requested_ms"].as_i64(),
    };
    match &cfg.mode {
        Mode::Node { node_id, .. } => {
            let r = cfg
                .auth(http.get(format!("{}/nodes/{node_id}/desired", cfg.server)))
                .await
                .send()
                .await
                .map_err(|e| Fetch::Other(e.to_string()))?;
            if r.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(Fetch::Unauthorized);
            }
            if !r.status().is_success() {
                return Err(Fetch::Other(format!("HTTP {}", r.status())));
            }
            let v: serde_json::Value = r.json().await.map_err(|e| Fetch::Other(e.to_string()))?;
            // 以值班工牌行事;没有值班的就用分配到本节点的第一张
            let badges = v["badges"].as_array().cloned().unwrap_or_default();
            let pick = badges
                .iter()
                .find(|b| b["duty"] == true)
                .or_else(|| badges.first())
                .and_then(|b| b["id"].as_str().map(String::from));
            let prev = cfg.badge.read().await.clone();
            if pick != prev {
                if let Some(b) = &pick {
                    tracing::info!(badge = %b, "以这张工牌行事");
                }
                *cfg.badge.write().await = pick;
            }
            Ok(v["meetings"].as_array().map(|a| a.iter().map(to_wanted).filter(|w| !w.id.is_empty()).collect()).unwrap_or_default())
        }
        Mode::Password { .. } => {
            let r = cfg
                .auth(http.get(format!("{}/meetings/agent-wanted", cfg.server)))
                .await
                .send()
                .await
                .map_err(|e| Fetch::Other(e.to_string()))?;
            if r.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(Fetch::Unauthorized);
            }
            if !r.status().is_success() {
                return Err(Fetch::Other(format!("HTTP {}", r.status())));
            }
            let v: Vec<serde_json::Value> = r.json().await.map_err(|e| Fetch::Other(e.to_string()))?;
            Ok(v.iter().map(to_wanted).filter(|w| !w.id.is_empty()).collect())
        }
    }
}

// ---------------------------------------------------------------- 一场会

/// 在一场会里干活,直到房间断开或被叫停(散会 / 被请离 / 熔断)。
async fn serve_meeting(
    cfg: Arc<Cfg>,
    meeting: String,
    channel: String,
    title: String,
    requested_ms: Option<i64>,
    stop: Arc<tokio::sync::Notify>,
) -> Result<(), String> {
    serve_inner(cfg, &meeting, &channel, &title, requested_ms, stop).await.map_err(|e| {
        let msg = e.to_string();
        tracing::warn!(meeting = %meeting, error = %msg, "这场会没能服务");
        msg
    })
}

async fn serve_inner(
    cfg: Arc<Cfg>,
    meeting: &str,
    channel: &str,
    title: &str,
    requested_ms: Option<i64>,
    stop: Arc<tokio::sync::Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    // 这场会全程用入会这一刻的令牌与工牌
    let tok = cfg.token.read().await.clone();
    let badge = cfg.badge.read().await.clone();
    let join: serde_json::Value = cfg
        .auth(http.post(format!("{}/meetings/{meeting}/join", cfg.server)))
        .await
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

    let streaming = cfg.stt_url.starts_with("ws://") || cfg.stt_url.starts_with("wss://");

    // 落点的动静:成功 → 进度;失败 → 熔断计数,连着三次同一类就叫停这场会
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<SinkEvent>();
    let sink = Arc::new(
        HttpSink::new(&cfg.server, &tok, meeting).with_lang(&cfg.lang).with_badge(badge).with_events(ev_tx),
    );
    {
        let (cfg_m, stop_m, meeting_m, sink_m) = (cfg.clone(), stop.clone(), meeting.to_string(), sink.clone());
        tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                match ev {
                    SinkEvent::Progress { at_ms } => {
                        cfg_m.last_progress.store(if at_ms > 0 { at_ms } else { now_ms() }, Ordering::SeqCst);
                        cfg_m.breakers.lock().unwrap().record_success(&meeting_m);
                    }
                    SinkEvent::Failure { kind } => {
                        let tripped = cfg_m.breakers.lock().unwrap().record_failure(&meeting_m, &kind, requested_ms);
                        if tripped {
                            tracing::warn!(meeting = %meeting_m, kind = %kind, "连续 3 次同一类错误,熔断:退出这场会,等人再点一次「请 Agent」");
                            // 先闭嘴再叫停:还在飞的转写会继续失败,退出后不能再往纪要里写回执
                            sink_m.mute();
                            stop_m.notify_one();
                        }
                    }
                }
            }
        });
    }

    // 批处理路径(whisper 兼容端点)。流式路径在下面按 streaming 分支建。
    let batch_pipeline = if streaming {
        None
    } else {
        let mut scfg = SpeechConfig::local_whisper(&cfg.stt_url);
        if let Some(m) = &cfg.stt_model {
            scfg.model = m.clone();
        }
        let stt: Arc<dyn SpeechProvider> = Arc::new(SpeechCompatProvider::new("whisper", scfg)?);
        let mut pipeline = Pipeline::new(Arc::new(SpeechRouter::new(vec![stt])), sink.clone())
            .with_language(cfg.lang.clone());
        if let Some(p) = &cfg.stt_prompt {
            pipeline = pipeline.with_prompt(p.clone());
        }
        Some(Arc::new(pipeline))
    };

    let mut policy = OrgPolicy::new(Sensitivity::Internal)?;
    if std::env::var("MUSTER_DRILL").is_ok() {
        policy.set_egress_locked(true);
    }

    // Provider 目录**优先向服务端要**。
    //
    // 本地那份 TOML 只在服务端没有目录时兜底,而且会告警:决定 restricted
    // 内容能不能出门的 `locality`,不该由跑模型的这台机器自己声明。
    let toml_from_server = fetch_catalog(&http, &cfg).await;
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
    if let Ok(r) = cfg.auth(http.get(format!("{}/meetings/{meeting}/transcript", cfg.server))).await.send().await {
        let rows: Vec<serde_json::Value> = r.json().await.unwrap_or_default();
        let n = rows.len();
        let mut c = ctx.lock().await;
        for row in rows {
            // 只补原文行;翻译行是原文的另一种语言,不该进上下文两遍
            if row["origin_id"].is_null() {
                if let (Some(sp), Some(tx)) = (row["speaker_id"].as_str(), row["text"].as_str()) {
                    c.push(sp, tx);
                }
            }
        }
        if n > 0 {
            tracing::info!(meeting = %meeting, n, "已补回既有纪要");
        }
    }

    let (ctx2, sink2, name2) = (ctx.clone(), sink.clone(), cfg.agent_name.clone());
    let name = cfg.agent_name.clone();
    let ex_live = extractor.clone();
    let langs = Arc::new(LangCache::new());
    let src_lang = cfg.lang.clone();

    // 一句落库之后的后续:翻译、@ 应答、会中派活。**流式与批处理两条路共用**,
    // 转写怎么来的不影响后面怎么处理。
    let after: OnTranscribed = Arc::new(move |speaker: String, transcribed: Transcribed| {
        let (ctx, ans, sink, name, ex, tr, langs, src) = (
            ctx.clone(),
            answerer.clone(),
            sink.clone(),
            name.clone(),
            ex_live.clone(),
            translator.clone(),
            langs.clone(),
            src_lang.clone(),
        );
        Box::pin(async move {
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
        })
    });

    let in_room: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), room::RoomError>> + Send>> =
        match batch_pipeline {
            None => {
                // 流式:一个说话人一条流,引擎端点落句;热词 = Agent 名 + 别名 + 配置的领域词
                let mut scfg = SpeechStreamConfig::local_sherpa(&cfg.stt_url);
                if let Some(m) = &cfg.stt_model {
                    scfg.model = m.clone();
                }
                let stt: Arc<dyn SpeechStreamProvider> = Arc::new(SherpaStreamProvider::new("sherpa", scfg)?);
                let mut hot = vec![cfg.agent_name.clone()];
                hot.extend(cfg.aliases.iter().cloned());
                hot.extend(cfg.stt_hotwords.iter().cloned());
                let sp = Arc::new(
                    StreamPipeline::new(Arc::new(SpeechStreamRouter::new(vec![stt])), sink2.clone())
                        .with_language(cfg.lang.clone())
                        .with_hotwords(hot),
                );
                let consumer = sp.consumer(policy.clone(), level, after.clone());
                let (u, t) = (lk_url.clone(), lk_token.clone());
                Box::pin(async move { room::run_frames(&u, &t, consumer).await })
            }
            Some(p) => {
                let (u, t, pol, after) = (lk_url.clone(), lk_token.clone(), policy.clone(), after.clone());
                Box::pin(async move {
                    room::run(&u, &t, ChunkConfig::default(), EnergyGate::default(), move |utt| {
                        let (p, pol, after) = (p.clone(), pol.clone(), after.clone());
                        async move {
                            let speaker = utt.speaker.clone();
                            let Some(tr) = p.handle(utt, &pol).await else { return };
                            after(speaker, tr).await;
                        }
                    })
                    .await
                })
            }
        };
    // 被叫停就放下房间(drop 即断开),然后照常做散会提炼——结论往往在最后
    tokio::select! {
        r = in_room => r?,
        _ = stop.notified() => {
            tracing::info!(meeting = %meeting, "被叫停,退出房间");
            // 还在飞的转写会继续失败:退出后不再往纪要里写回执
            sink2.mute();
        }
    }
    // 退出记一笔:服务端落 meeting.leave,参会记录收口——不记的话它在参会者列表里就一直"在"
    if sink2.leave().await {
        tracing::info!(meeting = %meeting, "已离会");
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
