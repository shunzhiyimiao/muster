//! 试次执行器:走**流式路径**(chat_stream + collect_stream),与 Runner 未来的
//! 生产路径一致——评的是"我们实际会用的通道",不是理想化的非流式接口。
//!
//! 错误处理三分法:
//! - 模型答错          → Fail(计入成功率分母)
//! - 传输类可重试错误  → 重试;重试耗尽 → Infra(**不**计入分母,单独披露)
//! - Auth/配置错误     → FatalProvider(整个 provider 终止,报告作废该列)

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use muster_provider::{
    collect_stream, ChatMessage, ChatRequest, ModelProvider, ProviderError, ToolChoice,
};

use crate::grade::{grade_turn, snippet, TurnExpectation};
use crate::samples::Sample;

pub const SYSTEM_PROMPT: &str = "你是 Muster 的代码协作 Agent。需要外部信息或执行操作时,必须调用已声明的工具,参数严格符合工具 schema;能直接回答的问题就直接用文本回答,不要调用无关工具。";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrialStatus {
    Pass,
    Fail,
    Infra,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrialRecord {
    pub sample_id: String,
    pub category: crate::samples::Category,
    pub trial: usize,
    pub status: TrialStatus,
    /// Fail 的评分原因或 Infra 的错误描述。
    pub reasons: Vec<String>,
    /// 每回合模型实际做了什么(报告排障用)。
    pub transcript: Vec<String>,
}

/// provider 级致命错误(密钥错等),调用方应终止该 provider 的整轮评测。
pub struct FatalProvider(pub String);

/// 生成参数。默认值即本评测的历史口径:temperature=0、max_tokens=512。
/// 思考型模型(如 Kimi K3:仅接受 temperature=1,且思考计入 completion tokens)
/// 需放开两者;实际取值写入报告一并披露,不允许静默偏离口径。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GenParams {
    pub temperature: f32,
    pub max_tokens: u32,
}

impl Default for GenParams {
    fn default() -> Self {
        Self { temperature: 0.0, max_tokens: 512 }
    }
}

pub async fn run_trial(
    provider: &Arc<dyn ModelProvider>,
    sample: &Sample,
    trial: usize,
    gen: &GenParams,
) -> Result<TrialRecord, FatalProvider> {
    let tools = crate::samples::tools_by_name(&sample.tools);
    let mut messages = vec![ChatMessage::system(SYSTEM_PROMPT)];
    let mut transcript = Vec::new();

    let record = |status: TrialStatus, reasons: Vec<String>, transcript: Vec<String>| TrialRecord {
        sample_id: sample.id.to_owned(),
        category: sample.category,
        trial,
        status,
        reasons,
        transcript,
    };

    for (turn_idx, turn) in sample.turns.iter().enumerate() {
        if let Some(user) = &turn.user_message {
            messages.push(ChatMessage::user(user.clone()));
        }
        let req = ChatRequest {
            messages: messages.clone(),
            tools: tools.clone(),
            tool_choice: Some(ToolChoice::Auto),
            temperature: Some(gen.temperature),
            max_tokens: Some(gen.max_tokens),
            run_id: Some(format!("eval:{}:{}", sample.id, trial)),
        };

        let resp = match call_with_retry(provider, req).await {
            Ok(resp) => resp,
            Err(RetryOutcome::Fatal(msg)) => return Err(FatalProvider(msg)),
            Err(RetryOutcome::Infra(msg)) => {
                return Ok(record(TrialStatus::Infra, vec![msg], transcript));
            }
        };

        transcript.push(describe_response(turn_idx, &resp));

        let fails = grade_turn(&resp, &tools, &turn.expect);
        if !fails.is_empty() {
            return Ok(record(TrialStatus::Fail, fails, transcript));
        }

        // 该回合通过;若还有后续回合,把对话续上。
        let is_last = turn_idx + 1 == sample.turns.len();
        if !is_last {
            messages.push(resp.message.clone());
            if let TurnExpectation::Calls { .. } = &turn.expect {
                let result = turn
                    .canned_tool_result
                    .clone()
                    .unwrap_or_else(|| "ok".to_owned());
                for call in &resp.message.tool_calls {
                    messages.push(ChatMessage::tool(call.id.clone(), result.clone()));
                }
            }
        }
    }

    Ok(record(TrialStatus::Pass, Vec::new(), transcript))
}

enum RetryOutcome {
    Infra(String),
    Fatal(String),
}

async fn call_with_retry(
    provider: &Arc<dyn ModelProvider>,
    req: ChatRequest,
) -> Result<muster_provider::ChatResponse, RetryOutcome> {
    const BACKOFF: [u64; 2] = [1, 3];
    let model = provider.metadata().model.clone();
    let mut attempt = 0usize;
    loop {
        let result = match provider.chat_stream(req.clone()).await {
            Ok(stream) => collect_stream(stream, model.clone()).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(resp) => return Ok(resp),
            Err(e @ (ProviderError::Auth(_) | ProviderError::Config(_))) => {
                return Err(RetryOutcome::Fatal(e.to_string()));
            }
            Err(e) if e.is_retryable() && attempt < BACKOFF.len() => {
                tokio::time::sleep(Duration::from_secs(BACKOFF[attempt])).await;
                attempt += 1;
            }
            Err(e) => return Err(RetryOutcome::Infra(e.to_string())),
        }
    }
}

fn describe_response(turn_idx: usize, resp: &muster_provider::ChatResponse) -> String {
    if resp.message.tool_calls.is_empty() {
        format!(
            "回合{}: 文本作答「{}」",
            turn_idx + 1,
            snippet(resp.message.content.as_deref().unwrap_or(""))
        )
    } else {
        let calls: Vec<String> = resp
            .message
            .tool_calls
            .iter()
            .map(|c| format!("{}({})", c.name, snippet(&c.arguments)))
            .collect();
        format!("回合{}: 调用 {}", turn_idx + 1, calls.join(" ; "))
    }
}
