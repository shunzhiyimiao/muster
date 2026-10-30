//! 流式转写流水线(2026-10-30):**一个说话人一条流**,引擎做端点检测落句,背压与降级都在这里。
//!
//! ## 为什么要有它
//!
//! 2026-10-29 线上验收,两个人一说话转写就崩:[`crate::pipeline`] 那条路是"切一句、发一个请求",
//! 没有任何东西限制并发——十几个请求同时压在 2 核的 whisper 上互相拖垮,120 秒超时连着三次,
//! 熔断退出,退出后还有十几条「未能转写」落进纪要。问题不在模型大小,在形态:批处理 + 无限扇出。
//!
//! 这里改成:音频连续流进**每个说话人自己的一条连接**,引擎决定句子在哪里结束,文本边识别边回来。
//! 没有"请求"就没有超时要调、没有扇出要限流。
//!
//! ## 背压:满了丢,不阻塞
//!
//! 每条流前面一个有界队列([`StreamConfig::queue_ms`])。房间主循环只做 `try_send`,**永远不等**——
//! 等就会堵住所有人的音频。队列满了丢这一帧并计数;后端那边也一样(服务端队列满了丢并告诉我们
//! 丢了多少毫秒)。丢到一定量就在纪要里留**一条**提示,而不是每帧一条。
//!
//! ## 降级不是故障
//!
//! 跟不上(丢帧、后端 `dropped`)是**降级**:引擎还在、连接还在,留一条提示继续开会,不计熔断。
//! 熔断只留给真卡住的情况——落点被拒、连不上、连接断——这些走 [`TranscriptSink::on_refused`],
//! 由 daemon 按类计数。一条流死了之后隔 [`StreamConfig::reopen_after`] 再试着重开,
//! 期间这个人的音频丢掉(也算降级);连着重开失败三次,熔断自然会跳。
//!
//! ## 喂音频的那条线上不能有慢事
//!
//! 一句落出来之后的事——回传服务端、翻译、@ 应答、会中派活——有的要调模型,一等就是几秒。
//! 这些**绝不能在喂音频的循环里 await**:循环一停,发送队列 3 秒就满,这个人接下来的话全丢。
//! 2026-10-30 线上验收第一次就撞上:owner 叫了一声小七,Agent 去问模型,3 秒音频被跳过。
//! 所以每条流另起一个"落句"任务:回传按顺序做(纪要行序要对),后续处理各自 spawn 出去;
//! 喂音频的循环只往它的队列里丢一下,永远不等。
//!
//! ## 外发计量
//!
//! 落点在开流时决定一次(见 [`muster_route::SpeechStreamRouter`]),一条流不换落点。
//! 云端落点按**上线的音频字节**记,每落一句记一次差值;本地落点记 0。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use muster_provider::{SpeechStreamEvent, SpeechStreamHandle, StreamOpen};
use muster_route::{LabelOrigin, LabelSource, OrgPolicy, RouteRequest, Sensitivity, SpeechStreamRouter};
use tokio::sync::mpsc;

use crate::chunk::Utterance;
use crate::pipeline::{Transcribed, TranscriptSink};
use crate::SAMPLE_RATE;

/// 一句落库之后的后续(翻译、@ 应答、会中派活)。参数:说话人、转写结果。
pub type OnTranscribed =
    Arc<dyn Fn(String, Transcribed) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// 帧的去处。房间把每一帧交给它——**不做判断,不阻塞**。
#[async_trait::async_trait]
pub trait FrameConsumer: Send {
    /// 一帧 16k 单声道 PCM。`ts_ms` 是这一帧开始的时刻(epoch 毫秒)。
    async fn on_frame(&mut self, speaker: &str, pcm: &[i16], ts_ms: u64);
    /// 参会者离开:收口他的流,不等。
    async fn on_leave(&mut self, speaker: &str);
    /// 散会:所有流收口,**等尾巴吐完**——最后一句往往是结论。
    async fn finish(&mut self);
}

#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// 每路发送队列深度(毫秒)。满了丢帧——**不阻塞采集**。
    pub queue_ms: u32,
    /// 一路死了以后隔多久再试着重开。
    pub reopen_after: Duration,
    /// 丢了多少毫秒才值得在纪要里留一条提示(少于这个只记日志)。
    pub note_after_dropped_ms: u32,
    /// 两条降级提示之间至少隔多久——提示本身不能把纪要刷屏。
    pub note_interval: Duration,
    /// `finish` 之后等后端吐尾巴最多等多久。
    pub flush_timeout: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            queue_ms: 3_000,
            reopen_after: Duration::from_secs(15),
            note_after_dropped_ms: 1_000,
            note_interval: Duration::from_secs(30),
            flush_timeout: Duration::from_secs(10),
        }
    }
}

