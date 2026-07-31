//! LiveKit 适配层:把房间里的音轨变成 [`Utterance`] 流。
//!
//! **本模块只做搬运,不做判断。** 切句归 [`crate::chunk`],转写与治理归
//! [`crate::pipeline`]。这样换掉媒体面时,要改的只有这一个文件。
//!
//! 接口形状取自 A1 探针实测(见 `docs/A1-livekit-探针结论.md`),不是猜的:
//! - `NativeAudioStream::new(track, sample_rate, num_channels)`
//!   由调用方指定格式 ⇒ 直接要 16k 单声道,不必自己重采样;
//! - `RoomEvent::TrackSubscribed` 带 `participant` ⇒ 说话人归属白拿,
//!   不做声纹分离。
//!
//! ## 为什么累加器只有一份、放在主循环里
//!
//! [`Accumulator`] 跨说话人持有状态,需要 `&mut`。若每条音轨各持一份,
//! "谁在说话"的全局视角就没了;若共享一份加锁,每帧都要抢锁(16k 单声道
//! 每 10ms 一帧,几个参会者就是每秒几百次)。所以:**每条轨一个任务只管
//! 把帧丢进 channel,主循环独占累加器**。

use std::sync::Arc;

use futures::StreamExt;
use livekit::prelude::*;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use tokio::sync::mpsc;

use crate::chunk::{Accumulator, ChunkConfig, Utterance};
use crate::gate::SpeechGate;
use crate::{CHANNELS, SAMPLE_RATE};

/// 一帧音频 + 它属于谁。
struct Frame {
    speaker: String,
    pcm: Vec<i16>,
    ts_ms: u64,
}

/// 本 crate 侧的帧队列深度。转写比采集慢,队列的作用是吸收抖动而不是无限缓冲——
/// **满了宁可丢帧也不阻塞采集**:阻塞会让 WebRTC 侧堆积,越拖越糟。
/// (SDK 内部另有一层队列,用它的默认深度。)
const FRAME_QUEUE: usize = 512;

#[derive(Debug, thiserror::Error)]
pub enum RoomError {
    #[error("连接房间失败:{0}")]
    Connect(String),
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 连上房间,持续产出切好的句子,直到断开。
///
/// `on_utterance` 每断出一句调用一次。它应当**尽快返回**(把活儿丢给别的任务),
/// 否则会拖住整个房间的帧消费。
pub async fn run<G, F, Fut>(
    url: &str,
    token: &str,
    cfg: ChunkConfig,
    gate: G,
    mut on_utterance: F,
) -> Result<(), RoomError>
where
    G: SpeechGate + 'static,
    F: FnMut(Utterance) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (room, mut events) = Room::connect(url, token, RoomOptions::default())
        .await
        .map_err(|e| RoomError::Connect(e.to_string()))?;
    tracing::info!(room = %room.name(), "会议 Agent 已入房间");

    let (tx, mut rx) = mpsc::channel::<Frame>(FRAME_QUEUE);
    let mut acc = Accumulator::new(cfg, gate);

    loop {
        tokio::select! {
            // 优先消费音频帧:积压会直接变成转写延迟
            biased;

            Some(f) = rx.recv() => {
                if let Some(u) = acc.push(&f.speaker, &f.pcm, f.ts_ms) {
                    on_utterance(u).await;
                }
            }

            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    RoomEvent::TrackSubscribed { track, participant, .. } => {
                        let who = participant.identity().to_string();
                        if let RemoteTrack::Audio(audio) = track {
                            tracing::info!(speaker = %who, "订阅到音轨");
                            spawn_pump(audio, who, tx.clone());
                        }
                    }
                    RoomEvent::ParticipantDisconnected(p) => {
                        // 人走了要把残留的半句吐出来——**最后一句往往是结论**
                        let who = p.identity().to_string();
                        if let Some(u) = acc.remove(&who) {
                            on_utterance(u).await;
                        }
                        tracing::info!(speaker = %who, "参会者离开");
                    }
                    RoomEvent::Disconnected { reason } => {
                        tracing::info!(?reason, "房间断开");
                        break;
                    }
                    _ => {}
                }
            }

            else => break,
        }
    }

    // 会议结束:所有人的残留都要吐出来,一句都不能丢
    let speakers: Vec<String> = acc.speakers();
    for s in speakers {
        if let Some(u) = acc.remove(&s) {
            on_utterance(u).await;
        }
    }
    Ok(())
}

/// 一条音轨一个任务:只负责把帧丢进 channel,不做任何判断。
fn spawn_pump(audio: RemoteAudioTrack, speaker: String, tx: mpsc::Sender<Frame>) {
    // 关键:直接要 whisper 想要的格式,SDK 负责重采样。
    // 注意签名是三参(队列深度用 SDK 默认值)——crates.io 上的 0.3.43 是四参,
    // 而我们构建的是 git 版。**读 API 要读实际构建的那个版本**,别读别的。
    let mut stream =
        NativeAudioStream::new(audio.rtc_track(), SAMPLE_RATE as i32, CHANNELS as i32);
    tokio::spawn(async move {
        let mut dropped = 0u64;
        while let Some(frame) = stream.next().await {
            let f = Frame {
                speaker: speaker.clone(),
                pcm: frame.data.to_vec(),
                ts_ms: now_ms(),
            };
            // 满了就丢这一帧,**不阻塞采集**:阻塞会让 WebRTC 侧堆积,越拖越糟。
            // 丢帧要计数并上报,不能悄悄丢——丢了帧就是丢了话。
            if tx.try_send(f).is_err() {
                dropped += 1;
                if dropped % 100 == 1 {
                    tracing::warn!(speaker = %speaker, dropped, "转写跟不上采集,已丢帧");
                }
            }
        }
        if dropped > 0 {
            tracing::warn!(speaker = %speaker, dropped, "音轨结束,累计丢帧");
        }
    });
}

/// 便利入口:连房间 + 跑转写流水线。
pub async fn run_with_pipeline<G>(
    url: &str,
    token: &str,
    cfg: ChunkConfig,
    gate: G,
    pipeline: Arc<crate::pipeline::Pipeline>,
    policy: muster_route::OrgPolicy,
) -> Result<(), RoomError>
where
    G: SpeechGate + 'static,
{
    run(url, token, cfg, gate, move |u| {
        let p = pipeline.clone();
        let pol = policy.clone();
        async move { p.handle(u, &pol).await; }
    })
    .await
}
