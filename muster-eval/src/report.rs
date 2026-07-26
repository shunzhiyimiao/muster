//! 报告产出:results.json(机器可读)+ report.md(G0′ 闸门证据)。
//!
//! 口径(写死并在报告中声明):
//! - 成功率 = Pass / (Pass + Fail)。Infra(传输失败)不进分母,但单列披露;
//! - Infra 占比 > 10% 时该 provider 的结论标记为**无效**——闸门证据不允许
//!   用"掉线"稀释分母来凑数;
//! - 闸门判定:所有目标 provider 成功率 ≥ 阈值(默认 0.90)且结论有效。

use std::collections::BTreeMap;

use serde::Serialize;

use crate::runner::{TrialRecord, TrialStatus};
use crate::samples::Sample;

#[derive(Debug, Serialize)]
pub struct EvalReport {
    pub generated_at: String,
    pub trials_per_sample: usize,
    pub threshold: f64,
    pub system_prompt: String,
    pub providers: Vec<ProviderReport>,
    /// 闸门总判定:所有 provider 有效且达标。
    pub gate_passed: bool,
}

#[derive(Debug, Serialize)]
pub struct ProviderReport {
    pub id: String,
    pub display_name: String,
    pub model: String,
    pub endpoint: String,
    pub locality: String,
    /// provider 级致命错误(如密钥无效);Some 时本列作废。
    pub fatal: Option<String>,
    pub trials: Vec<TrialRecord>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub passed: usize,
    pub failed: usize,
    pub infra: usize,
    /// passed / (passed + failed);分母为 0 时为 None。
    pub success_rate: Option<f64>,
    /// infra / 总试次。
    pub infra_rate: f64,
    /// 结论是否有效(无 fatal 且 infra_rate ≤ 0.1 且有已评分试次)。
    pub valid: bool,
    /// valid 且 success_rate ≥ threshold。
    pub meets_threshold: bool,
}

pub fn summarize(trials: &[TrialRecord], fatal: &Option<String>, threshold: f64) -> Summary {
    let passed = trials.iter().filter(|t| t.status == TrialStatus::Pass).count();
    let failed = trials.iter().filter(|t| t.status == TrialStatus::Fail).count();
    let infra = trials.iter().filter(|t| t.status == TrialStatus::Infra).count();
    let graded = passed + failed;
    let total = graded + infra;
    let success_rate = if graded > 0 { Some(passed as f64 / graded as f64) } else { None };
    let infra_rate = if total > 0 { infra as f64 / total as f64 } else { 0.0 };
    let valid = fatal.is_none() && graded > 0 && infra_rate <= 0.10;
    let meets_threshold = valid && success_rate.map(|r| r >= threshold).unwrap_or(false);
    Summary { passed, failed, infra, success_rate, infra_rate, valid, meets_threshold }
}

