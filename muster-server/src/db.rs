//! PostgreSQL 连接与迁移。
//!
//! 用运行时校验的 `sqlx::query`(而非 `query!` 宏),故 **`cargo build` 不需要
//! 一个活着的数据库**——本仓的约定是"动手前后都跑 cargo test",不能让编译
//! 依赖上外部服务。代价是 SQL 的类型错误留到运行时才发现,用集成测试兜。

use sqlx::postgres::{PgPool, PgPoolOptions};

#[derive(Clone)]
pub struct Db {
    pub pool: PgPool,
}

impl Db {
    /// 连接并跑迁移。`url` 形如 `postgres://muster:muster@localhost:5432/muster`。
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new().max_connections(16).connect(url).await?;
        sqlx::raw_sql(include_str!("../migrations/0001_init.sql")).execute(&pool).await?;
        Ok(Self { pool })
    }

    /// 从 `DATABASE_URL` 读连接串;缺失即快速失败(与 provider 密钥同一姿态:
    /// 配置缺了就炸响,别用默认值悄悄连到别处去)。
    pub async fn from_env() -> Result<Self, String> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL 未设置(见 deploy/docker-compose.yml)".to_string())?;
        Self::connect(&url).await.map_err(|e| format!("数据库连接失败:{e}"))
    }
}

pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
