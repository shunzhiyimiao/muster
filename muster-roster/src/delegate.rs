//! `delegate`:主 Agent 把子任务交给另一张工牌,在**同一个工作区**里跑一个子 run。
//!
//! ## 为什么是"同一个工作区、串行、只一层"
//!
//! - 同一个工作区:子 Agent 要看得见父 Agent 改到一半的东西,父 Agent 也要看得见子
//!   Agent 的产出;各开 worktree 再合并,冲突解决没人负责。diff 与合入仍由父 run 统一
//!   处理——审批人看到的是一份改动,不是两份。
//! - 串行:父 Agent 在工具调用里等子 run 跑完,拿到结论再继续。并行意味着两个 Agent
//!   同时改同一个目录,v0 不做。
//! - 只一层:子 run 拿不到 `delegate` 工具。递归委派会让"谁在干活"变得说不清,而这套
//!   系统的价值恰恰是说得清。
//!
//! ## 权限
//!
//! 子 run 按**被委派工牌**的岗位卡装配:岗位说明书、命令白名单、能力哈希;工具档位
//! 取父工作区可写与否**和**该工牌档位的交集——只读工牌到了可写的工作区仍然只读。
//! 密级上限照旧是硬闸:有效密级高于子工牌上限,委派被拒并留痕。
//!
//! ## 审计
//!
//! 父 run 落 `run.delegate`(去向、子 run_id、指令哈希),子 run 有自己的完整链
//! (actor 是子工牌,run_id 形如 `<父>/<工牌>-<序号>`,session_id 与父相同)。
//! 子 run 失败不拖垮父 run:结果以文本回给父 Agent,由它决定怎么办——这与工具被拒的
//! 处理方式一致(模型应当看见边界)。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use muster_audit::{Actor, AuditStore, ContentHash, EventBody, NewEvent, Scope};
use muster_provider::ToolSpec;
use muster_route::{LabelSource, Router, Sensitivity};
use muster_runner::{run_task, ExtraTool, RunnerConfig, RunnerEvent, TaskSpec, ToolOutcome, WorkspaceOverride};
use muster_skill::SkillMeta;

use crate::{AgentSpec, Roster, ToolMode};

/// 子 run 的事件去哪儿:调用方决定怎么显示(桌面壳把它们流进父任务卡)。
pub type EventSink = Arc<dyn Fn(RunnerEvent) + Send + Sync>;

/// 父 run 的上下文:子 run 沿用它的路由、审计、会话与密级来源。
pub struct DelegateContext {
    pub router: Arc<Router>,
    pub audit: Arc<Mutex<AuditStore>>,
    pub roster: Roster,
    /// 谁在委派(父工牌)。
    pub from: AgentSpec,
    pub parent_run_id: String,
    pub session_id: Option<String>,
    pub team: Option<String>,
    pub channel: Option<String>,
    pub sources: Vec<LabelSource>,
    pub effective: Sensitivity,
    pub policy_version: String,
    /// 父 run 的工作区路径与是否可写。worktree 模式下路径在 `WorkspaceReady` 之后才知道,
    /// 所以是一个可延迟填充的槽;直连只读模式下调用方一开始就填好。
    pub workspace: Arc<Mutex<Option<(PathBuf, bool)>>>,
    /// 工作区里的技能(子工牌按岗位卡从中挑)。
    pub skills: Vec<SkillMeta>,
    /// 子 run 的默认 provider(与父相同)。
    pub default_provider: Option<String>,
    pub max_turns: usize,
    pub on_event: EventSink,
}

pub struct DelegateTool {
    ctx: Arc<DelegateContext>,
    seq: AtomicUsize,
}

impl DelegateTool {
    pub fn new(ctx: DelegateContext) -> Arc<Self> {
        Arc::new(Self { ctx: Arc::new(ctx), seq: AtomicUsize::new(0) })
    }

