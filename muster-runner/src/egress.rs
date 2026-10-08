//! 命令执行的出网闸(第一刀):回环地址上的 HTTP 代理 + 主机白名单 + 逐连接记账。
//!
//! ## 为什么
//!
//! `run_command` 是整个系统唯一测不到外发的地方(见 [`crate::command`] 的诚实边界):
//! `cargo test` 跑的是工作区里的任意代码,它想 connect 就能 connect,而审计链对此一无所知。
//! 演习报告里的"外发 0 B"因此只覆盖 model.call。
//!
//! ## 做法:照 Codex 的 network-proxy,不链接它的代码
//!
//! Codex 的办法是在回环地址起一个策略代理,把子进程的 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY`
//! 指过去,allow / deny 在代理上判,`NO_PROXY` 放过本机与内网。这里照搬这个形态:
//!
//! - 每个**可写**的 run 起一个 [`EgressProxy`](只读 run 没有 `run_command`,不需要);
//! - 子进程环境里注入代理变量(见 [`EgressProxy::env`]),与 Codex 用同一组键名;
//! - 主机白名单来自工牌的 `egress` 字段,**默认为空 = 全部拒绝**;匹配规则与工作流 `http_get`
//!   一致:精确或子域;
//! - 每一次连接尝试(放行、拒绝、连不上)都成为一条 [`EgressRecord`],由 Runner 落成
//!   `tool.egress`(tool = `run_command`),字节按隧道两个方向实测。
//!
//! 为什么不直接链接 `codex-network-proxy`:它有三万多行、依赖 rama 的 alpha 版 HTTP 栈与
//! Codex 内部的工具 crate,还带 MITM、凭据代理、Windows 归因;Muster 需要的是其中
//! "回环代理 + 白名单 + 记账"这一小块,照它的形态写两百行比拖进一座山干净,也不给
//! 隔离网部署添一个必须联网才能编译的依赖。
//!
//! ## 诚实边界
//!
//! 环境变量代理只约束**认代理**的工具:curl / wget / cargo / npm / pip / git over https 都认,
//! 直接 connect 的进程、不认代理的工具仍能绕开。所以记账口径不变:**测到的算测到,测不到不算零**。
//! 硬封堵是第二刀——macOS `sandbox-exec` / Linux network namespace 把出网只放给代理端口,
//! 到那时这道闸才从"绝大多数工具会走的门"变成"唯一的门"。

use std::net::TcpListener as StdTcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 与 Codex 同一组默认不走代理的地址:本机与内网。
pub const DEFAULT_NO_PROXY: &str = "localhost,127.0.0.1,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16";
/// 子进程里指向代理的变量(大小写各一份,工具们认的不一样)。
pub const PROXY_ENV_KEYS: &[&str] = &["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"];
/// 与 Codex 同一组 NO_PROXY 键名(npm / yarn / bundler 各有自己的拼法)。
pub const NO_PROXY_ENV_KEYS: &[&str] = &["NO_PROXY", "no_proxy", "npm_config_noproxy", "NPM_CONFIG_NOPROXY", "YARN_NO_PROXY", "BUNDLE_NO_PROXY"];
/// 告诉子进程"你在出网闸后面"。
pub const EGRESS_ACTIVE_ENV_KEY: &str = "MUSTER_EGRESS_PROXY";

/// 请求头封顶:超过就是不正常的客户端。
const HEAD_CAP: usize = 16 * 1024;
/// 连上游 / 读请求头的超时。
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// 主机白名单。空 = 全部拒绝(默认)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    pub allow: Vec<String>,
}

impl EgressPolicy {
    pub fn deny_all() -> Self {
        Self::default()
    }

    pub fn allow<I: IntoIterator<Item = S>, S: Into<String>>(hosts: I) -> Self {
        Self { allow: hosts.into_iter().map(Into::into).map(|h| normalize_host(&h)).filter(|h| !h.is_empty()).collect() }
    }

    /// 精确或子域匹配(与工作流 `http_get` 同一规则):允许 `example.com` 即允许 `api.example.com`。
    pub fn host_allowed(&self, host: &str) -> bool {
        let h = normalize_host(host);
        !h.is_empty() && self.allow.iter().any(|a| h == *a || h.ends_with(&format!(".{a}")))
    }

    /// 公示文案。
    pub fn describe(&self) -> String {
        if self.allow.is_empty() { "无(全部拒绝)".into() } else { self.allow.join(" / ") }
    }
}

/// 主机名规范化:去空白、去 IPv6 方括号、去末尾点、小写。
pub fn normalize_host(h: &str) -> String {
    h.trim().trim_matches(|c| c == '[' || c == ']').trim_end_matches('.').to_ascii_lowercase()
}

