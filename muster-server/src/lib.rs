//! # muster-server — 团队协作服务端(P2/P3)
//!
//! 一句话:**把单机的点将台变成一个组织能共用的点将台**,而不把主权让出去。
//!
//! ## 关键设计决策
//!
//! 1. **权限语义不在这里重写**。`muster_identity::can()` 是纯函数内核,连同它
//!    12,150 组穷举测试一起搬过来用。服务端只负责**把请求翻译成 Principal +
//!    Action + Scope**,判定仍归那个内核。判定逻辑一旦有两份实现,
//!    「桌面端说能、服务端说不能」这种问题就永远查不清。
//!
//! 2. **服务端不持有源码**。代码、worktree、diff 都留在开发者机器上,
//!    服务端只知道「有一份 diff,哈希是 X,在节点 Y 上」。审批在这里裁决,
//!    合入由持有那份 worktree 的节点执行。
//!    *(遗留冲突:审批人要看 diff。三条路见 docs;当前先只传哈希与统计。)*
//!
//! 3. **审计链仍留在节点**。服务端不做全局链——每次 append 一次网络往返、
//!    且必须串行化,断网即不能干活(fail-closed 的必然后果),这对本地优先
//!    的产品是致命的。组织级防篡改由**节点链 + 服务端锚定**解决,
//!    见「诚实边界」。
//!
//! 4. **OIDC 是插拔点,不是前提**。当前内置账号 + JWT;
//!    [`auth::Identity`] 是唯一的身份入口,接企业 IdP 时只换它的实现。
//!
//! ## 诚实边界(v0 —— 明确以「先跑通」为目标)
//!
//! 本版**不做服务质量**,以下全部是已知欠账,不是疏忽:
//!
//! - **无 Transactional Outbox**:消息落库与 WebSocket 广播不在同一事务里,
//!   崩溃窗口内可能「落了库没广播」。补拉能纠正它,但补拉也还没做。
//! - **无断线补拉与幂等**(P3-05):`channel_seq` 已经在写了,消费侧还没用。
//! - **无节点链锚定**(设计已定:节点上报事件头 `event_id+hash+prev_hash`、
//!   不含 payload,服务端串链校验后记 `chain.anchor`)。**在它落地之前,
//!   组织级篡改检测等于没有**——节点自己的链仍然可验,但没人替组织盯着。
//! - **无速率限制、无连接数上限、无鉴权重放防护**。
//! - 服务端**没有图形化管理后台**——这是取舍不是欠账:管理走
//!   `examples/admin.rs`(CLI,可脚本化、SSH 进服务器即可用、不新增攻击面)。
//!   桌面壳接上服务端后会再有一套图形化编制管理,两者走同一套 HTTP 接口。
//! - 密码用 Argon2 存,但**没有密码策略、没有锁定、没有二因素**。
//!
//! 上线前必须逐条清掉——这份清单存在的意义就是别让"先跑通"悄悄变成"就这样了"。

pub mod action;
pub mod audit;
pub mod auth;
pub mod db;
pub mod livekit;
pub mod meeting;
pub mod message;
pub mod org;
pub mod routes;
pub mod ws;

pub use db::Db;

/// 服务端错误。对外一律翻译成 HTTP 状态码 + 中文说明,不泄漏内部细节。
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("数据库错误:{0}")]
    Db(#[from] sqlx::Error),
    #[error("未认证:{0}")]
    Unauthenticated(String),
    #[error("无权限:{0}")]
    Forbidden(String),
    #[error("请求非法:{0}")]
    BadRequest(String),
    #[error("找不到:{0}")]
    NotFound(String),
    #[error("内部错误:{0}")]
    Internal(String),
}

impl axum::response::IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let code = match &self {
            ServerError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            ServerError::Forbidden(_) => StatusCode::FORBIDDEN,
            ServerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ServerError::NotFound(_) => StatusCode::NOT_FOUND,
            // 数据库与内部错误不把原文吐给客户端(可能含连接串、表结构)
            ServerError::Db(e) => {
                tracing::error!("db error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            ServerError::Internal(e) => {
                tracing::error!("internal error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let msg = match &self {
            ServerError::Db(_) | ServerError::Internal(_) => "服务端内部错误".to_string(),
            other => other.to_string(),
        };
        (code, axum::Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

pub type Result<T> = std::result::Result<T, ServerError>;
