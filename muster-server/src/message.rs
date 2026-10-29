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
//!
//! ## 幂等(块 1,2026-10-29)
//!
//! 客户端重试是常态(断网、超时、点了两下),没有幂等就是同一句话落两条。
//! `POST` 带可选头 `Idempotency-Key`(≤128 字符,客户端每条消息生成一个 UUID,
//! 重试沿用):同一作者同一 key 再来一次,返回**第一次的结果**(同一 id、同一
//! stream_seq),不落第二条、不再广播。靠 `(author_id, idempotency_key)` 的部分唯一
//! 索引兜底并发——两条同 key 的请求同时进来,输的那条撞唯一约束,**事务回滚连带把
//! 已取的序号退回去**(发号与插入同事务,正是为此),然后去读赢的那条。

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use muster_identity::{Action, Scope};

use crate::auth::Identity;
use crate::db::now_ms;
use crate::org::require;
use crate::ws::Hub;
use crate::{Db, Result, ServerError};

/// `Idempotency-Key` 头的上限。够放 UUID 和带前缀的键,不够放正文。
pub const IDEMPOTENCY_KEY_MAX: usize = 128;

/// 从请求头取幂等键:没有就是 `None`;有但不合规(空、超长、非可见 ASCII)就拒——
/// 一个被静默丢掉的键会让客户端以为自己是幂等的。
pub fn idempotency_key_from(headers: &HeaderMap) -> Result<Option<String>> {
    let Some(v) = headers.get("idempotency-key") else { return Ok(None) };
    let k = v
        .to_str()
        .map_err(|_| ServerError::BadRequest("Idempotency-Key 必须是可见 ASCII".into()))?
        .trim();
    if k.is_empty() {
        return Err(ServerError::BadRequest("Idempotency-Key 不能为空".into()));
    }
    if k.len() > IDEMPOTENCY_KEY_MAX || !k.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(ServerError::BadRequest(format!("Idempotency-Key 最多 {IDEMPOTENCY_KEY_MAX} 个可见 ASCII 字符")));
    }
    Ok(Some(k.to_string()))
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MessageOut {
    pub id: Uuid,
    pub channel_id: String,
    pub channel_seq: i64,
    /// 全局投递序号(C2):SSE 的事件 id 就是它。**与 channel_seq 是两回事**——
    /// 前者用于"一条连接复用所有频道"时的恢复,后者是单频道的无空洞校验。
    #[serde(default)]
    pub stream_seq: i64,
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
        "SELECT id, channel_id, channel_seq, COALESCE(stream_seq,0) AS stream_seq,
                author_id, role, body, run_id, ts_ms
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
    headers: HeaderMap,
    Json(m): Json<NewMessage>,
) -> Result<Json<MessageOut>> {
    require(&db, &id, &Action::SendMessage, &Scope::Channel(cid.clone())).await?;
    let key = idempotency_key_from(&headers)?;
    let (out, created) =
        insert_with_key(&db, &cid, &id.account_id, &m.role, &m.body, m.run_id.as_deref(), key.as_deref()).await?;
    // 无 Outbox:落库与广播不在同一事务,崩溃窗口内可能"落了库没广播"。
    // 已登记在 lib.rs 的诚实边界里,SSE 的 Last-Event-ID 补拉能纠正它。
    // 重放的请求不再广播:第一次已经广播过,再广播就是界面上的重复消息。
    if created {
        hub.broadcast(&cid, &out);
    }
    Ok(Json(out))
}

/// 落一条消息并发号(无幂等键)。
pub async fn insert(
    db: &Db,
    channel_id: &str,
    author_id: &str,
    role: &str,
    body: &str,
    run_id: Option<&str>,
) -> Result<MessageOut> {
    insert_with_key(db, channel_id, author_id, role, body, run_id, None).await.map(|(m, _)| m)
}

const SELECT_COLS: &str = "id, channel_id, channel_seq, COALESCE(stream_seq,0) AS stream_seq,
                           author_id, role, body, run_id, ts_ms";

async fn find_by_key(db: &Db, author_id: &str, key: &str) -> Result<Option<MessageOut>> {
    Ok(sqlx::query_as::<_, MessageOut>(&format!(
        "SELECT {SELECT_COLS} FROM message WHERE author_id = $1 AND idempotency_key = $2"
    ))
    .bind(author_id)
    .bind(key)
    .fetch_optional(&db.pool)
    .await?)
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error().and_then(|d| d.code()).map(|c| c == "23505").unwrap_or(false)
}

