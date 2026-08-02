//! 路由装配。
//!
//! 只有 `/health` 与 `/auth/login` 免鉴权;其余 handler 的参数表里都有
//! [`crate::auth::Identity`]——**忘了写就拿不到身份**,让"忘记鉴权"在类型上
//! 尽量难发生,而不是靠中间件配置对不对。

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;

use crate::{action, events, meeting, message, org, ws, Db};

pub fn app(db: Db, hub: ws::Hub, audit: crate::audit::Audit) -> Router {
    let with_hub = (db.clone(), hub.clone());

    let auth_routes = Router::new()
        .route("/auth/login", post(org::login))
        .route("/whoami", get(org::whoami))
        .route("/teams", get(org::list_teams).post(org::create_team))
        .route("/channels", get(org::list_channels).post(org::create_channel))
        .route("/channels/:cid", get(org::get_channel))
        .route("/accounts", get(org::list_accounts).post(org::create_account))
        .route("/roles", get(org::list_bindings).post(org::grant_role).delete(org::revoke_role))
        .route("/accounts/:aid/disabled", post(org::set_account_disabled))
        .with_state((db.clone(), audit.clone()));

    let chan_routes = Router::new()
        .route("/channels/:cid/messages", get(message::list).post(message::post))
        .route("/channels/:cid/meetings", get(meeting::list).post(meeting::start))
        .route("/meetings/:mid/join", post(meeting::join))
        .route("/meetings/:mid/transcript", get(meeting::transcript).post(meeting::add_transcript))
        .route("/meetings/:mid/level", post(meeting::raise_level))
        .route("/meetings/:mid/end", post(meeting::end))
        .route("/meetings/:mid/agent", post(meeting::set_wants_agent))
        .route("/meetings/agent-wanted", get(meeting::agent_wanted))
        // C2:SSE 取代 WebSocket。/ws 暂留一版供未迁完的客户端过渡,
        // 迁完即删——两条实时通道并存久了,就会有人只修其中一条。
        .route("/events", get(events::handler))
        .route("/ws", get(ws::handler))
        .with_state(with_hub);

    // 行动项:提案由 Agent 落,**确认必须由人**——见 action.rs 的模块文档
    let action_routes = Router::new()
        .route(
            "/meetings/:mid/action-items",
            get(action::list).post(action::propose),
        )
        .route("/action-items/:aid/decide", post(action::decide))
        .route("/action-items/:aid/run", post(action::link_run))
        .with_state((db.clone(), hub.clone(), audit.clone()));

    Router::new()
        .route("/health", get(health))
        .route("/web", get(web_index))
        .route("/web/", get(web_index))
        .route("/app.js", get(web_app_js))
        .merge(auth_routes)
        .merge(chan_routes)
        .merge(action_routes)
        // 桌面壳与浏览器都要连,开发期放开;上线前收敛到白名单(已登记)
        .layer(CorsLayer::permissive())
}

/// 网页参会端。**嵌进二进制**,不读磁盘也不依赖 CDN——
/// 内网部署连不上外网,依赖必须随包走;而多一个静态文件目录就多一处
/// "部署时忘了拷"的失败方式。
///
/// 它只用来**参会**:发消息、开会、看纪要。跑任务、审批、能力库需要本地
/// Runner 与本地审计链,浏览器里做不了,页面上也如实写明了。
async fn web_index() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../../apps/web/dist/index.html"),
    )
}

async fn web_app_js() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        include_str!("../../apps/web/dist/app.js"),
    )
}

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "ok": true,
        "service": "muster-server",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
