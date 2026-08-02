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
/// 512 帧 ≈ 5 秒缓冲。转写已改为独立任务,这里只需吸收抖动;
/// 若仍持续丢帧,说明转写整体跟不上(该上 GPU 或降到 tiny),
/// 加大队列只会把延迟拖长,不解决问题。
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
/// `on_utterance` 每断出一句调用一次,**由本函数放到独立任务里跑**——
/// 调用方不必自己记得 spawn。
///
/// 为什么必须这样:转写要 ~1 秒、作答要几秒,而这期间主循环若在 await,
/// **就没人消费音频队列**,队列填满即丢帧;丢帧让送去转写的音频变得破碎,
/// whisper 于是开始重复吐字("音乐音乐音乐…")。单人短句时勉强跟得上,
/// 两个人一起说就塌——双人真机测试逼出来的。
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
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (room, mut events) = Room::connect(url, token, RoomOptions::default())
        .await
        .map_err(|e| RoomError::Connect(e.to_string()))?;
    tracing::info!(room = %room.name(), "会议 Agent 已入房间");

    let (tx, mut rx) = mpsc::channel::<Frame>(FRAME_QUEUE);
    let mut acc = Accumulator::new(cfg, gate);
    // 搬运任务的句柄。**散会时必须停掉**:否则它们继续往没人读的队列里塞,
    // 一边刷"转写跟不上采集,已丢帧"一边空转——真机上散会提炼那 20 秒里
    // 刷了 2000 多条警告,看着像出了大事,其实是没收摊。
    let mut pumps: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    loop {
        tokio::select! {
            // 优先消费音频帧:积压会直接变成转写延迟
            biased;

            // Ctrl-C 走**正常收尾**,不是杀进程。
            // 停会议最自然的方式就是 Ctrl-C,而直接被杀掉会丢两样东西:
            // 每个人**没断完的最后半句**,以及散会后的提炼——
            // 而最后一句往往是结论,提炼更是这场会的产出。
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("收到 Ctrl-C,正常收尾中(不要再按)…");
                break;
            }

            Some(f) = rx.recv() => {
                if let Some(u) = acc.push(&f.speaker, &f.pcm, f.ts_ms) {
                    // 丢给独立任务:主循环必须立刻回到消费音频上
                    tokio::spawn(on_utterance(u));
                }
            }

            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    RoomEvent::TrackSubscribed { track, participant, .. } => {
                        let who = participant.identity().to_string();
                        if let RemoteTrack::Audio(audio) = track {
                            tracing::info!(speaker = %who, "订阅到音轨");
                            pumps.push(spawn_pump(audio, who, tx.clone()));
                        }
                    }
                    RoomEvent::ParticipantDisconnected(p) => {
                        // 人走了要把残留的半句吐出来——**最后一句往往是结论**
                        let who = p.identity().to_string();
                        if let Some(u) = acc.remove(&who) {
                            tokio::spawn(on_utterance(u));
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

    // 先停搬运任务,再收残留:顺序反了的话,收残留期间还在往队列里塞。
    for h in pumps {
        h.abort();
    }

    // 会议结束:所有人的残留都要吐出来,一句都不能丢
    let speakers: Vec<String> = acc.speakers();
    let mut tail = Vec::new();
    for s in speakers {
        if let Some(u) = acc.remove(&s) {
            tail.push(tokio::spawn(on_utterance(u)));
        }
    }
    // 散会时**要等**残留处理完:后面紧接着就是提炼行动项,
    // 不等的话最后几句还没进纪要,提炼就看不见它们了
    for t in tail {
        let _ = t.await;
    }
    Ok(())
}

/// 一条音轨一个任务:只负责把帧丢进 channel,不做任何判断。
fn spawn_pump(
    audio: RemoteAudioTrack,
    speaker: String,
    tx: mpsc::Sender<Frame>,
) -> tokio::task::JoinHandle<()> {
    // 关键:直接要 whisper 想要的格式,SDK 负责重采样。
    // 注意签名是三参(队列深度用 SDK 默认值)——crates.io 上的 0.3.43 是四参,
    // 而我们构建的是 git 版。**读 API 要读实际构建的那个版本**,别读别的。
    let mut stream =
        NativeAudioStream::new(audio.rtc_track(), SAMPLE_RATE as i32, CHANNELS as i32);
    tokio::spawn(async move {
        let mut dropped = 0u64;
        // 电平采样:排查"说了没反应"时,这是唯一能分清
        // "音频没进来" / "进来了但太轻被闸挡住" / "闸放行了但转写空"的东西。
        // debug 级,平时不打;RUST_LOG=muster_meeting_agent=debug 打开。
        let (mut n_frames, mut sum_sq, mut peak) = (0u64, 0f64, 0i32);
        while let Some(frame) = stream.next().await {
            for s in frame.data.iter() {
                sum_sq += (*s as f64) * (*s as f64);
                peak = peak.max((*s as i32).abs());
            }
            n_frames += 1;
            if n_frames % 300 == 0 {
                let rms = (sum_sq / (n_frames as f64 * frame.data.len() as f64)).sqrt();
                tracing::debug!(
                    speaker = %speaker, rms = rms as i32, peak,
                    "近 3 秒电平(能量闸阈值 300;远低于它就是麦克风没拾到声)"
                );
                sum_sq = 0.0;
                n_frames = 0;
                peak = 0;
            }
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
    })
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
