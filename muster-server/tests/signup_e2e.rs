//! 自助注册与账号自助(2026-11-03,signup.rs)。
//!
//! ```bash
//! DATABASE_URL=postgres://muster:muster@localhost:5433/muster \
//!   cargo test -p muster-server --test signup_e2e -- --ignored
//! ```
mod common;

use common::{boot, seed, token, Server};
use serde_json::{json, Value};
use uuid::Uuid;

async fn public_post(s: &Server, path: &str, body: Value) -> (u16, Value) {
    let r = s.http.post(format!("{}{path}", s.base)).json(&body).send().await.expect("请求失败");
    let st = r.status().as_u16();
    (st, r.json::<Value>().await.unwrap_or(Value::Null))
}

async fn public_get(s: &Server, path: &str) -> (u16, Value) {
    let r = s.http.get(format!("{}{path}", s.base)).send().await.expect("请求失败");
    let st = r.status().as_u16();
    (st, r.json::<Value>().await.unwrap_or(Value::Null))
}

async fn register(s: &Server, id: &str, pw: &str, name: &str) -> (u16, Value) {
    public_post(s, "/auth/register", json!({ "id": id, "password": pw, "display_name": name })).await
}

async fn login(s: &Server, id: &str, pw: &str) -> (u16, Value) {
    public_post(s, "/auth/login", json!({ "id": id, "password": pw })).await
}

async fn set_mode(s: &Server, tok: &str, mode: &str) -> (u16, Value) {
    s.post("/org/registration", tok, json!({ "mode": mode })).await
}

#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn registration_modes_approval_and_self_service() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let sd = seed(&s.db, "internal").await;
    // alice 当组织所有者:改注册模式要 ChangePolicy,批准要 ManageMembers
    sqlx::query("INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms) VALUES($1,'owner','org',NULL,0)")
        .bind(&sd.alice)
        .execute(&s.db.pool)
        .await
        .unwrap();
    let admin = token(&sd.alice);
    let member = token(&sd.bob);
    // 账号 id 最长 32 字符(signup.rs 的校验),uuid 只取 8 位
    let tag = Uuid::new_v4().simple().to_string()[..8].to_string();
    let (u1, u2, u3) = (format!("reg1-{tag}"), format!("reg2-{tag}"), format!("reg3-{tag}"));

    // ---- closed:一律拒绝,而且不建号
    assert_eq!(set_mode(&s, &admin, "closed").await.0, 200);
    assert_eq!(public_get(&s, "/auth/registration").await.1["mode"], "closed");
    let (st, j) = register(&s, &u1, "password1", "注册一").await;
    assert_eq!(st, 403, "closed 要拒:{j}");

    // 成员改不了模式、批不了人
    assert_eq!(set_mode(&s, &member, "open").await.0, 403, "member 不能改组织策略");

    // ---- approval(缺省):建号但待批准,登录被拒
    assert_eq!(set_mode(&s, &admin, "approval").await.0, 200);
    let (st, j) = register(&s, &u1, "password1", "  注册一  ").await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["pending"], true);
    assert!(j.get("token").is_none(), "待批准不发令牌");
    assert_eq!(j["display_name"], "注册一", "显示名修掉前后空白");

    let (st, j) = login(&s, &u1, "password1").await;
    assert_eq!(st, 403, "待批准不能登录:{j}");
    assert!(j["error"].as_str().unwrap_or("").contains("批准"), "{j}");
    let (st, j) = login(&s, &u1, "wrong-password").await;
    assert_eq!(st, 401, "口令错时不泄露待批准状态:{j}");
    assert!(!j["error"].as_str().unwrap_or("").contains("批准"), "{j}");

    // 管理员看得见待批准
    let (st, j) = s.get("/org/registration", &admin).await;
    assert_eq!(st, 200);
    assert!(j["pending"].as_i64().unwrap() >= 1);
    let (_, accounts) = s.get("/accounts", &admin).await;
    let row = accounts.as_array().unwrap().iter().find(|a| a["id"] == u1.as_str()).expect("列表里有新账号");
    assert_eq!(row["pending"], true);

    // 批准:成员不行,管理员行;批两次第二次 404
    assert_eq!(s.post(&format!("/accounts/{u1}/approve"), &member, json!({})).await.0, 403);
    let (st, j) = s.post(&format!("/accounts/{u1}/approve"), &admin, json!({})).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(s.post(&format!("/accounts/{u1}/approve"), &admin, json!({})).await.0, 404);

    let (st, j) = login(&s, &u1, "password1").await;
    assert_eq!(st, 200, "批准后能登录:{j}");
    let tok1 = j["token"].as_str().unwrap().to_string();
    let (_, me) = s.get("/whoami", &tok1).await;
    assert_eq!(me["bindings"], 1, "批准即 member@org:{me}");

    // 重复 id:要说出来(注册语义),409
    let (st, _) = register(&s, &u1, "password1", "再来").await;
    assert_eq!(st, 409);

    // ---- open:注册即成员,直接给令牌
    assert_eq!(set_mode(&s, &admin, "open").await.0, 200);
    let (st, j) = register(&s, &u2, "password2", "注册二").await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["pending"], false);
    let tok2 = j["token"].as_str().expect("open 模式直接发令牌").to_string();
    let (st, me) = s.get("/whoami", &tok2).await;
    assert_eq!(st, 200);
    assert_eq!(me["bindings"], 1);

    // ---- 校验:坏 id 400(这一次也计入限流)
    let (st, _) = register(&s, "Bad Id", "password1", "x").await;
    assert_eq!(st, 400);

    // ---- 本人改口令:要原口令;新口令要够长;改完旧的失效
    let (st, _) = s.post("/me/password", &tok2, json!({ "old_password": "nope", "new_password": "password3" })).await;
    assert_eq!(st, 401, "原口令不对");
    let (st, _) = s.post("/me/password", &tok2, json!({ "old_password": "password2", "new_password": "short" })).await;
    assert_eq!(st, 400, "新口令太短");
    let (st, j) = s.post("/me/password", &tok2, json!({ "old_password": "password2", "new_password": "password3" })).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(login(&s, &u2, "password2").await.0, 401, "旧口令失效");
    assert_eq!(login(&s, &u2, "password3").await.0, 200, "新口令可用");

    // ---- 限流:同一 IP 第 6 次注册被拒(前面已经 5 次:closed、u1、重复、u2、坏 id)
    let (st, j) = register(&s, &u3, "password1", "注册三").await;
    assert_eq!(st, 429, "第 6 次要被限流:{j}");

    // 模式放回缺省,别把共用的开发库留在 open
    assert_eq!(set_mode(&s, &admin, "approval").await.0, 200);
    for u in [&u1, &u2, &u3] {
        sqlx::query("DELETE FROM account WHERE id = $1").bind(u).execute(&s.db.pool).await.unwrap();
    }
    sd.teardown(&s.db).await;
}
