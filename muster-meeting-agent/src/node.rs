//! 节点身份与熔断(docs/节点与工牌-公用私有值班.md,实施顺序 1)。
//!
//! ## 节点不用口令
//!
//! 第一次运行时本地生成一对 ed25519 密钥,私钥留在 `$MUSTER_NODE_HOME/node.key`(0600),
//! 只把公钥交给服务端入网;此后每次会话都是服务端出 challenge、这里签名换一张短期 JWT。
//! 服务器上落着的是**节点的密钥**,不是任何人的口令;吊销一个节点不影响任何账号。
//!
//! ## 熔断不是退避
//!
//! 退避(daemon 里已有)解决"连不上就别每分钟撞一次";熔断解决"同一类错误反复出现
//! 就该停下来等人看"。同一场会连续 [`BREAKER_THRESHOLD`] 次以同一类错误失败 ⇒ 停止认领
//! 这场会,心跳里上报;**解除靠人**:再点一次「请 Agent」,服务端的 `agent_requested_ms`
//! 变新,这里据此清掉。不同类错误交替出现不算"同一类连续",计数从头来。
//!
//! 这一层不碰网络,全部可测;HTTP 往返在 daemon 里。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

/// 同一类错误连续几次触发熔断。
pub const BREAKER_THRESHOLD: u32 = 3;

// ---------------------------------------------------------------- 密钥

/// 节点私钥所在目录的缺省值(`MUSTER_NODE_HOME` 可覆盖)。
pub fn default_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".muster").join("node")
}

/// 节点的本地身份:私钥 + 入网后服务端给的 id。
#[derive(Debug)]
pub struct NodeKey {
    key: SigningKey,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeRecord {
    pub node_id: String,
    pub server: String,
    pub name: String,
}

impl NodeKey {
    /// 读 `node.key`;没有就生成一把存下(0600)。
    pub fn load_or_create(home: &Path) -> std::io::Result<(Self, bool)> {
        std::fs::create_dir_all(home)?;
        let path = home.join("node.key");
        if path.exists() {
            let hex = std::fs::read_to_string(&path)?;
            let seed = hex_decode(hex.trim()).and_then(|v| <[u8; 32]>::try_from(v).ok()).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "node.key 不是 64 位十六进制的 32 字节种子")
            })?;
            return Ok((Self { key: SigningKey::from_bytes(&seed) }, false));
        }
        let key = SigningKey::generate(&mut rand_core::OsRng);
        write_private(&path, &hex_encode(&key.to_bytes()))?;
        Ok((Self { key }, true))
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { key: SigningKey::from_bytes(&seed) }
    }

    pub fn pubkey_hex(&self) -> String {
        hex_encode(self.key.verifying_key().as_bytes())
    }

    /// 签服务端的 challenge。被签内容的格式与服务端 `node::challenge_message` 一致。
    pub fn sign_challenge(&self, node_id: &str, nonce_hex: &str) -> String {
        let msg = format!("muster-node-session:v1:{node_id}:{nonce_hex}");
        hex_encode(&self.key.sign(msg.as_bytes()).to_bytes())
    }
}

/// 私钥文件:只有本用户可读写。
fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn load_record(home: &Path) -> Option<NodeRecord> {
    let s = std::fs::read_to_string(home.join("node.json")).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn save_record(home: &Path, r: &NodeRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    std::fs::write(home.join("node.json"), serde_json::to_string_pretty(r).unwrap_or_default())
}

pub fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

// ---------------------------------------------------------------- 错误分类

/// 把一句错误文本归到一类。熔断按类计数:同类连续才算"卡住了"。
pub fn classify_error(err: &str) -> &'static str {
    let e = err.to_lowercase();
    if e.contains("入会被拒") || e.contains("403") || e.contains("401") {
        "join_refused"
    } else if e.contains("连接房间") || e.contains("signal") || e.contains("transport") || e.contains("ice") {
        "livekit_connect"
    } else if e.contains("转写") || e.contains("transcri") || e.contains("stt") || e.contains("whisper") || e.contains("9000") {
        "stt"
    } else if e.contains("connect") || e.contains("连不上") || e.contains("timed out") || e.contains("超时") {
        "server_unreachable"
    } else {
        "other"
    }
}

// ---------------------------------------------------------------- 熔断

#[derive(Debug, Clone, PartialEq)]
pub struct BreakerState {
    pub error_kind: String,
    pub failures: u32,
    /// 跳闸了。`requested_ms` 是跳闸时看到的「请 Agent」时刻,比它新的请求才算人来重试。
    pub tripped: bool,
    pub requested_ms: Option<i64>,
}

#[derive(Debug, Default)]
pub struct Breakers {
    map: HashMap<String, BreakerState>,
}

impl Breakers {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一次失败。返回这次是否**刚刚**跳闸(调用方据此停掉任务、打日志)。
    pub fn record_failure(&mut self, meeting: &str, kind: &str, requested_ms: Option<i64>) -> bool {
        let st = self.map.entry(meeting.to_string()).or_insert_with(|| BreakerState {
            error_kind: kind.to_string(),
            failures: 0,
            tripped: false,
            requested_ms,
        });
        if st.tripped {
            return false;
        }
        if st.error_kind != kind {
            // 换了一类错误:不是"同一类连续",从头数
            st.error_kind = kind.to_string();
            st.failures = 0;
        }
        st.failures += 1;
        st.requested_ms = requested_ms;
        if st.failures >= BREAKER_THRESHOLD {
            st.tripped = true;
            return true;
        }
        false
    }

