//! 块 3(2026-10-29):派发单——确认权收窄到被派到的本人。
//!
//! ```bash
//! DATABASE_URL=postgres://muster:muster@localhost:5433/muster \
//!   cargo test -p muster-server --test dispatch_e2e -- --ignored
//! ```

mod common;

use serde_json::{json, Value};
use uuid::Uuid;

use muster_audit::{queries::recent_events, store::AuditStore};
use muster_server::{action::expire_due, audit::Audit, db::now_ms, ws::Hub};

use common::{boot, seed, token};

/// 建一个账号;`role` 形如 `("member", Some(频道 id))`(频道级)或 `("owner", None)`(组织级)。
async fn add_account(s: &common::Server, id: &str, name: &str, kind: &str, role: Option<(&str, Option<&str>)>) {
    sqlx::query("INSERT INTO account(id, display_name, kind, created_ms) VALUES($1,$2,$3,0)")
        .bind(id)
        .bind(name)
        .bind(kind)
        .execute(&s.db.pool)
        .await
        .unwrap();
    if let Some((r, chan)) = role {
        sqlx::query("INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms) VALUES($1,$2,$3,$4,0)")
            .bind(id)
            .bind(r)
            .bind(if chan.is_some() { "channel" } else { "org" })
            .bind(chan)
            .execute(&s.db.pool)
            .await
            .unwrap();
    }
}

