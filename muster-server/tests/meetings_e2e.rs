//! 会议接口 e2e:**真起一个 axum 服务**,用 HTTP 把一场会从建到散走一遍,
//! 再翻服务端审计链核对——链上有没有、顺序对不对、正文在不在(不许在)。
//!
//! 需要一个活着的 PostgreSQL(与 stream_order.rs 同一方式):
//! ```bash
//! DATABASE_URL=postgres://muster:muster@localhost:5433/muster \
//!   cargo test -p muster-server --test meetings_e2e -- --ignored
//! ```
//! 默认 `#[ignore]`,免得 `cargo test --workspace` 依赖外部服务。
//! LiveKit **不需要真的在跑**:入会票是服务端本地签的 JWT,媒体面不在测试范围。
//!
//! JWT 密钥与 LiveKit 配置由测试自己设进环境:并行测试共用进程环境,
//! 所以只设一次(Once),所有测试都用同一组值。

use std::net::SocketAddr;
use std::sync::Once;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use muster_audit::{queries::recent_events, store::AuditStore, AuditEvent, ContentHash};
use muster_route::Sensitivity;
use muster_server::{audit::Audit, auth, db::Db, routes, ws::Hub};

const JWT: &str = "e2e-jwt-secret-0123456789abcdef0123456789abcdef";
const LK_SECRET: &str = "e2e-livekit-secret-0123456789abcdef0123456789";

fn env_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("MUSTER_JWT_SECRET", JWT);
        std::env::set_var("LIVEKIT_URL", "ws://127.0.0.1:7880");
        std::env::set_var("LIVEKIT_API_KEY", "e2ekey");
        std::env::set_var("LIVEKIT_API_SECRET", LK_SECRET);
        // CORS 判定看它:回环地址才允许不设白名单(见 routes::cors_mode)
        std::env::set_var("MUSTER_BIND", "127.0.0.1:0");
    });
}

struct Server {
    base: String,
    db: Db,
    audit_path: String,
    http: reqwest::Client,
}

impl Server {
    async fn post(&self, path: &str, token: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .expect("请求失败");
        let st = r.status().as_u16();
        (st, r.json::<Value>().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str, token: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .expect("请求失败");
        let st = r.status().as_u16();
        (st, r.json::<Value>().await.unwrap_or(Value::Null))
    }

    /// 读回整条链(正序),顺手验链——链不完整就直接 panic。
    fn chain(&self) -> Vec<AuditEvent> {
        let store = AuditStore::open(&self.audit_path).expect("打开审计库");
        store.verify_chain().expect("验链无法执行").expect("链应完整");
        let mut evs = recent_events(store.conn(), 10_000).expect("读事件");
        evs.reverse();
        evs
    }
}

async fn boot() -> Option<Server> {
    env_once();
    let url = std::env::var("DATABASE_URL").ok()?;
    let db = Db::connect(&url).await.ok()?;
    let audit_path = std::env::temp_dir()
        .join(format!("muster-e2e-audit-{}.db", Uuid::new_v4().simple()))
        .display()
        .to_string();
    let audit = Audit::open(&audit_path).expect("打开审计链");
    let app = routes::app(db.clone(), Hub::new(), audit)
        .into_make_service_with_connect_info::<SocketAddr>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Some(Server { base: format!("http://{addr}"), db, audit_path, http: reqwest::Client::new() })
}

struct Seed {
    chan: String,
    alice: String,
    bob: String,
    outsider: String,
}

/// 一个团队、一个频道、三个账号:alice / bob 是组织成员(能发言),outsider 没有任何角色。
async fn seed(db: &Db, level: &str) -> Seed {
    let tag = Uuid::new_v4().simple().to_string();
    let team = format!("t{tag}");
    let chan = format!("c{tag}");
    sqlx::query("INSERT INTO team(id,name,created_ms) VALUES($1,$1,0)")
        .bind(&team)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO channel(id,team_id,name,level,private,created_ms) VALUES($1,$2,$1,$3,false,0)",
    )
    .bind(&chan)
    .bind(&team)
    .bind(level)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO channel_cursor(channel_id,next_seq) VALUES($1,1)")
        .bind(&chan)
        .execute(&db.pool)
        .await
        .unwrap();
    let (alice, bob, outsider) =
        (format!("alice-{tag}"), format!("bob-{tag}"), format!("nobody-{tag}"));
    for a in [&alice, &bob, &outsider] {
        sqlx::query("INSERT INTO account(id, display_name, kind, created_ms) VALUES($1,$1,'human',0)")
            .bind(a)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    for a in [&alice, &bob] {
        sqlx::query(
            "INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms)
             VALUES($1,'member','org',NULL,0)",
        )
        .bind(a)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    Seed { chan, alice, bob, outsider }
}

fn token(id: &str) -> String {
    auth::issue_token(id, id, "human").expect("签令牌")
}

/// 订阅 SSE,把每条事件解析成 JSON 送进通道。连接建立(拿到响应头)后才返回——
/// 从那一刻起的事件都收得到。
async fn subscribe(base: &str, tok: &str) -> mpsc::UnboundedReceiver<Value> {
    let (tx, rx) = mpsc::unbounded_channel();
    let resp = reqwest::get(format!("{base}/events?token={tok}")).await.expect("订阅失败");
    assert!(resp.status().is_success(), "SSE 握手应成功:{}", resp.status());
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(Ok(chunk)) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find("\n\n") {
                let frame: String = buf.drain(..pos + 2).collect();
                let data: Vec<&str> =
                    frame.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).collect();
                if data.is_empty() {
                    continue; // keep-alive 注释行
                }
                if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) {
                    let _ = tx.send(v);
                }
            }
        }
    });
    rx
}