/// 落一条消息并发号。**发号与插入同事务**:拿到号却没插进去会留下空洞,
/// 而补拉是靠"无空洞"判断有没有漏收的。
///
/// 返回 `(消息, 是否新落的)`:带幂等键且同作者同键已存在时,返回已有那条且第二项为
/// `false`——调用方据此决定要不要广播。
pub async fn insert_with_key(
    db: &Db,
    channel_id: &str,
    author_id: &str,
    role: &str,
    body: &str,
    run_id: Option<&str>,
    idempotency_key: Option<&str>,
) -> Result<(MessageOut, bool)> {
    if let Some(k) = idempotency_key {
        if let Some(existing) = find_by_key(db, author_id, k).await? {
            return Ok((existing, false));
        }
    }
    let mut tx = db.pool.begin().await?;
    // UPDATE ... RETURNING 拿到行锁并取号,并发下天然串行
    let (seq,): (i64,) = sqlx::query_as(
        "UPDATE channel_cursor SET next_seq = next_seq + 1 WHERE channel_id = $1 RETURNING next_seq - 1",
    )
    .bind(channel_id)
    .fetch_one(&mut *tx)
    .await?;
    // 全局序号同事务分配:锁持有到提交 ⇒ **加锁顺序即提交顺序**,
    // 于是不会出现"序号小的后可见"而被 `> last` 永久跳过(见迁移里的注释)
    let (stream_seq,): (i64,) = sqlx::query_as(
        "UPDATE stream_cursor SET next_seq = next_seq + 1 WHERE id = 1 RETURNING next_seq - 1",
    )
    .fetch_one(&mut *tx)
    .await?;

    let id = Uuid::new_v4();
    let ts = now_ms();
    let inserted = sqlx::query(
        "INSERT INTO message(id, channel_id, channel_seq, author_id, role, body, run_id, ts_ms, stream_seq, idempotency_key)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(channel_id)
    .bind(seq)
    .bind(author_id)
    .bind(role)
    .bind(body)
    .bind(run_id)
    .bind(ts)
    .bind(stream_seq)
    .bind(idempotency_key)
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(_) => tx.commit().await?,
        Err(e) if idempotency_key.is_some() && is_unique_violation(&e) => {
            // 并发撞上同一把键:对方先落了。回滚(序号随事务退回,不留空洞),读赢的那条
            tx.rollback().await?;
            let key = idempotency_key.expect("上面判过");
            return find_by_key(db, author_id, key)
                .await?
                .map(|m| (m, false))
                .ok_or_else(|| ServerError::Internal("幂等键撞了唯一约束却读不到已有的那条".into()));
        }
        Err(e) => return Err(e.into()),
    }

    Ok((
        MessageOut {
            id,
            channel_id: channel_id.to_string(),
            channel_seq: seq,
            stream_seq,
            author_id: author_id.to_string(),
            role: role.to_string(),
            body: body.to_string(),
            run_id: run_id.map(String::from),
            ts_ms: ts,
        },
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(v: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = v {
            h.insert("idempotency-key", v.parse().unwrap());
        }
        h
    }

    /// 没有头 = 不幂等(老客户端照旧);有头就必须合规,不合规要拒而不是静默丢掉。
    #[test]
    fn idempotency_key_header_is_validated() {
        assert_eq!(idempotency_key_from(&headers(None)).unwrap(), None);
        assert_eq!(
            idempotency_key_from(&headers(Some(" 3f1c2d3e-0000-4000-8000-000000000001 "))).unwrap().as_deref(),
            Some("3f1c2d3e-0000-4000-8000-000000000001")
        );
        assert!(idempotency_key_from(&headers(Some(""))).is_err(), "空键");
        assert!(idempotency_key_from(&headers(Some(&"k".repeat(IDEMPOTENCY_KEY_MAX + 1)))).is_err(), "超长");
        assert!(idempotency_key_from(&headers(Some("a b"))).is_err(), "带空格");
        assert!(idempotency_key_from(&headers(Some(&"k".repeat(IDEMPOTENCY_KEY_MAX)))).is_ok(), "刚好上限");
    }
}
