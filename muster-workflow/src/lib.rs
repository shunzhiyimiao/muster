//! # muster-workflow — 自定义工作流(定时 + Agent + 技能 → 报告)
//!
//! 一句话:**让"每天 12:00 抓数据、比昨天、出报告"这种事,变成一个文件 + 一条审计链。**
//!
//! 用户在 `WORKFLOW.md` 里写清楚:什么时候跑(`schedule`)、用哪些技能(`skills`)、
//! 允许访问哪些外部主机(`egress`)、要什么产物(`outputs`)、结果发到哪个频道
//! (`channel`),以及正文——用自然语言写的步骤。到点由调度器(v0 在桌面壳里)拉起
//! muster-runner 跑一次;Agent 在工作流**自有的仓库**里抓数据、写报告;系统合入产出、
//! 渲染 PDF、发到频道、落审计。
//!
//! ## 与已有部件的分工
//!
//! - **muster-runner** 负责跑:路由、工具循环、隔离 worktree、审计链。本 crate 只给它
//!   三样东西——提示词、技能清单、一个额外的异步工具(`http_get`)。
//! - **muster-skill** 负责"怎么做":工作流里的 `skills` 就是技能名,运行时原样提供给
//!   Agent(报告怎么排版、图表怎么画,写成技能比写进每个工作流强)。
//! - **Capsule(P4)** 是"锻造自成功运行的能力";工作流是"人写的定时计划"。
//!   两者不互相替代:工作流的每次运行都是普通 run,成功了照样可以锻造。
//!
//! ## 关键设计决策
//!
//! 1. **工作流有自己的仓库,产出自动合入**(见 [`Runs`])。Runner 的铁律是"只申请、
//!    永不自行合入",那条铁律保护的是**人的代码**;工作流仓库里只有 Agent 自己写的
//!    数据与报告,没有人的代码,每天等人点一次"批准"只会让定时变成假的。所以本 crate
//!    合入,但**只合入自己名下的仓库**([`Runs::merge_branch`] 对路径做归属校验,
//!    指向别处一律拒绝),并落 `workflow.output` 事件(合入 commit 进审计)。
//!    要改人的仓库?那不是工作流,是任务,走审批。
//! 2. **外部数据只能来自白名单主机**([`HttpFetch`]):`egress` 不写就没有网络工具;
//!    写了也只是 GET、只取文本、不跟随重定向(重定向可以把请求带到白名单之外)。
//!    每次抓取落 `tool.egress` 事件,字节数与 `model.call` 同一口径进演习报告——
//!    这是模型通道之外的第二条外发路径,不能因为它"只是抓数据"就不记账(铁律 4)。
//!    演习锁定或有效密级高于上云阈值时,调用方不提供这个工具(与"什么密级能上云"
//!    同一条线:外发就是外发)。
//! 3. **定时语义是"到点跑一次,错过补跑一次"**([`WorkflowSpec::next_due`]):
//!    下一次时刻从 max(上次触发, 定义文件修改时间) 往后算。壳没开错过了 12:00,
//!    下次打开补跑**一次**而不是把错过的每天都补;从没跑过的新工作流不会一发现就跑,
//!    而是等它的第一个时刻。上次触发从审计链查(`workflow.trigger`),不另存状态。
//! 4. **审计只存哈希**:`workflow.trigger` 记定义文件哈希,`workflow.output` 记每个
//!    产物的哈希——"发到频道那份报告是不是产物目录里这份"可查;正文留磁盘。
//! 5. **格式与技能同一套**:`<根>/<name>/WORKFLOW.md`,frontmatter 扁平键值 + 列表,
//!    仓库级放 `.agents/workflows/`(随代码提交),个人级放 `~/.muster/workflows/`。
//!
//! ## 诚实边界(v0)
//!
//! - **调度器在桌面壳里**:壳没开就不跑;补跑只补一次。常驻节点服务属后续(它需要
//!    自己的审计链,不能与桌面壳共写一条——链是单写者)。
//! - **PDF 靠本机 Chrome / Chromium / Edge 的 headless 打印**;没有就只出 HTML。
//!    `pptx` 未实现:声明了会在问题列表里提示,不会假装出了。
//! - 图表由 Agent 用内联 SVG 画(见技能 report-html),没有图表引擎;复杂图表质量取决于模型。
//! - `http_get` 的外发字节是**载荷近似**(请求行 + 主机 + UA),与 model.call 同口径,
//!    不是 wire 级计量;响应最多读 2 MB,给模型看的截到 24 KB。
//! - 没有重试:一次运行失败就记 `failed`,下个时刻再跑。没有并发:同一工作流同一时刻只跑一个。
//! - 时区取本机;工作流不能指定时区。

pub mod http;
pub mod prompt;
pub mod runs;
pub mod schedule;
pub mod spec;

pub use http::HttpFetch;
pub use prompt::{build_prompt, PromptContext};
pub use runs::{find_chrome, Artifact, Rendered, Runs};
pub use schedule::{CronExpr, Schedule};
pub use spec::{discover, load, Discovery, OutputKind, WorkflowSpec, REPO_WORKFLOWS_DIR, WORKFLOW_FILE};

use chrono::{DateTime, Local, TimeZone};

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("读写失败:{0}")]
    Io(String),
    #[error("格式错误:{0}")]
    Format(String),
    #[error("git 失败:{0}")]
    Git(String),
    /// 只合入自己名下的仓库——指向别处的合入请求是设计上就不该出现的。
    #[error("拒绝合入:{0} 不是工作流自有仓库(只有 workflow-runs 目录下的仓库才自动合入)")]
    NotOwned(String),
    #[error("HTTP 客户端错误:{0}")]
    Http(String),
}

/// 毫秒时间戳 → 本地时间;非法值退回"现在"(只用于展示与算下一次,不用于证据)。
pub fn ms_to_local(ms: u64) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms as i64).single().unwrap_or_else(Local::now)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 产物目录名:可排序、无冒号(Windows 文件名不能有冒号)。
pub fn stamp(t: &DateTime<Local>) -> String {
    t.format("%Y-%m-%d_%H%M%S").to_string()
}

pub fn today(t: &DateTime<Local>) -> String {
    t.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 仓库自带的示例工作流(.agents/workflows/daily-finance-brief)必须能加载,
    /// 它引用的技能(report-html)也必须存在——坏了要在这里炸,而不是界面上悄悄少一个。
    #[test]
    fn bundled_example_workflow_and_its_skill_load_cleanly() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let d = discover(&[repo.join(REPO_WORKFLOWS_DIR)]);
        assert!(d.problems.is_empty(), "{:?}", d.problems);
        let w = d.workflows.iter().find(|w| w.name == "daily-finance-brief").expect("示例工作流应被发现");
        assert_eq!(w.schedule.describe_zh(), "每天 12:00");
        assert!(w.outputs.contains(&OutputKind::Pdf));
        assert!(!w.egress_hosts.is_empty());
        let skills = muster_skill::discover(&[repo.join(muster_skill::REPO_SKILLS_DIR)]);
        for s in &w.skills {
            assert!(skills.skills.iter().any(|k| &k.name == s), "示例工作流引用的技能 {s} 不存在");
        }
    }
}
