//! 用户资料、登录流水、找回口令(2026-11-03,profile.rs)。
//!
//! ```bash
//! DATABASE_URL=postgres://muster:muster@localhost:5433/muster \
//!   cargo test -p muster-server --test profile_e2e -- --ignored
//! ```
mod common;

use common::{boot, seed, token, Server};
use serde_json::{json, Value};

async fn public_post(s: &Server, path: &str, body: Value) -> (u16, Value) {
    let r = s.http.post(format!("{}{path}", s.base)).json(&body).send().await.expect("请求失败");
    let st = r.status().as_u16();
    (st, r.json::<Value>().await.unwrap_or(Value::Null))
}

async fn put(s: &Server, path: &str, tok: &str, body: Value) -> (u16, Value) {
    let r = s.http.put(format!("{}{path}", s.base)).bearer_auth(tok).json(&body).send().await.expect("请求失败");
    let st = r.status().as_u16();
    (st, r.json::<Value>().await.unwrap_or(Value::Null))
}

#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn profile_logins_and_password_reset() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let avatars = std::env::temp_dir().join(format!("muster-e2e-avatars-{}", uuid::Uuid::new_v4().simple()));
    std::env::set_var("MUSTER_AVATARS_DIR", &avatars);
    let sd = seed(&s.db, "internal").await;
    sqlx::query("INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms) VALUES($1,'owner','org',NULL,0)")
        .bind(&sd.alice)
        .execute(&s.db.pool)
        .await
        .unwrap();
    let (alice, bob) = (token(&sd.alice), token(&sd.bob));

    // ---- 资料:默认为空;改了之后邮箱小写;邮箱唯一;校验
    let (st, p) = s.get("/me/profile", &alice).await;
    assert_eq!(st, 200, "{p}");
    assert!(p["email"].is_null() && p["avatar_url"].is_null());

    let (st, p) = put(&s, "/me/profile", &alice, json!({ "email": " Alice@Example.COM ", "department": "平台组", "title": "工程师", "bio": "第一行\n第二行" })).await;
    assert_eq!(st, 200, "{p}");
    assert_eq!(p["email"], "alice@example.com");
    assert_eq!(p["department"], "平台组");
    assert_eq!(p["email_verified"], false);

    let (st, j) = put(&s, "/me/profile", &bob, json!({ "email": "ALICE@example.com" })).await;
    assert_eq!(st, 409, "邮箱唯一(不分大小写):{j}");
    assert_eq!(put(&s, "/me/profile", &bob, json!({ "email": "not-an-email" })).await.0, 400);
    assert_eq!(put(&s, "/me/profile", &bob, json!({ "bio": "x".repeat(501) })).await.0, 400);
    assert_eq!(put(&s, "/me/profile", &bob, json!({ "preferred_provider": "no-such-provider" })).await.0, 400, "偏好通道必须在目录里");
    // 清空邮箱 = 空串
    let (st, p) = put(&s, "/me/profile", &alice, json!({ "email": "", "display_name": "爱丽丝" })).await;
    assert_eq!(st, 200, "{p}");
    assert!(p["email"].is_null());
    assert_eq!(p["display_name"], "爱丽丝");
    assert_eq!(put(&s, "/me/profile", &alice, json!({ "email": "alice@example.com" })).await.0, 200);

    // ---- 头像:按魔数认,不认扩展名;别人看得到;没传的 404
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(std::iter::repeat(0u8).take(64));
    let r = s.http.post(format!("{}/me/avatar", s.base)).bearer_auth(&alice).header("content-type", "application/octet-stream").body(png.clone()).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200, "{}", r.text().await.unwrap_or_default());
    let r = s.http.get(format!("{}/avatars/{}", s.base, sd.alice)).bearer_auth(&bob).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.headers().get("content-type").unwrap(), "image/png");
    assert_eq!(r.bytes().await.unwrap().to_vec(), png);
    let r = s.http.get(format!("{}/avatars/{}", s.base, sd.bob)).bearer_auth(&alice).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let r = s.http.post(format!("{}/me/avatar", s.base)).bearer_auth(&bob).body(b"<svg/>".to_vec()).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 400, "SVG 不收");
    let (_, p) = s.get("/me/profile", &alice).await;
    assert_eq!(p["avatar_url"], format!("/avatars/{}", sd.alice));

    // ---- 登录流水:错一次、对一次;快照与连续失败计数
    let phc = muster_server::auth::hash_password("alicepass123").unwrap();
    sqlx::query("UPDATE account SET password_hash = $1 WHERE id = $2").bind(&phc).bind(&sd.alice).execute(&s.db.pool).await.unwrap();
    assert_eq!(public_post(&s, "/auth/login", json!({ "id": sd.alice, "password": "wrong" })).await.0, 401);
    let (_, p) = s.get("/me/profile", &alice).await;
    assert_eq!(p["failed_logins"], 1);
    let (st, j) = public_post(&s, "/auth/login", json!({ "id": sd.alice, "password": "alicepass123" })).await;
    assert_eq!(st, 200, "{j}");
    let (_, p) = s.get("/me/profile", &alice).await;
    assert_eq!(p["failed_logins"], 0, "成功即清零");
    assert!(p["last_login_ms"].as_i64().unwrap() > 0);
    let (st, logins) = s.get(&format!("/accounts/{}/logins", sd.alice), &alice).await;
    assert_eq!(st, 200);
    let l = logins.as_array().unwrap();
    assert_eq!(l.len(), 2);
    assert_eq!(l[0]["ok"], true, "最新的在前:{logins}");
    assert_eq!(l[1]["reason"], "bad_password");
    assert_eq!(s.get(&format!("/accounts/{}/logins", sd.alice), &bob).await.0, 403, "成员看不了别人的流水");
    let (st, mine) = s.get("/me/logins", &alice).await;
    assert_eq!(st, 200);
    assert_eq!(mine.as_array().unwrap().len(), 2);
    // 不存在的账号登录失败不记流水(枚举器日志)
    let (before,): (i64,) = sqlx::query_as("SELECT count(*) FROM login_event").fetch_one(&s.db.pool).await.unwrap();
    assert_eq!(public_post(&s, "/auth/login", json!({ "id": "no-such-user-xyz", "password": "x" })).await.0, 401);
    let (after,): (i64,) = sqlx::query_as("SELECT count(*) FROM login_event").fetch_one(&s.db.pool).await.unwrap();
    assert_eq!(before, after);

    // 列表带资料摘要
    let (_, accounts) = s.get("/accounts", &alice).await;
    let row = accounts.as_array().unwrap().iter().find(|a| a["id"] == sd.alice.as_str()).unwrap();
    assert_eq!(row["email"], "alice@example.com");
    assert!(row["last_login_ms"].as_i64().unwrap() > 0);

    // ---- 管理员看资料
    let (st, v) = s.get(&format!("/accounts/{}/profile", sd.alice), &alice).await;
    assert_eq!(st, 200);
    assert_eq!(v["profile"]["department"], "平台组");
    assert_eq!(v["logins"].as_array().unwrap().len(), 2);

    // ---- 找回口令:管理员签发链接 → 本人重置 → 旧令牌作废;成员签不了
    assert_eq!(s.post(&format!("/accounts/{}/reset-link", sd.alice), &bob, json!({})).await.0, 403);
    let (st, link) = s.post(&format!("/accounts/{}/reset-link", sd.bob), &alice, json!({})).await;
    assert_eq!(st, 200, "{link}");
    let tok = link["token"].as_str().unwrap().to_string();
    assert_eq!(tok.len(), 64);
    assert_eq!(public_post(&s, "/auth/reset", json!({ "token": tok, "new_password": "short" })).await.0, 400);
    let (st, j) = public_post(&s, "/auth/reset", json!({ "token": tok, "new_password": "bobnewpass1" })).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["id"], sd.bob);
    assert_eq!(public_post(&s, "/auth/login", json!({ "id": sd.bob, "password": "bobnewpass1" })).await.0, 200, "新口令可登录");
    assert_eq!(public_post(&s, "/auth/reset", json!({ "token": tok, "new_password": "bobnewpass2" })).await.0, 401, "令牌只能用一次");
    assert_eq!(public_post(&s, "/auth/reset", json!({ "token": "deadbeef", "new_password": "bobnewpass2" })).await.0, 401);

    // 自助申请:永远 200,且响应与邮箱存不存在无关;邮件通道未接 → delivery none
    let (st, a) = public_post(&s, "/auth/recover", json!({ "email": "alice@example.com" })).await;
    let (st2, b) = public_post(&s, "/auth/recover", json!({ "email": "nobody@example.com" })).await;
    assert_eq!((st, st2), (200, 200));
    assert_eq!(a, b, "不能从响应看出邮箱存不存在:{a} vs {b}");
    assert_eq!(a["delivery"], "none");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM password_reset WHERE account_id = $1 AND issued_by IS NULL").bind(&sd.alice).fetch_one(&s.db.pool).await.unwrap();
    assert_eq!(n, 1, "存在的邮箱签发了令牌(只是没投递)");

    let _ = std::fs::remove_dir_all(&avatars);
    sd.teardown(&s.db).await;
}
