//! # muster-server — 团队协作服务端(P2/P3)
//!
//! 一句话:**把单机的智能协作平台变成一个组织能共用的智能协作平台**,而不把主权让出去。
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
//! 本版**不做服务质量**,以下全部是已知欠账,不是疏忽。每条欠账都带**负责人 · 复查条件**
//! (2026-10-29 块 4 起):没有负责人的欠账不会自己消失,没有复查条件的欠账永远「下次再说」。
//!
//! - **无 Transactional Outbox**:消息落库与 SSE 广播不在同一事务里,崩溃窗口内可能
//!   「落了库没广播」。客户端重连带 `Last-Event-ID` 补拉能纠正它,所以列进下面的「明确不做」。
//!   负责人 Li · 复查条件:收到第一例「别人发了我没看到、刷新才有」的实际报告,或服务端要起第二个实例。
//! - ~~无断线补拉~~ 已由 SSE 的 `Last-Event-ID` + 全局 `stream_seq` 解决(C2)。
//!   ~~仍缺的是幂等~~ 已解决(2026-10-29 块 1):`POST /channels/:cid/messages` 认
//!   `Idempotency-Key` 头,同作者同键只落一条、不重复广播(`message.rs`)。
//! - ~~令牌走 query 参数~~ `/events` 已改为一次性订阅票(`POST /events/ticket` 换票,
//!   60 秒、用一次即作废,服务端只存哈希)。**`?token=` 兼容路径仍在**,用一次 warn 一次。
//!   负责人 Li · 复查条件:桌面壳下一版发布后删除。录像回放 `GET /recordings/:rid/file?token=`
//!   仍是登录令牌走 query——`<video>` 要发多次 Range 请求,一次性票不适用,待换成短期回放票。
//!   负责人 Li · 复查条件:会议线阶段 2 收口时。
//! - ~~`/ws` 仍在,迁完即删~~ 已删(2026-09-23):实时通道只剩 SSE 一条(`events.rs`)。
//! - **无节点链锚定**(设计已定:节点上报事件头 `event_id+hash+prev_hash`、
//!   不含 payload,服务端串链校验后记 `chain.anchor`)。**在它落地之前,
//!   组织级篡改检测等于没有**——节点自己的链仍然可验,但没人替组织盯着。
//!   负责人 Li · 复查条件:节点设计第 3 步(私有 Agent)开工前必须先做——私有 Agent 的产出只在
//!   本人节点的链上,不锚定组织侧就什么都看不见。
//! - ~~无速率限制~~ 登录已限流(账号 10 次/5 分钟、IP 60 次/5 分钟,`ratelimit.rs`);
//!   其余接口仍无限流,也无连接数上限、无鉴权重放防护。
//!   负责人 Li · 复查条件:muster.top 放进第一个组织外的用户(外部协作者或公开注册)之前。
//! - 服务端**没有图形化管理后台**——这是取舍不是欠账:管理走
//!   `examples/admin.rs`(CLI,可脚本化、SSH 进服务器即可用、不新增攻击面)。
//!   桌面壳接上服务端后会再有一套图形化编制管理,两者走同一套 HTTP 接口。不设复查。
//! - 密码用 Argon2 存,但**没有密码策略、没有锁定、没有二因素**。
//!   负责人 Li · 复查条件:阶段 8 接 OIDC 时交给 IdP;若 OIDC 排到组织外用户进来之后,先补锁定。
//! - 会议线(2026-10-08 起)**已进服务端链**:建会 / 入会 / 离会 / 结束 / 每行纪要的哈希 /
//!   密级抬升 / 请 Agent,先记账再动库(见 `meeting.rs`)。仍缺:掉线不会记 leave
//!   (只有客户端主动离开才记),LiveKit 侧的真实在场要靠它的 webhook,未接。
//!   负责人 Li · 复查条件:2 真人 + Agent 实测时出席记录与实际对不上,就接 webhook。
//! - **节点(块 2,2026-10-29)**:会议 daemon 以节点身份入网、签 challenge 换会话、以值班工牌
//!   行事(`node.rs`)。仍留着的**口令登录路径**(daemon 的 MUSTER_ACCOUNT / MUSTER_PASSWORD,
//!   服务端这边就是普通账号登录):负责人 Li · 复查条件:muster.top 切到节点身份后删除
//!   (块 4 部署即满足,删除排进下一个改动)。未做:节点吊销接口(status=revoked 只能改库),
//!   负责人 Li · 复查条件:第二台节点入网时;节点公钥签审计锚点(边界三),与上面锚定那条同期;
//!   challenge 与票仓一样是进程内存单节点,负责人 Li · 复查条件:服务端起第二个实例时。
//! - **派发单(块 3,2026-10-29)**:行动项的确认权收窄到被派到的本人(`action.rs`,迁移 0004):
//!   值班 Agent 只提案,`owner_hint` 解析成 `assignee`(候选只有能在该频道说话的人,拿不准就空着),
//!   主持人 / 组织管理员指派,接受 / 驳回只认本人、驳回必须带理由,proposed 满 72 小时转 expired、
//!   在频道提醒主持人、不自动转派;指派、裁决、过期全进链。**明确不做**:`accepted(private)`
//!   (交给私有 Agent,节点设计第 3 步)。欠账:候选解析逐人跑 `can()`(负责人 Li · 复查条件:
//!   账号过 200 时改成一次查询);过期扫描是进程内 600 秒定时器,多实例会重复提醒(负责人 Li ·
//!   复查条件:服务端起第二个实例时);提醒发到会议所属频道而不是主持人私信——还没有私信
//!   (负责人 Li · 复查条件:私信落地后)。
//! - 录制(`recording.rs`)靠自托管的 LiveKit Egress 写本地文件,服务端**轮询**它的状态
//!   收口(没接 egress webhook);录像文件服务端只按 Range 回放,没有转码、没有保留期。
//!   负责人 Li · 复查条件:录像盘占到一半,或用户定下保留期与导出口径(用户待决项)。
//!   翻译与总结的模型调用发生在会议 Agent 进程里,**那边目前没有自己的审计链**——
//!   服务端链上有翻译行与总结的哈希,但"用的哪个模型、出没出网"只在 Agent 日志里。
//!   负责人 Li · 复查条件:节点链锚定落地时一并——组织节点的链就是会议 Agent 的链。
//!
//! ### 明确不做(2026-10-26 改造计划定下;写在这里是为了别偷偷开工)
//!
//! 私有 Agent(节点设计第 3 步,含 `accepted(private)` 与 `badge.scope/owner`)· Transactional
//! Outbox(断线补拉已兜住大部分)· GHCR 镜像(仍是 rsync 源码到服务器上编)· Agent 产品手册 /
//! 剧本 · 外部 Agent · 手机确认 · 动作档位。这些不是忘了,是决定了不做;要开工先改这份清单。
//!
//! 上线前必须逐条清掉——这份清单存在的意义就是别让"先跑通"悄悄变成"就这样了"。

pub mod action;
pub mod audit;
pub mod auth;
pub mod db;
pub mod events;
pub mod livekit;
pub mod meeting;
pub mod message;
pub mod node;
pub mod org;
pub mod provider;
pub mod ratelimit;
pub mod recording;
pub mod routes;
pub mod signup;
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
    #[error("请求过于频繁:{0}")]
    TooManyRequests(String),
    #[error("冲突:{0}")]
    Conflict(String),
}

impl axum::response::IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let code = match &self {
            ServerError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            ServerError::Forbidden(_) => StatusCode::FORBIDDEN,
            ServerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ServerError::NotFound(_) => StatusCode::NOT_FOUND,
            ServerError::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            ServerError::Conflict(_) => StatusCode::CONFLICT,
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