/// 值班 Agent 提案 →「李」解析成 li;只有 li 能接受 / 驳回;主持人不能替他接受;
/// 没解析出来的由主持人指派;驳回要理由;Agent 不能裁决;超时过期且不转派;每步进链。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn decisions_belong_to_the_assignee() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let seed = seed(&s.db, "internal").await;
    let tag = &seed.chan[1..7];
    let (li, agent, admin, wai) = (format!("li-{tag}"), format!("A-{tag}"), format!("admin-{tag}"), format!("wai-{tag}"));
    // li 只是这个频道的成员;wai 也叫「李四」但不在频道里——不该成为候选
    add_account(&s, &li, "李四", "human", Some(("member", Some(&seed.chan)))).await;
    add_account(&s, &agent, "小七", "agent", Some(("member", None))).await;
    add_account(&s, &admin, "管理员", "human", Some(("owner", None))).await;
    add_account(&s, &wai, "李四", "human", None).await;
    let (ta, tb, tl, tadmin) = (token(&seed.alice), token(&seed.bob), token(&li), token(&admin));
    let tagent = muster_server::auth::issue_token(&agent, "小七", "agent").unwrap();

    // alice 建会 = 主持人
    let (st, m) = s.post(&format!("/channels/{}/meetings", seed.chan), &ta, json!({ "title": "派发测试" })).await;
    assert_eq!(st, 200, "{m}");
    assert_eq!(m["host"], seed.alice, "建会的人是主持人");
    let mid = m["id"].as_str().unwrap().to_string();

    // Agent 提案,负责人提示「李」→ 解析成 li;带密级;72 小时到期
    let (st, it) = s
        .post(&format!("/meetings/{mid}/action-items"), &tagent,
              json!({ "text": "把重试逻辑定下来", "owner_hint": "李", "source_quote": "李负责把重试逻辑定下来" }))
        .await;
    assert_eq!(st, 200, "{it}");
    assert_eq!(it["assignee"], li, "候选只有能在频道里说话的人,频道外的同名者不算");
    assert_eq!(it["owner_hint"], "李");
    assert_eq!(it["sensitivity"], "internal");
    assert!(it["expires_ms"].as_i64().unwrap() > now_ms() + 71 * 3600 * 1000);
    let aid = it["id"].as_str().unwrap().to_string();

    // 收件箱:只有 li 的里面有
    let (_, inbox) = s.get("/me/inbox", &tl).await;
    assert_eq!(inbox.as_array().map(|a| a.len()), Some(1));
    assert_eq!(inbox[0]["meeting_title"], "派发测试");
    assert_eq!(inbox[0]["source_quote"], "李负责把重试逻辑定下来");
    let (_, inbox_b) = s.get("/me/inbox", &tb).await;
    assert_eq!(inbox_b.as_array().map(|a| a.len()), Some(0));

    // 闸 1:非 assignee 接受 403;主持人也不能替他接受;Agent 不能裁决
    assert_eq!(s.post(&format!("/action-items/{aid}/decide"), &tb, json!({ "confirm": true })).await.0, 403);
    assert_eq!(s.post(&format!("/action-items/{aid}/decide"), &ta, json!({ "confirm": true })).await.0, 403, "主持人不能替本人接受");
    assert_eq!(s.post(&format!("/action-items/{aid}/decide"), &tadmin, json!({ "confirm": true })).await.0, 403, "管理员也不能");
    assert_eq!(s.post(&format!("/action-items/{aid}/decide"), &tagent, json!({ "confirm": true })).await.0, 403, "Agent 不能裁决");
    // 驳回要理由
    assert_eq!(s.post(&format!("/action-items/{aid}/decide"), &tl, json!({ "confirm": false })).await.0, 400, "驳回无理由");
    // 本人接受
    let (st, j) = s.post(&format!("/action-items/{aid}/decide"), &tl, json!({ "confirm": true, "note": "我来" })).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["status"], "accepted");
    assert_eq!(j["accepted_mode"], "self");
    assert_eq!(j["decided_by"], li);
    assert_eq!(s.post(&format!("/action-items/{aid}/decide"), &tl, json!({ "confirm": true })).await.0, 400, "不可重复裁决");
    let (_, inbox) = s.get("/me/inbox", &tl).await;
    assert_eq!(inbox.as_array().map(|a| a.len()), Some(0), "接受后离开收件箱");

    // 没解析出负责人的提案:谁也不能裁决,主持人指派后才行;bob 不是主持人不能指派
    let (st, it2) = s
        .post(&format!("/meetings/{mid}/action-items"), &tagent, json!({ "text": "整理会议纪要", "owner_hint": "那个谁" }))
        .await;
    assert_eq!(st, 200, "{it2}");
    assert!(it2["assignee"].is_null(), "拿不准就空着");
    let aid2 = it2["id"].as_str().unwrap().to_string();
    assert_eq!(s.post(&format!("/action-items/{aid2}/decide"), &tb, json!({ "confirm": true })).await.0, 400, "没指派不能裁决");
    assert_eq!(s.post(&format!("/action-items/{aid2}/assign"), &tb, json!({ "assignee": seed.bob })).await.0, 403, "非主持人不能指派");
    assert_eq!(s.post(&format!("/action-items/{aid2}/assign"), &tagent, json!({ "assignee": seed.bob })).await.0, 403, "Agent 不能指派");
    assert_eq!(s.post(&format!("/action-items/{aid2}/assign"), &ta, json!({ "assignee": wai })).await.0, 400, "不能派给看不见频道的人");
    let (st, j) = s.post(&format!("/action-items/{aid2}/assign"), &ta, json!({ "assignee": seed.bob })).await;
    assert_eq!(st, 200, "主持人指派:{j}");
    assert_eq!(j["assignee"], seed.bob);
    let (st, _) = s.post(&format!("/action-items/{aid2}/assign"), &tadmin, json!({ "assignee": seed.bob })).await;
    assert_eq!(st, 200, "组织管理员也能指派");
    // bob 驳回带理由
    let (st, j) = s.post(&format!("/action-items/{aid2}/decide"), &tb, json!({ "confirm": false, "note": "这不该是我" })).await;
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["status"], "rejected");
    assert_eq!(j["reject_reason"], "这不该是我");

    // 超时:第三条派给 li,把到期时刻倒回去 → 过期、提醒主持人、不转派
    let (st, it3) = s
        .post(&format!("/meetings/{mid}/action-items"), &tagent, json!({ "text": "补周报", "owner_hint": "李四" }))
        .await;
    assert_eq!(st, 200, "{it3}");
    let aid3 = Uuid::parse_str(it3["id"].as_str().unwrap()).unwrap();
    sqlx::query("UPDATE meeting_action_item SET expires_ms = $1 WHERE id = $2")
        .bind(now_ms() - 1000)
        .bind(aid3)
        .execute(&s.db.pool)
        .await
        .unwrap();
    let audit = Audit::open(&s.audit_path).unwrap();
    // 开发库是共用的:别的会议里到期的 proposed 也会一起过期,只看我们这一条
    let expired = expire_due(&s.db, &Hub::new(), &audit, now_ms()).await.unwrap();
    let mine = expired.iter().find(|e| e.id == aid3).expect("到期的那条要在里面");
    assert_eq!(mine.status, "expired");
    assert_eq!(mine.assignee.as_deref(), Some(li.as_str()), "不自动转派");
    assert!(!expired.iter().any(|e| e.id == Uuid::parse_str(&aid).unwrap() || e.id == Uuid::parse_str(&aid2).unwrap()), "已裁决的不过期");
    let (_, inbox) = s.get("/me/inbox", &tl).await;
    assert_eq!(inbox.as_array().map(|a| a.len()), Some(0), "过期的不再在收件箱");
    let (_, msgs) = s.get(&format!("/channels/{}/messages", seed.chan), &ta).await;
    let last = msgs.as_array().unwrap().last().unwrap();
    assert!(last["body"].as_str().unwrap().contains("已过期回收"), "要提醒主持人:{last}");
    assert!(last["body"].as_str().unwrap().contains(&seed.alice));

    // 审计链:指派 ×2、裁决 ×2(接受、驳回)、过期 ×1,正文不进链
    drop(audit);
    let store = AuditStore::open(&s.audit_path).unwrap();
    store.verify_chain().unwrap().expect("链应完整");
    let evs = recent_events(store.conn(), 1000).unwrap();
    let types: Vec<String> = evs.iter().map(|e| e.payload["event_type"].as_str().unwrap().to_string()).collect();
    assert_eq!(types.iter().filter(|t| *t == "dispatch.assign").count(), 2);
    assert_eq!(types.iter().filter(|t| *t == "approval.decision").count(), 2);
    let expire_mine = evs
        .iter()
        .filter(|e| e.payload["event_type"] == "dispatch.expire" && e.payload["item_id"] == format!("ACT-{aid3}"))
        .count();
    assert_eq!(expire_mine, 1, "过期进链一次");
    let decisions: Vec<&Value> = evs.iter().filter(|e| e.payload["event_type"] == "approval.decision").map(|e| &e.payload).collect();
    assert!(decisions.iter().any(|d| d["granted"] == true) && decisions.iter().any(|d| d["granted"] == false));
    let all: String = evs.iter().map(|e| e.payload.to_string()).collect();
    assert!(!all.contains("把重试逻辑定下来") && !all.contains("这不该是我"), "正文不进链");

    for a in [&li, &agent, &admin, &wai] {
        sqlx::query("DELETE FROM account WHERE id = $1").bind(a).execute(&s.db.pool).await.unwrap();
    }
    seed.teardown(&s.db).await;
}
