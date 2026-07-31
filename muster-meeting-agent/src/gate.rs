//! 说话/静音判定。
//!
//! 这道闸的意义不只是省算力:**whisper 在纯静音上会产生幻觉**——凭空转出
//! "谢谢观看"之类的句子。把静音送进去,等于往会议纪要里掺假话。所以宁可
//! 漏掉一点边缘的轻声,也不能把静音当语音送出去。
//!
//! A2 用能量阈值(无依赖、够用);A3 换 Silero VAD。两者实现同一个 trait,
//! 上层不动。

/// 一帧是不是"有人在说话"。实现要**便宜**:它在每一帧上被调用。
pub trait SpeechGate: Send {
    fn is_speech(&mut self, pcm: &[i16]) -> bool;
}

/// 能量(RMS)阈值。
///
/// 诚实边界:它区分不了"人声"和"键盘敲击/空调噪声"。嘈杂环境下会一直判成
/// 在说话,此时只能靠 [`crate::ChunkConfig::max_utterance_ms`] 硬切兜底。
/// 这是 A2 的临时方案,不是终点。
pub struct EnergyGate {
    /// RMS 阈值(i16 量纲)。安静房间的底噪 RMS 通常在个位数到几十,
    /// 正常说话在几百以上;默认取 300 是偏保守的——**宁可漏,不可掺**。
    threshold: f32,
}

impl EnergyGate {
    pub fn new(threshold: f32) -> Self {
        Self { threshold }
    }
}

impl Default for EnergyGate {
    fn default() -> Self {
        Self::new(300.0)
    }
}

impl SpeechGate for EnergyGate {
    fn is_speech(&mut self, pcm: &[i16]) -> bool {
        if pcm.is_empty() {
            return false;
        }
        // 用 f64 累加:i16 平方和在长帧上会溢出 i32/f32 的有效精度
        let sum: f64 = pcm.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        let rms = (sum / pcm.len() as f64).sqrt();
        rms as f32 >= self.threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, amp: i16) -> Vec<i16> {
        (0..n).map(|i| if i % 2 == 0 { amp } else { -amp }).collect()
    }

    /// 静音判否——这条最要紧:放过去就等于往纪要里掺 whisper 的幻觉。
    #[test]
    fn silence_is_not_speech() {
        let mut g = EnergyGate::default();
        assert!(!g.is_speech(&vec![0i16; 320]));
        assert!(!g.is_speech(&[]), "空帧不得判成说话");
        assert!(!g.is_speech(&tone(320, 20)), "底噪级别不算说话");
    }

    #[test]
    fn loud_audio_is_speech() {
        let mut g = EnergyGate::default();
        assert!(g.is_speech(&tone(320, 3000)));
    }

    /// 长帧不得因累加溢出而误判。i16 平方和在几万个样本上就会超出
    /// f32 的有效精度,这里显式验一遍。
    #[test]
    fn long_frames_do_not_overflow_the_accumulator() {
        let mut g = EnergyGate::default();
        let long_loud = tone(16_000 * 10, 8000); // 10 秒满响
        assert!(g.is_speech(&long_loud));
        let long_silent = vec![0i16; 16_000 * 10];
        assert!(!g.is_speech(&long_silent));
    }

    /// 阈值可调,且判定就是 RMS 与阈值的比较——没有隐藏的滞回或状态。
    #[test]
    fn threshold_is_honoured() {
        let quiet = tone(320, 100);
        assert!(!EnergyGate::new(300.0).is_speech(&quiet));
        assert!(EnergyGate::new(50.0).is_speech(&quiet));
    }
}
