//! `muster-admin` — 服务端管理工具。
//!
//! ## 为什么是 CLI 而不是网页后台
//!
//! - **管理工作要能脚本化**:开二十个账号、批量授权,循环一行就完了。
//! - **不新增攻击面**:网页后台是第三个客户端、第二套会话管理,
//!   而它带来的能力和这里一模一样。
//! - **管理员本来就坐在服务器上**:SSH 进去就能用。
//! - 本仓已有这个惯例(`examples/approve.rs`、`examples/live_task.rs`)。
//!
//! 将来桌面壳接上服务端后,会再有一套图形化的编制管理——那时这个 CLI 仍然
//! 有用:无头环境、批量操作、UI 挂了的时候。两者走**同一套 HTTP 接口**,
//! 不会出现"只有某一边能干的事"。
//!
//! ```bash
//! export MUSTER_SERVER=http://localhost:8787
//! muster-admin login owner 口令              # 拿令牌,存到 ~/.muster/admin-token
//! muster-admin accounts                      # 列账号
//! muster-admin roles [账号]                  # 列角色绑定
//! muster-admin account-add bob 口令 Bob
//! muster-admin grant bob approver group 平台组
//! muster-admin revoke bob approver group 平台组
//! muster-admin disable bob / enable bob
//! muster-admin team-add platform 平台组
//! muster-admin channel-add platform-main platform 主频道 internal
//! ```

use std::process::exit;

fn base() -> String {
    std::env::var("MUSTER_SERVER").unwrap_or_else(|_| "http://localhost:8787".into())
}

fn token_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.muster/admin-token")
}

fn token() -> String {
    std::fs::read_to_string(token_path())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| {
            eprintln!("尚未登录:先跑 `muster-admin login <账号> <口令>`");
            exit(2);
        })
}

/// 极简 HTTP:只用到 GET/POST/DELETE + Bearer,不值得为它引一个客户端库。
/// 失败一律把服务端的原文打出来——管理工具最忌讳把错误咽下去。
fn call(method: &str, path: &str, body: Option<serde_json::Value>, auth: bool) -> serde_json::Value {
    let url = format!("{}{}", base(), path);
    let mut args = vec!["-sS".to_string(), "-X".into(), method.into(), url];
    args.push("-H".into());
    args.push("content-type: application/json".into());
    if auth {
        args.push("-H".into());
        args.push(format!("authorization: Bearer {}", token()));
    }
    if let Some(b) = &body {
        args.push("-d".into());
        args.push(b.to_string());
    }
    let out = std::process::Command::new("curl").args(&args).output().unwrap_or_else(|e| {
        eprintln!("调用失败(curl 不可用?):{e}");
        exit(1);
    });
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| {
        eprintln!("服务端返回不是 JSON:{text}");
        exit(1);
    });
    if let Some(err) = v.get("error") {
        eprintln!("✗ {}", err.as_str().unwrap_or(&err.to_string()));
        exit(1);
    }
    v
}