/// 等到一条 `type == ty` 且满足条件的事件;10 秒等不到就失败。
async fn expect_event(
    rx: &mut mpsc::UnboundedReceiver<Value>,
    ty: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let v = rx.recv().await.expect("推送流断了");
            if v["type"] == ty && pred(&v) {
                return v;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("10 秒内没等到 {ty} 事件"))
}

fn ids_of(list: &Value) -> Vec<String> {
    list.as_array()
        .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// 一场会从建到散:接口行为 + 库里的参会记录 + 链上的七种事件,
/// 以及**正文不在链里**。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn lifecycle_is_chained_without_plaintext() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let seed = seed(&s.db, "internal").await;
    let (ta, tb) = (token(&seed.alice), token(&seed.bob));
    let title = "平台组周会·预算口径(内部)";

    // 建会:密级继承频道;空标题拒绝
    let (st, m) = s.post(&format!("/channels/{}/meetings", seed.chan), &ta, json!({ "title": title })).await;
    assert_eq!(st, 200, "{m}");
    assert_eq!(m["level"], "internal", "会议密级 = 频道密级");
    assert_eq!(m["title"], title);
    assert_eq!(m["wants_agent"], false);
    let mid = m["id"].as_str().unwrap().to_string();
    let (st, _) = s.post(&format!("/channels/{}/meetings", seed.chan), &ta, json!({ "title": "   " })).await;
    assert_eq!(st, 400, "空标题不该建会");

    // 列表里有它
    let (st, list) = s.get(&format!("/channels/{}/meetings", seed.chan), &tb).await;
    assert_eq!(st, 200);
    assert!(ids_of(&list).contains(&mid));

    // 两人入会:票上写着能开麦(组织成员能发言),房间与建会时一致
    for t in [&ta, &tb] {
        let (st, j) = s.post(&format!("/meetings/{mid}/join"), t, json!({})).await;
        assert_eq!(st, 200, "{j}");
        assert_eq!(j["can_publish"], true);
        assert_eq!(j["room"], m["room"]);
        assert_eq!(j["level"], "internal");
        assert!(j["token"].as_str().map(|t| t.len() > 20).unwrap_or(false), "入会票应是一枚 JWT");
    }

    // 纪要三行;空行拒绝;历史按序读回
    let lines = ["先对一下预算口径", "幂等键的事下周再说", "小七,记一下:周五前出草案"];
    for l in lines {
        let (st, j) = s
            .post(&format!("/meetings/{mid}/transcript"), &ta, json!({ "speaker_id": seed.alice, "text": l }))
            .await;
        assert_eq!(st, 200, "{j}");
    }
    let (st, _) = s
        .post(&format!("/meetings/{mid}/transcript"), &ta, json!({ "speaker_id": seed.alice, "text": " " }))
        .await;
    assert_eq!(st, 400, "空纪要行不该落库");
    let (st, hist) = s.get(&format!("/meetings/{mid}/transcript"), &tb).await;
    assert_eq!(st, 200);
    let got: Vec<&str> = hist.as_array().unwrap().iter().map(|r| r["text"].as_str().unwrap()).collect();
    assert_eq!(got, lines, "中途进会补的历史必须按说话顺序");

    // 请 Agent → agent-wanted 列出
    let (st, _) = s.post(&format!("/meetings/{mid}/agent"), &ta, json!({ "want": true })).await;
    assert_eq!(st, 200);
    let (_, wanted) = s.get("/meetings/agent-wanted", &tb).await;
    assert!(ids_of(&wanted).contains(&mid), "请了 Agent 的会要被 daemon 看见");

    // bob 离会;再离一次 = 没在会里
    let (st, j) = s.post(&format!("/meetings/{mid}/leave"), &tb, json!({})).await;
    assert_eq!(st, 200);
    assert_eq!(j["was_in"], true);
    let (_, j) = s.post(&format!("/meetings/{mid}/leave"), &tb, json!({})).await;
    assert_eq!(j["was_in"], false, "离开只记一次");

    // 结束:幂等;结束后入会 / 纪要 / 请 Agent 都拒;daemon 不再看见它
    let (st, j) = s.post(&format!("/meetings/{mid}/end"), &ta, json!({})).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["already"], Value::Null);
    let (st, j) = s.post(&format!("/meetings/{mid}/end"), &ta, json!({})).await;
    assert_eq!(st, 200);
    assert_eq!(j["already"], true, "再结束一次只回 already");
    assert_eq!(s.post(&format!("/meetings/{mid}/join"), &tb, json!({})).await.0, 400);
    assert_eq!(
        s.post(&format!("/meetings/{mid}/transcript"), &ta, json!({ "speaker_id": seed.alice, "text": "散会后" }))
            .await
            .0,
        400,
        "散会后不再收纪要"
    );
    assert_eq!(s.post(&format!("/meetings/{mid}/agent"), &ta, json!({ "want": false })).await.0, 400);
    let (_, wanted) = s.get("/meetings/agent-wanted", &tb).await;
    assert!(!ids_of(&wanted).contains(&mid));

    // 参会记录:bob 的 left_ms 由离会写,alice 的由结束统一补
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT account_id, left_ms FROM meeting_participant WHERE meeting_id = $1 ORDER BY account_id",
    )
    .bind(Uuid::parse_str(&mid).unwrap())
    .fetch_all(&s.db.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(_, left)| left.is_some()), "散会后不该还有人在会里:{rows:?}");

    // 审计链:七种事件按发生顺序;全部挂在 meeting:<id> 这个 session 上;正文不在链里
    let evs = s.chain();
    let types: Vec<&str> = evs.iter().map(|e| e.payload["event_type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "meeting.start",
            "meeting.join",
            "meeting.join",
            "meeting.transcript",
            "meeting.transcript",
            "meeting.transcript",
            "meeting.agent",
            "meeting.leave",
            "meeting.end",
        ],
        "被拒的请求(空标题、空纪要、散会后的操作、重复结束)不该在链上留下任何事件"
    );
    for e in &evs {
        assert_eq!(e.session_id.as_deref(), Some(format!("meeting:{mid}").as_str()));
        assert_eq!(e.scope.channel.as_deref(), Some(seed.chan.as_str()));
        assert_eq!(e.label, Some(Sensitivity::Internal), "信封 label = 当时的会议密级");
        assert_eq!(e.policy_version.as_deref(), Some("policy-v1"));
    }
    let all: String = evs.iter().map(|e| e.payload.to_string()).collect();
    assert!(!all.contains(title), "标题正文进了审计链");
    for l in lines {
        assert!(!all.contains(l), "纪要正文进了审计链:{l}");
    }
    // 但哈希能和正文对上:事后可验"纪要有没有被改过"
    let hashes: Vec<&str> = evs
        .iter()
        .filter(|e| e.payload["event_type"] == "meeting.transcript")
        .map(|e| e.payload["text_hash"].as_str().unwrap())
        .collect();
    let expect: Vec<String> = lines.iter().map(|l| ContentHash::sha256(l.as_bytes()).0).collect();
    assert_eq!(hashes, expect);
    assert_eq!(evs[0].payload["title_hash"], ContentHash::sha256(title.as_bytes()).0);
    assert_eq!(evs[0].actor.id, seed.alice);
    assert_eq!(evs[7].actor.id, seed.bob, "离会事件的 actor 是离开的人");
    let end = evs.last().unwrap();
    assert_eq!(end.payload["transcript_lines"], 3);
    assert!(end.payload["duration_ms"].as_u64().is_some());
}

