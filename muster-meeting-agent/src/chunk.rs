//! 按说话人缓冲音频,切成"一句话"再送去转写。
//!
//! ## 为什么不按固定窗口切
//!
//! 固定窗口会把词切成两半,两边都转错。这里按**静音间隔**断句:说话 → 静音
//! 持续 `silence_ms` ⇒ 认为一句说完了。
//!
//! 但静音断句不能是唯一出口:嘈杂环境里能量闸会一直判成"在说话",于是
//! 永远等不到静音。所以还有 `max_utterance_ms` 硬切兜底——**宁可切断一句,
//! 也不能无限缓冲**(内存会涨,而且转写延迟会大到没法用)。
//!
//! ## 为什么丢弃过短的片段
//!
//! 咳嗽、鼠标点击、"嗯"能过能量闸但转写没有意义,而每次转写都是一次模型调用。
//! `min_utterance_ms` 把它们挡掉。

use std::collections::HashMap;

use crate::gate::SpeechGate;
use crate::SAMPLE_RATE;

/// 裁尾时保留的余量(100ms)。贴着最后一个语音帧切会把词尾吃掉。
const PAD_SAMPLES: usize = (SAMPLE_RATE as usize) / 10;

/// 切好的一句话。`speaker` 是 Muster 账号 id(见 crate 文档的设计决策 2)。
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    pub speaker: String,
    /// 16 kHz 单声道 PCM。
    pub pcm: Vec<i16>,
    /// 这句话开始的时刻(由调用方提供的单调毫秒时钟)。
    pub started_ms: u64,
    pub duration_ms: u32,
}

#[derive(Debug, Clone)]
pub struct ChunkConfig {
    /// 静音持续多久算一句说完。太短会把句中停顿切开,太长会让转写迟迟不出。
    pub silence_ms: u32,
    /// 一句最长多久,超了硬切。嘈杂环境等不到静音时的兜底。
    pub max_utterance_ms: u32,
    /// 短于此长度的片段直接丢弃(咳嗽、点击声)。
    pub min_utterance_ms: u32,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self { silence_ms: 700, max_utterance_ms: 20_000, min_utterance_ms: 400 }
    }
}

#[derive(Default)]
struct SpeakerState {
    buf: Vec<i16>,
    started_ms: u64,
    /// 连续静音已累计多少毫秒(只在 buf 非空时有意义)。
    silence_ms: u32,
    /// 缓冲里**真正有语音**的时长。判"够不够长"用它,不用 buf 总长——
    /// 否则 50ms 咳嗽 + 700ms 静音会算成 750ms 的"一句话"。
    speech_ms: u32,
    /// 最后一帧语音在 buf 中的结束下标。发送前按它裁掉尾部静音:
    /// 把 700ms 静音原样送给 whisper,正是幻觉的温床。
    speech_end: usize,
}

/// 多说话人音频累加器。**每个说话人独立缓冲**——两人同时说话时各转各的,
/// 纪要里会出现两条时间重叠的句子,这是如实反映,不是缺陷。
pub struct Accumulator<G: SpeechGate> {
    cfg: ChunkConfig,
    gate: G,
    speakers: HashMap<String, SpeakerState>,
}

impl<G: SpeechGate> Accumulator<G> {
    pub fn new(cfg: ChunkConfig, gate: G) -> Self {
        Self { cfg, gate, speakers: HashMap::new() }
    }

    fn ms_of(samples: usize) -> u32 {
        (samples as u64 * 1000 / SAMPLE_RATE as u64) as u32
    }

    /// 喂一帧。`now_ms` 是这一帧**开始**的时刻。返回本帧触发的完整句子
    /// (通常是 0 或 1 句)。
    pub fn push(&mut self, speaker: &str, pcm: &[i16], now_ms: u64) -> Option<Utterance> {
        let is_speech = self.gate.is_speech(pcm);
        let frame_ms = Self::ms_of(pcm.len());

        // 没开口就别建状态:一个开了很久的会里,一直没说话的人不该占内存
        if !is_speech && !self.speakers.contains_key(speaker) {
            return None;
        }
        let st = self.speakers.entry(speaker.to_string()).or_default();
        if st.buf.is_empty() {
            if !is_speech {
                return None;
            }
            st.started_ms = now_ms;
        }

        st.buf.extend_from_slice(pcm);
        if is_speech {
            st.silence_ms = 0;
            st.speech_ms += frame_ms;
            st.speech_end = st.buf.len();
        } else {
            st.silence_ms += frame_ms;
        }

        let ended_by_silence = st.silence_ms >= self.cfg.silence_ms;
        let ended_by_cap = Self::ms_of(st.buf.len()) >= self.cfg.max_utterance_ms;
        if !ended_by_silence && !ended_by_cap {
            return None;
        }
        Self::take(st, self.cfg.min_utterance_ms, speaker)
    }

