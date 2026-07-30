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
use crate::worktree::{enforce_retention, RetentionPolicy, RunDiff, Worktree};

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub badge: String,
    pub system_prompt: String,
    pub policy_version: String,
    pub max_turns: usize,
    /// 隔离工作区保留策略(§7.4「再依据保留策略清理」)。
    pub retention: RetentionPolicy,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            badge: "A-007".into(),
            // A1 正式提示词,与 A7 评测同源(muster-prompt);改它必须重跑评测。
            system_prompt: muster_prompt::SYSTEM_PROMPT.into(),
            policy_version: "policy-v1".into(),
            max_turns: 8,
            retention: RetentionPolicy::default(),
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
    /// worktree 落地根目录(总规划的 `WORKSPACE_ROOT`)。
    ///
    /// `Some` 且 `workspace` 是 git 仓 ⇒ 每 run 建独立 worktree、启用写工具、
    /// 结束产出 diff;`None` ⇒ 直连 workspace 的**只读**模式(v0 行为)。
    /// 非 git 仓时如实降级为只读并发 Notice,不假装隔离。
    pub workspace_root: Option<PathBuf>,
    /// 产出变更后是否申请合入(默认 true)。
    ///
    /// **影子重放必须设为 false**:它在锻造基线上重跑,产出是用来**比对**的,
    /// 不是要落地的。若照常申请合入,审批队列会被历史基线的改动污染,
    /// 还可能诱使人批准一份并非针对当前代码的 diff。
    pub propose_merge: bool,
}