fn need(a: &[String], n: usize, usage: &str) {
    if a.len() < n {
        eprintln!("用法:muster-admin {usage}");
        exit(2);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let cmd = a.first().map(String::as_str).unwrap_or("help");
    let rest = &a[1.min(a.len())..];

    match cmd {
        "login" => {
            need(rest, 2, "login <账号> <口令>");
            let v = call(
                "POST",
                "/auth/login",
                Some(serde_json::json!({ "id": rest[0], "password": rest[1] })),
                false,
            );
            let t = v["token"].as_str().unwrap_or_default();
            let p = token_path();
            if let Some(dir) = std::path::Path::new(&p).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(&p, t).unwrap_or_else(|e| {
                eprintln!("令牌写入 {p} 失败:{e}");
                exit(1);
            });
            println!("✓ 已登录 {}({}),令牌存于 {p}", rest[0], v["display_name"]);
        }
        "whoami" => {
            let v = call("GET", "/whoami", None, true);
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        "accounts" => {
            let v = call("GET", "/accounts", None, true);
            println!("{:<16} {:<14} {:<8} {}", "账号", "显示名", "类型", "状态");
            for r in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<16} {:<14} {:<8} {}",
                    r["id"].as_str().unwrap_or("?"),
                    r["display_name"].as_str().unwrap_or("?"),
                    r["kind"].as_str().unwrap_or("?"),
                    if r["disabled"].as_bool().unwrap_or(false) { "已停用" } else { "正常" }
                );
            }
        }
        "roles" => {
            let q = rest.first().map(|s| format!("?account_id={s}")).unwrap_or_default();
            let v = call("GET", &format!("/roles{q}"), None, true);
            let rows = v.as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("(没有角色绑定)");
            }
            for r in rows {
                println!(
                    "{:<16} {:<14} {}:{}",
                    r["account_id"].as_str().unwrap_or("?"),
                    r["role"].as_str().unwrap_or("?"),
                    r["scope_kind"].as_str().unwrap_or("?"),
                    r["scope_id"].as_str().unwrap_or("-")
                );
            }
        }
        "account-add" => {
            need(rest, 3, "account-add <账号> <口令> <显示名> [kind]");
            call(
                "POST",
                "/accounts",
                Some(serde_json::json!({
                    "id": rest[0], "password": rest[1], "display_name": rest[2],
                    "kind": rest.get(3).cloned().unwrap_or_else(|| "human".into()),
                })),
                true,
            );
            println!("✓ 已创建账号 {}", rest[0]);
        }
        "grant" | "revoke" => {
            need(rest, 2, "grant|revoke <账号> <角色> [org|group|channel] [作用域id]");
            let body = serde_json::json!({
                "account_id": rest[0],
                "role": rest[1],
                "scope_kind": rest.get(2).cloned().unwrap_or_else(|| "org".into()),
                "scope_id": rest.get(3).cloned(),
            });
            if cmd == "grant" {
                call("POST", "/roles", Some(body), true);
                println!("✓ 已授权 {} → {}", rest[0], rest[1]);
            } else {
                let v = call("DELETE", "/roles", Some(body), true);
                println!("✓ 已撤销 {} 的 {}(影响 {} 条)", rest[0], rest[1], v["revoked"]);
            }
        }
        "disable" | "enable" => {
            need(rest, 1, "disable|enable <账号>");
            call(
                "POST",
                &format!("/accounts/{}/disabled", rest[0]),
                Some(serde_json::json!({ "disabled": cmd == "disable" })),
                true,
            );
            println!("✓ {} 已{}", rest[0], if cmd == "disable" { "停用" } else { "启用" });
        }
        "team-add" => {
            need(rest, 2, "team-add <id> <名称>");
            call("POST", "/teams", Some(serde_json::json!({ "id": rest[0], "name": rest[1] })), true);
            println!("✓ 已创建团队 {}", rest[1]);
        }
        "channel-add" => {
            need(rest, 3, "channel-add <id> <团队id> <名称> [open|internal|restricted]");
            call(
                "POST",
                "/channels",
                Some(serde_json::json!({
                    "id": rest[0], "team_id": rest[1], "name": rest[2],
                    "level": rest.get(3).cloned().unwrap_or_else(|| "open".into()),
                })),
                true,
            );
            println!("✓ 已创建频道 #{}", rest[2]);
        }
        "channels" => {
            let v = call("GET", "/channels", None, true);
            for r in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<18} {:<10} {:<12} {}",
                    r["id"].as_str().unwrap_or("?"),
                    r["team_id"].as_str().unwrap_or("?"),
                    r["name"].as_str().unwrap_or("?"),
                    r["level"].as_str().unwrap_or("?")
                );
            }
        }
        "health" => {
            let v = call("GET", "/health", None, false);
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        _ => {
            eprintln!(
                "muster-admin — 服务端管理\n\n\
                 环境:MUSTER_SERVER(默认 http://localhost:8787)\n\n\
                 login <账号> <口令>            登录并保存令牌\n\
                 whoami                         我是谁 / 我能做什么\n\
                 health                         服务健康\n\
                 accounts                       列账号\n\
                 account-add <id> <口令> <名> [kind]\n\
                 disable|enable <账号>          停用 / 启用(不删除:删了历史里的作者会变孤儿)\n\
                 roles [账号]                   列角色绑定\n\
                 grant  <账号> <角色> [作用域类型] [作用域id]\n\
                 revoke <账号> <角色> [作用域类型] [作用域id]\n\
                 teams? 用 team-add <id> <名称>\n\
                 channels / channel-add <id> <团队id> <名称> [密级]\n\n\
                 角色:owner admin group_admin publisher approver member guest\n\
                 作用域类型:org(默认) group channel\n\n\
                 注:所有权限变更都会写进服务端审计链(badge.update)。"
            );
            exit(2);
        }
    }
}
