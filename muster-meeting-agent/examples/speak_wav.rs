//! 把一个 WAV 当作"某人在说话"推进 LiveKit 房间。**测试夹具,不是产品功能。**
//!
//! 为什么要它:验证会议链路总得有人说话,而"人对着麦克风说"不可脚本化、
//! 不可重复。用它推一段固定音频进去,整条链就能**每次跑出同样的结果**——
//! 从测试夹具变成回归测试。
//!
//! ```bash
//! cargo run -p muster-meeting-agent --features livekit --example speak_wav -- \
//!   ws://localhost:7880 <入会令牌> speech.wav
//! ```
//!
//! 按真实速率推送(不是一股脑灌进去):会议 Agent 的切句逻辑依赖静音间隔,
//! 灌太快会让整段音频挤在一个窗口里,验证出来的东西就不作数了。

use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::prelude::{AudioFrame, RtcAudioSource};

const RATE: u32 = 16_000;
const CHANNELS: u32 = 1;
/// 每次推 10ms,与 WebRTC 的原生帧长一致。
const FRAME: usize = (RATE as usize) / 100;

fn read_wav_i16(bytes: &[u8]) -> Result<Vec<i16>, String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" {
        return Err("不是 WAV".into());
    }
    let mut i = 12usize;
    while i + 8 <= bytes.len() {
        let len =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        if &bytes[i..i + 4] == b"data" {
            let end = (i + 8 + len).min(bytes.len());
            return Ok(bytes[i + 8..end]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect());
        }
        i += 8 + len + (len & 1);
    }
    Err("找不到 data 块".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("用法:speak_wav <livekit_url> <token> <file.wav>");
        std::process::exit(2);
    }
    let pcm = read_wav_i16(&std::fs::read(&a[2])?)?;
    println!("音频 {:.1}s", pcm.len() as f32 / RATE as f32);

    let (room, _events) = Room::connect(&a[0], &a[1], RoomOptions::default()).await?;
    println!("✓ 已入房间 {} · 身份 {}", room.name(), room.local_participant().identity());

    let source = NativeAudioSource::new(AudioSourceOptions::default(), RATE, CHANNELS, 1_000);
    let track = LocalAudioTrack::create_audio_track("speech", RtcAudioSource::Native(source.clone()));
    room.local_participant()
        .publish_track(LocalTrack::Audio(track), TrackPublishOptions::default())
        .await?;
    println!("✓ 已发布音轨,开始推流…");

    // 先推 1 秒静音:订阅方需要一点时间把轨挂上,一上来就说话会丢开头
    let silence = vec![0i16; FRAME];
    for _ in 0..100 {
        source
            .capture_frame(&AudioFrame {
                data: silence.as_slice().into(),
                sample_rate: RATE,
                num_channels: CHANNELS,
                samples_per_channel: FRAME as u32,
            })
            .await?;
    }

    for (n, chunk) in pcm.chunks(FRAME).enumerate() {
        let mut buf = chunk.to_vec();
        buf.resize(FRAME, 0); // 末帧补齐
        source
            .capture_frame(&AudioFrame {
                data: buf.as_slice().into(),
                sample_rate: RATE,
                num_channels: CHANNELS,
                samples_per_channel: FRAME as u32,
            })
            .await?;
        if n % 100 == 0 {
            println!("  已推 {}s", n / 100);
        }
    }

    // 末尾补静音:切句靠静音间隔,不补的话最后一句永远等不到断点
    println!("推完,补 2s 静音让 Agent 断句…");
    for _ in 0..200 {
        source
            .capture_frame(&AudioFrame {
                data: silence.as_slice().into(),
                sample_rate: RATE,
                num_channels: CHANNELS,
                samples_per_channel: FRAME as u32,
            })
            .await?;
    }
    println!("✓ 完成");
    Ok(())
}