/// 会议状态变化要**推**给在线的人:大厅、侧栏徽标、会议室页和 agent-daemon 都靠这条流。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn meeting_pushes_reach_sse_subscribers() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let seed = seed(&s.db, "open").await;
    let (ta, tb) = (token(&seed.alice), token(&seed.bob));
    let mut rx = subscribe(&s.base, &tb).await;

    let (st, m) = s.post(&format!("/channels/{}/meetings", seed.chan), &ta, json!({ "title": "推送测试" })).await;
    assert_eq!(st, 200, "{m}");
    let mid = m["id"].as_str().unwrap().to_string();
    let ev = expect_event(&mut rx, "meeting", |v| v["meeting_id"] == mid.as_str()).await;
    assert_eq!(ev["event"], "started");
    assert_eq!(ev["channel_id"], seed.chan);
    assert_eq!(ev["title"], "推送测试");
    assert_eq!(ev["level"], "open");

    s.post(&format!("/meetings/{mid}/transcript"), &ta, json!({ "speaker_id": seed.alice, "text": "有人听见吗" })).await;
    let ev = expect_event(&mut rx, "transcript", |v| v["meeting_id"] == mid.as_str()).await;
    assert_eq!(ev["text"], "有人听见吗");
    assert_eq!(ev["speaker_id"], seed.alice);

    s.post(&format!("/meetings/{mid}/agent"), &ta, json!({ "want": true })).await;
    let ev = expect_event(&mut rx, "meeting", |v| v["meeting_id"] == mid.as_str() && v["event"] == "agent").await;
    assert_eq!(ev["wants_agent"], true, "daemon 据此立刻认领");

    let (st, j) = s
        .post(&format!("/meetings/{mid}/level"), &ta, json!({ "level": "internal", "cause": "共享了内部排期" }))
        .await;
    assert_eq!(st, 200, "{j}");
    let ev = expect_event(&mut rx, "meeting", |v| v["meeting_id"] == mid.as_str() && v["event"] == "level").await;
    assert_eq!(ev["level"], "internal");
    let sys = expect_event(&mut rx, "transcript", |v| v["meeting_id"] == mid.as_str() && v["speaker_id"] == "系统").await;
    assert!(sys["text"].as_str().unwrap().contains("internal"), "纪要里要有一行系统说明密级抬升了");

    s.post(&format!("/meetings/{mid}/end"), &ta, json!({})).await;
    let ev = expect_event(&mut rx, "meeting", |v| v["meeting_id"] == mid.as_str() && v["event"] == "ended").await;
    assert_eq!(ev["title"], "推送测试");
}

