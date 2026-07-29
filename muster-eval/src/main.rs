//! muster-eval CLI。
//!
//! 用法:
//!   cargo run -p muster-eval -- --config provider.example.toml --providers deepseek,qwen --trials 3
//!
//! 退出码:0 = 闸门通过;1 = 未通过或结论无效;2 = 参数/配置错误。

use std::sync::Arc;

use muster_eval::orchestrate::run_eval;
use muster_eval::report::render_markdown;
use muster_eval::runner::GenParams;
use muster_eval::samples::{samples, Sample};
use muster_provider::{ModelProvider, ProviderRegistry};

struct Args {
    config: String,
    providers: Vec<String>,
    trials: usize,
    threshold: f64,
    delay_ms: u64,
    gen: GenParams,
    filter: Option<String>,
    out_dir: String,
    list_only: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        config: "provider.example.toml".into(),
        providers: vec!["deepseek".into(), "qwen".into()],
        trials: 1,
        threshold: 0.90,
        delay_ms: 500,
        gen: GenParams::default(),
        filter: None,
        out_dir: "eval-reports".into(),
        list_only: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut take = |name: &str| it.next().ok_or(format!("{name} 需要一个值"));
        match flag.as_str() {
            "--config" => args.config = take("--config")?,
            "--providers" => {
                args.providers = take("--providers")?
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "--trials" => args.trials = take("--trials")?.parse().map_err(|e| format!("--trials: {e}"))?,
            "--threshold" => {
                args.threshold = take("--threshold")?.parse().map_err(|e| format!("--threshold: {e}"))?
            }
            "--delay-ms" => args.delay_ms = take("--delay-ms")?.parse().map_err(|e| format!("--delay-ms: {e}"))?,
            // 思考型模型(如 Kimi K3 仅接受 temperature=1、思考计入输出 token)用这两项
            // 显式偏离默认口径;取值会写入 report.md / results.json 公示。
            "--temperature" => {
                args.gen.temperature = take("--temperature")?.parse().map_err(|e| format!("--temperature: {e}"))?
            }
            "--max-tokens" => {
                args.gen.max_tokens = take("--max-tokens")?.parse().map_err(|e| format!("--max-tokens: {e}"))?
            }
            "--filter" => args.filter = Some(take("--filter")?),
            "--out" => args.out_dir = take("--out")?,
            "--list" => args.list_only = true,
            other => return Err(format!("未知参数 {other}")),
        }
    }
    if args.trials == 0 {
        return Err("--trials 至少为 1".into());
    }
    Ok(args)
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误: {e}");
            std::process::exit(2);
        }
    };

    let all_samples = samples();
    let selected: Vec<Sample> = match &args.filter {
        None => all_samples,
        Some(f) => all_samples.into_iter().filter(|s| s.id.contains(f.as_str())).collect(),
    };
    if selected.is_empty() {
        eprintln!("filter 未命中任何样本");
        std::process::exit(2);
    }

    if args.list_only {
        for (i, s) in selected.iter().enumerate() {
            println!("{:>2}. [{:?}] {} — {}(tools: {})", i + 1, s.category, s.id, s.title, s.tools.join(","));
        }
        return;
    }

    let toml_text = match std::fs::read_to_string(&args.config) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读取配置 {} 失败: {e}", args.config);
            std::process::exit(2);
        }
    };
    let registry = match ProviderRegistry::from_toml_str(&toml_text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("配置解析失败: {e}");
            std::process::exit(2);
        }
    };

    let mut providers: Vec<Arc<dyn ModelProvider>> = Vec::new();
    for id in &args.providers {
        match registry.get(id) {
            Some(p) => providers.push(p),
            None => {
                eprintln!("配置中不存在 provider `{id}`(现有:{:?})", registry.ids());
                std::process::exit(2);
            }
        }
    }

    let report = run_eval(&providers, &selected, args.trials, args.threshold, args.delay_ms, args.gen).await;

    std::fs::create_dir_all(&args.out_dir).expect("创建输出目录");
    let json_path = format!("{}/results.json", args.out_dir);
    let md_path = format!("{}/report.md", args.out_dir);
    std::fs::write(&json_path, serde_json::to_string_pretty(&report).unwrap()).expect("写 results.json");
    std::fs::write(&md_path, render_markdown(&report, &selected)).expect("写 report.md");

    println!("\n报告已写入:{md_path} / {json_path}");
    for p in &report.providers {
        let rate = p.summary.success_rate.map(|r| format!("{:.1}%", r * 100.0)).unwrap_or_else(|| "—".into());
        println!(
            "  {:<12} 成功率 {}({} 通过 / {} 评分,{} infra){}",
            p.id,
            rate,
            p.summary.passed,
            p.summary.passed + p.summary.failed,
            p.summary.infra,
            if p.summary.meets_threshold { " ✅" } else { " ❌" }
        );
    }
    println!("闸门:{}", if report.gate_passed { "✅ 通过" } else { "❌ 未通过" });
    std::process::exit(if report.gate_passed { 0 } else { 1 });
}
