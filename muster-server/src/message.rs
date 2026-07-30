//! 消息(P3-01)与频道序号(P3-04)。
//!
//! ## channel_seq 是投递序,不是证据序
//!
//! 别拿它去实现哈希链,也别拿哈希链去实现它。两者要求不同:
//! - **证据序**(各节点的 `muster_audit` 哈希链):必须防篡改,必须严格线性。
//! - **投递序**(这里):只需每频道单调无空洞,供断线补拉从某个 seq 之后拉。
//!
//! 发号靠 `channel_cursor` 的行锁,不是 `MAX(seq)+1`——后者在并发下会产生
//! 重号或空洞,而补拉正是靠"无空洞"判断有没有漏收。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use muster_identity::{Action, Scope};

use crate::auth::Identity;
use crate::db::now_ms;
use crate::org::require;
use crate::ws::Hub;
use crate::{Db, Result};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MessageOut {
    pub id: Uuid,
    pub channel_id: String,
    pub channel_seq: i64,
    pub author_id: String,
    pub role: String,
    pub body: String,
    pub run_id: Option<String>,
    pub ts_ms: i64,
}

#[derive(Deserialize)]
pub struct NewMessage {
    pub body: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default = "default_role")]
    pub role: String,
}
fn default_role() -> String {
    "user".into()
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// 只要序号大于它的(断线补拉用);缺省从头。
    #[serde(default)]
    pub after_seq: Option<i64>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}
fn default_limit() -> i64 {
    100
}

pub async fn list(
    State((db, _hub)): State<(Db, Hub)>,
    id: Identity,
    Path(cid): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<MessageOut>>> {
    require(&db, &id, &Action::SendMessage, &Scope::Channel(cid.clone())).await?;
    let limit = q.limit.clamp(1, 500);
    let rows = sqlx::query_as::<_, MessageOut>(
        "SELECT id, channel_id, channel_seq, author_id, role, body, run_id, ts_ms
         FROM message WHERE channel_id = $1 AND channel_seq > $2
         ORDER BY channel_seq ASC LIMIT $3",
    )
    .bind(&cid)
    .bind(q.after_seq.unwrap_or(0))
    .bind(limit)
    .fetch_all(&db.pool)
    .await?;
    Ok(Json(rows))
}

pub async fn post(
    State((db, hub)): State<(Db, Hub)>,
    id: Identity,
    Path(cid): Path<String>,
    Json(m): Json<NewMessage>,
) -> Result<Json<MessageOut>> {
    require(&db, &id, &Action::SendMessage, &Scope::Channel(cid.clone())).await?;
    let out = insert(&db, &cid, &id.account_id, &m.role, &m.body, m.run_id.as_deref()).await?;
    // 无 Outbox:落库与广播不在同一事务,崩溃窗口内可能"落了库没广播"。
    // 已登记在 lib.rs 的诚实边界里,补拉能纠正它——但补拉也还没做。
    hub.broadcast(&cid, &out);
    Ok(Json(out))
}

/// 落一条消息并发号。**发号与插入同事务**:拿到号却没插进去会留下空洞,
/// 而补拉是靠"无空洞"判断有没有漏收的。
pub async fn insert(
    db: &Db,
    channel_id: &str,
    author_id: &str,
    role: &str,
    body: &str,
    run_id: Option<&str>,
) -> Result<MessageOut> {
    let mut tx = db.pool.begin().await?;
    // UPDATE ... RETURNING 拿到行锁并取号,并发下天然串行
    let (seq,): (i64,) = sqlx::query_as(
        "UPDATE channel_cursor SET next_seq = next_seq + 1 WHERE channel_id = $1 RETURNING next_seq - 1",
    )
    .bind(channel_id)
    .fetch_one(&mut *tx)
    .await?;

    let id = Uuid::new_v4();
    let ts = now_ms();
    sqlx::query(
        "INSERT INTO message(id, channel_id, channel_seq, author_id, role, body, run_id, ts_ms)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id)
    .bind(channel_id)
    .bind(seq)
    .bind(author_id)
    .bind(role)
    .bind(body)
    .bind(run_id)
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(MessageOut {
        id,
        channel_id: channel_id.to_string(),
        channel_seq: seq,
        author_id: author_id.to_string(),
        role: role.to_string(),
        body: body.to_string(),
        run_id: run_id.map(String::from),
        ts_ms: ts,
    })
}
