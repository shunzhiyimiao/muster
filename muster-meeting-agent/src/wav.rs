//! PCM → WAV 封装。
//!
//! i16 PCM 正是 WAV 的原生采样格式,所以"封装"只是在前面加 44 字节头——
//! 不需要任何编解码库。转写后端(OpenAI 兼容 `/audio/transcriptions`)靠
//! 文件头识别容器,裸 PCM 送过去会被当成损坏文件。

use crate::{CHANNELS, SAMPLE_RATE};

/// 把 16 kHz 单声道 PCM 封成 WAV。
pub fn pcm16_to_wav(pcm: &[i16]) -> Vec<u8> {
    pcm16_to_wav_with(pcm, SAMPLE_RATE, CHANNELS)
}

pub fn pcm16_to_wav_with(pcm: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bits = 16u16;
    let data_len = (pcm.len() * 2) as u32;
    let byte_rate = sample_rate * channels as u32 * (bits / 8) as u32;
    let block_align = channels * (bits / 8);

    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    // RIFF chunk size = 文件总长 - 8
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk 长度
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    // WAV 是小端;i16 逐个写,不能直接 transmute 切片(大端机器上会静默出错)
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }
    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }

    #[test]
    fn header_declares_the_right_format() {
        let pcm = vec![0i16; 16_000]; // 1 秒
        let w = pcm16_to_wav(&pcm);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(u16_at(&w, 20), 1, "必须是 PCM 格式");
        assert_eq!(u16_at(&w, 22), 1, "单声道");
        assert_eq!(u32_at(&w, 24), 16_000, "采样率");
        assert_eq!(u32_at(&w, 28), 32_000, "byte rate = 16000 * 1 * 2");
        assert_eq!(u16_at(&w, 32), 2, "block align");
        assert_eq!(u16_at(&w, 34), 16, "位深");
        assert_eq!(&w[36..40], b"data");
        assert_eq!(u32_at(&w, 40), 32_000, "data 长度 = 样本数 * 2");
        assert_eq!(w.len(), 44 + 32_000, "总长 = 头 + 数据");
        assert_eq!(u32_at(&w, 4), (w.len() - 8) as u32, "RIFF 长度 = 总长 - 8");
    }

    /// 样本按**小端**写。直接 transmute 切片在大端机器上会静默产出噪音,
    /// 这条锁住逐字节写入。
    #[test]
    fn samples_are_written_little_endian() {
        let w = pcm16_to_wav(&[0x0102i16, -2]);
        assert_eq!(&w[44..48], &[0x02, 0x01, 0xFE, 0xFF]);
    }

    /// 空音频也要产出合法的头(长度为 0),而不是空字节流——
    /// 后端拿到空文件会报解析错,拿到合法空 WAV 会正常返回空转写。
    #[test]
    fn empty_pcm_still_yields_a_valid_header() {
        let w = pcm16_to_wav(&[]);
        assert_eq!(w.len(), 44);
        assert_eq!(u32_at(&w, 40), 0);
        assert_eq!(u32_at(&w, 4), 36);
    }
}