    /// 取出一句并复位。裁掉尾部静音,只留一点余量——留一点是因为
    /// 贴着最后一个语音帧切会把词尾吃掉。
    fn take(st: &mut SpeakerState, min_ms: u32, speaker: &str) -> Option<Utterance> {
        let started_ms = st.started_ms;
        let speech_ms = st.speech_ms;
        let keep = (st.speech_end + PAD_SAMPLES).min(st.buf.len());
        let mut pcm = std::mem::take(&mut st.buf);
        pcm.truncate(keep);

        st.silence_ms = 0;
        st.speech_ms = 0;
        st.speech_end = 0;

        // 够不够长看**语音时长**,不看缓冲总长——否则咳嗽 + 静音会蒙混过关
        if speech_ms < min_ms {
            return None;
        }
        let duration_ms = Self::ms_of(pcm.len());
        Some(Utterance { speaker: speaker.to_string(), pcm, started_ms, duration_ms })
    }

    /// 会议结束/参会者离开时把残留的半句吐出来。
    /// **不吐等于丢掉最后一句**——而最后一句往往是结论。
    pub fn flush(&mut self, speaker: &str) -> Option<Utterance> {
        let min = self.cfg.min_utterance_ms;
        let st = self.speakers.get_mut(speaker)?;
        if st.buf.is_empty() {
            return None;
        }
        Self::take(st, min, speaker)
    }

    /// 参会者离开:吐出残留并清掉状态,免得房间开久了状态越攒越多。
    pub fn remove(&mut self, speaker: &str) -> Option<Utterance> {
        let out = self.flush(speaker);
        self.speakers.remove(speaker);
        out
    }

    pub fn tracked_speakers(&self) -> usize {
        self.speakers.len()
    }

