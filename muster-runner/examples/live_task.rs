//! 真机跑一个完整任务:worktree → 写工具 → diff → 申请合入 → 裁决。
//!
//! Mock 测试只能证明管道通,证明不了**真实模型会不会正确使用写工具**
//! (会不会调、参数格式对不对、old 文本的缩进空白抄得准不准)。这个 example
//! 就是补这块验证的。
//!
//! ```bash
//! KIMI_API_KEY=… cargo run -p muster-runner --example live_task -- \
//!   <provider.toml> <provider_id> <git 仓路径> "任务描述" [approve|reject]
//! ```
//! 不传裁决动作则只申请、不裁决(改动留在隔离分支上待人工处置)。

use std::path::Path;
use std::sync::{Arc, Mutex};

use muster_audit::{pending_approval_list, run_chain, AuditStore, Scope};
use muster_provider::ProviderRegistry;
use muster_route::{OrgPolicy, Router, Sensitivity};
use muster_runner::{decide, run_task, RunnerConfig, RunnerEvent, TaskSpec};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 4 {
        eprintln!("用法:live_task <provider.toml> <provider_id> <git 仓> \"任务\" [approve|reject]");
        std::process::exit(2);
    }
    let (cfg_path, provider_id, repo, prompt) = (&a[0], &a[1], &a[2], &a[3]);
    let verdict = a.get(4).map(String::as_str);

    let registry = ProviderRegistry::from_toml_str(&std::fs::read_to_string(cfg_path)?)?;
    let provider = registry.get(provider_id).ok_or("配置中无此 provider")?;
    let router = Router::new(vec![provider], OrgPolicy::new(Sensitivity::Internal)?);
    let audit = Arc::new(Mutex::new(AuditStore::open_in_memory()?));

    let root = std::env::temp_dir().join("muster-live-worktrees");
    let run_id = format!("RUN-LIVE-{}", std::process::id());
    let spec = TaskSpec {
        run_id: run_id.clone(),
        session_id: Some("session:live".into()),
        team: Some("平台组".into()),
        channel: Some("live".into()),
        sources: vec![],
        requested_provider: None,
        default_provider: Some(provider_id.clone()),
        prompt: prompt.clone(),
        workspace: repo.into(),
        workspace_root: Some(root.clone()),
    };

    println!("== 任务 {run_id}:{prompt}\n");
    let mut wt_path = None;
    let summary = run_task(&router, &audit, &RunnerConfig::default(), spec, |e| match e {
        RunnerEvent::WorkspaceReady { branch, path, .. } => {
            println!("🌿 隔离工作区 {branch}\n   {path}");
        }
        RunnerEvent::ToolCall { turn, name, arguments } => {
            let args: String = arguments.chars().take(160).collect();
            println!("🔧 [回合 {turn}] {name} {args}");
        }
        RunnerEvent::ToolResult { name, summary, .. } => {
            println!("   ↳ {name}: {}", summary.chars().take(100).collect::<String>());
        }
        RunnerEvent::Notice { text } => println!("⚠️  {text}"),
        RunnerEvent::Diff { diff, .. } => {
            println!("\n📄 变更 {} 文件 +{} −{}", diff.files_changed, diff.insertions, diff.deletions);
            for f in &diff.files {
                println!("   {} {} (+{} −{})", f.status, f.path, f.added, f.removed);
            }
        }
        RunnerEvent::ApprovalRequested { approval_id, worktree_path, .. } => {
            println!("\n⏳ 申请合入:{approval_id}");
            wt_path = Some(worktree_path);
        }
        RunnerEvent::Finished { outcome, latency_ms, turns, prompt_tokens, completion_tokens } => {
            println!(
                "\n== {outcome} · {turns} 回合 · {latency_ms}ms · in {prompt_tokens}/out {completion_tokens} tokens"
            );
        }
        _ => {}
    })
    .await?;

    if let Some(d) = &summary.diff {
        if !d.is_empty() {
            println!("\n--- patch ---\n{}", d.patch.chars().take(1200).collect::<String>());
        }
    }

    {
        let store = audit.lock().unwrap();
        let pending = pending_approval_list(store.conn(), None)?;
        println!("\n待裁决审批:{}", pending.len());
        for p in &pending {
            println!("  {} · {}", p.approval_id, p.reason);
        }
    }

    if let (Some(v), Some(p)) = (verdict, wt_path.as_ref()) {
        let granted = v == "approve";
        let out = decide(
            &audit,
            "owner",
            "policy-v1",
            &run_id,
            Scope { team: Some("平台组".into()), channel: Some("live".into()) },
            Path::new(repo),
            Some(Path::new(p)),
            granted,
            Some("live_task 例行裁决"),
        )?;
        println!("\n🧑‍⚖️  裁决:{}", out.detail);
    }

    let store = audit.lock().unwrap();
    let chain: Vec<String> = run_chain(store.conn(), &run_id)?
        .iter()
        .map(|e| e.payload["event_type"].as_str().unwrap_or("?").to_string())
        .collect();
    println!("\n审计链:{}", chain.join(" → "));
    println!("哈希链校验:{:?}", store.verify_chain()?);
    Ok(())
}