/// 没有角色的人什么都做不了;没带令牌的连门都进不来。拒绝不留审计事件。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn outsiders_are_refused_before_anything_happens() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let seed = seed(&s.db, "open").await;
    let (ta, tn) = (token(&seed.alice), token(&seed.outsider));
    let (st, m) = s.post(&format!("/channels/{}/meetings", seed.chan), &ta, json!({ "title": "闭门会" })).await;
    assert_eq!(st, 200, "{m}");
    let mid = m["id"].as_str().unwrap().to_string();

    assert_eq!(s.post(&format!("/channels/{}/meetings", seed.chan), &tn, json!({ "title": "x" })).await.0, 403);
    assert_eq!(s.get(&format!("/channels/{}/meetings", seed.chan), &tn).await.0, 403);
    assert_eq!(s.post(&format!("/meetings/{mid}/join"), &tn, json!({})).await.0, 403);
    assert_eq!(s.get(&format!("/meetings/{mid}/transcript"), &tn).await.0, 403);
    assert_eq!(
        s.post(&format!("/meetings/{mid}/transcript"), &tn, json!({ "speaker_id": seed.outsider, "text": "我也来说两句" }))
            .await
            .0,
        403
    );
    assert_eq!(s.post(&format!("/meetings/{mid}/end"), &tn, json!({})).await.0, 403);
    assert_eq!(s.post(&format!("/meetings/{mid}/join"), "not-a-token", json!({})).await.0, 401);
    assert_eq!(s.post(&format!("/meetings/{}/join", Uuid::new_v4()), &ta, json!({})).await.0, 404);

    let evs = s.chain();
    let types: Vec<&str> = evs.iter().map(|e| e.payload["event_type"].as_str().unwrap()).collect();
    assert_eq!(types, ["meeting.start"], "被拒的操作不在链上留痕,链上只有 alice 建的那场会");
    let (_, hist) = s.get(&format!("/meetings/{mid}/transcript"), &ta).await;
    assert_eq!(hist.as_array().map(|a| a.len()), Some(0), "外人的话不该落库");
}