/// 一次连接尝试的账。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRecord {
    pub host: String,
    pub port: u16,
    /// `CONNECT` 隧道(HTTPS 等)为真;明文 HTTP 转发为假。
    pub tunneled: bool,
    /// 客户端 → 上游的字节(隧道里是 TLS 密文,按字节计)。
    pub bytes_out: u64,
    /// 上游 → 客户端的字节。
    pub bytes_in: u64,
    /// 明文 HTTP 转发时上游的状态码;隧道看不见,为 `None`。
    pub status: Option<u16>,
    pub latency_ms: u64,
    /// `host`(不在白名单)/ `connect`(连不上)/ `timeout` / `request`(请求不合法);`None` = 放行且完成。
    pub refused: Option<String>,
}

/// 回环地址上的策略代理,自带线程与运行时,与调用方的 tokio 运行时无关——
/// `run_command` 是同步阻塞执行的,代理不能指望调用方的线程来跑。
pub struct EgressProxy {
    port: u16,
    records: Arc<Mutex<Vec<EgressRecord>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EgressProxy {
    /// 绑定 `127.0.0.1` 的随机端口并开始服务。
    pub fn start(policy: EgressPolicy) -> std::io::Result<Self> {
        let std_listener = StdTcpListener::bind(("127.0.0.1", 0))?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        let records: Arc<Mutex<Vec<EgressRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let records_in_thread = records.clone();
        let thread = std::thread::Builder::new().name("muster-egress-proxy".into()).spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            rt.block_on(async move {
                let Ok(listener) = TcpListener::from_std(std_listener) else { return };
                serve(listener, Arc::new(policy), records_in_thread, rx).await;
            });
        })?;
        Ok(Self { port, records, shutdown: Some(tx), thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 注入子进程的环境:代理变量指向本代理,NO_PROXY 放过本机与内网,再加一个"在闸后面"的标记。
    pub fn env(&self) -> Vec<(String, String)> {
        let url = format!("http://127.0.0.1:{}", self.port);
        let mut out: Vec<(String, String)> = PROXY_ENV_KEYS.iter().map(|k| (k.to_string(), url.clone())).collect();
        out.extend(NO_PROXY_ENV_KEYS.iter().map(|k| (k.to_string(), DEFAULT_NO_PROXY.to_string())));
        out.push((EGRESS_ACTIVE_ENV_KEY.to_string(), "1".to_string()));
        out
    }

    /// 取走到目前为止的账(取走即清空)。
    pub fn take_records(&self) -> Vec<EgressRecord> {
        std::mem::take(&mut *self.records.lock().unwrap())
    }

    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

async fn serve(
    listener: TcpListener,
    policy: Arc<EgressPolicy>,
    records: Arc<Mutex<Vec<EgressRecord>>>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let (policy, records) = (policy.clone(), records.clone());
                tokio::spawn(async move {
                    if let Some(rec) = handle(stream, &policy).await {
                        records.lock().unwrap().push(rec);
                    }
                });
            }
        }
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// `host:port` / `[v6]:port` / `host` → (host, port)。
fn split_host_port(s: &str, default_port: u16) -> (String, u16) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[') {
        // IPv6:[::1]:443
        if let Some(end) = rest.find(']') {
            let host = &rest[..end];
            let port = rest[end + 1..].strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default_port);
            return (normalize_host(host), port);
        }
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (normalize_host(h), p.parse().unwrap_or(default_port)),
        _ => (normalize_host(s), default_port),
    }
}

async fn respond(client: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = client.write_all(msg.as_bytes()).await;
    let _ = client.shutdown().await;
}

