//! collab-server 入口。
//!
//! 配置缺失一律**快速失败**,不用默认值悄悄连到别处去——与 provider 密钥
//! 同一姿态(见 muster-server/src/lib.rs)。

use muster_server::{db::Db, routes, ws::Hub};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "muster_server=debug,tower_http=info".into()),
        )
        .init();

    // 先验密钥再连库:配置错就别浪费一次连接,也别让人以为"连上了就没问题"
    if let Err(e) = muster_server::auth::secret() {
        eprintln!("启动失败:{e}");
        std::process::exit(2);
    }
    let db = match Db::from_env().await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("启动失败:{e}");
            std::process::exit(2);
        }
    };

    let bind = std::env::var("MUSTER_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("启动失败:无法监听 {bind}:{e}");
            std::process::exit(2);
        }
    };
    tracing::info!("muster-server 监听 {bind}");

    let app = routes::app(db, Hub::new());
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("服务异常退出:{e}");
        std::process::exit(1);
    }
}