    pub fn spec(&self) -> ToolSpec {
        let others: Vec<String> = self
            .ctx
            .roster
            .agents
            .iter()
            .filter(|a| a.badge != self.ctx.from.badge)
            .map(|a| format!("{}({},{})", a.badge, a.name, a.role))
            .collect();
        ToolSpec {
            name: "delegate".into(),
            description: format!(
                "把一个明确的子任务交给另一张工牌去做,在当前工作区里同步完成后把结论回给你。\
                 适合需要不同岗位的活(比如让实现工程师改代码、让评审员复核)。\
                 一次只交一件事,说清楚要做什么、做到什么程度算完。可委派:{}。",
                if others.is_empty() { "(名册里没有别的工牌)".to_string() } else { others.join("、") }
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "badge": { "type": "string", "description": "工牌号、名字或别名" },
                    "instructions": { "type": "string", "description": "交给对方的完整任务说明" }
                },
                "required": ["badge", "instructions"]
            }),
        }
    }

    /// 包成 Runner 的外部工具。
    pub fn into_extra_tool(self: Arc<Self>) -> ExtraTool {
        let spec = self.spec();
        let me = self;
        ExtraTool {
            spec,
            call: Arc::new(move |args: String| {
                let me = me.clone();
                Box::pin(async move { me.call(&args).await })
            }),
        }
    }

    fn refuse(text: String) -> ToolOutcome {
        ToolOutcome { text, audit: None }
    }

    pub async fn call(&self, args: &str) -> ToolOutcome {
        let v: serde_json::Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(e) => return Self::refuse(format!("参数不是合法 JSON:{e}")),
        };
        let Some(key) = v["badge"].as_str().filter(|s| !s.trim().is_empty()) else {
            return Self::refuse("缺少必填参数 badge".into());
        };
        let Some(instructions) = v["instructions"].as_str().filter(|s| !s.trim().is_empty()) else {
            return Self::refuse("缺少必填参数 instructions".into());
        };
        let ctx = &self.ctx;
        let Some(to) = ctx.roster.find(key).cloned() else {
            return Self::refuse(format!("拒绝:名册里没有工牌 {key}"));
        };
        if to.badge == ctx.from.badge {
            return Self::refuse("拒绝:不能委派给自己".into());
        }
        if !to.allows_level(ctx.effective) {
            return Self::refuse(format!(
                "拒绝:工牌 {}({})的密级上限低于本任务的有效密级 {:?},不能把活交给它",
                to.badge, to.name, ctx.effective
            ));
        }
        let Some((path, parent_writable)) = ctx.workspace.lock().unwrap().clone() else {
            return Self::refuse("拒绝:父任务的工作区尚未就绪".into());
        };
        let skills = if to.skills.is_empty() {
            ctx.skills.clone()
        } else {
            let mut out = Vec::new();
            for s in &to.skills {
                match ctx.skills.iter().find(|k| &k.name == s) {
                    Some(k) => out.push(k.clone()),
                    None => return Self::refuse(format!("拒绝:工牌 {} 的岗位卡要求技能 {s},但工作区里没有它", to.badge)),
                }
            }
            out
        };

        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let sub_run_id = format!("{}/{}-{n}", ctx.parent_run_id, to.badge);
        let scope = Scope { team: ctx.team.clone(), channel: ctx.channel.clone() };

        // 父 run 留痕:交给谁、子 run 是哪条、指令哈希
        let delegate_event = NewEvent {
            ts_ms: None,
            actor: Actor::agent(&ctx.from.badge),
            scope: scope.clone(),
            run_id: Some(ctx.parent_run_id.clone()),
            session_id: ctx.session_id.clone(),
            policy_version: Some(ctx.policy_version.clone()),
            label: Some(ctx.effective),
            locality: None,
            body: EventBody::RunDelegate {
                to_badge: to.badge.clone(),
                sub_run_id: sub_run_id.clone(),
                instructions_hash: ContentHash::sha256(instructions.as_bytes()),
            },
        };
        if let Err(e) = ctx.audit.lock().unwrap().append(delegate_event) {
            return Self::refuse(format!("拒绝:审计写入失败,不能在没有留痕的情况下委派({e})"));
        }

        (ctx.on_event)(RunnerEvent::Notice {
            text: format!("🤝 {} 把子任务交给 {}({}):{}", ctx.from.name, to.name, to.badge, first_line(instructions)),
        });

        // 子 run:被委派工牌的岗位卡 + 父工作区(档位取交集)+ 不再有 delegate
        let writable = parent_writable && to.tools == ToolMode::Worktree;
        // 父 run 本次只读(隔离工作区没建起来,或父工牌本就只读)时,能写的工牌交过来也只能只读。
        // 这要在结果里明说,不然父 Agent 会以为对方"写好了"
        let readonly_note = if !parent_writable && to.tools == ToolMode::Worktree {
            "(注意:父任务本次没有可写的隔离工作区,子任务只能只读;要真改文件得在任务模式下重来)"
        } else {
            ""
        };
        let cfg = RunnerConfig {
            policy_version: ctx.policy_version.clone(),
            badge: to.badge.clone(),
            persona: Some(to.persona_prompt()),
            skills,
            command_policy: to.commands.as_ref().map(|c| muster_runner::CommandPolicy::with_allow(c.clone())),
            capabilities_hash: Some(to.capabilities_hash.clone()),
            workspace_override: Some(WorkspaceOverride { path: path.clone(), writable }),
            max_turns: ctx.max_turns,
            ..Default::default()
        };
        let task = TaskSpec {
            run_id: sub_run_id.clone(),
            session_id: ctx.session_id.clone(),
            team: ctx.team.clone(),
            channel: ctx.channel.clone(),
            sources: ctx.sources.clone(),
            requested_provider: None,
            default_provider: to.model.clone().or_else(|| ctx.default_provider.clone()),
            prompt: format!(
                "这是一次委派:{}({})把下面的子任务交给你,在同一个工作区里完成。做完把结论写清楚,\
                 它会原样回给委派人。\n\n{instructions}",
                ctx.from.name, ctx.from.badge
            ),
            workspace: path.clone(),
            workspace_root: None,
            propose_merge: false,
        };
        let sink = ctx.on_event.clone();
        let (to_name, to_badge) = (to.name.clone(), to.badge.clone());
        let result = run_task(&ctx.router, &ctx.audit, &cfg, task, move |ev| {
            // 子 run 的事件外面包一层"是谁":收尾转成通告,不冒充父 run 的;
            // 正文增量不转发(子 Agent 的结论会作为工具结果整段回给父 Agent)
            let wrap = |inner: RunnerEvent| RunnerEvent::Delegated { badge: to_badge.clone(), name: to_name.clone(), inner: Box::new(inner) };
            match ev {
                RunnerEvent::Finished { outcome, turns, .. } => {
                    sink(wrap(RunnerEvent::Notice { text: format!("完成子任务({outcome},{turns} 回合)") }))
                }
                RunnerEvent::ToolCall { .. }
                | RunnerEvent::ToolResult { .. }
                | RunnerEvent::Notice { .. }
                | RunnerEvent::Progress { .. } => sink(wrap(ev)),
                _ => {}
            }
        })
        .await;

        if let Ok(s) = &result {
            (ctx.on_event)(RunnerEvent::Delegated {
                badge: to.badge.clone(),
                name: to.name.clone(),
                inner: Box::new(RunnerEvent::Notice { text: format!("结论:{}", cap(&s.final_text, 600).replace('\n', " ")) }),
            });
        }
        match result {
            Ok(s) => ToolOutcome {
                text: format!(
                    "{}({})已完成子任务({},{} 回合){readonly_note}。对方的结论:\n{}",
                    to.name, to.badge, s.outcome, s.turns, cap(&s.final_text, 12_000)
                ),
                audit: None,
            },
            Err(e) => ToolOutcome {
                text: format!("{}({})没能完成子任务:{e}。子 run {} 的过程在审计链上;请你决定是自己做、换人、还是如实报告。", to.name, to.badge, sub_run_id),
                audit: None,
            },
        }
    }
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("").trim();
    let mut out: String = l.chars().take(80).collect();
    if l.chars().count() > 80 {
        out.push('…');
    }
    out
}

fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let cut = (0..=max).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
    format!("{}\n…(结论超过 {max} 字节,已截断)", &s[..cut])
}
