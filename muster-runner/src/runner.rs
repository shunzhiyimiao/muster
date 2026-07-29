//! 工具循环执行器:resolve 一次 → 逐回合开流 → 工具执行回传 → 审计链。
//!
//! 重试策略(v0,属 Runner 不属路由):同一 provider、同一回合、整回合重试
//! 一次;再失败即 `run.finish(Failed)`。绝不换落点——链的合法性在 resolve
//! 时已定,换落点=重新决策,那是新任务。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::StreamExt;
use serde::Serialize;
use thiserror::Error;

use muster_audit::{
    Actor, AuditStore, ContentHash, EgressBytes, EventBody, ModelRef, NewEvent, ReplayRefs,
    RunOutcome, Scope,
};
use muster_provider::{
    ChatMessage, ChatRequest, Locality, Role, StreamEvent, ToolCallAccumulator, ToolChoice,
};
use muster_route::{LabelSource, RoutePlan, RouteRequest, Router};

use crate::tools::ToolSet;

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub badge: String,
    pub system_prompt: String,
    pub policy_version: String,
    pub max_turns: usize,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            badge: "A-007".into(),
            system_prompt: "你是 Muster 点将台的协作 Agent。需要了解工作区内容时调用只读工具;\
                            回答用中文,引用文件请带相对路径。"
                .into(),
            policy_version: "policy-v1".into(),
            max_turns: 8,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub run_id: String,
    pub session_id: Option<String>,
    pub team: Option<String>,
    pub channel: Option<String>,
    pub sources: Vec<LabelSource>,
    pub requested_provider: Option<String>,
    pub default_provider: Option<String>,
    pub prompt: String,
    pub workspace: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerEvent {
    /// 路由已定(含"为什么落在这里"的全部依据)。
    Planned {
        run_id: String,
        plan: RoutePlan,
        provider_id: String,
        provider_name: String,
        model: String,
        locality: String,
        attempts: Vec<String>,
    },
    TextDelta { text: String },
    ToolCall { turn: usize, name: String, arguments: String },
    ToolResult { turn: usize, name: String, summary: String },
    /// 过程性通告(如整回合重试)。
    Notice { text: String },
    Finished {
        outcome: String,
        latency_ms: u64,
        turns: usize,
        prompt_tokens: u64,
        completion_tokens: u64,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub outcome: String,
    pub final_text: String,
    pub turns: usize,
    pub latency_ms: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("路由拒绝:{0}")]
    Refused(String),
    #[error("工作区不可用:{0}")]
    Workspace(String),
    #[error("审计写入失败:{0}")]
    Audit(String),
    #[error("模型调用失败(已重试):{0}")]
    Model(String),
}

// ---------------------------------------------------------------- ReplayRefs 取真值

/// git HEAD 优先(`git-head:` 前缀);非 git 目录降级为顶层清单(`dir:` 前缀)。
/// 前缀进哈希原文——重放校验时能看出快照口径,不伪造精度。
fn repo_snapshot(ws: &std::path::Path) -> ContentHash {
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(["rev-parse", "HEAD"])
        .output();
    if let Ok(o) = head {
        if o.status.success() {
            let head = String::from_utf8_lossy(&o.stdout);
            return ContentHash::sha256(format!("git-head:{}", head.trim()).as_bytes());
        }
    }
    let mut names: Vec<String> = std::fs::read_dir(ws)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    ContentHash::sha256(format!("dir:{}", names.join(",")).as_bytes())
}

fn deps_lock_hash(ws: &std::path::Path) -> ContentHash {
    for lock in ["Cargo.lock", "pnpm-lock.yaml", "package-lock.json"] {
        if let Ok(bytes) = std::fs::read(ws.join(lock)) {
            return ContentHash::sha256(&bytes);
        }
    }
    ContentHash::sha256(b"deps:none")
}

// ---------------------------------------------------------------- 主循环

