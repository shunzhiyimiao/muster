//! muster-gateway CLI。
//!
//! 用法:
//!   KIMI_API_KEY=… cargo run -p muster-gateway -- \
//!     --config provider.example.toml --provider kimi --port 8787 \
//!     [--sensitivity internal] [--cloud-max internal] [--actor codex] \
//!     [--audit ~/.muster/gateway-audit.db] [--state-dir ~/.muster]
//!
//! 退出码:2 = 参数/配置错误;1 = 审计链不完整(fail-closed,拒绝启动)或服务异常。

use std::sync::{Arc, Mutex};

use muster_audit::AuditStore;
use muster_gateway::{govern::parse_sensitivity, serve, GatewayState, Governance};
use muster_provider::ProviderRegistry;
use muster_route::{OrgPolicy, Router, Sensitivity};

struct Args {
    config: String,
    provider: Option<String>,
    port: u16,
    sensitivity: Sensitivity,
    cloud_max: Sensitivity,
    actor: String,
    audit: Option<String>,
    state_dir: Option<String>,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        config: "provider.example.toml".into(),
        provider: None,
        port: 8787,
        sensitivity: Sensitivity::Internal,
        cloud_max: Sensitivity::Internal,
        actor: "codex".into(),
        audit: None,
        state_dir: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut take = |n: &str| it.next().ok_or(format!("{n} 需要一个值"));
        match flag.as_str() {
            "--config" => a.config = take("--config")?,
            "--provider" => a.provider = Some(take("--provider")?),
            "--port" => a.port = take("--port")?.parse().map_err(|e| format!("--port: {e}"))?,
            "--sensitivity" => {
                let v = take("--sensitivity")?;
                a.sensitivity = parse_sensitivity(&v).ok_or(format!("--sensitivity 只能是 open / internal / restricted,得到 `{v}`"))?;
            }
            "--cloud-max" => {
                let v = take("--cloud-max")?;
                a.cloud_max = parse_sensitivity(&v).ok_or(format!("--cloud-max 只能是 open / internal / restricted,得到 `{v}`"))?;
            }
            "--actor" => a.actor = take("--actor")?,
            "--audit" => a.audit = Some(take("--audit")?),
            "--state-dir" => a.state_dir = Some(take("--state-dir")?),
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(a)
}

fn home_dir() -> Option<String> {
    std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok())
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
    // 默认 provider 只是偏好:restricted 时路由会改落本地,落不下就拒绝
    let default_provider = match &args.provider {
        Some(id) => {
            if registry.get(id).is_none() {
                eprintln!("配置中不存在 provider `{id}`(现有:{:?})", registry.ids());
                std::process::exit(2);
            }
            Some(id.clone())
        }
        None => registry.default_provider().map(|p| p.metadata().id.clone()),
    };
    if default_provider.is_none() {
        eprintln!("配置未声明 default,且未用 --provider 指定");
        std::process::exit(2);
    }
    let policy = match OrgPolicy::new(args.cloud_max) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("--cloud-max 不合法:{e}");
            std::process::exit(2);
        }
    };
    let router = Arc::new(Router::from_registry(&registry, policy));

    // 审计链:网关自己的一条(节点各自留链)。链不完整就拒绝启动——证据层坏了不能接着记
    let state_dir = args.state_dir.clone().or_else(|| home_dir().map(|h| format!("{h}/.muster"))).unwrap_or_else(|| ".".into());
    if let Err(e) = std::fs::create_dir_all(&state_dir) {
        eprintln!("创建状态目录 {state_dir} 失败:{e}");
        std::process::exit(2);
    }
    let audit_path = args.audit.clone().unwrap_or_else(|| format!("{state_dir}/gateway-audit.db"));
    let audit = match AuditStore::open(&audit_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("审计库 {audit_path} 打开失败:{e}");
            std::process::exit(1);
        }
    };
    match audit.verify_chain() {
        Ok(Ok(n)) => eprintln!("审计链完整({n} 行)· {audit_path}"),
        Ok(Err(e)) => {
            eprintln!("审计链不完整,拒绝启动(fail-closed):{e:?}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("审计链校验失败:{e}");
            std::process::exit(1);
        }
    }
    let gov = Governance::new(
        router,
        default_provider,
        Arc::new(Mutex::new(audit)),
        args.sensitivity,
        args.actor.clone(),
        format!("gateway:{}", args.port),
        Some(std::path::PathBuf::from(format!("{state_dir}/gateway-ratchet.json"))),
    );
    eprintln!("密级底线 {:?}(含棘轮:{:?})· 云端上限 {:?} · actor {}", args.sensitivity, gov.floor_now(), args.cloud_max, args.actor);

    if let Err(e) = serve(GatewayState::governed(gov), args.port).await {
        eprintln!("服务异常退出:{e}");
        std::process::exit(1);
    }
}
