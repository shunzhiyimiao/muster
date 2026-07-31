//! 拿一个 WAV 文件走完整条转写链:SpeechRouter 决策 → provider 转写 → 打印。
//!
//! 用来回答"**转写这一端到底通不通**",不必先把 LiveKit 和会议跑起来。
//! 排查顺序上它应该排在最前面:整条链里最可能出问题的是后端接口对不对,
//! 而那与音视频无关。
//!
//! ```bash
//! # 本地 whisper(正常路径)
//! cargo run -p muster-meeting-agent --example transcribe_file -- speech.wav
//!
//! # 演习模式:验证云端 STT 会被 fail-closed 拒掉
//! MUSTER_DRILL=1 cargo run -p muster-meeting-agent --example transcribe_file -- speech.wav
//! ```

use std::sync::Arc;

use muster_meeting_agent::{Pipeline, TranscriptSink, Utterance};
use muster_provider::{Locality, SpeechCompatProvider, SpeechConfig, SpeechProvider};
use muster_route::{OrgPolicy, Sensitivity, SpeechRouter};

struct Printer;

#[async_trait::async_trait]
impl TranscriptSink for Printer {
    async fn on_text(&self, u: &Utterance, text: &str, egress: u64) {
        println!("✓ [{}] {text}", u.speaker);
        println!("  外发 {egress} 字节({})", if egress == 0 { "本地落点" } else { "⚠ 出了本机" });
    }
    async fn on_refused(&self, u: &Utterance, reason: &str) {
        println!("⛔ [{}] 未能转写:{reason}", u.speaker);
    }
}

/// 读 WAV 取样本。只认 16-bit PCM;别的格式如实报错,不猜。
fn read_wav_i16(bytes: &[u8]) -> Result<Vec<i16>, String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("不是 WAV 文件".into());
    }
    let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
    if bits != 16 {
        return Err(format!("只支持 16-bit PCM,该文件是 {bits}-bit"));
    }
    let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let ch = u16::from_le_bytes([bytes[22], bytes[23]]);
    if rate != 16_000 || ch != 1 {
        eprintln!("⚠️ 该文件是 {rate}Hz/{ch} 声道,不是 16k 单声道——转写质量可能受影响");
    }
    // 找 data 块(fmt 块长度可能不是 16,不能写死 44)
    let mut i = 12usize;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        if id == b"data" {
            let end = (i + 8 + len).min(bytes.len());
            return Ok(bytes[i + 8..end]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect());
        }
        i += 8 + len + (len & 1);
    }
    Err("WAV 里找不到 data 块".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("用法:transcribe_file <file.wav>")?;
    let bytes = std::fs::read(&path)?;
    let pcm = read_wav_i16(&bytes)?;
    let secs = pcm.len() as f32 / 16_000.0;
    println!("读入 {path}:{} 样本 · {secs:.1}s\n", pcm.len());

    let base = std::env::var("MUSTER_STT_URL").unwrap_or_else(|_| "http://localhost:9000/v1".into());
    // 是否把它当云端落点(用来验证演习封锁真的会拦下来)
    let as_cloud = std::env::var("MUSTER_STT_CLOUD").is_ok();
    let mut cfg = SpeechConfig::local_whisper(&base);
    if let Ok(m) = std::env::var("MUSTER_STT_MODEL") {
        cfg.model = m;
    }
    if as_cloud {
        cfg.locality = Locality::Cloud;
        cfg.display_name = "stt·假装云端".into();
    }
    let p: Arc<dyn SpeechProvider> = Arc::new(SpeechCompatProvider::new("stt", cfg)?);
    println!("转写落点 {base}(locality={:?})", p.metadata().locality);
    match p.health_check().await {
        Ok(()) => println!("探活:通\n"),
        Err(e) => println!("探活:{e}(继续尝试转写)\n"),
    }

    let mut policy = OrgPolicy::new(Sensitivity::Internal)?;
    if std::env::var("MUSTER_DRILL").is_ok() {
        policy.set_egress_locked(true);
        println!("⚑ 演习模式:外联已切断\n");
    }

    let pipeline = Pipeline::new(Arc::new(SpeechRouter::new(vec![p])), Arc::new(Printer))
        .with_language("zh");
    let pipeline = match std::env::var("MUSTER_STT_PROMPT") {
        Ok(p) => pipeline.with_prompt(p),
        Err(_) => pipeline,
    };
    let u = Utterance {
        speaker: "测试".into(),
        pcm,
        started_ms: 0,
        duration_ms: (secs * 1000.0) as u32,
    };
    let t = std::time::Instant::now();
    pipeline.handle(u, &policy).await;
    println!("\n耗时 {:.1}s(音频 {secs:.1}s)", t.elapsed().as_secs_f32());
    Ok(())
}
