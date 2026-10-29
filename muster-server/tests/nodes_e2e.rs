//! 块 2(2026-10-29):组织节点 + 值班工牌。
//!
//! ```bash
//! DATABASE_URL=postgres://muster:muster@localhost:5433/muster \
//!   cargo test -p muster-server --test nodes_e2e -- --ignored
//! ```

mod common;

use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use uuid::Uuid;

use muster_audit::{queries::recent_events, store::AuditStore};
use muster_server::node::challenge_message;

use common::{boot, seed, token, Server};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct NodeClient {
    sk: SigningKey,
    id: Uuid,
    user_code: String,
    poll_token: String,
}

/// 入网:本地造一对密钥,只把公钥交出去。
async fn enroll(s: &Server, name: &str, caps: Value) -> NodeClient {
    let mut seed_bytes = [0u8; 32];
    seed_bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    seed_bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    let sk = SigningKey::from_bytes(&seed_bytes);
    let r = s
        .http
        .post(format!("{}/nodes/enroll", s.base))
        .json(&json!({ "name": name, "pubkey": hex(sk.verifying_key().as_bytes()), "capabilities": caps }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let j: Value = r.json().await.unwrap();
    NodeClient {
        sk,
        id: Uuid::parse_str(j["node_id"].as_str().unwrap()).unwrap(),
        user_code: j["user_code"].as_str().unwrap().to_string(),
        poll_token: j["poll_token"].as_str().unwrap().to_string(),
    }
}

async fn poll(s: &Server, n: &NodeClient) -> (u16, Value) {
    let r = s
        .http
        .get(format!("{}/nodes/enroll/poll?node_id={}&poll_token={}", s.base, n.id, n.poll_token))
        .send()
        .await
        .unwrap();
    let st = r.status().as_u16();
    (st, r.json().await.unwrap_or(Value::Null))
}

/// challenge → 签名 → 会话。返回 (状态码, 响应)。
async fn session(s: &Server, n: &NodeClient, tamper: bool) -> (u16, Value) {
    let r = s.http.post(format!("{}/nodes/{}/challenge", s.base, n.id)).send().await.unwrap();
    let st = r.status().as_u16();
    if st != 200 {
        return (st, r.json().await.unwrap_or(Value::Null));
    }
    let nonce = r.json::<Value>().await.unwrap()["nonce"].as_str().unwrap().to_string();
    let msg = if tamper { challenge_message(Uuid::new_v4(), &nonce) } else { challenge_message(n.id, &nonce) };
    let sig = hex(&n.sk.sign(&msg).to_bytes());
    let r = s.http.post(format!("{}/nodes/{}/session", s.base, n.id)).json(&json!({ "sig": sig })).send().await.unwrap();
    let st = r.status().as_u16();
    (st, r.json().await.unwrap_or(Value::Null))
}

/// 以节点会话 + 工牌头发请求。
async fn as_badge(s: &Server, method: &str, path: &str, node_tok: &str, badge: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{path}", s.base);
    let mut req = match method {
        "GET" => s.http.get(&url),
        _ => s.http.post(&url),
    };
    req = req.bearer_auth(node_tok);
    if let Some(b) = badge {
        req = req.header("X-Muster-Badge", b);
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    let r = req.send().await.unwrap();
    let st = r.status().as_u16();
    (st, r.json().await.unwrap_or(Value::Null))
}

async fn make_owner_and_badge(s: &Server, tag: &str) -> (String, String) {
    let owner = format!("owner-{tag}");
    let badge = format!("A-{}", &tag[..6]);
    sqlx::query("INSERT INTO account(id, display_name, kind, created_ms) VALUES($1,$1,'human',0)")
        .bind(&owner)
        .execute(&s.db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms) VALUES($1,'owner','org',NULL,0)")
        .bind(&owner)
        .execute(&s.db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO account(id, display_name, kind, created_ms) VALUES($1,'小七','agent',0)")
        .bind(&badge)
        .execute(&s.db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms) VALUES($1,'member','org',NULL,0)")
        .bind(&badge)
        .execute(&s.db.pool)
        .await
        .unwrap();
    (owner, badge)
}

/// 入网 → 批准 → 会话 → 以托管的工牌行事;另一个节点拿不到这张工牌;非值班节点拿不到会议;
/// 熔断上报进链、再点「请 Agent」即清;心跳超时后工牌显示不可用。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn node_lifecycle_and_badge_boundaries() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let seed = seed(&s.db, "internal").await;
    let tag = seed.chan.trim_start_matches('c').to_string();
    let (owner, badge) = make_owner_and_badge(&s, &tag).await;
    // 值班工牌是全组织一个:开发库里可能正有真的值班设置,测完要原样放回去
    let prev_duty: Option<String> = sqlx::query_as::<_, (String,)>("SELECT value FROM org_setting WHERE key = 'duty_badge'")
        .fetch_optional(&s.db.pool)
        .await
        .unwrap()
        .map(|r| r.0);
    let (to, ta) = (token(&owner), token(&seed.alice));

    // 入网:两个节点,A 有本地转写能力,B 没有
    let a = enroll(&s, "server-node", json!({ "stt_local": true })).await;
    let b = enroll(&s, "laptop", json!({})).await;
    assert_eq!(a.user_code.len(), 9);
    assert_eq!(poll(&s, &a).await.1["status"], "pending");

    // 批准前:拿不到 challenge,也就拿不到会话
    let (st, _) = session(&s, &a, false).await;
    assert_eq!(st, 403, "未批准的节点不能拿会话");
    // 错的 poll_token 看不到状态
    let r = s.http.get(format!("{}/nodes/enroll/poll?node_id={}&poll_token=nope", s.base, a.id)).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 404);

    // 管理员批准(普通成员不能)
    let (st, _) = s.post(&format!("/nodes/enroll/{}/approve", a.user_code), &ta, json!({})).await;
    assert_eq!(st, 403, "普通成员不能批准入网");
    let (st, j) = s.post(&format!("/nodes/enroll/{}/approve", a.user_code), &to, json!({})).await;
    assert_eq!(st, 200, "{j}");
    let (st, j) = s.post(&format!("/nodes/enroll/{}/approve", b.user_code), &to, json!({})).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(poll(&s, &a).await.1["status"], "approved");

    // 会话:坏签名 401,好签名拿到节点会话
    let (st, _) = session(&s, &a, true).await;
    assert_eq!(st, 401, "签错内容的签名验不过");
    let (st, j) = session(&s, &a, false).await;
    assert_eq!(st, 200, "{j}");
    let tok_a = j["token"].as_str().unwrap().to_string();
    let (st, j) = session(&s, &b, false).await;
    assert_eq!(st, 200, "{j}");
    let tok_b = j["token"].as_str().unwrap().to_string();

    // 节点会话不是账号:直接当账号用 401;带没分配的工牌 403
    assert_eq!(as_badge(&s, "GET", "/whoami", &tok_a, None, None).await.0, 401);
    assert_eq!(as_badge(&s, "GET", "/whoami", &tok_a, Some(&badge), None).await.0, 403, "工牌还没分配到节点");

    // 把工牌分配到 A;以工牌行事 = 工牌身份
    let (st, j) = s.post(&format!("/accounts/{badge}/node"), &to, json!({ "node_id": a.id })).await;
    assert_eq!(st, 200, "{j}");
    let (st, who) = as_badge(&s, "GET", "/whoami", &tok_a, Some(&badge), None).await;
    assert_eq!(st, 200, "{who}");
    assert_eq!((who["id"].as_str(), who["kind"].as_str()), (Some(badge.as_str()), Some("agent")));
    // 节点 B 不能以 A 托管的工牌行事
    assert_eq!(as_badge(&s, "GET", "/whoami", &tok_b, Some(&badge), None).await.0, 403, "别的节点的工牌一律 403");
    // 人的账号不能分配到节点
    let (st, _) = s.post(&format!("/accounts/{}/node", seed.alice), &to, json!({ "node_id": a.id })).await;
    assert_eq!(st, 400);

    // 设定值:有工牌、非值班、没有会议
    let (st, d) = as_badge(&s, "GET", &format!("/nodes/{}/desired", a.id), &tok_a, None, None).await;
    assert_eq!(st, 200, "{d}");
    assert_eq!(d["badges"][0]["id"], badge);
    assert_eq!(d["badges"][0]["duty"], false);
    assert_eq!(d["meetings"].as_array().map(|m| m.len()), Some(0));
    assert_eq!(as_badge(&s, "GET", &format!("/nodes/{}/desired", b.id), &tok_a, None, None).await.0, 403, "只能看自己的设定值");

    // 值班:工牌在 A 上(有本地转写)才行;先把工牌挪到 B 试一下 → 拒
    let (st, _) = s.post(&format!("/accounts/{badge}/node"), &to, json!({ "node_id": b.id })).await;
    assert_eq!(st, 200);
    let (st, j) = s.post("/org/duty", &to, json!({ "badge": badge })).await;
    assert_eq!(st, 400, "没有本地转写能力的节点不能值班:{j}");
    let (st, _) = s.post(&format!("/accounts/{badge}/node"), &to, json!({ "node_id": a.id })).await;
    assert_eq!(st, 200);
    let (st, j) = s.post("/org/duty", &to, json!({ "badge": badge })).await;
    assert_eq!(st, 200, "{j}");
    let (_, duty) = s.get("/org/duty", &ta).await;
    assert_eq!(duty["badge"], badge);
    assert_eq!(duty["node_online"], true, "刚换过会话算在线");

    // 开会并请 Agent:只有值班工牌所在的节点 A 在设定值里看得到
    let (st, m) = s.post(&format!("/channels/{}/meetings", seed.chan), &ta, json!({ "title": "节点测试" })).await;
    assert_eq!(st, 200, "{m}");
    let mid = m["id"].as_str().unwrap().to_string();
    s.post(&format!("/meetings/{mid}/agent"), &ta, json!({ "want": true })).await;
    // 开发库里可能还有别的"请了 Agent"的会,按 id 找自己这场
    let find = |d: &Value| d["meetings"].as_array().unwrap().iter().find(|m| m["id"] == mid.as_str()).cloned();
    let (_, d) = as_badge(&s, "GET", &format!("/nodes/{}/desired", a.id), &tok_a, None, None).await;
    assert_eq!(d["badges"][0]["duty"], true);
    let mine = find(&d).expect("值班节点的设定值里应有这场会");
    let first_req = mine["agent_requested_ms"].as_i64().expect("请 Agent 的时刻");
    let (_, db_) = as_badge(&s, "GET", &format!("/nodes/{}/desired", b.id), &tok_b, None, None).await;
    assert_eq!(db_["meetings"].as_array().map(|m| m.len()), Some(0), "非值班节点拿不到会议");

    // 节点 A 以工牌身份落一行纪要:链上 actor 是工牌
    let (st, j) = as_badge(&s, "POST", &format!("/meetings/{mid}/transcript"), &tok_a, Some(&badge),
        Some(json!({ "speaker_id": seed.alice, "text": "节点来转写了" }))).await;
    assert_eq!(st, 200, "{j}");

    // 心跳:带熔断上报 → 节点列表能看到,链上有 node.breaker;再点一次「请 Agent」即清
    let (st, j) = as_badge(&s, "POST", &format!("/nodes/{}/heartbeat", a.id), &tok_a, None,
        Some(json!({ "version": "test", "progress": { "kind": "transcript", "at_ms": 1 },
                     "breakers": [{ "meeting_id": mid, "error_kind": "stt", "failures": 3 }] }))).await;
    assert_eq!(st, 200, "{j}");
    let (_, nodes) = s.get("/nodes", &to).await;
    let na = nodes.as_array().unwrap().iter().find(|n| n["id"] == a.id.to_string()).unwrap();
    assert_eq!(na["breakers"][0]["error_kind"], "stt");
    assert_eq!(na["online"], true);
    assert_eq!(na["badges"][0], badge);
    assert_eq!(na["last_progress_kind"], "transcript");
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    s.post(&format!("/meetings/{mid}/agent"), &ta, json!({ "want": true })).await;
    let (_, nodes) = s.get("/nodes", &to).await;
    let na = nodes.as_array().unwrap().iter().find(|n| n["id"] == a.id.to_string()).unwrap();
    assert_eq!(na["breakers"].as_array().map(|b| b.len()), Some(0), "人再点一次就清");
    let (_, d) = as_badge(&s, "GET", &format!("/nodes/{}/desired", a.id), &tok_a, None, None).await;
    assert!(find(&d).unwrap()["agent_requested_ms"].as_i64().unwrap() > first_req, "请求时刻更新,节点据此解除熔断");

    // 心跳超时:工牌显示不可用
    let (_, accts) = s.get("/accounts", &to).await;
    let me = accts.as_array().unwrap().iter().find(|x| x["id"] == badge).unwrap();
    assert_eq!(me["node_online"], true);
    sqlx::query("UPDATE node SET last_seen_ms = $1 WHERE id = $2")
        .bind(muster_server::db::now_ms() - 120_000)
        .bind(a.id)
        .execute(&s.db.pool)
        .await
        .unwrap();
    let (_, accts) = s.get("/accounts", &to).await;
    let me = accts.as_array().unwrap().iter().find(|x| x["id"] == badge).unwrap();
    assert_eq!(me["node_online"], false, "90 秒没心跳 = 不可用");
    let (_, duty) = s.get("/org/duty", &ta).await;
    assert_eq!(duty["node_online"], false);

    // 审计链:入网批准 ×2、分配、值班、纪要(actor 是工牌)、熔断
    let store = AuditStore::open(&s.audit_path).unwrap();
    store.verify_chain().unwrap().expect("链应完整");
    let mut evs = recent_events(store.conn(), 1000).unwrap();
    evs.reverse();
    let types: Vec<String> = evs.iter().map(|e| e.payload["event_type"].as_str().unwrap().to_string()).collect();
    assert_eq!(types.iter().filter(|t| *t == "node.enroll").count(), 2);
    assert!(types.contains(&"node.breaker".to_string()));
    let tr = evs.iter().find(|e| e.payload["event_type"] == "meeting.transcript").unwrap();
    assert_eq!(tr.actor.id, badge, "节点以工牌行事,链上的 actor 是工牌");
    let enroll = evs.iter().find(|e| e.payload["event_type"] == "node.enroll").unwrap();
    assert_eq!(enroll.payload["approved_by"]["id"], owner);
    assert!(!evs.iter().map(|e| e.payload.to_string()).collect::<String>().contains("节点来转写了"));

    // 收尾
    sqlx::query("DELETE FROM node WHERE id = $1 OR id = $2").bind(a.id).bind(b.id).execute(&s.db.pool).await.unwrap();
    match &prev_duty {
        Some(v) => sqlx::query("UPDATE org_setting SET value = $1 WHERE key = 'duty_badge'").bind(v).execute(&s.db.pool).await.unwrap(),
        None => sqlx::query("DELETE FROM org_setting WHERE key = 'duty_badge'").execute(&s.db.pool).await.unwrap(),
    };
    for acc in [&owner, &badge] {
        sqlx::query("DELETE FROM account WHERE id = $1").bind(acc).execute(&s.db.pool).await.unwrap();
    }
    seed.teardown(&s.db).await;
}
