//! 登录限流。
//!
//! ## 为什么内网时可以没有,上公网就必须有
//!
//! `/auth/login` 是唯一免鉴权的写接口。放在局域网里,能碰到它的人本来就在
//! 楼里;暴露在公网上,它每天会被扫到成千上万次。
//!
//! Argon2 让**单次**猜测很贵,但那是防拖库后离线爆破的;在线爆破面前,
//! 昂贵的哈希反而变成了拒绝服务的把手——每次尝试都吃掉一份 CPU,
//! 攻击者只要并发够多就能把服务器压住,连密码都不用猜对。
//!
//! ## 两个维度都要限
//!
//! - **按来源 IP**:挡住一个来源对着一堆账号喷密码。
//! - **按账号**:挡住一堆来源对着一个账号猜(分布式爆破绕得开 IP 限流)。
//!
//! 只做前者,换 IP 就绕过;只做后者,一个 IP 可以慢慢遍历所有账号。
//!
//! ## 诚实边界
//!
//! **进程内存,单节点。** 重启即清零,多副本各限各的。这与本仓其他地方的
//! 单节点假设一致(审计链、ULID 事件号),不是这里偷懒。真要多副本,
//! 这张表得挪到 Redis 之类的共享存储——那时改的是实现,不是这里的语义。
//!
//! **限的是尝试,不是成功。** 成功登录也计数:否则知道正确口令的人可以
//! 无限刷令牌。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 单个**账号**在窗口内允许的尝试次数。
///
/// 这个可以卡得紧:正常人不会 5 分钟内输错 10 次自己的密码。
pub const MAX_PER_ACCOUNT: usize = 10;

/// 单个**来源 IP** 在窗口内允许的尝试次数。
///
/// 比账号维度宽得多,而且必须宽——**一整间办公室共用一个出口 IP**。
/// 按账号那个数字来限 IP 的话,几个同事先后输错密码就把全公司锁在外面,
/// 而他们看到的是"请求过于频繁",完全不知道发生了什么。
///
/// 60 次/5 分钟对在线爆破仍然是很慢的(一天不到两万次,而 Argon2 让
/// 每次猜测都很贵),对正常办公室却绰绰有余。
pub const MAX_PER_IP: usize = 60;
/// 滑动窗口长度。
const WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub struct LoginLimiter {
    hits: Mutex<HashMap<String, Vec<Instant>>>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一次尝试。超限返回**还要等多久**。
    ///
    /// 传 `now` 进来是为了能测——不然只能靠 sleep 把测试拖成秒级。
    pub fn check_at(&self, key: &str, max: usize, now: Instant) -> Result<(), Duration> {
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());

        // 顺手清掉已经全过期的键。不清的话,被扫一天就攒下几十万个死键——
        // 这张表没有别的地方会收缩它。
        hits.retain(|_, v| v.iter().any(|t| now.duration_since(*t) < WINDOW));

        let v = hits.entry(key.to_string()).or_default();
        v.retain(|t| now.duration_since(*t) < WINDOW);
        if v.len() >= max {
            // 最早那次滑出窗口时才放行
            let oldest = v[0];
            return Err(WINDOW.saturating_sub(now.duration_since(oldest)));
        }
        v.push(now);
        Ok(())
    }

    pub fn check(&self, key: &str, max: usize) -> Result<(), Duration> {
        self.check_at(key, max, Instant::now())
    }
}

/// 从请求头里取来源 IP。
///
/// ## 为什么默认不信 `X-Forwarded-For`
///
/// 那个头是客户端可以随便写的。直连时信它,等于把限流的键交给攻击者控制——
/// 每次请求换一个假 IP 就永远限不到。只有确实站在反向代理后面时才该信,
/// 因为那时代理会覆写它。
///
/// 所以要显式打开 `MUSTER_TRUST_PROXY=1`,而不是"有这个头就用"。
pub fn client_ip(headers: &axum::http::HeaderMap, peer: Option<std::net::SocketAddr>) -> String {
    if std::env::var("MUSTER_TRUST_PROXY").is_ok() {
        if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            // XFF 是逗号分隔的链,**最左边是最初的客户端**
            if let Some(first) = v.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    peer.map(|a| a.ip().to_string()).unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_the_limit_then_refuses() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for i in 0..MAX_PER_ACCOUNT {
            assert!(l.check_at("acct:a", MAX_PER_ACCOUNT, t0).is_ok(), "第 {i} 次不该被限");
        }
        let Err(wait) = l.check_at("acct:a", MAX_PER_ACCOUNT, t0) else {
            panic!("超过上限必须拒绝");
        };
        assert!(wait <= WINDOW && wait > Duration::ZERO);
    }

    #[test]
    fn window_slides_so_a_lockout_is_not_permanent() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for _ in 0..MAX_PER_ACCOUNT {
            let _ = l.check_at("k", MAX_PER_ACCOUNT, t0);
        }
        assert!(l.check_at("k", MAX_PER_ACCOUNT, t0).is_err());
        // 窗口过完:必须重新放行,否则一次误触就把人永久锁在外面
        assert!(l.check_at("k", MAX_PER_ACCOUNT, t0 + WINDOW + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn keys_are_independent() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for _ in 0..MAX_PER_IP {
            let _ = l.check_at("ip:1.1.1.1", MAX_PER_IP, t0);
        }
        // 一个来源被限,不该殃及别人——否则攻击者打满一个 IP 就能锁死全站
        assert!(l.check_at("ip:2.2.2.2", MAX_PER_IP, t0).is_ok());
        assert!(l.check_at("acct:alice", MAX_PER_ACCOUNT, t0).is_ok());
    }

    #[test]
    fn stale_keys_are_reclaimed() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for i in 0..500 {
            let _ = l.check_at(&format!("ip:10.0.0.{i}"), MAX_PER_IP, t0);
        }
        // 窗口过后再来一次:旧键必须被回收,否则被扫一天就攒下几十万个死键
        let _ = l.check_at("ip:fresh", MAX_PER_IP, t0 + WINDOW + Duration::from_secs(1));
        assert_eq!(l.hits.lock().unwrap().len(), 1, "过期的键没有被清理");
    }

    /// **一间办公室共用一个出口 IP** ——这是这次实测撞出来的。
    ///
    /// 按账号的额度去限 IP 的话,几个同事先后输错密码就把全公司锁在外面,
    /// 而他们看到的只是"请求过于频繁"。所以 IP 那一维必须宽得多。
    #[test]
    fn a_shared_office_ip_is_not_locked_out_by_a_few_typos() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        // 5 个人各输错 3 次,共 15 次——远超单账号额度,但同一个出口 IP
        for person in 0..5 {
            for _ in 0..3 {
                let _ = l.check_at(&format!("acct:u{person}"), MAX_PER_ACCOUNT, t0);
                assert!(
                    l.check_at("ip:203.0.113.7", MAX_PER_IP, t0).is_ok(),
                    "同一出口 IP 的第 {} 次尝试不该被限——办公室里没人做错事",
                    person * 3
                );
            }
        }
    }

    #[test]
    fn forwarded_for_is_ignored_unless_proxy_is_trusted() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
        let peer = Some("1.2.3.4:5000".parse().unwrap());

        // 默认不信:这个头客户端能随便写,信了等于把限流的键交给攻击者
        assert_eq!(client_ip(&h, peer), "1.2.3.4");
    }
}
