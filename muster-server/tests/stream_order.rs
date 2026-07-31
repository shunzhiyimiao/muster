//! C2 的核心不变量:**全局序号的顺序 == 可见顺序**。
//!
//! 这条不成立的话,SSE 用 `Last-Event-ID` 恢复就会**永久跳过**某些消息——
//! 拿到序号 5 的事务若比拿到 6 的晚提交,消费者按 `> last` 拉取时,
//! 在 6 已可见之后再也不会去看 5。这个坑在 outbox/CDC 里很经典,
//! 而它丢的是消息。
//!
//! 需要一个活着的 PostgreSQL:
//! ```bash
//! DATABASE_URL=postgres://muster:muster@localhost:5433/muster \
//!   cargo test -p muster-server --test stream_order -- --ignored
//! ```
//! 默认 `#[ignore]`,免得 `cargo test --workspace` 依赖外部服务
//! (本仓的约定是"动手前后都跑 cargo test")。

use muster_server::db::Db;

async fn db() -> Option<Db> {
    let url = std::env::var("DATABASE_URL").ok()?;
    Db::connect(&url).await.ok()
}

/// 并发插入大量消息,验证:序号连续、无重复、且**按序号读出的顺序与
/// 逐条可见的顺序一致**。
#[tokio::test]
#[ignore = "需要 DATABASE_URL 指向一个活着的 PostgreSQL"]
async fn stream_seq_is_gapless_under_concurrency() {
    let Some(db) = db().await else {
        eprintln!("跳过:未设 DATABASE_URL");
        return;
    };
    let team = format!("t{}", uuid::Uuid::new_v4().simple());
    let chan = format!("c{}", uuid::Uuid::new_v4().simple());
    sqlx::query("INSERT INTO team(id,name,created_ms) VALUES($1,$1,0)")
        .bind(&team)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO channel(id,team_id,name,level,private,created_ms) VALUES($1,$2,$1,'open',false,0)")
        .bind(&chan)
        .bind(&team)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO channel_cursor(channel_id,next_seq) VALUES($1,1)")
        .bind(&chan)
        .execute(&db.pool)
        .await
        .unwrap();

    // 并发写:发号必须仍然串行
    const N: usize = 60;
    let mut tasks = Vec::new();
    for i in 0..N {
        let (db, chan) = (db.clone(), chan.clone());
        tasks.push(tokio::spawn(async move {
            muster_server::message::insert(&db, &chan, "tester", "user", &format!("m{i}"), None)
                .await
                .unwrap()
        }));
    }
    let mut seqs: Vec<i64> = Vec::new();
    for t in tasks {
        seqs.push(t.await.unwrap().stream_seq);
    }

    // 无重号
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), N, "全局序号出现重复:{seqs:?}");

    // 连续:这一批占据的是一段连号区间
    let (lo, hi) = (*sorted.first().unwrap(), *sorted.last().unwrap());
    assert_eq!(hi - lo + 1, N as i64, "全局序号有空洞:{lo}..={hi} 只有 {N} 条");

    // 按序号读回来,与写入的集合一致
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT stream_seq FROM message WHERE channel_id=$1 ORDER BY stream_seq ASC",
    )
    .bind(&chan)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let read: Vec<i64> = rows.into_iter().map(|r| r.0).collect();
    assert_eq!(read, sorted, "按 stream_seq 读出的顺序应当就是连号顺序");

    // 每频道序号同样无空洞(补拉靠它判断有没有漏收)
    let ch: Vec<(i64,)> = sqlx::query_as(
        "SELECT channel_seq FROM message WHERE channel_id=$1 ORDER BY channel_seq ASC",
    )
    .bind(&chan)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let ch: Vec<i64> = ch.into_iter().map(|r| r.0).collect();
    assert_eq!(ch, (1..=N as i64).collect::<Vec<_>>(), "channel_seq 应当是 1..N 连号");
}
