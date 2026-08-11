//! 路由装配。
//!
//! 只有 `/health` 与 `/auth/login` 免鉴权;其余 handler 的参数表里都有
//! [`crate::auth::Identity`]——**忘了写就拿不到身份**,让"忘记鉴权"在类型上
//! 尽量难发生,而不是靠中间件配置对不对。

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::ratelimit::LoginLimiter;
use crate::{action, events, meeting, message, org, provider, ws, Db};

pub fn app(db: Db, hub: ws::Hub, audit: crate::audit::Audit) -> Router {
    let with_hub = (db.clone(), hub.clone());

    let auth_routes = Router::new()
        .route("/auth/login", post(org::login))
        // Provider 目录:读只要已认证(每个跑模型的节点都要),
        // 写要 ChangePolicy(见 provider.rs 模块文档)
        .route("/providers/catalog", get(provider::catalog))
        .route("/providers", get(provider::list_all).post(provider::upsert))
        .route("/providers/:pid", axum::routing::delete(provider::disable))
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
        .layer(axum::Extension(std::sync::Arc::new(LoginLimiter::new())))
        .layer(cors())
}

/// CORS 策略。
///
/// ## 为什么这里必须**显式**配置才能对外开放
///
/// 放开 CORS 的意思是"任何网站都能拿着浏览器去调这个 API 并读到结果"。
/// 在局域网里这只是方便;暴露到公网上,它把每一个 XSS、每一个恶意页面
/// 都变成了对本服务的通道。
///
/// 所以规则是:**要么设白名单,要么只能监听回环**。判定放在
/// [`cors_mode`] 里,是个纯函数,好测——启动时拒绝比上线后补救便宜得多。
fn cors() -> CorsLayer {
    let origins = std::env::var("MUSTER_ALLOWED_ORIGINS").ok();
    let bind = std::env::var("MUSTER_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    match cors_mode(origins.as_deref(), &bind) {
        Ok(None) => CorsLayer::permissive(),
        Ok(Some(list)) => CorsLayer::new()
            .allow_origin(AllowOrigin::list(
                list.iter().filter_map(|o| o.parse().ok()).collect::<Vec<_>>(),
            ))
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any),
        Err(e) => {
            eprintln!("启动失败:{e}");
            std::process::exit(2);
        }
    }
}

/// `Ok(None)` = 可以放开(只监听回环);`Ok(Some(白名单))` = 按名单;
/// `Err` = 对外监听却没给名单,**不许启动**。
pub fn cors_mode(origins: Option<&str>, bind: &str) -> Result<Option<Vec<String>>, String> {
    let list: Vec<String> = origins
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !list.is_empty() {
        return Ok(Some(list));
    }
    // 只看主机部分:回环地址上没有"别的网站"能碰到它
    let host = bind.rsplit_once(':').map(|(h, _)| h).unwrap_or(bind);
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]");
    if loopback {
        Ok(None)
    } else {
        Err(format!(
            "监听 {bind}(非回环)却没有设 MUSTER_ALLOWED_ORIGINS。\n  \
             放开 CORS 等于让任何网站都能拿着浏览器调这个 API。\n  \
             设成你的站点来源,例如:MUSTER_ALLOWED_ORIGINS=https://muster.example.com"
        ))
    }
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

#[cfg(test)]
mod cors_tests {
    use super::cors_mode;

    /// 对外监听却没给白名单 ⇒ **不许启动**。
    ///
    /// 这条是这次上公网加的最要紧的一道闸。放开 CORS 在局域网里只是方便,
    /// 暴露到公网上,它把每一个 XSS、每一个恶意页面都变成对本服务的通道。
    /// 而这种事故不会有任何症状——一切正常工作,直到出事。
    #[test]
    fn refuses_to_start_when_public_without_an_allowlist() {
        for bind in ["0.0.0.0:8787", "192.168.3.59:8787", "[::]:8787"] {
            assert!(cors_mode(None, bind).is_err(), "{bind} 没有白名单不该放行");
            assert!(cors_mode(Some(""), bind).is_err(), "空白名单等同于没设");
            assert!(cors_mode(Some("  , ,"), bind).is_err(), "只有分隔符也等同于没设");
        }
    }

    /// 回环地址不必设:那上面没有"别的网站"能碰到它,
    /// 强制配置只会让本机开发平白多一步。
    #[test]
    fn loopback_may_stay_open() {
        for bind in ["127.0.0.1:8787", "localhost:8787", "[::1]:8787"] {
            assert_eq!(cors_mode(None, bind), Ok(None), "{bind}");
        }
    }

    #[test]
    fn allowlist_is_parsed_and_trimmed() {
        assert_eq!(
            cors_mode(Some("https://a.example.com, https://b.example.com"), "0.0.0.0:8787"),
            Ok(Some(vec!["https://a.example.com".into(), "https://b.example.com".into()]))
        );
        // 给了名单,回环也照名单来——显式配置优先于推断
        assert_eq!(
            cors_mode(Some("https://a.example.com"), "127.0.0.1:8787"),
            Ok(Some(vec!["https://a.example.com".into()]))
        );
    }
}
