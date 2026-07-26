//! 评测编排:providers × samples × trials → EvalReport。

use std::sync::Arc;
use std::time::Duration;

use muster_provider::ModelProvider;

use crate::report::{summarize, EvalReport, ProviderReport};
use crate::runner::{run_trial, TrialRecord, SYSTEM_PROMPT};
use crate::samples::Sample;

pub async fn run_eval(
    providers: &[Arc<dyn ModelProvider>],
    selected: &[Sample],
    trials: usize,
    threshold: f64,
    delay_ms: u64,
) -> EvalReport {
    let mut provider_reports = Vec::new();

    for provider in providers {
        let meta = provider.metadata().clone();
        eprintln!("== provider {}({},{:?})==", meta.id, meta.model, meta.locality);
        let mut trial_records: Vec<TrialRecord> = Vec::new();
        let mut fatal: Option<String> = None;

        'provider: for sample in selected {
            for trial in 0..trials {
                match run_trial(provider, sample, trial).await {
                    Ok(rec) => {
                        eprintln!("  [{}] {} trial {} -> {:?}", meta.id, sample.id, trial + 1, rec.status);
                        trial_records.push(rec);
                    }
                    Err(f) => {
                        eprintln!("  [{}] 致命错误,终止该 provider:{}", meta.id, f.0);
                        fatal = Some(f.0);
                        break 'provider;
                    }
                }
                if delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }

        let summary = summarize(&trial_records, &fatal, threshold);
        provider_reports.push(ProviderReport {
            id: meta.id,
            display_name: meta.display_name,
            model: meta.model,
            endpoint: meta.endpoint,
            locality: format!("{:?}", meta.locality),
            fatal,
            trials: trial_records,
            summary,
        });
    }

    let gate_passed =
        !provider_reports.is_empty() && provider_reports.iter().all(|p| p.summary.meets_threshold);

    EvalReport {
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string(),
        trials_per_sample: trials,
        threshold,
        system_prompt: SYSTEM_PROMPT.to_owned(),
        providers: provider_reports,
        gate_passed,
    }
}