pub struct StreamPipeline {
    router: Arc<SpeechStreamRouter>,
    sink: Arc<dyn TranscriptSink>,
    language: Option<String>,
    /// 热词:Agent 名、别名、与会者名、领域词。后端不支持就忽略;**绝不放机密**。
    hotwords: Vec<String>,
    cfg: StreamConfig,
}

impl StreamPipeline {
    pub fn new(router: Arc<SpeechStreamRouter>, sink: Arc<dyn TranscriptSink>) -> Self {
        Self { router, sink, language: None, hotwords: vec![], cfg: StreamConfig::default() }
    }

    pub fn with_language(mut self, lang: impl Into<String>) -> Self {
        self.language = Some(lang.into());
        self
    }

    pub fn with_hotwords(mut self, words: Vec<String>) -> Self {
        self.hotwords = words.into_iter().map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect();
        self
    }

    pub fn with_config(mut self, cfg: StreamConfig) -> Self {
        self.cfg = cfg;
        self
    }

    /// 为一场会造一个帧消费者。`level` 是会议密级——它是路由决策的来源之一,
    /// 受限级会议的流只会落在本地。
    pub fn consumer(self: &Arc<Self>, policy: OrgPolicy, level: Sensitivity, on_text: OnTranscribed) -> StreamConsumer {
        StreamConsumer {
            pipe: self.clone(),
            policy,
            sources: vec![LabelSource::new(LabelOrigin::Channel, level, "meeting")],
            on_text,
            lanes: HashMap::new(),
        }
    }
}

/// 一场会里所有说话人的流。
pub struct StreamConsumer {
    pipe: Arc<StreamPipeline>,
    policy: OrgPolicy,
    sources: Vec<LabelSource>,
    on_text: OnTranscribed,
    lanes: HashMap<String, Lane>,
}

/// 一个说话人的一条流:发送队列 + 后台任务。
struct Lane {
    tx: Option<mpsc::Sender<Vec<i16>>>,
    /// 任务退出时置假(正常收尾或死了都算)。
    alive: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
    /// 第一次观察到它不活了的时刻,据此决定何时重开。
    died_at: Option<Instant>,
    /// 本地队列溢出丢掉的毫秒数(当前这一轮)。
    dropped_ms: u32,
    /// 这一轮丢帧已经提示过了。
    noted: bool,
    last_note: Option<Instant>,
}

fn frame_ms(pcm: &[i16]) -> u32 {
    (pcm.len() as u64 * 1000 / SAMPLE_RATE as u64) as u32
}

fn now_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 被拒 / 失败留痕用的"空句":只有说话人与时刻,没有音频(流式路径不留 PCM)。
fn marker(speaker: &str, at_ms: u64) -> Utterance {
    Utterance { speaker: speaker.to_string(), pcm: vec![], started_ms: at_ms, duration_ms: 0 }
}

impl StreamConsumer {
    fn open_lane(&self, speaker: &str) -> Lane {
        let frames = (self.pipe.cfg.queue_ms / 10).max(1) as usize; // 10ms 一帧的量级
        let (tx, rx) = mpsc::channel::<Vec<i16>>(frames);
        let alive = Arc::new(AtomicBool::new(true));
        let task = tokio::spawn(run_lane(
            self.pipe.clone(),
            self.policy.clone(),
            self.sources.clone(),
            speaker.to_string(),
            rx,
            alive.clone(),
            self.on_text.clone(),
        ));
        Lane { tx: Some(tx), alive, task, died_at: None, dropped_ms: 0, noted: false, last_note: None }
    }

    pub fn lanes(&self) -> usize {
        self.lanes.len()
    }
}