pub async fn run_task(
    router: &Router,
    audit: &Arc<Mutex<AuditStore>>,
    cfg: &RunnerConfig,
    spec: TaskSpec,
    mut on_event: impl FnMut(RunnerEvent) + Send,
) -> Result<RunSummary, RunnerError> {
    let tools = ToolSet::new(&spec.workspace).map_err(|e| RunnerError::Workspace(e.to_string()))?;
    let started = Instant::now();

    // ---- 路由(一次;拒绝目前不落审计,缺口已在 lib.rs 登记)
    let route_req = RouteRequest {
        sources: &spec.sources,
        requested_provider: spec.requested_provider.as_deref(),
        default_provider: spec.default_provider.as_deref(),
    };
    let resolution =
        router.resolve(&route_req).await.map_err(|e| RunnerError::Refused(e.to_string()))?;
    let plan = resolution.plan.clone();
    let meta = resolution.provider.metadata().clone();
    on_event(RunnerEvent::Planned {
        run_id: spec.run_id.clone(),
        plan: plan.clone(),
        provider_id: meta.id.clone(),
        provider_name: meta.display_name.clone(),
        model: meta.model.clone(),
        locality: format!("{:?}", meta.locality).to_lowercase(),
        attempts: resolution.attempts.iter().map(|a| format!("{a:?}")).collect(),
    });

    let scope = Scope { team: spec.team.clone(), channel: spec.channel.clone() };
    let append = |audit: &Arc<Mutex<AuditStore>>, body: EventBody| -> Result<(), RunnerError> {
        audit
            .lock()
            .unwrap()
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::agent(&cfg.badge),
                scope: scope.clone(),
                run_id: Some(spec.run_id.clone()),
                session_id: spec.session_id.clone(),
                policy_version: Some(cfg.policy_version.clone()),
                label: Some(plan.effective),
                locality: Some(plan.primary_locality),
                body,
            })
            .map(|_| ())
            .map_err(|e| RunnerError::Audit(e.to_string()))
    };

    // ---- run.start(Capsule-ready:ReplayRefs 全真值)
    let params_repr = serde_json::json!({
        "system_prompt": cfg.system_prompt,
        "temperature": null,
        "max_tokens": null,
    })
    .to_string();
    append(
        audit,
        EventBody::RunStart {
            task_kind: "chat.tools.v0".into(),
            replay: ReplayRefs {
                repo_snapshot: repo_snapshot(tools.workspace()),
                deps_lock: deps_lock_hash(tools.workspace()),
                model: ModelRef {
                    provider_id: meta.id.clone(),
                    model: meta.model.clone(),
                    params_hash: ContentHash::sha256(params_repr.as_bytes()),
                },
                tool_env: ContentHash::sha256(
                    serde_json::json!({
                        "tools": ["list_dir", "read_file", "grep"],
                        "workspace": tools.workspace().display().to_string(),
                        "mode": "read_only",
                    })
                    .to_string()
                    .as_bytes(),
                ),
            },
            label: plan.effective,
            locality_planned: plan.primary_locality,
        },
    )?;

    // ---- route.decide
    append(
        audit,
        EventBody::RouteDecide {
            effective_label: plan.effective,
            deciders: plan.deciders.clone(),
            policy_version: cfg.policy_version.clone(),
            locality: plan.primary_locality,
            provider_id: plan.primary.clone(),
            fallbacks: plan.fallbacks.clone(),
            downgrade: plan.downgraded.as_ref().map(|d| d.reason),
        },
    )?;

    // ---- 工具循环
    let mut messages = vec![
        ChatMessage::system(format!(
            "{}\n工作区:{}(只读工具:list_dir / read_file / grep)",
            cfg.system_prompt,
            tools.workspace().display()
        )),
        ChatMessage::user(spec.prompt.clone()),
    ];
    let specs = tools.specs();
    let mut total_prompt: u64 = 0;
    let mut total_completion: u64 = 0;
    let mut final_text = String::new();

    let finish_failed = |audit: &Arc<Mutex<AuditStore>>,
                         class: &str,
                         started: &Instant,
                         append_ok: &dyn Fn(&Arc<Mutex<AuditStore>>, EventBody) -> Result<(), RunnerError>|
     -> Result<(), RunnerError> {
        append_ok(
            audit,
            EventBody::RunFinish {
                outcome: RunOutcome::Failed { class: class.into() },
                duration_ms: started.elapsed().as_millis() as u64,
                output_hash: None,
            },
        )
    };

    for turn in 1..=cfg.max_turns {
        let mut attempt = 0usize;
        let (text, tool_calls) = loop {
            attempt += 1;
            let req = ChatRequest {
                messages: messages.clone(),
                tools: specs.clone(),
                tool_choice: Some(ToolChoice::Auto),
                run_id: Some(spec.run_id.clone()),
                ..Default::default()
            };
            let request_repr = serde_json::json!({
                "provider": meta.id, "model": meta.model, "turn": turn, "attempt": attempt,
                "messages": &messages,
            })
            .to_string();

            let call_started = Instant::now();
            let mut retryable_err: Option<String> = None;
            let mut text = String::new();
            let mut acc = ToolCallAccumulator::new();
            let mut prompt_tokens = 0u64;
            let mut completion_tokens = 0u64;

            match resolution.provider.chat_stream(req).await {
                Ok(mut stream) => {
                    while let Some(ev) = stream.next().await {
                        match ev {
                            Ok(StreamEvent::TextDelta(t)) => {
                                text.push_str(&t);
                                on_event(RunnerEvent::TextDelta { text: t });
                            }
                            Ok(ev @ StreamEvent::ToolCallDelta { .. }) => acc.push_event(&ev),
                            Ok(StreamEvent::Usage(u)) => {
                                prompt_tokens = u.prompt_tokens as u64;
                                completion_tokens = u.completion_tokens as u64;
                            }
                            Ok(StreamEvent::Finish(_)) => {}
                            Err(e) => {
                                retryable_err = Some(e.to_string());
                                break;
                            }
                        }
                    }
                }
                Err(e) => retryable_err = Some(e.to_string()),
            }

            // model.call:成功与失败的尝试都记账(外发已经发生)。
            append(
                audit,
                EventBody::ModelCall {
                    provider_id: meta.id.clone(),
                    model: meta.model.clone(),
                    locality: plan.primary_locality,
                    label: plan.effective,
                    tokens_in: (prompt_tokens > 0).then_some(prompt_tokens),
                    tokens_out: (completion_tokens > 0).then_some(completion_tokens),
                    bytes_in: text.len() as u64,
                    bytes_out: if plan.primary_locality == Locality::Cloud {
                        EgressBytes::Measured(request_repr.len() as u64)
                    } else {
                        EgressBytes::Measured(0)
                    },
                    latency_ms: call_started.elapsed().as_millis() as u64,
                    request_hash: ContentHash::sha256(request_repr.as_bytes()),
                },
            )?;
            total_prompt += prompt_tokens;
            total_completion += completion_tokens;

            match retryable_err {
                None => break (text, acc.finish()),
                Some(msg) if attempt == 1 => {
                    on_event(RunnerEvent::Notice {
                        text: format!("回合 {turn} 中流失败,整回合重试一次:{msg}"),
                    });
                    continue;
                }
                Some(msg) => {
                    finish_failed(audit, "stream", &started, &append)?;
                    on_event(RunnerEvent::Finished {
                        outcome: "failed:stream".into(),
                        latency_ms: started.elapsed().as_millis() as u64,
                        turns: turn,
                        prompt_tokens: total_prompt,
                        completion_tokens: total_completion,
                    });
                    return Err(RunnerError::Model(msg));
                }
            }
        };

        if tool_calls.is_empty() {
            final_text = text;
            append(
                audit,
                EventBody::RunFinish {
                    outcome: RunOutcome::Success,
                    duration_ms: started.elapsed().as_millis() as u64,
                    output_hash: Some(ContentHash::sha256(final_text.as_bytes())),
                },
            )?;
            let latency_ms = started.elapsed().as_millis() as u64;
            on_event(RunnerEvent::Finished {
                outcome: "success".into(),
                latency_ms,
                turns: turn,
                prompt_tokens: total_prompt,
                completion_tokens: total_completion,
            });
            return Ok(RunSummary {
                run_id: spec.run_id,
                outcome: "success".into(),
                final_text,
                turns: turn,
                latency_ms,
                prompt_tokens: total_prompt,
                completion_tokens: total_completion,
            });
        }

        // 有工具调用:执行并回传,进入下一回合。
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: (!text.is_empty()).then_some(text),
            tool_calls: tool_calls.clone(),
            tool_call_id: None,
        });
        for call in &tool_calls {
            on_event(RunnerEvent::ToolCall {
                turn,
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });
            let result = tools.execute(&call.name, &call.arguments);
            let summary: String = {
                let one_line = result.replace('\n', " ⏎ ");
                let mut s: String = one_line.chars().take(120).collect();
                if one_line.chars().count() > 120 {
                    s.push('…');
                }
                s
            };
            on_event(RunnerEvent::ToolResult { turn, name: call.name.clone(), summary });
            messages.push(ChatMessage::tool(call.id.clone(), result));
        }
    }

    finish_failed(audit, "max_turns", &started, &append)?;
    let latency_ms = started.elapsed().as_millis() as u64;
    on_event(RunnerEvent::Finished {
        outcome: "failed:max_turns".into(),
        latency_ms,
        turns: cfg.max_turns,
        prompt_tokens: total_prompt,
        completion_tokens: total_completion,
    });
    Ok(RunSummary {
        run_id: spec.run_id,
        outcome: "failed:max_turns".into(),
        final_text,
        turns: cfg.max_turns,
        latency_ms,
        prompt_tokens: total_prompt,
        completion_tokens: total_completion,
    })
}
