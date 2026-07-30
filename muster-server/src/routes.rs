//! 路由装配。
//!
//! 只有 `/health` 与 `/auth/login` 免鉴权;其余 handler 的参数表里都有
//! [`crate::auth::Identity`]——**忘了写就拿不到身份**,让"忘记鉴权"在类型上
//! 尽量难发生,而不是靠中间件配置对不对。

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;

use crate::{meeting, message, org, ws, Db};

pub fn app(db: Db, hub: ws::Hub) -> Router {
    let with_hub = (db.clone(), hub.clone());

    let auth_routes = Router::new()
        .route("/auth/login", post(org::login))
        .route("/whoami", get(org::whoami))
        .route("/teams", get(org::list_teams).post(org::create_team))
        .route("/channels", get(org::list_channels).post(org::create_channel))
        .route("/channels/:cid", get(org::get_channel))
        .route("/accounts", post(org::create_account))
        .route("/roles", post(org::grant_role))
        .with_state(db.clone());

    let chan_routes = Router::new()
        .route("/channels/:cid/messages", get(message::list).post(message::post))
        .route("/channels/:cid/meetings", get(meeting::list).post(meeting::start))
        .route("/meetings/:mid/join", post(meeting::join))
        .route("/meetings/:mid/transcript", post(meeting::add_transcript))
        .route("/meetings/:mid/level", post(meeting::raise_level))
        .route("/meetings/:mid/end", post(meeting::end))
        .route("/ws", get(ws::handler))
        .with_state(with_hub);

    Router::new()
        .route("/health", get(health))
        .merge(auth_routes)
        .merge(chan_routes)
        // 桌面壳与浏览器都要连,开发期放开;上线前收敛到白名单(已登记)
        .layer(CorsLayer::permissive())
}

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "ok": true,
        "service": "muster-server",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