/// 密级只升不降,升到 restricted 后入会票上**禁录**;抬升理由不进链。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn level_only_rises_and_restricted_forbids_recording() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let seed = seed(&s.db, "open").await;
    let ta = token(&seed.alice);
    let (st, m) = s.post(&format!("/channels/{}/meetings", seed.chan), &ta, json!({ "title": "合同评审" })).await;
    assert_eq!(st, 200, "{m}");
    let mid = m["id"].as_str().unwrap().to_string();
    let cause = "共享了《供应商合同-终稿.pdf》";

    let (st, j) = s.post(&format!("/meetings/{mid}/level"), &ta, json!({ "level": "restricted", "cause": cause })).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["level"], "restricted");
    let (st, j) = s.post(&format!("/meetings/{mid}/level"), &ta, json!({ "level": "internal", "cause": "想降回去" })).await;
    assert_eq!(st, 400, "降级必须被拒:{j}");
    let (st, _) = s.post(&format!("/meetings/{mid}/level"), &ta, json!({ "level": "restricted", "cause": "原地" })).await;
    assert_eq!(st, 400, "设成同一级也不是抬升");
    let (st, _) = s.post(&format!("/meetings/{mid}/level"), &ta, json!({ "level": "绝密", "cause": "x" })).await;
    assert_eq!(st, 400, "未知密级不得兜底");

    let (st, j) = s.post(&format!("/meetings/{mid}/join"), &ta, json!({})).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["level"], "restricted");
    assert_eq!(j["can_record"], false, "restricted 一律禁录,不看权限");
    let (_, list) = s.get(&format!("/channels/{}/meetings", seed.chan), &ta).await;
    let row = list.as_array().unwrap().iter().find(|x| x["id"] == mid.as_str()).unwrap();
    assert_eq!(row["level"], "restricted");

    let evs = s.chain();
    let types: Vec<&str> = evs.iter().map(|e| e.payload["event_type"].as_str().unwrap()).collect();
    assert_eq!(types, ["meeting.start", "meeting.level.raise", "meeting.join"]);
    let raise = &evs[1];
    assert_eq!(raise.payload["from_level"], "open");
    assert_eq!(raise.payload["to_level"], "restricted");
    assert_eq!(raise.label, Some(Sensitivity::Restricted));
    assert_eq!(raise.payload["cause_hash"], ContentHash::sha256(cause.as_bytes()).0);
    assert!(!raise.payload.to_string().contains("合同"), "抬升理由是正文,不进链");
    assert_eq!(evs[2].label, Some(Sensitivity::Restricted), "抬升后的入会按新密级记");
    assert_eq!(evs[2].payload["can_record"], false);
}
