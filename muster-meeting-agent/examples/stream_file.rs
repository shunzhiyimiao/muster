//! 把一个 16k 单声道 WAV 按实时速度推进流式转写边车,打印落出来的句子。
//!
//! **验转写通不通最便宜的一步**:不用开会、不用 LiveKit、不用登录。
//!
//! ```bash
//! MUSTER_STT_URL=ws://localhost:9000/stream \
//! cargo run -p muster-meeting-agent --example stream_file -- ~/stt-eval/audio/s01_Tingting.wav
//! # --fast:不按实时速度,一口气灌完(看吞吐,不看延迟)
//! ```
//!
//! 走的是和 daemon 完全相同的 provider(`SherpaStreamProvider`)与协议,只是帧的来源换成文件。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use muster_provider::{
    SherpaStreamProvider, SpeechStreamConfig, SpeechStreamEvent, SpeechStreamHandle, SpeechStreamProvider, StreamOpen,
};

/// 最小 WAV 解析:只认 PCM 16 位。返回(采样率、声道数、样本)。
fn read_wav(bytes: &[u8]) -> Result<(u32, u16, Vec<i16>), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("不是 WAV 文件".into());
    }
    let (mut pos, mut rate, mut channels, mut bits) = (12usize, 0u32, 0u16, 0u16);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        let body = &bytes[pos + 8..(pos + 8 + size).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            if bits != 16 {
                return Err(format!("只认 16 位 PCM,这个文件是 {bits} 位"));
            }
            let pcm: Vec<i16> = body.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            return Ok((rate, channels, pcm));
        }
        pos += 8 + size + (size & 1);
    }
    Err("没有 data 块".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fast = args.iter().any(|a| a == "--fast");
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("用法:stream_file <16k 单声道 wav> [--fast]");
        std::process::exit(2);
    };
    let url = std::env::var("MUSTER_STT_URL").unwrap_or_else(|_| "ws://localhost:9000/stream".into());
    let (rate, channels, pcm) = read_wav(&std::fs::read(path)?)?;
    if rate != 16_000 || channels != 1 {
        return Err(format!(
            "要 16k 单声道,这个文件是 {rate}Hz {channels} 声道(afconvert -f WAVE -d LEI16@16000 -c 1)"
        )
        .into());
    }
    let dur = pcm.len() as f64 / 16_000.0;
    println!("{dur:.1} 秒音频 → {url}{}", if fast { "(不限速)" } else { "(实时速度)" });

    let p: Arc<dyn SpeechStreamProvider> =
        Arc::new(SherpaStreamProvider::new("sherpa", SpeechStreamConfig::local_sherpa(&url))?);
    p.health_check().await.map_err(|e| format!("边车不健康:{e}"))?;
    let open = StreamOpen { speaker: "file".into(), language: Some("zh".into()), hotwords: vec!["小七".into()] };
    let SpeechStreamHandle { mut input, mut events, wire_bytes } = p.open(open).await?;

    // 收事件的任务:边喂边打,才看得到部分结果与延迟
    let t0 = Instant::now();
    let printer = tokio::spawn(async move {
        let mut finals = Vec::new();
        while let Some(ev) = events.recv().await {
            match ev {
                SpeechStreamEvent::Partial { text } => eprintln!("      … {text}"),
                SpeechStreamEvent::Final { text, start_ms, end_ms } => {
                    println!("[{:>5.1}s] {start_ms}–{end_ms}ms  {text}", t0.elapsed().as_secs_f32());
                    finals.push(text);
                }
                SpeechStreamEvent::Dropped { ms } => eprintln!("      ⚠ 边车过载,丢了 {ms}ms"),
                SpeechStreamEvent::Error(e) => {
                    eprintln!("      ✗ {e}");
                    break;
                }
                SpeechStreamEvent::Closed => break,
            }
        }
        finals
    });

    // 20ms 一帧,和 LiveKit 送来的量级一样
    let frame = 320;
    let t_feed = Instant::now();
    for (i, chunk) in pcm.chunks(frame).enumerate() {
        input.feed(chunk).await?;
        if !fast {
            let due = t_feed + Duration::from_millis((i as u64 + 1) * 20);
            tokio::time::sleep_until(due.into()).await;
        }
    }
    let fed_in = t_feed.elapsed();
    input.finish().await?;
    let finals = printer.await?;
    let tail = t_feed.elapsed() - fed_in;
    println!(
        "\n{} 句 · 喂入 {:.1}s · 尾延迟 {:.2}s · 上线 {} 字节(本地落点外发记 0)",
        finals.len(),
        fed_in.as_secs_f32(),
        tail.as_secs_f32(),
        wire_bytes.load(Ordering::Relaxed)
    );
    println!("{}", finals.join(""));
    Ok(())
}