pub fn render_markdown(report: &EvalReport, samples: &[Sample]) -> String {
    let mut md = String::new();
    md.push_str("# Muster A7 · 工具调用评测报告(G0′ 闸门证据)\n\n");
    md.push_str(&format!(
        "- 生成时间:{}\n- 样本数:{} × 每样本试次:{}\n- 阈值:{:.0}%\n- 系统提示词:见附录 B\n\n",
        report.generated_at,
        samples.len(),
        report.trials_per_sample,
        report.threshold * 100.0
    ));

    md.push_str(&format!(
        "## 闸门判定:{}\n\n",
        if report.gate_passed { "✅ 通过" } else { "❌ 未通过" }
    ));

    md.push_str("| Provider | 模型 | 位置 | 成功率 | 通过/评分 | Infra | 结论 |\n|---|---|---|---|---|---|---|\n");
    for p in &report.providers {
        let rate = p
            .summary
            .success_rate
            .map(|r| format!("{:.1}%", r * 100.0))
            .unwrap_or_else(|| "—".into());
        let verdict = if p.fatal.is_some() {
            "作废(致命错误)".to_owned()
        } else if !p.summary.valid {
            "无效(Infra>10%)".to_owned()
        } else if p.summary.meets_threshold {
            "达标".to_owned()
        } else {
            "未达标".to_owned()
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {}/{} | {} | {} |\n",
            p.id,
            p.model,
            p.locality,
            rate,
            p.summary.passed,
            p.summary.passed + p.summary.failed,
            p.summary.infra,
            verdict
        ));
    }
    md.push('\n');

    for p in &report.providers {
        md.push_str(&format!("## {}({},端点 {})\n\n", p.id, p.display_name, p.endpoint));
        if let Some(fatal) = &p.fatal {
            md.push_str(&format!("**Provider 级致命错误,本列作废**:{fatal}\n\n"));
            continue;
        }
        // 分类统计。
        let mut by_cat: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for t in &p.trials {
            let entry = by_cat.entry(format!("{:?}", t.category)).or_insert((0, 0));
            match t.status {
                TrialStatus::Pass => {
                    entry.0 += 1;
                    entry.1 += 1;
                }
                TrialStatus::Fail => entry.1 += 1,
                TrialStatus::Infra => {}
            }
        }
        md.push_str("| 类别 | 通过/评分 |\n|---|---|\n");
        for (cat, (pass, graded)) in &by_cat {
            md.push_str(&format!("| {cat} | {pass}/{graded} |\n"));
        }
        md.push('\n');

        let failures: Vec<&TrialRecord> =
            p.trials.iter().filter(|t| t.status != TrialStatus::Pass).collect();
        if failures.is_empty() {
            md.push_str("全部试次通过。\n\n");
        } else {
            md.push_str("### 未通过明细\n\n");
            for t in failures {
                md.push_str(&format!(
                    "- **{}**(trial {},{}):{}\n",
                    t.sample_id,
                    t.trial + 1,
                    match t.status {
                        TrialStatus::Fail => "评分未过",
                        TrialStatus::Infra => "传输失败",
                        TrialStatus::Pass => unreachable!(),
                    },
                    t.reasons.join(";")
                ));
                for line in &t.transcript {
                    md.push_str(&format!("  - {line}\n"));
                }
            }
            md.push('\n');
        }
    }

    md.push_str("## 附录 A · 评测集全览\n\n");
    md.push_str("| # | id | 类别 | 标题 | 工具面板 | 期望 |\n|---|---|---|---|---|---|\n");
    for (i, s) in samples.iter().enumerate() {
        let expects: Vec<String> = s
            .turns
            .iter()
            .map(|t| {
                serde_json::to_string(&t.expect).unwrap_or_default()
            })
            .collect();
        md.push_str(&format!(
            "| {} | {} | {:?} | {} | {} | `{}` |\n",
            i + 1,
            s.id,
            s.category,
            s.title,
            s.tools.join(", "),
            expects.join(" → ").replace('|', "\\|")
        ));
    }
    md.push('\n');

    md.push_str("## 附录 B · 系统提示词\n\n");
    md.push_str(&format!("> {}\n\n", report.system_prompt));
    md.push_str("> 注:A1 的正式 agent 系统提示词落地后,须用正式提示词重跑本评测——G0′ 度量的是「提示词 + provider」整体,而非裸模型。\n");
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::Category;

    fn rec(id: &str, status: TrialStatus) -> TrialRecord {
        TrialRecord {
            sample_id: id.into(),
            category: Category::Basic,
            trial: 0,
            status,
            reasons: vec![],
            transcript: vec![],
        }
    }

    #[test]
    fn summary_math_excludes_infra_from_denominator() {
        let trials = vec![
            rec("a", TrialStatus::Pass),
            rec("b", TrialStatus::Pass),
            rec("c", TrialStatus::Fail),
            rec("d", TrialStatus::Infra),
        ];
        let s = summarize(&trials, &None, 0.9);
        assert_eq!(s.passed, 2);
        assert_eq!(s.failed, 1);
        assert_eq!(s.infra, 1);
        assert!((s.success_rate.unwrap() - 2.0 / 3.0).abs() < 1e-9);
        // infra_rate = 1/4 = 25% > 10% → 结论无效。
        assert!(!s.valid);
        assert!(!s.meets_threshold);
    }

    #[test]
    fn threshold_and_validity() {
        let mut trials: Vec<TrialRecord> = (0..19).map(|i| rec(&format!("s{i}"), TrialStatus::Pass)).collect();
        trials.push(rec("s19", TrialStatus::Fail));
        let s = summarize(&trials, &None, 0.9);
        assert!(s.valid);
        assert!((s.success_rate.unwrap() - 0.95).abs() < 1e-9);
        assert!(s.meets_threshold);

        let s_strict = summarize(&trials, &None, 0.96);
        assert!(!s_strict.meets_threshold);

        let s_fatal = summarize(&trials, &Some("auth".into()), 0.9);
        assert!(!s_fatal.valid);
    }
}
