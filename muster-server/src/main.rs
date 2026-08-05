//! collab-server 入口。
//!
//! 配置缺失一律**快速失败**,不用默认值悄悄连到别处去——与 provider 密钥
//! 同一姿态(见 muster-server/src/lib.rs)。

use muster_server::{audit::Audit, db::Db, routes, ws::Hub};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "muster_server=debug,tower_http=info".into()),
        )
        .init();

    // 配置一次验完,再碰任何外部依赖。
    //
    // 分散着验的坏处在一台新服务器上才显出来:连库要几秒、失败了才轮到下一项,
    // 于是**一次重启只能发现一个配置错误**,改完再来一遍。把纯配置的检查
    // 集中在最前面,一次把话说全。
    let bind = std::env::var("MUSTER_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let mut bad: Vec<String> = Vec::new();
    if let Err(e) = muster_server::auth::secret() {
        bad.push(e);
    }
    if let Err(e) = routes::cors_mode(std::env::var("MUSTER_ALLOWED_ORIGINS").ok().as_deref(), &bind)
    {
        bad.push(e);
    }
    if !bad.is_empty() {
        eprintln!("启动失败,配置有 {} 处问题:", bad.len());
        for (i, e) in bad.iter().enumerate() {
            eprintln!("  {}. {e}", i + 1);
        }
        std::process::exit(2);
    }
    let db = match Db::from_env().await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("启动失败:{e}");
            std::process::exit(2);
        }
    };

    // 服务端自己的审计链(它也是一个节点)。链坏了拒绝启动——
    // 与桌面壳同一姿态,不在坏账本上继续记账。
    let chain_path = std::env::var("MUSTER_SERVER_AUDIT_DB")
        .unwrap_or_else(|_| "./muster-server-audit.db".into());
    let audit = match Audit::open(&chain_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("启动失败:{e}");
            std::process::exit(2);
        }
    };

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("启动失败:无法监听 {bind}:{e}");
            std::process::exit(2);
        }
    };
    tracing::info!("muster-server 监听 {bind}");

    let app = routes::app(db, Hub::new(), audit);
    // with_connect_info:限流要按来源 IP 计数,没有它拿不到对端地址
    let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("服务异常退出:{e}");
        std::process::exit(1);
    }
}
