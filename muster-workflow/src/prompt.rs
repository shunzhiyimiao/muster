//! 交给 Runner 的任务提示词:把工作流的约定(目录、文件、联网边界、产物)说清楚,
//! 再接上用户写的步骤。约定写在这里而不是每个 WORKFLOW.md 里,免得每个人各写各的。

use crate::spec::WorkflowSpec;

pub struct PromptContext<'a> {
    /// `YYYY-MM-DD`。
    pub today: &'a str,
    /// `scheduled` / `manual`。
    pub trigger: &'a str,
    pub scheduled_for: Option<&'a str>,
    /// `data/` 里已有的文件名(排好序),第一次运行为空。
    pub data_files: &'a [String],
    /// 本次是否提供了 http_get(白名单非空且未被演习/密级挡下)。
    pub network: bool,
}

pub fn build_prompt(spec: &WorkflowSpec, ctx: &PromptContext<'_>) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "这是定时工作流「{}」的一次运行(触发方式:{}{})。今天是 {}。\n{}\n\n",
        spec.name,
        if ctx.trigger == "manual" { "手动" } else { "定时" },
        ctx.scheduled_for.map(|t| format!(",计划时刻 {t}")).unwrap_or_default(),
        ctx.today,
        spec.description
    ));
    s.push_str("工作区是这个工作流**自有的仓库**,约定如下,请严格遵守:\n");
    s.push_str(&format!(
        "- data/:原始数据。本次抓到的数据存为 data/{}.json(结构自定,但同一工作流要保持一致,方便逐日对比)。\n",
        ctx.today
    ));
    if ctx.data_files.is_empty() {
        s.push_str("  目前没有历史数据:这是第一次运行,没有可对比的前一天,报告里如实说明。\n");
    } else {
        s.push_str(&format!("  已有历史数据文件:{}。做环比就读最近的一份。\n", ctx.data_files.join("、")));
    }
    s.push_str(
        "- report.html:本次报告,**必须写出**。单文件、自包含:内联 CSS,图表用内联 SVG 画(柱状 / 折线即可),\
         不引用任何外部资源、不用外链脚本。\n\
         - summary.md:3 到 5 行的要点摘要,会原样发到频道。\n",
    );
    if spec.egress_hosts.is_empty() {
        s.push_str("本工作流不联网:只用工作区里已有的数据。\n");
    } else if ctx.network {
        s.push_str(&format!(
            "外部数据用 http_get 获取,只允许这些主机:{}。不跟随重定向;取回的是文本,自行解析 JSON / CSV。\
             某个数据源取不到就换白名单里的另一个,都取不到就在报告里如实标注缺失,不要编造数字。\n",
            spec.egress_hosts.join("、")
        ));
    } else {
        s.push_str(
            "本次运行**不允许联网**(演习锁定,或有效密级高于上云阈值):不要调用外部数据源,\
             用 data/ 里已有的历史数据完成报告,并在报告开头注明「本次数据未更新」。\n",
        );
    }
    s.push_str(&format!(
        "输出格式:{}(PDF 等由系统在你写出 report.html 之后生成,你只负责 HTML)。\n",
        spec.outputs.iter().map(|o| o.as_str()).collect::<Vec<_>>().join(" / ")
    ));
    s.push_str("把 data 与报告都写进工作区就算完成;写完用 read_file 自查一次 report.html 是否完整。\n\n");
    s.push_str("工作流步骤:\n");
    s.push_str(&spec.body);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn spec(extra: &str) -> WorkflowSpec {
        let text = format!(
            "---\nname: a\ndescription: 演示\nschedule: daily 12:00\n{extra}\n---\n1. 抓。\n2. 比。\n"
        );
        WorkflowSpec::parse(&text, Path::new("/x/a/WORKFLOW.md"), Path::new("/x"), 0).unwrap()
    }

    #[test]
    fn prompt_states_conventions_network_and_steps() {
        let s = spec("egress: [api.example.test]\noutputs: [html, pdf]");
        let files = vec!["2026-09-22.json".to_string()];
        let p = build_prompt(&s, &PromptContext { today: "2026-09-23", trigger: "scheduled", scheduled_for: Some("12:00"), data_files: &files, network: true });
        assert!(p.contains("data/2026-09-23.json"));
        assert!(p.contains("2026-09-22.json"));
        assert!(p.contains("api.example.test"));
        assert!(p.contains("report.html"));
        assert!(p.contains("html / pdf"));
        assert!(p.ends_with("1. 抓。\n2. 比。"));

        let p2 = build_prompt(&s, &PromptContext { today: "2026-09-23", trigger: "manual", scheduled_for: None, data_files: &[], network: false });
        assert!(p2.contains("不允许联网"));
        assert!(p2.contains("第一次运行"));
        let p3 = build_prompt(&spec(""), &PromptContext { today: "d", trigger: "manual", scheduled_for: None, data_files: &[], network: false });
        assert!(p3.contains("不联网"));
    }
}
