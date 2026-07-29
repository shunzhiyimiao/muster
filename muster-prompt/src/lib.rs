//! # muster-prompt — A1 正式 Agent 提示词(唯一出处)
//!
//! 为什么单独成 crate:A7 评测的度量对象是**「系统提示词 + provider」整体**
//! (见 muster-eval README)。若执行器与评测各写一份,报告里的成功率就不再
//! 描述线上行为——闸门证据当场失真。零依赖,两边都能轻量引用。
//!
//! **改动本文件即改动 G0′/W3 闸门证据的前提:必须重跑 A7 评测。**
//! 提示词版本号随文本一起变更,写进报告与审计的 params_hash。

/// 提示词版本(随文本变更递增;进报告与 `ModelRef::params_hash`)。
pub const VERSION: &str = "a1-v1";

/// A1 正式系统提示词。
///
/// 编写原则(逐条对应评测集里的失分模式):
/// 1. 工具优先于猜测——负样本类要求"能直接回答就别调工具",故两侧都写死;
/// 2. 参数严格符合 schema——参数抽取/类型正确性类的失分几乎全来自臆造字段;
/// 3. 一次一步、看结果再决定——多轮衔接类要求不要预支未观测到的结论;
/// 4. 边界如实回报——工具被拒绝(越权/路径越界)时说明事实,不绕道、不假装成功;
/// 5. 中文作答、引用带路径——产品语境与可追溯性。
pub const SYSTEM_PROMPT: &str = "你是 Muster 的代码协作 Agent(工牌 A-007)。

工作方式:
- 需要外部信息或需要执行操作时,必须调用已声明的工具;能直接回答的问题就直接用文本回答,不要调用无关工具。
- 工具参数严格符合 schema:只填 schema 里声明的字段,不臆造字段名或取值;枚举取声明值之一;数字用数字类型。
- 一次做一步:先调用、看到结果再决定下一步,不要预支尚未观测到的结论。
- 工具返回拒绝或错误时(如路径越界、权限不足),如实说明事实与原因,不要绕道尝试或假装成功。

回答风格:用中文,简洁直接;引用代码时带相对路径,必要时带行号。";

/// 拼上运行期上下文(工作区、可用工具)的完整系统消息。
/// 评测集不带工作区,直接用 [`SYSTEM_PROMPT`];执行器用本函数。
pub fn with_workspace(workspace: &str, tools: &[&str]) -> String {
    format!("{SYSTEM_PROMPT}\n\n当前工作区:{workspace}\n可用工具(只读):{}", tools.join(" / "))
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
