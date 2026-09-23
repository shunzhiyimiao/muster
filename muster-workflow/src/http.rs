//! `http_get`:白名单主机的一次 GET,取文本,不跟随重定向;每次调用落 `tool.egress`。
//!
//! 为什么不跟随重定向:重定向可以把请求带到白名单之外的主机,而白名单是这个工具
//! 存在的全部理由。为什么只取文本:二进制内容模型读不了,也没法在审计里说明拿了什么。

use std::sync::Arc;
use std::time::{Duration, Instant};

use muster_audit::{ContentHash, EgressBytes, EventBody};
use muster_provider::ToolSpec;
use muster_runner::{ExtraTool, ToolOutcome};

use crate::WorkflowError;

/// 给模型看的最大字节数(超出截断并注明)。
pub const SHOW_CAP: usize = 24_000;
/// 从网络上最多读取的字节数(超出停止读取;计量按实际读取数)。
pub const READ_CAP: usize = 2 * 1024 * 1024;
/// URL 长度上限。抓回来的网页是不可信输入,模型可能被它带偏;出口只有白名单主机,
/// 但查询串仍能带走信息——审计只记 URL 哈希,看不见内容。限长是给这个口子封顶。
pub const MAX_URL_LEN: usize = 2048;

pub struct HttpFetch {
    allow: Vec<String>,
    client: reqwest::Client,
}

