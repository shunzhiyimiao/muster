//! 建第一个组织所有者。
//!
//! **唯一绕过鉴权的入口**,且只在库里一个账号都没有时可用——否则它就成了
//! 一个人人可用的提权后门。服务端刻意不内置默认账号:默认口令是最常见的
//! 入侵路径,而"记得改掉默认口令"从来就没人记得。
//!
//! ```bash
//! DATABASE_URL=… cargo run -p muster-server --example bootstrap -- alice 口令 Alice
//! ```

use muster_server::auth::hash_password;
use muster_server::db::{now_ms, Db};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("用法:bootstrap <账号id> <口令> <显示名>");
        std::process::exit(2);
    }
    let (id, pw, name) = (&a[0], &a[1], &a[2]);
    if pw.chars().count() < 8 {
        eprintln!("口令至少 8 个字符");
        std::process::exit(2);
    }

    let db = Db::from_env().await?;
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM account").fetch_one(&db.pool).await?;
    if n > 0 {
        eprintln!("库里已有 {n} 个账号——引导入口只在空库时可用,否则它就是提权后门。");
        eprintln!("请让现有管理员用 POST /accounts 建号。");
        std::process::exit(1);
    }

    let phc = hash_password(pw).map_err(|e| e.to_string())?;
    let mut tx = db.pool.begin().await?;
    sqlx::query(
        "INSERT INTO account(id, display_name, password_hash, kind, created_ms) VALUES($1,$2,$3,'human',$4)",
    )
    .bind(id)
    .bind(name)
    .bind(&phc)
    .bind(now_ms())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO role_binding(account_id, role, scope_kind, scope_id, created_ms)
         VALUES($1,'owner','org',NULL,$2)",
    )
    .bind(id)
    .bind(now_ms())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    println!("✓ 已创建组织所有者 {id}({name})");
    println!("  登录:POST /auth/login {{\"id\":\"{id}\",\"password\":\"…\"}}");
    Ok(())
}
