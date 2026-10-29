//! e2e 测试共用的起服务 / 造数据工具。**不是**公共 API,只给 tests/ 下的文件用。
//! 与 meetings_e2e.rs 里那份同源;新测试用这份,老的不动。
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Once;

use serde_json::Value;
use uuid::Uuid;

use muster_server::{audit::Audit, auth, db::Db, routes, ws::Hub};

const JWT: &str = "e2e-jwt-secret-0123456789abcdef0123456789abcdef";
const LK_SECRET: &str = "e2e-livekit-secret-0123456789abcdef0123456789";

pub fn env_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("MUSTER_JWT_SECRET", JWT);
        std::env::set_var("LIVEKIT_URL", "ws://127.0.0.1:7880");
        std::env::set_var("LIVEKIT_API_KEY", "e2ekey");
        std::env::set_var("LIVEKIT_API_SECRET", LK_SECRET);
        std::env::set_var("MUSTER_BIND", "127.0.0.1:0");
    });
}

pub struct Server {
    pub base: String,
    pub db: Db,
    pub audit_path: String,
    pub http: reqwest::Client,
}

impl Server {
    pub async fn post(&self, path: &str, token: &str, body: Value) -> (u16, Value) {
        self.post_with(path, token, body, &[]).await
    }

    pub async fn post_with(&self, path: &str, token: &str, body: Value, headers: &[(&str, &str)]) -> (u16, Value) {
        let mut req = self.http.post(format!("{}{path}", self.base)).bearer_auth(token).json(&body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let r = req.send().await.expect("请求失败");
        let st = r.status().as_u16();
        (st, r.json::<Value>().await.unwrap_or(Value::Null))
    }

    pub async fn get(&self, path: &str, token: &str) -> (u16, Value) {
        let r = self.http.get(format!("{}{path}", self.base)).bearer_auth(token).send().await.expect("请求失败");
        let st = r.status().as_u16();
        (st, r.json::<Value>().await.unwrap_or(Value::Null))
    }
}

/// 起一个真实的 axum 服务(随机端口)。没设 DATABASE_URL 就返回 None,测试据此跳过。
pub async fn boot() -> Option<Server> {
    env_once();
    let url = std::env::var("DATABASE_URL").ok()?;
    let db = Db::connect(&url).await.ok()?;
    let audit_path = std::env::temp_dir()
        .join(format!("muster-e2e-audit-{}.db", Uuid::new_v4().simple()))
        .display()
        .to_string();
    let audit = Audit::open(&audit_path).expect("打开审计链");
    let app = routes::app(db.clone(), Hub::new(), audit).into_make_service_with_connect_info::<SocketAddr>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Some(Server { base: format!("http://{addr}"), db, audit_path, http: reqwest::Client::new() })
}

pub struct Seed {
    pub team: String,
    pub chan: String,
    pub alice: String,
    pub bob: String,
}

/// 一个团队、一个频道、两个组织成员。用完 `teardown`:开发库是共用的。
pub async fn seed(db: &Db, level: &str) -> Seed {
    let tag = Uuid::new_v4().simple().to_string();
    let team = format!("t{tag}");
    let chan = format!("c{tag}");
    sqlx::query("INSERT INTO team(id,name,created_ms) VALUES($1,$1,0)").bind(&team).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO channel(id,team_id,name,level,private,created_ms) VALUES($1,$2,$1,$3,false,0)")
        .bind(&chan)
        .bind(&team)
        .bind(level)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO channel_cursor(channel_id,next_seq) VALUES($1,1)").bind(&chan).execute(&db.pool).await.unwrap();
    let (alice, bob) = (format!("alice-{tag}"), format!("bob-{tag}"));
    for a in [&alice, &bob] {
        sqlx::query("INSERT INTO account(id, display_name, kind, created_ms) VALUES($1,$1,'human',0)")
            .bind(a)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms) VALUES($1,'member','org',NULL,0)")
            .bind(a)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    Seed { team, chan, alice, bob }
}

impl Seed {
    pub async fn teardown(self, db: &Db) {
        sqlx::query("DELETE FROM team WHERE id = $1").bind(&self.team).execute(&db.pool).await.unwrap();
        for a in [&self.alice, &self.bob] {
            sqlx::query("DELETE FROM account WHERE id = $1").bind(a).execute(&db.pool).await.unwrap();
        }
    }
}

pub fn token(id: &str) -> String {
    auth::issue_token(id, id, "human").expect("签令牌")
}

/// 换一张一次性订阅票。
pub async fn ticket(s: &Server, tok: &str) -> String {
    let (st, j) = s.post("/events/ticket", tok, serde_json::json!({})).await;
    assert_eq!(st, 200, "换票:{j}");
    j["ticket"].as_str().expect("票").to_string()
}