impl HttpFetch {
    pub fn new(allow_hosts: Vec<String>) -> Result<Self, WorkflowError> {
        let client = reqwest::Client::builder()
            .user_agent("muster-workflow/0.1")
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| WorkflowError::Http(e.to_string()))?;
        Ok(Self { allow: allow_hosts.into_iter().map(|h| h.to_ascii_lowercase()).collect(), client })
    }

    /// 精确匹配或子域匹配(`a.b.com` 允许 `x.a.b.com`)。带端口时整体比较。
    pub fn host_allowed(&self, host: &str) -> bool {
        let h = host.to_ascii_lowercase();
        self.allow.iter().any(|a| h == *a || h.ends_with(&format!(".{a}")))
    }

    pub fn tool_spec(&self) -> ToolSpec {
        ToolSpec {
            name: "http_get".into(),
            description: format!(
                "向白名单内的主机发一次 HTTP GET,取回文本响应(JSON / CSV / HTML;最多 {SHOW_CAP} 字节,超出截断;\
                 不跟随重定向)。允许的主机:{}。",
                self.allow.join(" / ")
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "完整 URL,主机必须在白名单内" }
                },
                "required": ["url"]
            }),
        }
    }

    pub async fn get(&self, url_str: &str) -> ToolOutcome {
        let started = Instant::now();
        let url_hash = ContentHash::sha256(url_str.as_bytes());
        let egress = |host: String, bytes_out: u64, bytes_in: u64, status: Option<u16>, refused: Option<&str>, started: Instant| {
            EventBody::ToolEgress {
                tool: "http_get".into(),
                host,
                url_hash: url_hash.clone(),
                bytes_out: EgressBytes::Measured(bytes_out),
                bytes_in,
                status,
                latency_ms: started.elapsed().as_millis() as u64,
                refused: refused.map(str::to_owned),
            }
        };
        if url_str.len() > MAX_URL_LEN {
            return ToolOutcome {
                text: format!("拒绝:URL 超过 {MAX_URL_LEN} 字节(只允许简短的公开接口地址)"),
                audit: Some(egress(String::new(), 0, 0, None, Some("url_too_long"), started)),
            };
        }
        let url = match reqwest::Url::parse(url_str) {
            Ok(u) => u,
            Err(e) => {
                return ToolOutcome {
                    text: format!("拒绝:URL 不合法({e})"),
                    audit: Some(egress(String::new(), 0, 0, None, Some("url"), started)),
                }
            }
        };
        let host = url.host_str().unwrap_or("").to_string();
        let host_port = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.clone(),
        };
        if !matches!(url.scheme(), "http" | "https") {
            return ToolOutcome {
                text: format!("拒绝:只支持 http / https,不支持 {}", url.scheme()),
                audit: Some(egress(host, 0, 0, None, Some("scheme"), started)),
            };
        }
        if !(self.host_allowed(&host) || self.host_allowed(&host_port)) {
            return ToolOutcome {
                text: format!(
                    "拒绝:主机 {host_port} 不在本工作流的 egress 白名单里(允许:{})",
                    self.allow.join(" / ")
                ),
                audit: Some(egress(host, 0, 0, None, Some("host"), started)),
            };
        }
        // 载荷近似(与 model.call 同口径):请求行 + Host + UA
        let bytes_out = (4 + url.as_str().len() + 17 + host.len() + 36) as u64;
        let mut resp = match self.client.get(url.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                return ToolOutcome {
                    text: format!("请求失败:{e}"),
                    audit: Some(egress(host, bytes_out, 0, None, Some("network"), started)),
                }
            }
        };
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let textual = ctype.is_empty()
            || ctype.starts_with("text/")
            || ctype.contains("json")
            || ctype.contains("xml")
            || ctype.contains("csv")
            || ctype.contains("javascript")
            || ctype.contains("x-www-form-urlencoded");
        let mut body: Vec<u8> = Vec::new();
        let mut read_note = String::new();
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => {
                    let room = READ_CAP.saturating_sub(body.len());
                    if c.len() > room {
                        body.extend_from_slice(&c[..room]);
                        read_note = format!("(响应超过 {READ_CAP} 字节,已停止读取)");
                        break;
                    }
                    body.extend_from_slice(&c);
                }
                Ok(None) => break,
                Err(e) => {
                    read_note = format!("(读取中断:{e})");
                    break;
                }
            }
        }
        let bytes_in = body.len() as u64;
        if !textual {
            return ToolOutcome {
                text: format!("拒绝:响应不是文本({ctype}),http_get 只取文本内容"),
                audit: Some(egress(host, bytes_out, bytes_in, Some(status), Some("binary"), started)),
            };
        }
        let text = String::from_utf8_lossy(&body);
        let shown = if text.len() > SHOW_CAP {
            let cut = (0..=SHOW_CAP).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
            format!("{}\n…(已截断到 {SHOW_CAP} 字节,原始 {bytes_in} 字节)", &text[..cut])
        } else {
            text.into_owned()
        };
        let redirect_note = match (&location, status) {
            (Some(l), 300..=399) => format!(
                "\n重定向到:{l}\n(本工具不跟随重定向;若该主机在白名单内,请直接请求这个地址;不在则说明白名单在起作用)"
            ),
            _ => String::new(),
        };
        ToolOutcome {
            text: format!("HTTP {status} · {} · {bytes_in} 字节{read_note}{redirect_note}\n\n{shown}", if ctype.is_empty() { "无 content-type" } else { &ctype }),
            audit: Some(egress(host, bytes_out, bytes_in, Some(status), None, started)),
        }
    }

    /// 包成 Runner 的外部工具。
    pub fn into_extra_tool(self: Arc<Self>) -> ExtraTool {
        let spec = self.tool_spec();
        let me = self;
        ExtraTool {
            spec,
            call: Arc::new(move |args: String| {
                let me = me.clone();
                Box::pin(async move {
                    let url = serde_json::from_str::<serde_json::Value>(&args)
                        .ok()
                        .and_then(|v| v["url"].as_str().map(str::to_owned));
                    match url {
                        Some(u) => me.get(&u).await,
                        None => "缺少必填参数 url".to_string().into(),
                    }
                })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 起一个只答一次的本地 HTTP 服务,返回 (地址, 线程句柄)。
    fn one_shot_server(status: &'static str, ctype: &'static str, body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body);
        });
        format!("127.0.0.1:{}", addr.port())
    }

    fn refused_class(o: &ToolOutcome) -> Option<String> {
        match &o.audit {
            Some(EventBody::ToolEgress { refused, .. }) => refused.clone(),
            _ => panic!("应有 tool.egress 事件"),
        }
    }

    #[test]
    fn allowlist_is_exact_or_subdomain() {
        let f = HttpFetch::new(vec!["Api.Example.test".into(), "stooq.com".into()]).unwrap();
        assert!(f.host_allowed("api.example.test"));
        assert!(f.host_allowed("v2.api.example.test"));
        assert!(!f.host_allowed("example.test"));
        assert!(!f.host_allowed("notstooq.com"));
        assert!(f.tool_spec().description.contains("api.example.test"));
    }

    #[tokio::test]
    async fn refuses_before_any_network_when_host_or_scheme_is_off_list() {
        let f = HttpFetch::new(vec!["api.example.test".into()]).unwrap();
        let o = f.get("https://evil.test/x").await;
        assert!(o.text.starts_with("拒绝"));
        assert_eq!(refused_class(&o).as_deref(), Some("host"));
        let o = f.get("ftp://api.example.test/x").await;
        assert_eq!(refused_class(&o).as_deref(), Some("scheme"));
        let o = f.get("not a url").await;
        assert_eq!(refused_class(&o).as_deref(), Some("url"));
        let long = format!("https://api.example.test/q?x={}", "a".repeat(MAX_URL_LEN));
        let o = f.get(&long).await;
        assert_eq!(refused_class(&o).as_deref(), Some("url_too_long"));
    }

    #[tokio::test]
    async fn fetches_text_from_an_allowed_host_and_meters_it() {
        let addr = one_shot_server("200 OK", "application/json", br#"{"spx": 5000}"#);
        let f = HttpFetch::new(vec![addr.clone()]).unwrap();
        let o = f.get(&format!("http://{addr}/quote")).await;
        assert!(o.text.starts_with("HTTP 200"), "{}", o.text);
        assert!(o.text.contains(r#"{"spx": 5000}"#));
        match o.audit {
            Some(EventBody::ToolEgress { host, bytes_in, bytes_out: EgressBytes::Measured(out), status, refused, .. }) => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(bytes_in, 13);
                assert!(out > 40, "请求字节是载荷近似,但不该是 0");
                assert_eq!(status, Some(200));
                assert!(refused.is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn redirects_are_not_followed_but_the_target_is_shown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://api.example.dev/v1/latest\r\nContent-Type: text/html\r\nContent-Length: 4\r\nConnection: close\r\n\r\n<ok>");
        });
        let f = HttpFetch::new(vec![addr.clone()]).unwrap();
        let o = f.get(&format!("http://{addr}/latest")).await;
        assert!(o.text.starts_with("HTTP 301"), "{}", o.text);
        assert!(o.text.contains("重定向到:https://api.example.dev/v1/latest"), "{}", o.text);
        assert!(refused_class(&o).is_none(), "3xx 不是拒绝,是如实回报");
    }

    #[tokio::test]
    async fn binary_responses_are_refused_but_still_metered() {
        let addr = one_shot_server("200 OK", "image/png", b"\x89PNG....");
        let f = HttpFetch::new(vec![addr.clone()]).unwrap();
        let o = f.get(&format!("http://{addr}/logo.png")).await;
        assert!(o.text.starts_with("拒绝"));
        assert_eq!(refused_class(&o).as_deref(), Some("binary"));
        match o.audit {
            Some(EventBody::ToolEgress { bytes_in, status, .. }) => {
                assert_eq!(bytes_in, 8);
                assert_eq!(status, Some(200));
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn extra_tool_wrapper_reads_url_argument() {
        let f = Arc::new(HttpFetch::new(vec!["api.example.test".into()]).unwrap());
        let tool = f.into_extra_tool();
        assert_eq!(tool.spec.name, "http_get");
        let o = (tool.call)(r#"{"url":"https://evil.test"}"#.into()).await;
        assert_eq!(refused_class(&o).as_deref(), Some("host"));
        let o = (tool.call)("{}".into()).await;
        assert!(o.text.contains("url"));
    }
}