    /// 成功一次:这场会的计数归零(跳闸的不受影响,跳闸只能由人解除)。
    pub fn record_success(&mut self, meeting: &str) {
        if let Some(st) = self.map.get_mut(meeting) {
            if !st.tripped {
                st.failures = 0;
            }
        }
    }

    /// 这场会现在是不是被熔断着。传入服务端此刻的「请 Agent」时刻:
    /// 比跳闸时新 ⇒ 人重试了,解除并返回 false。
    pub fn is_tripped(&mut self, meeting: &str, requested_ms: Option<i64>) -> bool {
        let Some(st) = self.map.get(meeting) else { return false };
        if !st.tripped {
            return false;
        }
        if requested_ms > st.requested_ms {
            self.map.remove(meeting);
            return false;
        }
        true
    }

    /// 会议不再被请(散会 / 请离):忘掉它。
    pub fn forget(&mut self, meeting: &str) {
        self.map.remove(meeting);
    }

    /// 心跳里要上报的:所有跳闸中的会。
    pub fn reports(&self) -> Vec<(String, String, u32)> {
        self.map
            .iter()
            .filter(|(_, s)| s.tripped)
            .map(|(m, s)| (m.clone(), s.error_kind.clone(), s.failures))
            .collect()
    }

    pub fn failures(&self, meeting: &str) -> u32 {
        self.map.get(meeting).map(|s| s.failures).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_signs_the_same_message_shape_as_the_server() {
        let k = NodeKey::from_seed(*b"muster-node-test-seed-0123456789");
        assert_eq!(k.pubkey_hex().len(), 64);
        let sig = k.sign_challenge("0b7e2c1a-0000-4000-8000-000000000001", "abcd");
        assert_eq!(sig.len(), 128, "ed25519 签名 64 字节");
        // 同一把钥匙对同一内容的签名是确定的;换 nonce 就变
        assert_eq!(sig, k.sign_challenge("0b7e2c1a-0000-4000-8000-000000000001", "abcd"));
        assert_ne!(sig, k.sign_challenge("0b7e2c1a-0000-4000-8000-000000000001", "abce"));
    }

    #[test]
    fn key_file_is_created_once_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("muster-node-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (k1, created) = NodeKey::load_or_create(&dir).unwrap();
        assert!(created);
        let (k2, created2) = NodeKey::load_or_create(&dir).unwrap();
        assert!(!created2, "第二次是读出来的");
        assert_eq!(k1.pubkey_hex(), k2.pubkey_hex());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("node.key")).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "私钥只有本用户可读");
        }
        save_record(&dir, &NodeRecord { node_id: "n1".into(), server: "http://s".into(), name: "x".into() }).unwrap();
        assert_eq!(load_record(&dir).unwrap().node_id, "n1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_are_classified_by_kind() {
        assert_eq!(classify_error("连接房间失败:engine: signal failure: transport timed out"), "livekit_connect");
        assert_eq!(classify_error("入会被拒:\"forbidden\""), "join_refused");
        assert_eq!(classify_error("转写失败:connection refused 9000"), "stt");
        assert_eq!(classify_error("connect error: 连不上服务端"), "server_unreachable");
        assert_eq!(classify_error("something odd"), "other");
    }

    /// 同一类连续三次才跳闸;换类从头数;成功归零;人重试(请求时刻更新)才解除。
    #[test]
    fn breaker_trips_on_three_same_kind_failures_and_resets_only_by_people() {
        let mut b = Breakers::new();
        assert!(!b.record_failure("m", "stt", Some(100)));
        assert!(!b.record_failure("m", "stt", Some(100)));
        assert!(!b.record_failure("m", "livekit_connect", Some(100)), "换类:从头数");
        assert_eq!(b.failures("m"), 1);
        b.record_success("m");
        assert_eq!(b.failures("m"), 0, "成功归零");
        assert!(!b.record_failure("m", "stt", Some(100)));
        assert!(!b.record_failure("m", "stt", Some(100)));
        assert!(b.record_failure("m", "stt", Some(100)), "第三次跳闸");
        assert!(b.is_tripped("m", Some(100)), "同一次请求之内仍熔断");
        assert_eq!(b.reports(), vec![("m".to_string(), "stt".to_string(), 3)]);
        b.record_success("m");
        assert!(b.is_tripped("m", Some(100)), "成功也解不了闸,只有人能");
        assert!(!b.record_failure("m", "stt", Some(100)), "跳闸后不再重复跳");
        assert!(!b.is_tripped("m", Some(101)), "人又点了一次「请 Agent」:解除");
        assert!(b.reports().is_empty());
        assert!(!b.is_tripped("other", None));
        b.record_failure("gone", "stt", None);
        b.forget("gone");
        assert_eq!(b.failures("gone"), 0);
    }
}
