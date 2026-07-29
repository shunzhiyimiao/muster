//! muster-gateway CLI。
//!
//! 用法:
//!   KIMI_API_KEY=… cargo run -p muster-gateway -- \
//!     --config provider.example.toml --provider kimi --port 8787
//!
//! 退出码:2 = 参数/配置错误。

use std::sync::Arc;

use muster_gateway::{serve, GatewayState};
use muster_provider::{ModelProvider, ProviderRegistry};

struct Args {
    config: String,
    provider: Option<String>,
    port: u16,
}

fn parse() -> Result<Args, String> {
    let mut a = Args { config: "provider.example.toml".into(), provider: None, port: 8787 };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut take = |n: &str| it.next().ok_or(format!("{n} 需要一个值"));
        match flag.as_str() {
            "--config" => a.config = take("--config")?,
            "--provider" => a.provider = Some(take("--provider")?),
            "--port" => a.port = take("--port")?.parse().map_err(|e| format!("--port: {e}"))?,
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(a)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "muster_gateway=info".into()),
        )
        .init();

    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误:{e}");
            std::process::exit(2);
        }
    };
    let toml_text = match std::fs::read_to_string(&args.config) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读取配置 {} 失败:{e}", args.config);
            std::process::exit(2);
        }
    };
    let registry = match ProviderRegistry::from_toml_str(&toml_text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("配置解析失败:{e}");
            std::process::exit(2);
        }
    };
    let provider: Arc<dyn ModelProvider> = match &args.provider {
        Some(id) => match registry.get(id) {
            Some(p) => p,
            None => {
                eprintln!("配置中不存在 provider `{id}`(现有:{:?})", registry.ids());
                std::process::exit(2);
            }
        },
        None => match registry.default_provider() {
            Some(p) => p,
            None => {
                eprintln!("配置未声明 default,且未用 --provider 指定");
                std::process::exit(2);
            }
        },
    };

    if let Err(e) = serve(GatewayState::new(provider), args.port).await {
        eprintln!("服务异常退出:{e}");
        std::process::exit(1);
    }
}