#[async_trait::async_trait]
impl FrameConsumer for StreamConsumer {
    async fn on_frame(&mut self, speaker: &str, pcm: &[i16], _ts_ms: u64) {
        let reopen_after = self.pipe.cfg.reopen_after;
        // 死了的流:到点就重开,没到点就丢掉这帧(算降级,计在 dropped_ms 里)
        if let Some(l) = self.lanes.get_mut(speaker) {
            if !l.alive.load(Ordering::SeqCst) {
                let died = *l.died_at.get_or_insert_with(Instant::now);
                if died.elapsed() >= reopen_after {
                    tracing::info!(speaker, "重开转写流");
                    let fresh = self.open_lane(speaker);
                    self.lanes.insert(speaker.to_string(), fresh);
                } else {
                    l.dropped_ms = l.dropped_ms.saturating_add(frame_ms(pcm));
                    return;
                }
            }
        }
        if !self.lanes.contains_key(speaker) {
            let fresh = self.open_lane(speaker);
            self.lanes.insert(speaker.to_string(), fresh);
        }
        let cfg = &self.pipe.cfg;
        let lane = self.lanes.get_mut(speaker).expect("刚插入");
        let Some(tx) = lane.tx.as_ref() else { return };
        match tx.try_send(pcm.to_vec()) {
            Ok(()) => {
                // 一轮丢帧结束:提示(如果够量且还没提示过),计数归零
                if lane.dropped_ms > 0 {
                    if lane.dropped_ms >= cfg.note_after_dropped_ms
                        && !lane.noted
                        && lane.last_note.map(|t| t.elapsed() >= cfg.note_interval).unwrap_or(true)
                    {
                        let note = format!("[转写跟不上,{speaker} 的发言跳过了约 {} 秒]", (lane.dropped_ms + 500) / 1000);
                        lane.last_note = Some(Instant::now());
                        self.pipe.sink.on_degraded(speaker, &note).await;
                    }
                    tracing::warn!(speaker, dropped_ms = lane.dropped_ms, "转写发送队列溢出,这一轮丢帧结束");
                    lane.dropped_ms = 0;
                    lane.noted = false;
                }
            }
            Err(_) => {
                // 满了就丢,**不等**。够量就提示一次,其余只计数
                lane.dropped_ms = lane.dropped_ms.saturating_add(frame_ms(pcm));
                if lane.dropped_ms >= cfg.note_after_dropped_ms
                    && !lane.noted
                    && lane.last_note.map(|t| t.elapsed() >= cfg.note_interval).unwrap_or(true)
                {
                    lane.noted = true;
                    lane.last_note = Some(Instant::now());
                    let note = format!("[转写跟不上,{speaker} 的发言正在被跳过]");
                    self.pipe.sink.on_degraded(speaker, &note).await;
                }
            }
        }
    }

    async fn on_leave(&mut self, speaker: &str) {
        // 关掉发送端 ⇒ 任务收到 None ⇒ finish ⇒ 等后端吐尾巴。**不等它**:
        // 等会堵住别人的音频;任务自己会在 flush_timeout 内结束。
        if let Some(mut l) = self.lanes.remove(speaker) {
            l.tx.take();
        }
    }

    async fn finish(&mut self) {
        let lanes: Vec<(String, Lane)> = self.lanes.drain().collect();
        for (speaker, mut l) in lanes {
            l.tx.take();
            if let Err(e) = l.task.await {
                tracing::warn!(speaker = %speaker, error = %e, "转写流任务异常结束");
            }
        }
    }
}

