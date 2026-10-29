//! 块 1(2026-10-29):消息 POST 幂等(`Idempotency-Key`)。
//!
//! ```bash
//! DATABASE_URL=postgres://muster:muster@localhost:5433/muster \
//!   cargo test -p muster-server --test message_idempotency_e2e -- --ignored
//! ```

mod common;

use serde_json::json;

use common::{boot, seed, token};

async fn count(s: &common::Server, chan: &str) -> i64 {
    sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM message WHERE channel_id = $1")
        .bind(chan)
        .fetch_one(&s.db.pool)
        .await
        .unwrap()
        .0
}

/// 同作者同键:并发两次只落一条,两次拿到的是同一条(同 id、同 stream_seq);
/// 不同作者同键互不影响;没带键的照旧每次都落。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn same_key_lands_once_even_when_concurrent() {
    let Some(s) = boot().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let seed = seed(&s.db, "open").await;
    let (ta, tb) = (token(&seed.alice), token(&seed.bob));
    let path = format!("/channels/{}/messages", seed.chan);
    let key = "3f1c2d3e-0000-4000-8000-000000000001";

    // 并发两次同键
    let hdr = [("Idempotency-Key", key)];
    let (r1, r2) = tokio::join!(
        s.post_with(&path, &ta, json!({ "body": "重试了一次" }), &hdr),
        s.post_with(&path, &ta, json!({ "body": "重试了一次" }), &hdr),
    );
    assert_eq!((r1.0, r2.0), (200, 200), "{:?} {:?}", r1.1, r2.1);
    assert_eq!(r1.1["id"], r2.1["id"], "两次拿到同一条");
    assert_eq!(r1.1["stream_seq"], r2.1["stream_seq"]);
    assert_eq!(count(&s, &seed.chan).await, 1, "只落一条");

    // 第三次(串行重试)还是那一条
    let (st, r3) = s.post_with(&path, &ta, json!({ "body": "重试了一次" }), &[("Idempotency-Key", key)]).await;
    assert_eq!(st, 200);
    assert_eq!(r3["id"], r1.1["id"]);
    assert_eq!(count(&s, &seed.chan).await, 1);

    // 不同作者同键:各落各的
    let (st, rb) = s.post_with(&path, &tb, json!({ "body": "我也用这个键" }), &[("Idempotency-Key", key)]).await;
    assert_eq!(st, 200, "{rb}");
    assert_ne!(rb["id"], r1.1["id"]);
    assert_eq!(count(&s, &seed.chan).await, 2);

    // 同作者不同键:新的一条;没带键:每次都落
    let (st, _) = s.post_with(&path, &ta, json!({ "body": "另一句" }), &[("Idempotency-Key", "k-2")]).await;
    assert_eq!(st, 200);
    let (st, _) = s.post(&path, &ta, json!({ "body": "没带键" })).await;
    assert_eq!(st, 200);
    let (st, _) = s.post(&path, &ta, json!({ "body": "没带键" })).await;
    assert_eq!(st, 200);
    assert_eq!(count(&s, &seed.chan).await, 5);

    // 频道序号无空洞:并发输掉的那条把序号退回去了
    let seqs: Vec<(i64,)> = sqlx::query_as("SELECT channel_seq FROM message WHERE channel_id = $1 ORDER BY channel_seq")
        .bind(&seed.chan)
        .fetch_all(&s.db.pool)
        .await
        .unwrap();
    assert_eq!(seqs.into_iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);

    // 键不合规要拒,不是静默丢掉
    let long = "k".repeat(129);
    let (st, _) = s.post_with(&path, &ta, json!({ "body": "x" }), &[("Idempotency-Key", long.as_str())]).await;
    assert_eq!(st, 400, "超长键");
    let (st, _) = s.post_with(&path, &ta, json!({ "body": "x" }), &[("Idempotency-Key", "")]).await;
    assert_eq!(st, 400, "空键");

    seed.teardown(&s.db).await;
}