    /// 当前有缓冲状态的说话人。会议结束时用它逐个 flush——**一句都不能丢**。
    pub fn speakers(&self) -> Vec<String> {
        self.speakers.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::EnergyGate;

    const FRAME: usize = 160; // 10ms @16k

    fn loud() -> Vec<i16> {
        (0..FRAME).map(|i| if i % 2 == 0 { 5000 } else { -5000 }).collect()
    }
    fn quiet() -> Vec<i16> {
        vec![0i16; FRAME]
    }

    fn acc() -> Accumulator<EnergyGate> {
        Accumulator::new(ChunkConfig::default(), EnergyGate::default())
    }

    /// 纯静音永远不产生句子——**这条最要紧**:whisper 在静音上会幻觉出
    /// "谢谢观看"之类的句子,送进去等于往纪要里掺假话。
    #[test]
    fn silence_never_produces_an_utterance() {
        let mut a = acc();
        for i in 0..500 {
            assert!(a.push("alice", &quiet(), i * 10).is_none());
        }
        assert_eq!(a.tracked_speakers(), 0, "静音不该占用任何缓冲状态");
    }

    /// 说话 → 静音 ⇒ 断出一句,时长与起始时刻都对。
    #[test]
    fn speech_then_silence_emits_one_utterance() {
        let mut a = acc();
        let mut t = 1_000u64;
        let mut out = None;
        for _ in 0..100 {
            // 1 秒说话
            assert!(a.push("alice", &loud(), t).is_none());
            t += 10;
        }
        for _ in 0..100 {
            // 静音,应在 silence_ms(700ms)处断句
            if let Some(u) = a.push("alice", &quiet(), t) {
                out = Some(u);
                break;
            }
            t += 10;
        }
        let u = out.expect("说完停顿应当断出一句");
        assert_eq!(u.speaker, "alice");
        assert_eq!(u.started_ms, 1_000, "起始时刻应是开口那一刻,不是缓冲创建时刻");
        // **尾部静音被裁掉**:1s 语音 + 700ms 静音,送出去的应当是
        // 1s 语音 + 100ms 余量,而不是 1.7s。把 700ms 静音原样喂给 whisper
        // 正是幻觉的温床。
        assert!(
            (1_050..=1_150).contains(&u.duration_ms),
            "应裁到 1s 语音 + 100ms 余量,实际 {}ms",
            u.duration_ms
        );
    }

    /// 裁尾必须**留一点余量**——贴着最后一个语音帧切会把词尾吃掉。
    #[test]
    fn trimming_keeps_a_little_padding_after_the_last_speech() {
        let mut a = acc();
        let mut t = 0u64;
        for _ in 0..60 {
            a.push("alice", &loud(), t);
            t += 10;
        }
        let mut got = None;
        for _ in 0..100 {
            if let Some(u) = a.push("alice", &quiet(), t) {
                got = Some(u);
                break;
            }
            t += 10;
        }
        let u = got.unwrap();
        assert!(u.duration_ms > 600, "不能贴着语音切:{}ms", u.duration_ms);
        assert!(u.duration_ms < 900, "余量也不能留成整段静音:{}ms", u.duration_ms);
    }

    /// 一直在说(嘈杂环境等不到静音)⇒ 硬切兜底,不无限缓冲。
    #[test]
    fn continuous_speech_is_force_cut_at_the_cap() {
        let cfg = ChunkConfig { max_utterance_ms: 2_000, ..Default::default() };
        let mut a = Accumulator::new(cfg, EnergyGate::default());
        let mut cuts = 0;
        for i in 0..600 {
            if a.push("bob", &loud(), i * 10).is_some() {
                cuts += 1;
            }
        }
        assert!(cuts >= 2, "6 秒连续说话、2 秒上限,应切出多句,实际 {cuts}");
    }

    /// 咳嗽/点击这种过短片段丢掉,不浪费一次模型调用。
    #[test]
    fn blips_shorter_than_the_floor_are_dropped() {
        let mut a = acc();
        let mut t = 0u64;
        for _ in 0..5 {
            // 50ms 响声
            a.push("carol", &loud(), t);
            t += 10;
        }
        for _ in 0..100 {
            if let Some(u) = a.push("carol", &quiet(), t) {
                panic!("50ms 的响声不该转写:{}ms", u.duration_ms);
            }
            t += 10;
        }
    }

    /// **多人同时说话各归各的**——这是 A2 的核心要求。
    /// 说话人归属靠的是每人一条轨,不做声纹分离。
    #[test]
    fn speakers_are_buffered_and_attributed_independently() {
        let mut a = acc();
        let mut t = 0u64;
        // alice 与 bob 交替喂帧(模拟两条轨并行)
        for _ in 0..100 {
            a.push("alice", &loud(), t);
            a.push("bob", &loud(), t);
            t += 10;
        }
        assert_eq!(a.tracked_speakers(), 2);

        // bob 先停,alice 继续说
        let mut bob_out = None;
        for _ in 0..100 {
            a.push("alice", &loud(), t);
            if let Some(u) = a.push("bob", &quiet(), t) {
                bob_out = Some(u);
                break;
            }
            t += 10;
        }
        let b = bob_out.expect("bob 停下后应断句");
        assert_eq!(b.speaker, "bob");

        // alice 的缓冲不受影响,仍在累积
        let a_left = a.flush("alice").expect("alice 还有未断的半句");
        assert_eq!(a_left.speaker, "alice");
        assert!(a_left.duration_ms > 1_000, "alice 一直在说,残留应当不短");
    }

    /// 会议结束时残留的半句要吐出来——**最后一句往往是结论**。
    #[test]
    fn flush_yields_the_trailing_half_sentence() {
        let mut a = acc();
        for i in 0..80 {
            a.push("dave", &loud(), i * 10);
        }
        let u = a.flush("dave").expect("残留应当被吐出");
        assert_eq!(u.speaker, "dave");
        assert!(u.duration_ms >= 700);
        assert!(a.flush("dave").is_none(), "吐过一次就没了,不得重复产出");
    }

    /// 参会者离开后状态清干净,免得房间开久了越攒越多。
    #[test]
    fn removing_a_speaker_clears_state() {
        let mut a = acc();
        for i in 0..80 {
            a.push("eve", &loud(), i * 10);
        }
        assert_eq!(a.tracked_speakers(), 1);
        assert!(a.remove("eve").is_some(), "离开时也要吐残留");
        assert_eq!(a.tracked_speakers(), 0);
        assert!(a.remove("eve").is_none());
    }
}
