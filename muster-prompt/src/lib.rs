//! # muster-prompt — A1 正式 Agent 提示词(唯一出处)
//!
//! 为什么单独成 crate:A7 评测的度量对象是**「系统提示词 + provider」整体**
//! (见 muster-eval README)。若执行器与评测各写一份,报告里的成功率就不再
//! 描述线上行为——闸门证据当场失真。零依赖,两边都能轻量引用。
//!
//! **改动本文件即改动 G0′/W3 闸门证据的前提:必须重跑 A7 评测。**
//! 提示词版本号随文本一起变更,写进报告与审计的 params_hash。

/// 提示词版本(随文本变更递增;进报告与 `ModelRef::params_hash`)。
///
/// - `a1-v1`:首版。W3 实测 kimi-k3 93.3%(56/60)。
/// - `a1-v2`:修 v1 的回归——"一次做一步"被模型读成"任何事都先查一遍",
///   连"在第 12 行留评论"这种已明确到位的指令也先 `read_file`。现改为
///   按信息是否充分分流:够了就直接执行,不够才先查。
pub const VERSION: &str = "a1-v2";

/// A1 正式系统提示词。
///
/// 编写原则(逐条对应评测集里的失分模式):
/// 1. 工具优先于猜测——负样本类要求"能直接回答就别调工具",故两侧都写死;
/// 2. 参数严格符合 schema——参数抽取/类型正确性类的失分几乎全来自臆造字段;
/// 3. **信息够就直接做**——v1 的"一次一步"被读成"凡事先查一遍",连已明确到
///    行号的指令也先读文件(实测回归);改为按信息充分度分流;
/// 4. 不预支未观测到的结论——多轮衔接类的真实要求(与第 3 条并存,不矛盾);
/// 5. 边界如实回报——工具被拒绝(越权/路径越界)时说明事实,不绕道、不假装成功;
/// 6. 中文作答、引用带路径——产品语境与可追溯性。
pub const SYSTEM_PROMPT: &str = "你是 Muster 的代码协作 Agent(工牌 A-007)。

工作方式:
- 需要外部信息或需要执行操作时,必须调用已声明的工具;能直接回答的问题就直接用文本回答,不要调用无关工具。
- 工具参数严格符合 schema:只填 schema 里声明的字段,不臆造字段名或取值;枚举取声明值之一;数字用数字类型。
- 用户已给足执行所需信息时(如已指明文件与行号、已给出完整内容),直接执行对应工具,不要先做多余的查看。信息确实不足时才先查,查到再执行。
- 不要预支尚未观测到的结论:工具结果没回来之前,不臆断它会返回什么。
- 工具返回拒绝或错误时(如路径越界、权限不足),如实说明事实与原因,不要绕道尝试或假装成功。

回答风格:用中文,简洁直接;引用代码时带相对路径,必要时带行号。";

/// 拼上运行期上下文(工作区、可用工具)的完整系统消息。
/// 评测集不带工作区,直接用 [`SYSTEM_PROMPT`];执行器用本函数。
///
/// **本函数不属于 A7 的度量对象**([`SYSTEM_PROMPT`] 才是),故此处的运行期
/// 措辞变更不使闸门证据失效——改上面那个常量才需要重跑评测。
pub fn with_workspace(workspace: &str, tools: &[&str]) -> String {
    with_mode(workspace, tools, false)
}

/// 同上,但按工作区是否可写分流措辞。
///
/// `writable` 为真时意味着这是本次任务的**隔离 worktree**,此时才会有
/// `run_command`。多加的那两句是有来由的:工具摆在那里不等于模型会用——
/// 交付前先自己跑一遍,才是「产出经人裁决」而不是「人替机器当编译器」。
pub fn with_mode(workspace: &str, tools: &[&str], writable: bool) -> String {
    let mut s = format!(
        "{SYSTEM_PROMPT}\n\n当前工作区:{workspace}\n可用工具{}:{}",
        if writable { "" } else { "(只读)" },
        tools.join(" / ")
    );
    if writable {
        s.push_str(
            "\n\n交付前自检:改完代码后,用 run_command 跑一遍构建或测试确认它真的能跑\
             (清单见该工具说明)。测试失败就继续修,别把没跑过的改动交出去;\
             确实跑不了(缺依赖、无对应命令)就在结论里如实说明,不要含糊带过。",
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_suffix_keeps_base_prompt_intact() {
        let s = with_workspace("/tmp/ws", &["list_dir", "read_file"]);
        assert!(s.starts_with(SYSTEM_PROMPT), "基础提示词必须原样在前");
        assert!(s.contains("/tmp/ws") && s.contains("list_dir / read_file"));
    }

    /// 提示词是闸门证据的一部分:改文本必须同时改版本号,否则报告无法区分。
    #[test]
    fn version_is_declared() {
        assert!(VERSION.starts_with("a1-v"));
        assert!(!SYSTEM_PROMPT.trim().is_empty());
    }
}
