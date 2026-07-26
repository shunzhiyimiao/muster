//! 单调 ULID 生成器。
//!
//! 选 ULID 而非 UUIDv4 的原因:**字典序 = 时间序**,`ORDER BY event_id` 即
//! 时间线,哈希链与游标分页都不需要额外排序列。同毫秒内随机段 +1 保证严格
//! 单调(单节点足够;多节点合并是 v1.x 的明确不做项)。

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub struct UlidGen {
    state: Mutex<(u64, u128)>, // (last_ts_ms, last_rand80)
}

impl Default for UlidGen {
    fn default() -> Self {
        Self { state: Mutex::new((0, 0)) }
    }
}

impl UlidGen {
    /// 生成一个 ULID;`ts_ms` 可由调用方注入(测试确定性),`None` 取系统时钟。
    pub fn next(&self, ts_ms: Option<u64>) -> String {
        let now = ts_ms.unwrap_or_else(now_ms);
        let mut st = self.state.lock().expect("ulid lock poisoned");
        // 先钳时钟(回拨沿用 last_ts,审计层不允许卡死),再决定随机段策略:
        // 同毫秒 → 上一随机段 +1(严格单调);新毫秒 → 全新随机段。
        let ts = now.max(st.0);
        let rand80: u128 = if ts == st.0 {
            let bumped = st.1.wrapping_add(1) & MASK80;
            if bumped == 0 {
                // 2^80 次同毫秒写入后的理论回绕:推进 1ms 换新随机段。
                *st = (ts + 1, rand::random::<u128>() & MASK80);
                return encode((u128::from(st.0) << 80) | st.1);
            }
            bumped
        } else {
            rand::random::<u128>() & MASK80
        };
        *st = (ts, rand80);
        encode((u128::from(ts) << 80) | rand80)
    }
}

const MASK80: u128 = (1u128 << 80) - 1;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn encode(v: u128) -> String {
    let mut out = [0u8; 26];
    for i in 0..26 {
        let shift = 5 * (25 - i);
        out[i] = ALPHABET[((v >> shift) & 0x1F) as usize];
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexicographic_order_follows_time() {
        let g = UlidGen::default();
        let a = g.next(Some(1_000));
        let b = g.next(Some(2_000));
        assert!(a < b);
        assert_eq!(a.len(), 26);
    }

    #[test]
    fn same_millisecond_is_strictly_monotonic() {
        let g = UlidGen::default();
        let mut prev = g.next(Some(5_000));
        for _ in 0..100 {
            let next = g.next(Some(5_000));
            assert!(next > prev, "{next} !> {prev}");
            prev = next;
        }
    }

    #[test]
    fn clock_rollback_stays_monotonic() {
        let g = UlidGen::default();
        let a = g.next(Some(9_000));
        let b = g.next(Some(8_000)); // 回拨
        assert!(b > a);
    }
}