/// 处理一个客户端连接;返回这次尝试的账(请求本身不合法时返回 None)。
async fn handle(mut client: TcpStream, policy: &EgressPolicy) -> Option<EgressRecord> {
    let started = Instant::now();
    // 读请求头:到空行为止,封顶 16 KiB
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let head_end = loop {
        let mut chunk = [0u8; 2048];
        let n = match tokio::time::timeout(IO_TIMEOUT, client.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return None,
            Ok(Ok(n)) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_head_end(&buf) {
            break pos;
        }
        if buf.len() > HEAD_CAP {
            respond(&mut client, 431, "Request Header Fields Too Large", "请求头过大").await;
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let body_start = head_end + 4;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    let (host, port, tunneled, origin_target) = if method.eq_ignore_ascii_case("CONNECT") {
        let (h, p) = split_host_port(&target, 443);
        (h, p, true, String::new())
    } else if let Some(rest) = target.strip_prefix("http://") {
        // 绝对 URI:http://host[:port]/path
        let (hp, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (h, p) = split_host_port(hp, 80);
        (h, p, false, path.to_string())
    } else {
        respond(&mut client, 400, "Bad Request", "出网闸只接受 CONNECT 或绝对 URI 的代理请求").await;
        return None;
    };

    let mut rec = EgressRecord { host: host.clone(), port, tunneled, bytes_out: 0, bytes_in: 0, status: None, latency_ms: 0, refused: None };
    let finish = |mut rec: EgressRecord, refused: Option<&str>| {
        rec.refused = refused.map(str::to_owned);
        rec.latency_ms = started.elapsed().as_millis() as u64;
        Some(rec)
    };
    if host.is_empty() {
        respond(&mut client, 400, "Bad Request", "请求里没有主机名").await;
        return finish(rec, Some("request"));
    }
    if !policy.host_allowed(&host) {
        let body = format!(
            "拒绝:主机 {host} 不在本次任务的出网允许清单里(允许:{})。这是 Muster 的出网闸,本次尝试已记入审计。",
            policy.describe()
        );
        respond(&mut client, 403, "Forbidden", &body).await;
        return finish(rec, Some("host"));
    }

    let mut upstream = match tokio::time::timeout(IO_TIMEOUT, TcpStream::connect((host.as_str(), port))).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            respond(&mut client, 502, "Bad Gateway", &format!("连不上 {host}:{port}:{e}")).await;
            return finish(rec, Some("connect"));
        }
        Err(_) => {
            respond(&mut client, 504, "Gateway Timeout", &format!("连接 {host}:{port} 超时")).await;
            return finish(rec, Some("timeout"));
        }
    };

    let leftover = &buf[body_start..];
    if tunneled {
        if client.write_all(format!("{version} 200 Connection Established\r\n\r\n").as_bytes()).await.is_err() {
            return finish(rec, Some("request"));
        }
        if !leftover.is_empty() && upstream.write_all(leftover).await.is_ok() {
            rec.bytes_out += leftover.len() as u64;
        }
    } else {
        // 明文 HTTP:请求行改成 origin-form,去掉 Proxy-* 头,强制 Connection: close(一问一答,不复用)
        let mut out = format!("{method} {origin_target} {version}\r\n");
        for l in lines {
            if l.is_empty() {
                continue;
            }
            let lower = l.to_ascii_lowercase();
            if lower.starts_with("proxy-") || lower.starts_with("connection:") {
                continue;
            }
            out.push_str(l);
            out.push_str("\r\n");
        }
        out.push_str("Connection: close\r\n\r\n");
        if upstream.write_all(out.as_bytes()).await.is_err() {
            respond(&mut client, 502, "Bad Gateway", "向上游写请求失败").await;
            return finish(rec, Some("connect"));
        }
        rec.bytes_out += out.len() as u64;
        if !leftover.is_empty() && upstream.write_all(leftover).await.is_ok() {
            rec.bytes_out += leftover.len() as u64;
        }
        // 先读上游的第一块,解析状态码再原样转给客户端
        let mut first = [0u8; 4096];
        match tokio::time::timeout(IO_TIMEOUT, upstream.read(&mut first)).await {
            Ok(Ok(n)) if n > 0 => {
                let head = String::from_utf8_lossy(&first[..n]);
                rec.status = head.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok());
                if client.write_all(&first[..n]).await.is_err() {
                    return finish(rec, Some("request"));
                }
                rec.bytes_in += n as u64;
            }
            _ => return finish(rec, Some("connect")),
        }
    }

    // 双向转发到两边都关为止,顺便把两个方向的字节数记下
    if let Ok((c2u, u2c)) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        rec.bytes_out += c2u;
        rec.bytes_in += u2c;
    }
    finish(rec, None)
}