impl TaskSpec {
    /// 常规任务的默认值(申请合入)。
    pub fn proposing(mut self) -> Self {
        self.propose_merge = true;
        self
    }
    /// 影子重放:只产出与比对,不申请合入。
    pub fn shadow(mut self) -> Self {
        self.propose_merge = false;
        self
    }
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
    /// 隔离工作区就绪(worktree 模式);UI 可据此显示"在分支上工作"。
    WorkspaceReady { path: String, branch: String, writable: bool },
    /// 本次运行的真实代码变更(worktree 模式,`Finished` 之前发出)。
    Diff { diff: RunDiff, branch: String },
    /// 已提出合入申请,等待人工裁决(P5)。Runner 自己永不合入。
    ApprovalRequested { approval_id: String, branch: String, worktree_path: String },
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
    /// worktree 模式下的真实代码变更(只读模式为 None)。
    /// 正文属 run 存储侧;审计只存其 [`ContentHash`]。
    pub diff: Option<RunDiff>,
    /// 隔离分支名(供人工检出复核;合入与 push 需单独授权,Runner 不做)。
    pub branch: Option<String>,
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

/// 仓库快照:同时给出**用于比对的哈希**与**用于检出的原值**。
///
/// git 仓返回 `(sha256("git-head:<commit>"), Some(commit))`;非 git 目录降级为
/// 顶层清单哈希且原值为 `None`——降级口径进哈希原文(`dir:` 前缀),
/// 重放校验时能看出精度,不伪造。
///
/// **锻造与验真必须用同一个函数**,否则比的是两把尺子。
pub fn repo_snapshot_of(ws: &std::path::Path) -> (ContentHash, Option<String>) {
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(["rev-parse", "HEAD"])
        .output();
    if let Ok(o) = head {
        if o.status.success() {
            let commit = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            return (
                ContentHash::sha256(format!("git-head:{commit}").as_bytes()),
                Some(commit),
            );
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
    (ContentHash::sha256(format!("dir:{}", names.join(",")).as_bytes()), None)
}

fn deps_lock_hash(ws: &std::path::Path) -> ContentHash {
    for lock in ["Cargo.lock", "pnpm-lock.yaml", "package-lock.json"] {
        if let Ok(bytes) = std::fs::read(ws.join(lock)) {
            return ContentHash::sha256(&bytes);
        }
    }
    ContentHash::sha256(b"deps:none")
}

/// 取走本次运行的变更并广播,随后执行保留策略。
///
/// 失败**不吞**:发 Notice 如实告知,因为"没有 diff"与"取 diff 失败"
/// 在证据层面是两回事。
///
/// 保留策略(§7.4 后半句,见 [`crate::worktree::RetentionPolicy`]):
/// 无变更 ⇒ 立即回收;有变更 ⇒ 留到处置完毕(P5 审批),仅受数量上限兜底。
/// **注意 `worktree` 被 take 走**:清理会消费所有权,调用方之后拿到的是 None。
fn take_diff(
    worktree: &mut Option<Worktree>,
    policy: RetentionPolicy,
    badge: &str,
    commit_subject: &str,
    on_event: &mut impl FnMut(RunnerEvent),
) -> (Option<RunDiff>, Option<String>) {
    let Some(wt) = worktree.as_ref() else { return (None, None) };
    let branch = wt.branch.clone();
    let (diff, keep) = match wt.diff() {
        Ok(d) => {
            let has_change = !d.is_empty();
            on_event(RunnerEvent::Diff { diff: d.clone(), branch: branch.clone() });
            (Some(d), has_change)
        }
        Err(e) => {
            on_event(RunnerEvent::Notice { text: format!("取 diff 失败:{e}") });
            // 取不到 diff 时保守保留:宁可留个空壳待人工查看,也不销毁可能的证据
            (None, true)
        }
    };

    if keep {
        // 有变更:先提交到隔离分支——不提交则分支等于没动过,合入会是一场空。
        if let Some(wt) = worktree.as_ref() {
            if diff.as_ref().is_some_and(|d| !d.is_empty()) {
                if let Err(e) = wt.commit(badge, &format!("{}(Agent 产出,待审批)", commit_subject))
                {
                    on_event(RunnerEvent::Notice { text: format!("提交到隔离分支失败:{e}") });
                }
            }
        }
        // 留待处置。仅做数量兜底,防止审批流失灵时无限堆积。
        if let Some(wt) = worktree.as_ref() {
            match enforce_retention(&wt.base, wt.path.parent().unwrap_or(&wt.base), policy) {
                Ok(removed) if !removed.is_empty() => on_event(RunnerEvent::Notice {
                    text: format!("保留上限 {} 已达,回收了 {} 个最旧的隔离工作区", policy.keep, removed.len()),
                }),
                Err(e) => on_event(RunnerEvent::Notice { text: format!("保留策略执行失败:{e}") }),
                _ => {}
            }
        }
        return (diff, Some(branch));
    }

    // 无变更:没有任何保留价值,立即回收(证据已在审计链里)
    if let Some(wt) = worktree.take() {
        if let Err(e) = wt.cleanup() {
            on_event(RunnerEvent::Notice { text: format!("回收隔离工作区失败:{e}") });
        }
    }
    (diff, Some(branch))
}

// ---------------------------------------------------------------- 主循环

pub async fn run_task(
    router: &Router,
    audit: &Arc<Mutex<AuditStore>>,
    cfg: &RunnerConfig,
    spec: TaskSpec,
    on_event: impl FnMut(RunnerEvent) + Send,
) -> Result<RunSummary, RunnerError> {
    run_task_at(router, audit, cfg, spec, None, on_event).await
}

/// 与 [`run_task`] 相同,但把隔离工作区建在**指定基线** commit 上。
///
/// 影子重放靠它把代码状态对齐到锻造时刻——拿今天的 HEAD 去跑昨天的能力,
/// 比出来的差异说明不了任何问题。`at = None` 即普通任务(从当前 HEAD)。
pub async fn run_task_at(
    router: &Router,
    audit: &Arc<Mutex<AuditStore>>,
    cfg: &RunnerConfig,
    spec: TaskSpec,
    at: Option<&str>,
    mut on_event: impl FnMut(RunnerEvent) + Send,
) -> Result<RunSummary, RunnerError> {
    // commit 主题取任务提示词首行(截断),让主仓历史一眼能看出这次改动为何而来
    let subject: String = {
        let line = spec.prompt.lines().next().unwrap_or("Agent 任务").trim();
        let s: String = line.chars().take(50).collect();
        if s.is_empty() { "Agent 任务".into() } else { s }
    };

    // ---- 工作区:worktree 隔离(可写)或直连(只读)
    // 非 git 仓不假装隔离——如实降级为只读并告知,绝不在用户工作区上开写权限。
    let mut worktree: Option<Worktree> = None;
    if let Some(root) = &spec.workspace_root {
        match Worktree::create_at(&spec.workspace, root, &spec.run_id, at) {
            Ok(wt) => {
                on_event(RunnerEvent::WorkspaceReady {
                    path: wt.path.display().to_string(),
                    branch: wt.branch.clone(),
                    writable: true,
                });
                worktree = Some(wt);
            }
            Err(e) => on_event(RunnerEvent::Notice {
                text: format!("无法建立隔离工作区({e}),本次以只读模式运行"),
            }),
        }
    }
    let tools = match &worktree {
        Some(wt) => ToolSet::writable(&wt.path),
        None => ToolSet::new(&spec.workspace),
    }
    .map_err(|e| RunnerError::Workspace(e.to_string()))?;
    let started = Instant::now();

    // ---- 路由(一次)。拒绝也是证据:落 route.refuse(E4),写失败按审计失败处理。
    let route_req = RouteRequest {
        sources: &spec.sources,
        requested_provider: spec.requested_provider.as_deref(),
        default_provider: spec.default_provider.as_deref(),
    };
    let resolution = match router.resolve(&route_req).await {
        Ok(r) => r,
        Err(e) => {
            let (body, label, locality) = EventBody::route_refuse(&e, cfg.policy_version.clone());
            audit
                .lock()
                .unwrap()
                .append(NewEvent {
                    ts_ms: None,
                    actor: Actor::agent(&cfg.badge),
                    scope: Scope { team: spec.team.clone(), channel: spec.channel.clone() },
                    run_id: Some(spec.run_id.clone()),
                    session_id: spec.session_id.clone(),
                    policy_version: Some(cfg.policy_version.clone()),
                    label,
                    locality,
                    body,
                })
                .map_err(|er| RunnerError::Audit(er.to_string()))?;
            return Err(RunnerError::Refused(e.to_string()));
        }
    };
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
    // 快照取**基础仓库**而非 worktree:worktree 是从基线派生的副本,
    // 记它的 HEAD 等于记一个用完就删的引用,重放时无从检出。
    let (snapshot_hash, snapshot_ref) = repo_snapshot_of(
        worktree.as_ref().map(|w| w.base.as_path()).unwrap_or(tools.workspace()),
    );
    let specs = tools.specs();
    let tool_names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
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
                repo_snapshot: snapshot_hash,
                repo_ref: snapshot_ref,
                deps_lock: deps_lock_hash(tools.workspace()),
                model: ModelRef {
                    provider_id: meta.id.clone(),
                    model: meta.model.clone(),
                    params_hash: ContentHash::sha256(params_repr.as_bytes()),
                },
                // 工具环境按实际形态记账:读写模式不同 ⇒ 哈希不同,
                // Capsule 重放时不会把可写运行误认成只读运行。
                tool_env: ContentHash::sha256(
                    serde_json::json!({
                        "tools": tool_names,
                        "workspace": tools.workspace().display().to_string(),
                        "mode": if tools.is_writable() { "worktree_rw" } else { "read_only" },
                        "branch": worktree.as_ref().map(|w| w.branch.clone()),
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
    let tool_list: Vec<&str> = tool_names.iter().map(String::as_str).collect();
    let mut messages = vec![
        ChatMessage::system(if cfg.system_prompt == muster_prompt::SYSTEM_PROMPT {
            // 默认提示词走 A1 的标准拼装(工作区 + 工具清单在同一处维护)
            let mut s = muster_prompt::with_workspace(
                &tools.workspace().display().to_string(),
                &tool_list,
            );
            if let Some(wt) = &worktree {
                s.push_str(&format!(
                    "\n这是本任务的隔离工作区(分支 {}),改动不影响主仓;完成后会以 diff 呈交人工复核。",
                    wt.branch
                ));
            }
            s
        } else {
            format!(
                "{}\n工作区:{}(工具:{})",
                cfg.system_prompt,
                tools.workspace().display(),
                tool_list.join(" / ")
            )
        }),
        ChatMessage::user(spec.prompt.clone()),
    ];
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
                    // 失败也要保住已发生的改动:半成品同样是证据(可复核、可丢弃)
                    take_diff(&mut worktree, cfg.retention, &cfg.badge, &subject, &mut on_event);
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
            // §7.4:先保存 Diff 与证据,再谈清理。取 diff 失败不吞——如实回报。
            let (diff, branch) = take_diff(&mut worktree, cfg.retention, &cfg.badge, &subject, &mut on_event);

            // P5:有变更就提合入申请。Runner 只申请,绝不自行合入。
            // 影子重放(propose_merge=false)例外:其产出用于比对而非落地。
            if let (Some(d), Some(wt)) = (&diff, worktree.as_ref()) {
                if !d.is_empty() && spec.propose_merge {
                    match crate::approval::request_merge(
                        audit,
                        &cfg.badge,
                        &cfg.policy_version,
                        &spec.run_id,
                        scope.clone(),
                        d,
                    ) {
                        Ok(approval_id) => on_event(RunnerEvent::ApprovalRequested {
                            approval_id,
                            branch: wt.branch.clone(),
                            worktree_path: wt.path.display().to_string(),
                        }),
                        Err(e) => return Err(RunnerError::Audit(e.to_string())),
                    }
                }
            }
            append(
                audit,
                EventBody::RunFinish {
                    outcome: RunOutcome::Success,
                    duration_ms: started.elapsed().as_millis() as u64,
                    // 有变更时输出哈希取 diff(证据指向代码变更本身),
                    // 否则取回答文本。审计只存哈希,正文留 run 存储侧。
                    output_hash: Some(match &diff {
                        Some(d) if !d.is_empty() => ContentHash::sha256(d.patch.as_bytes()),
                        _ => ContentHash::sha256(final_text.as_bytes()),
                    }),
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
                diff,
                branch,
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

    let (diff, branch) = take_diff(&mut worktree, cfg.retention, &cfg.badge, &subject, &mut on_event);
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
        diff,
        branch,
    })
}
