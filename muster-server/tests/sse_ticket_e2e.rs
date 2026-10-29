//! 块 1(2026-10-29):`/events` 的一次性订阅票。
//!
//! ```bash
//! DATABASE_URL=postgres://muster:muster@localhost:5433/muster \
//!   cargo test -p muster-server --test sse_ticket_e2e -- --ignored
//! ```

mod common;

use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};

use common::{boot, seed, ticket, token};

/// 订上流,读到第一条 `message` 事件就返回(10 秒超时)。
async fn first_message(url: String) -> Option<Value> {
    let resp = reqwest::get(&url).await.expect("订阅失败");
    assert!(resp.status().is_success(), "握手应成功:{}", resp.status());
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(chunk)) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find("\n\n") {
                let frame: String = buf.drain(..pos + 2).collect();
                let data: Vec<&str> = frame.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).collect();
                if data.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) {
                    if v["type"] == "message" {
                        return Some(v);
                    }
                }
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// 票换得到、用得上、**只能用一次**;伪造的票和没票都被拒。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn ticket_subscribes_once_and_never_again() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let seed = seed(&s.db, "open").await;
    let (ta, tb) = (token(&seed.alice), token(&seed.bob));

    // 换票要正常鉴权
    let (st, _) = s.post("/events/ticket", "not-a-token", json!({})).await;
    assert_eq!(st, 401, "没登录换不到票");
    let t = ticket(&s, &tb).await;
    assert_eq!(t.len(), 64, "32 字节随机数的十六进制");

    // 用票订上流,alice 发一条,bob 收得到
    let url = format!("{}/events?ticket={t}", s.base);
    let sub = tokio::spawn(first_message(url.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (st, posted) = s.post(&format!("/channels/{}/messages", seed.chan), &ta, json!({ "body": "票通了吗" })).await;
    assert_eq!(st, 200, "{posted}");
    let got = sub.await.unwrap().expect("用票订到的流应收到消息");
    assert_eq!(got["body"], "票通了吗");
    assert_eq!(got["id"], posted["id"]);

    // 同一张票再用一次:拒绝,不是另一条流
    let r = reqwest::get(&url).await.unwrap();
    assert_eq!(r.status().as_u16(), 401, "票用过即作废");
    // 伪造的票 / 没票
    let r = reqwest::get(format!("{}/events?ticket={}", s.base, "0".repeat(64))).await.unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let r = reqwest::get(format!("{}/events", s.base)).await.unwrap();
    assert_eq!(r.status().as_u16(), 401, "没凭据不许订");

    // 兼容路径:?token= 仍可用(服务端打 warn)
    let r = reqwest::get(format!("{}/events?token={tb}", s.base)).await.unwrap();
    assert!(r.status().is_success(), "?token= 兼容路径在桌面壳下一版发布前仍要能用");

    seed.teardown(&s.db).await;
}