/// 拒绝原因的人话。
pub fn refusal_zh(why: &str) -> &'static str {
    match why {
        "host" => "不在出网允许清单",
        "connect" => "连不上目标",
        "timeout" => "连接超时",
        "request" => "请求不合法",
        _ => "被拒绝",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener as L, TcpStream as S};

    /// 回声服务器:原样回写,对端关了就关。返回端口。
    fn echo_server() -> u16 {
        let l = L::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut s = s;
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = s.read(&mut buf) {
                        if n == 0 || s.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    fn wait_records(p: &EgressProxy) -> Vec<EgressRecord> {
        for _ in 0..100 {
            let r = p.take_records();
            if !r.is_empty() {
                return r;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Vec::new()
    }

    fn read_until_head_end(s: &mut S) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 512];
        loop {
            let n = s.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if find_head_end(&buf).is_some() {
                break;
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    #[test]
    fn policy_matches_exact_and_subdomains_only() {
        let p = EgressPolicy::allow(["Example.com", "[::1]"]);
        assert!(p.host_allowed("example.com") && p.host_allowed("API.example.com.") && p.host_allowed("::1"));
        assert!(!p.host_allowed("notexample.com") && !p.host_allowed("example.com.evil.net") && !p.host_allowed(""));
        assert!(!EgressPolicy::deny_all().host_allowed("example.com"));
        assert_eq!(EgressPolicy::deny_all().describe(), "无(全部拒绝)");
    }

    #[test]
    fn env_points_children_at_the_proxy() {
        let p = EgressProxy::start(EgressPolicy::deny_all()).unwrap();
        let env = p.env();
        let url = format!("http://127.0.0.1:{}", p.port());
        for k in PROXY_ENV_KEYS {
            assert_eq!(env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str()), Some(url.as_str()), "{k}");
        }
        assert_eq!(env.iter().find(|(k, _)| k == "NO_PROXY").map(|(_, v)| v.as_str()), Some(DEFAULT_NO_PROXY));
        assert!(env.iter().any(|(k, v)| k == EGRESS_ACTIVE_ENV_KEY && v == "1"));
    }

    #[test]
    fn connect_tunnel_to_allowed_host_is_metered() {
        let echo = echo_server();
        let p = EgressProxy::start(EgressPolicy::allow(["127.0.0.1"])).unwrap();
        let mut c = S::connect(("127.0.0.1", p.port())).unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{echo} HTTP/1.1\r\nHost: 127.0.0.1:{echo}\r\n\r\n").as_bytes()).unwrap();
        let head = read_until_head_end(&mut c);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        c.write_all(b"hello").unwrap();
        let mut back = [0u8; 5];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"hello");
        drop(c);
        let recs = wait_records(&p);
        assert_eq!(recs.len(), 1, "{recs:?}");
        let r = &recs[0];
        assert!(r.tunneled && r.refused.is_none() && r.host == "127.0.0.1" && r.port == echo, "{r:?}");
        assert!(r.bytes_out >= 5 && r.bytes_in >= 5, "两个方向都要计量:{r:?}");
    }

    #[test]
    fn denied_host_gets_403_and_is_recorded() {
        let p = EgressProxy::start(EgressPolicy::deny_all()).unwrap();
        let mut c = S::connect(("127.0.0.1", p.port())).unwrap();
        c.write_all(b"CONNECT evil.example:443 HTTP/1.1\r\nHost: evil.example:443\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 403"), "{resp}");
        assert!(resp.contains("不在本次任务的出网允许清单"), "{resp}");
        let recs = wait_records(&p);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].refused.as_deref(), Some("host"));
        assert_eq!((recs[0].host.as_str(), recs[0].port, recs[0].bytes_out, recs[0].bytes_in), ("evil.example", 443, 0, 0));
    }

    #[test]
    fn plain_http_is_forwarded_in_origin_form_and_status_recorded() {
        // 一个只认一问一答的 HTTP 服务器,顺便把收到的请求头交回来检查
        let l = L::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let head = read_until_head_end(&mut s);
            tx.send(head).unwrap();
            s.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let p = EgressProxy::start(EgressPolicy::allow(["127.0.0.1"])).unwrap();
        let mut c = S::connect(("127.0.0.1", p.port())).unwrap();
        c.write_all(format!("GET http://127.0.0.1:{port}/x?y=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nProxy-Connection: keep-alive\r\nUser-Agent: t\r\n\r\n").as_bytes()).unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 204"), "{resp}");
        let seen = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(seen.starts_with("GET /x?y=1 HTTP/1.1\r\n"), "请求行要改成 origin-form:{seen}");
        assert!(!seen.to_ascii_lowercase().contains("proxy-connection"), "代理头不该漏给上游:{seen}");
        assert!(seen.contains("User-Agent: t"), "其余头原样转发:{seen}");
        // 账在两边都关了之后才记:客户端这头也得关
        drop(c);
        let recs = wait_records(&p);
        assert_eq!(recs.len(), 1);
        assert!(!recs[0].tunneled && recs[0].refused.is_none() && recs[0].status == Some(204), "{:?}", recs[0]);
    }

    #[test]
    fn split_host_port_handles_v6_and_defaults() {
        assert_eq!(split_host_port("example.com:8443", 443), ("example.com".into(), 8443));
        assert_eq!(split_host_port("example.com", 80), ("example.com".into(), 80));
        assert_eq!(split_host_port("[::1]:9000", 443), ("::1".into(), 9000));
        assert_eq!(split_host_port("[::1]", 443), ("::1".into(), 443));
    }
}
