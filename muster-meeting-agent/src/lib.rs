//! # muster-meeting-agent — 会议里的那个 Agent
//!
//! 它作为一个**参会者**坐在 LiveKit 房间里:订阅每个人的音轨、切句、转写、
//! 把文本回传给 collab-server;被 @ 到时经 `muster_route` 问模型并作答。
//!
//! ## 关键设计决策
//!
//! 1. **核心链路不依赖 LiveKit**。缓冲 → 切句 → 转写 → 回传这条链只认
//!    「谁说的 + 一段 16k 单声道 PCM」,LiveKit 只是帧的一个来源,测试是另一个。
//!    好处不只是可测:换掉媒体面(或者接一路会议录音文件)时,这条链一行不用改。
//!    SDK 适配层在 `livekit` feature 后面,**默认不编**——它传递依赖原生 WebRTC,
//!    构建期要从 GitHub 拉 100MB+、target 1GB+(见 `docs/A1-livekit-探针结论.md`),
//!    而本仓的约定是"动手前后都跑 cargo test"。
//!
//! 2. **说话人归属不靠声纹分离**。LiveKit 每个参会者各一条音轨,订阅事件直接带
//!    participant;而入会令牌的 `sub` 就是 Muster 账号 id,于是"这句话是谁说的"
//!    天然对得上人。整条链因此只处理**单人单轨**,不做 diarization。
//!
//! 3. **静音不送去转写**。这不只是省算力:whisper 在纯静音上会产生幻觉
//!    (凭空转出"谢谢观看"之类的句子),送进去等于往会议纪要里掺假话。
//!    [`SpeechGate`] 是这道闸,A2 先用能量阈值,A3 换 VAD——同一个 trait,不改上层。
//!
//! 4. **转写走 `muster_route`,不直连**。会议音频是全系统密级最高的数据流;
//!    详见 `docs/服务端架构.md` 的边界五。本 crate 只负责把音频切好、
//!    交给 [`muster_route::SpeechRouter`],密级与演习封锁由那一层保证。
//!
//! ## 诚实边界
//!
//! - 切句当前是**能量阈值 + 静音间隔**,不是 VAD:嘈杂环境会误判成一直在说话,
//!   靠 `max_utterance_ms` 硬切兜底。A3 换 Silero。
//! - 不做打断处理、不做回声消除——那些是 Agent 要"开口说话"(B4)时才需要的。
//! - 不做重叠说话分离:两人同时说话时各自的轨是独立的,各转各的,
//!   纪要里会出现两条时间重叠的句子。这是如实反映,不是缺陷。

pub mod actions;
pub mod answer;
pub mod chunk;
pub mod gate;
pub mod mention;
pub mod pipeline;
pub mod sink;
pub mod wav;

#[cfg(feature = "livekit")]
pub mod room;

pub use actions::{ActionItem, ExtractOutcome, Extractor};
pub use answer::{Answer, Answerer, Context};
pub use chunk::{Accumulator, ChunkConfig, Utterance};
pub use mention::MentionRules;
pub use gate::{EnergyGate, SpeechGate};
pub use pipeline::{Pipeline, TranscriptSink};
pub use sink::HttpSink;
pub use wav::pcm16_to_wav;

/// 会议 Agent 处理音频的采样格式。**固定 16k 单声道**:whisper 要这个,
/// 而 LiveKit 的 `NativeAudioStream` 允许调用方直接指定,不必自己重采样。
pub const SAMPLE_RATE: u32 = 16_000;
pub const CHANNELS: u16 = 1;