/// 一条流的一生:决定落点 → 开流 → 喂音频 / 收句子 → 收尾。退出时把 `alive` 置假。
async fn run_lane(
    pipe: Arc<StreamPipeline>,
    policy: OrgPolicy,
    sources: Vec<LabelSource>,
    speaker: String,
    mut rx: mpsc::Receiver<Vec<i16>>,
    alive: Arc<AtomicBool>,
    on_text: OnTranscribed,
) {
    struct Dead(Arc<AtomicBool>);
    impl Drop for Dead {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _dead = Dead(alive);
    let opened_at = now_epoch_ms();
    let sink = pipe.sink.clone();

    // 落点一次定终身:一条流不换落点
    let req = RouteRequest { sources: &sources, requested_provider: None, default_provider: None };
    let res = match pipe.router.resolve(&policy, &req) {
        Ok(r) => r,
        Err(e) => {
            sink.on_refused(&marker(&speaker, opened_at), &e.to_string()).await;
            return;
        }
    };
    let is_local = SpeechStreamRouter::is_local(&res);
    let open = StreamOpen { speaker: speaker.clone(), language: pipe.language.clone(), hotwords: pipe.hotwords.clone() };
    let SpeechStreamHandle { input, events, wire_bytes } = match res.provider.open(open).await {
        Ok(h) => h,
        Err(e) => {
            sink.on_refused(&marker(&speaker, opened_at), &format!("转写流打不开:{e}")).await;
            return;
        }
    };
    tracing::info!(speaker = %speaker, provider = %res.plan.primary, local = is_local, "转写流已开");

    // 落句任务:回传按顺序、后续处理各自 spawn。喂音频的循环只 send,永远不等(见模块文档)
    let (post_tx, mut post_rx) = mpsc::unbounded_channel::<(Utterance, String, u64)>();
    let poster = {
        let (sink, on_text, speaker) = (sink.clone(), on_text.clone(), speaker.clone());
        tokio::spawn(async move {
            let mut downstream: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            while let Some((u, text, egress)) = post_rx.recv().await {
                let line_id = sink.on_text(&u, &text, egress).await;
                let (cb, sp) = (on_text.clone(), speaker.clone());
                downstream.push(tokio::spawn(async move { cb(sp, Transcribed { text, line_id }).await }));
                downstream.retain(|h| !h.is_finished());
            }
            // 流收尾时等后续处理做完:散会提炼紧跟在后面,它得看见最后几句的翻译与作答
            for h in downstream {
                let _ = h.await;
            }
        })
    };
    lane_loop(&pipe, &sink, &speaker, &mut rx, input, events, wire_bytes, is_local, opened_at, post_tx).await;
    let _ = poster.await;
}

/// 喂音频 / 收事件的循环。返回即这条流结束(正常收尾或死了);落句交给 `post_tx`。
#[allow(clippy::too_many_arguments)]
async fn lane_loop(
    pipe: &StreamPipeline,
    sink: &Arc<dyn TranscriptSink>,
    speaker: &str,
    rx: &mut mpsc::Receiver<Vec<i16>>,
    mut input: Box<dyn muster_provider::SpeechStreamInput>,
    mut events: mpsc::Receiver<SpeechStreamEvent>,
    wire_bytes: Arc<std::sync::atomic::AtomicU64>,
    is_local: bool,
    opened_at: u64,
    post_tx: mpsc::UnboundedSender<(Utterance, String, u64)>,
) {
    let speaker = speaker.to_string();
    let mut metered: u64 = 0;
    let mut finishing = false;
    let mut flush_deadline = tokio::time::Instant::now() + Duration::from_secs(3600);
    let mut backend_dropped_ms: u32 = 0;
    let mut last_drop_note: Option<Instant> = None;
    let cfg = &pipe.cfg;

    loop {
        tokio::select! {
            frame = rx.recv(), if !finishing => {
                match frame {
                    Some(pcm) => {
                        if let Err(e) = input.feed(&pcm).await {
                            sink.on_refused(&marker(&speaker, now_epoch_ms()), &format!("转写流断了:{e}")).await;
                            return;
                        }
                    }
                    None => {
                        finishing = true;
                        flush_deadline = tokio::time::Instant::now() + cfg.flush_timeout;
                        if let Err(e) = input.finish().await {
                            sink.on_refused(&marker(&speaker, now_epoch_ms()), &format!("转写流收尾失败:{e}")).await;
                            return;
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(flush_deadline), if finishing => {
                tracing::warn!(speaker = %speaker, "等后端吐尾巴超时,放弃最后半句");
                return;
            }
            ev = events.recv() => {
                match ev {
                    Some(SpeechStreamEvent::Final { text, start_ms, end_ms }) => {
                        let text = text.trim();
                        if text.is_empty() {
                            continue;
                        }
                        let u = Utterance {
                            speaker: speaker.clone(),
                            pcm: vec![],
                            started_ms: opened_at + start_ms,
                            duration_ms: end_ms.saturating_sub(start_ms) as u32,
                        };
                        let total = wire_bytes.load(Ordering::Relaxed);
                        let egress = if is_local { 0 } else { total.saturating_sub(metered) };
                        metered = total;
                        // 只丢进落句队列,不等:回传与后续处理在另一个任务里
                        let _ = post_tx.send((u, text.to_string(), egress));
                    }
                    Some(SpeechStreamEvent::Partial { .. }) => {}
                    Some(SpeechStreamEvent::Dropped { ms }) => {
                        backend_dropped_ms = backend_dropped_ms.saturating_add(ms);
                        tracing::warn!(speaker = %speaker, ms, total = backend_dropped_ms, "转写后端过载,丢了音频");
                        if backend_dropped_ms >= cfg.note_after_dropped_ms
                            && last_drop_note.map(|t| t.elapsed() >= cfg.note_interval).unwrap_or(true)
                        {
                            last_drop_note = Some(Instant::now());
                            let note = format!("[转写引擎过载,{speaker} 的发言跳过了约 {} 秒]", (backend_dropped_ms + 500) / 1000);
                            backend_dropped_ms = 0;
                            sink.on_degraded(&speaker, &note).await;
                        }
                    }
                    Some(SpeechStreamEvent::Error(e)) => {
                        sink.on_refused(&marker(&speaker, now_epoch_ms()), &format!("转写流出错:{e}")).await;
                        return;
                    }
                    Some(SpeechStreamEvent::Closed) => {
                        if !finishing {
                            sink.on_refused(&marker(&speaker, now_epoch_ms()), "转写后端提前关闭了连接").await;
                        }
                        return;
                    }
                    None => {
                        if !finishing {
                            sink.on_refused(&marker(&speaker, now_epoch_ms()), "转写连接中断").await;
                        }
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, AtomicU64};
    use std::sync::Mutex;

    use muster_provider::{
        Locality, ProviderError, ProviderMetadata, SpeechStreamInput, SpeechStreamProvider,
    };

    use super::*;

    /// 测试桩后端的行为。
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// 每满 1 秒音频出一句;finish 时把余下的吐成最后一句
        Echo,
        /// 开流就失败
        OpenFails,
        /// 第一帧之后报错(连接断)
        DiesAfterFirstFrame,
        /// 每帧都说丢了 500ms(过载)
        Overloaded,
        /// feed 永远不返回(把本地队列憋满)
        Stuck,
    }

    struct Fake {
        meta: ProviderMetadata,
        mode: Mutex<Mode>,
        opens: AtomicU32,
        /// 第 N 次开流之后切换到的模式(模拟"死过一次又好了")
        after_first_open: Option<Mode>,
    }

    enum Msg {
        Frame(Vec<i16>),
        Finish,
    }

    struct FakeInput {
        tx: mpsc::UnboundedSender<Msg>,
        bytes: Arc<AtomicU64>,
        stuck: bool,
    }

    #[async_trait::async_trait]
    impl SpeechStreamInput for FakeInput {
        async fn feed(&mut self, pcm: &[i16]) -> Result<(), ProviderError> {
            if self.stuck {
                std::future::pending::<()>().await;
            }
            self.bytes.fetch_add(pcm.len() as u64 * 2, Ordering::Relaxed);
            self.tx.send(Msg::Frame(pcm.to_vec())).map_err(|_| ProviderError::Unreachable("后端没了".into()))
        }
        async fn finish(&mut self) -> Result<(), ProviderError> {
            self.tx.send(Msg::Finish).map_err(|_| ProviderError::Unreachable("后端没了".into()))
        }
    }

    #[async_trait::async_trait]
    impl SpeechStreamProvider for Fake {
        fn metadata(&self) -> &ProviderMetadata {
            &self.meta
        }
        async fn open(&self, o: StreamOpen) -> Result<SpeechStreamHandle, ProviderError> {
            let n = self.opens.fetch_add(1, Ordering::SeqCst);
            let mode = *self.mode.lock().unwrap();
            if n == 0 {
                if let Some(m) = self.after_first_open {
                    *self.mode.lock().unwrap() = m;
                }
            }
            if mode == Mode::OpenFails {
                return Err(ProviderError::Unreachable("开不了".into()));
            }
            assert_eq!(o.hotwords, vec!["小七".to_string()], "热词要透传到后端");
            let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
            let (etx, events) = mpsc::channel(64);
            let bytes = Arc::new(AtomicU64::new(0));
            tokio::spawn(async move {
                let mut samples = 0u64;
                let mut seg = 0u32;
                let mut seg_start = 0u64;
                while let Some(m) = rx.recv().await {
                    match m {
                        Msg::Frame(f) => {
                            samples += f.len() as u64;
                            match mode {
                                Mode::DiesAfterFirstFrame => {
                                    let _ = etx.send(SpeechStreamEvent::Error("连接断了".into())).await;
                                    return;
                                }
                                Mode::Overloaded => {
                                    let _ = etx.send(SpeechStreamEvent::Dropped { ms: 500 }).await;
                                }
                                _ => {
                                    if samples - seg_start >= 16_000 {
                                        seg += 1;
                                        let _ = etx
                                            .send(SpeechStreamEvent::Final {
                                                text: format!("第{seg}句"),
                                                start_ms: seg_start / 16,
                                                end_ms: samples / 16,
                                            })
                                            .await;
                                        seg_start = samples;
                                    }
                                }
                            }
                        }
                        Msg::Finish => {
                            if samples > seg_start && mode == Mode::Echo {
                                seg += 1;
                                let _ = etx
                                    .send(SpeechStreamEvent::Final {
                                        text: format!("尾巴{seg}"),
                                        start_ms: seg_start / 16,
                                        end_ms: samples / 16,
                                    })
                                    .await;
                            }
                            let _ = etx.send(SpeechStreamEvent::Closed).await;
                            return;
                        }
                    }
                }
            });
            Ok(SpeechStreamHandle {
                input: Box::new(FakeInput { tx, bytes: bytes.clone(), stuck: mode == Mode::Stuck }),
                events,
                wire_bytes: bytes,
            })
        }
        async fn health_check(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn fake(locality: Locality, mode: Mode, after_first_open: Option<Mode>) -> Arc<Fake> {
        Arc::new(Fake {
            meta: ProviderMetadata {
                id: "stt".into(),
                display_name: "stt".into(),
                model: "paraformer".into(),
                locality,
                endpoint: "ws://x".into(),
            },
            mode: Mutex::new(mode),
            opens: AtomicU32::new(0),
            after_first_open,
        })
    }

    #[derive(Default)]
    struct Recorder {
        texts: Mutex<Vec<(String, String, u64)>>,
        refusals: Mutex<Vec<(String, String)>>,
        degraded: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl TranscriptSink for Recorder {
        async fn on_text(&self, u: &Utterance, text: &str, egress: u64) -> Option<i64> {
            self.texts.lock().unwrap().push((u.speaker.clone(), text.into(), egress));
            Some(self.texts.lock().unwrap().len() as i64)
        }
        async fn on_refused(&self, u: &Utterance, reason: &str) {
            self.refusals.lock().unwrap().push((u.speaker.clone(), reason.into()));
        }
        async fn on_degraded(&self, speaker: &str, note: &str) {
            self.degraded.lock().unwrap().push((speaker.into(), note.into()));
        }
    }

    struct Rig {
        rec: Arc<Recorder>,
        consumer: StreamConsumer,
        downstream: Arc<Mutex<Vec<(String, Transcribed)>>>,
        fake: Arc<Fake>,
    }

    fn rig(locality: Locality, mode: Mode, after: Option<Mode>, level: Sensitivity, cfg: StreamConfig) -> Rig {
        let rec = Arc::new(Recorder::default());
        let fake = fake(locality, mode, after);
        let p: Arc<dyn SpeechStreamProvider> = fake.clone();
        let pipe = Arc::new(
            StreamPipeline::new(Arc::new(SpeechStreamRouter::new(vec![p])), rec.clone())
                .with_language("zh")
                .with_hotwords(vec!["小七".into(), "  ".into()])
                .with_config(cfg),
        );
        let downstream = Arc::new(Mutex::new(vec![]));
        let d2 = downstream.clone();
        let on_text: OnTranscribed = Arc::new(move |s, t| {
            let d = d2.clone();
            Box::pin(async move {
                d.lock().unwrap().push((s, t));
            })
        });
        let consumer = pipe.consumer(OrgPolicy::new(Sensitivity::Internal).unwrap(), level, on_text);
        Rig { rec, consumer, downstream, fake }
    }

    const FRAME: usize = 160; // 10ms

    async fn feed_ms(c: &mut StreamConsumer, speaker: &str, ms: u32) {
        for i in 0..(ms / 10) {
            c.on_frame(speaker, &[100i16; FRAME], i as u64 * 10).await;
            // 让后台任务有机会跑;真实房间里帧本来就是 10ms 一来
            tokio::task::yield_now().await;
        }
    }

    /// 音频流进去,句子按端点落出来:带说话人、本地落点外发记 0、后续回调拿到行号。
    #[tokio::test]
    async fn frames_become_sentences_with_speaker_and_zero_local_egress() {
        let mut r = rig(Locality::Local, Mode::Echo, None, Sensitivity::Internal, StreamConfig::default());
        feed_ms(&mut r.consumer, "alice", 2_000).await;
        r.consumer.finish().await;

        let t = r.rec.texts.lock().unwrap();
        assert_eq!(t.len(), 2, "2 秒音频 = 2 句:{t:?}");
        assert_eq!((t[0].0.as_str(), t[0].1.as_str(), t[0].2), ("alice", "第1句", 0));
        let d = r.downstream.lock().unwrap();
        assert_eq!(d.len(), 2, "每句都要触发后续(翻译 / 作答)");
        assert!(
            d.iter().any(|x| x.1 == Transcribed { text: "第2句".into(), line_id: Some(2) }),
            "行号来自落点:{d:?}"
        );
        assert!(r.rec.refusals.lock().unwrap().is_empty());
        assert!(r.rec.degraded.lock().unwrap().is_empty());
    }

    /// **后续处理慢(问模型要几秒)不能挡住喂音频**:2026-10-30 线上第一次验收就是这么丢了 3 秒。
    /// 队列只给 200ms,后续每句睡 500ms;要是在喂音频的循环里等它,队列必溢出。
    #[tokio::test]
    async fn slow_downstream_does_not_stall_feeding() {
        let cfg = StreamConfig { queue_ms: 200, note_after_dropped_ms: 100, ..StreamConfig::default() };
        let rec = Arc::new(Recorder::default());
        let p: Arc<dyn SpeechStreamProvider> = fake(Locality::Local, Mode::Echo, None);
        let pipe = Arc::new(
            StreamPipeline::new(Arc::new(SpeechStreamRouter::new(vec![p])), rec.clone())
                .with_hotwords(vec!["小七".into()])
                .with_config(cfg),
        );
        let calls = Arc::new(Mutex::new(0u32));
        let c2 = calls.clone();
        let slow: OnTranscribed = Arc::new(move |_s, _t| {
            let c = c2.clone();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                *c.lock().unwrap() += 1;
            })
        });
        let mut consumer = pipe.consumer(OrgPolicy::new(Sensitivity::Internal).unwrap(), Sensitivity::Internal, slow);
        feed_ms(&mut consumer, "alice", 3_000).await;
        consumer.finish().await;
        assert!(rec.degraded.lock().unwrap().is_empty(), "后续慢不该让音频被跳过:{:?}", rec.degraded.lock().unwrap());
        assert_eq!(rec.texts.lock().unwrap().len(), 3, "3 秒 = 3 句都要落");
        assert_eq!(*calls.lock().unwrap(), 3, "finish 要等后续处理做完");
    }

    /// 散会时最后半句不能丢:finish 要等后端把尾巴吐完。
    #[tokio::test]
    async fn finish_flushes_the_last_half_sentence() {
        let mut r = rig(Locality::Local, Mode::Echo, None, Sensitivity::Internal, StreamConfig::default());
        feed_ms(&mut r.consumer, "alice", 500).await;
        r.consumer.finish().await;
        let t = r.rec.texts.lock().unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].1, "尾巴1");
    }

    /// 云端落点按**上线的音频字节**记外发,一句记一次差值,总和等于全部音频。
    #[tokio::test]
    async fn cloud_lane_meters_audio_bytes_not_text() {
        let mut r = rig(Locality::Cloud, Mode::Echo, None, Sensitivity::Internal, StreamConfig::default());
        feed_ms(&mut r.consumer, "alice", 2_000).await;
        r.consumer.finish().await;
        let t = r.rec.texts.lock().unwrap();
        let total: u64 = t.iter().map(|x| x.2).sum();
        assert_eq!(total, 2_000 * 32, "2 秒 16k PCM = 64000 字节,实际 {total}");
        assert!(t.iter().all(|x| x.2 > 0), "云端每句都要有外发量");
    }

    /// **受限级会议只能落本地**:只有云端引擎时整条流被拒,而且只留一条痕,不是每帧一条。
    #[tokio::test]
    async fn restricted_meeting_with_cloud_only_is_refused_once() {
        let mut r = rig(Locality::Cloud, Mode::Echo, None, Sensitivity::Restricted, StreamConfig::default());
        feed_ms(&mut r.consumer, "alice", 1_000).await;
        r.consumer.finish().await;
        assert!(r.rec.texts.lock().unwrap().is_empty(), "受限级不得上云转写");
        let refusals = r.rec.refusals.lock().unwrap();
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert_eq!(refusals[0].0, "alice");
    }

    /// 后端过载是降级:留提示、不计熔断(不走 on_refused)。
    #[tokio::test]
    async fn backend_overload_is_a_note_not_a_refusal() {
        let mut r = rig(Locality::Local, Mode::Overloaded, None, Sensitivity::Internal, StreamConfig::default());
        feed_ms(&mut r.consumer, "alice", 1_000).await;
        r.consumer.finish().await;
        assert!(r.rec.refusals.lock().unwrap().is_empty(), "过载不是故障");
        let d = r.rec.degraded.lock().unwrap();
        assert_eq!(d.len(), 1, "30 秒内只提示一次:{d:?}");
        assert!(d[0].1.contains("过载"), "{}", d[0].1);
    }

    /// 本地队列满了丢帧**不阻塞**,够量提示一次。
    #[tokio::test]
    async fn queue_overflow_drops_without_blocking_and_notes_once() {
        let cfg = StreamConfig { queue_ms: 200, note_after_dropped_ms: 300, ..StreamConfig::default() };
        let mut r = rig(Locality::Local, Mode::Stuck, None, Sensitivity::Internal, cfg);
        let t0 = Instant::now();
        feed_ms(&mut r.consumer, "alice", 1_000).await;
        assert!(t0.elapsed() < Duration::from_secs(2), "on_frame 必须立刻返回,不等后端");
        let d = r.rec.degraded.lock().unwrap();
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].1.contains("跳过"), "{}", d[0].1);
        assert!(r.rec.refusals.lock().unwrap().is_empty(), "丢帧是降级不是故障");
    }

    /// 流死了留一条痕,到点重开;重开后句子照常落。开不了流也是同一条路(熔断由 daemon 按类计数)。
    #[tokio::test]
    async fn dead_lane_is_reported_once_and_reopened_after_cooldown() {
        let cfg = StreamConfig { reopen_after: Duration::from_millis(50), ..StreamConfig::default() };
        let mut r = rig(Locality::Local, Mode::DiesAfterFirstFrame, Some(Mode::Echo), Sensitivity::Internal, cfg);
        feed_ms(&mut r.consumer, "alice", 200).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        feed_ms(&mut r.consumer, "alice", 1_000).await;
        r.consumer.finish().await;

        assert_eq!(r.fake.opens.load(Ordering::SeqCst), 2, "冷却后重开一次");
        let refusals = r.rec.refusals.lock().unwrap();
        assert_eq!(refusals.len(), 1, "死一次留一条痕:{refusals:?}");
        assert!(refusals[0].1.contains("转写流"), "{}", refusals[0].1);
        assert!(!r.rec.texts.lock().unwrap().is_empty(), "重开后要能继续落句");
    }

    #[tokio::test]
    async fn open_failure_is_refused_and_counts_for_the_breaker() {
        let mut r = rig(Locality::Local, Mode::OpenFails, None, Sensitivity::Internal, StreamConfig::default());
        feed_ms(&mut r.consumer, "bob", 300).await;
        r.consumer.finish().await;
        let refusals = r.rec.refusals.lock().unwrap();
        assert_eq!(refusals.len(), 1);
        assert!(refusals[0].1.contains("打不开"), "{}", refusals[0].1);
        assert_eq!(crate::node::classify_error(&format!("转写 {}", refusals[0].1)), "stt", "按 stt 类计熔断");
    }

    /// 两个人各自一条流,互不影响;其中一个离开只收他自己的。
    #[tokio::test]
    async fn each_speaker_has_their_own_lane() {
        let mut r = rig(Locality::Local, Mode::Echo, None, Sensitivity::Internal, StreamConfig::default());
        feed_ms(&mut r.consumer, "alice", 1_000).await;
        feed_ms(&mut r.consumer, "bob", 1_000).await;
        assert_eq!(r.consumer.lanes(), 2);
        r.consumer.on_leave("alice").await;
        assert_eq!(r.consumer.lanes(), 1);
        r.consumer.finish().await;
        let t = r.rec.texts.lock().unwrap();
        assert!(t.iter().any(|x| x.0 == "alice") && t.iter().any(|x| x.0 == "bob"), "{t:?}");
    }
}
