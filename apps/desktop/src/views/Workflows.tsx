/* 计划表 · 自定义工作流:WORKFLOW.md 定义的定时计划,到点由桌面壳拉起 Agent 跑一次。
   这一页回答:有哪些工作流、下次什么时候跑、上次跑成什么样、产物在哪、为什么有的没加载。 */
import { useState } from "react";
import { AlertTriangle, Calendar, Clock, FolderOpen, Globe, Lock, Play, RefreshCw, Sparkles } from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { WorkflowsOut, WorkflowRunOut, api } from "../api";

const EXAMPLE = `---
name: daily-finance-brief
description: 每天 12:00 抓全球指数与汇率,与前一天对比,出图文简报。
schedule: daily 12:00          # 也可 weekdays 09:30 / weekly mon 08:00 / hourly / manual / 五字段 cron
skills: [report-html]          # 用哪些技能
egress:                        # http_get 只允许访问这些主机;不写就不联网
  - query1.finance.yahoo.com
  - api.frankfurter.app
outputs: [html, pdf]           # pptx 尚未实现
channel: personal              # 结果发到哪个频道
---

1. 抓近 5 日收盘价与汇率,存到 data/<今天>.json。
2. 读 data/ 里上一份,做环比。
3. 写 report.html(内联 SVG 图表)与 summary.md。`;

const fmt = (ms: number | null | undefined) => (ms ? new Date(ms).toLocaleString("zh-CN", { hour12: false }) : "—");

function outcomeTag(r: WorkflowRunOut) {
  if (r.outcome === null) return <Tag tone="amb">未收尾</Tag>;
  if (r.outcome === "success") return <Tag tone="grn">成功</Tag>;
  if (r.outcome === "no_output") return <Tag tone="amb">没有产物</Tag>;
  return <Tag tone="red">{r.outcome}</Tag>;
}

export function WorkflowsView({ data, onRefresh }: { data: WorkflowsOut | null; onRefresh: () => void }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const flows = data?.workflows ?? [];
  const problems = data?.problems ?? [];
  const roots = data?.roots ?? [];

  const runNow = (name: string) => {
    setBusy(name);
    setMsg(null);
    api
      .workflowRunNow(name)
      .then((rid) => {
        setMsg(`已启动 ${rid}:过程在它所发往的频道里实时可见,跑完这里会出现产物。`);
        onRefresh();
      })
      .catch((e) => setMsg(`启动失败:${e}`))
      .finally(() => setBusy(null));
  };

  return (
    <div className="px-7 pb-8 pt-2">
      <div className="flex items-center mb-4">
        <div className="flex items-center gap-2">
          <Calendar size={16} style={{ color: T.indigo }} />
          <b className="text-[15px]">计划表 · 工作流</b>
          <span className="text-[11px]" style={{ color: T.sub }}>
            {flows.length} 个 · 壳开着才会到点执行,错过的时刻在下次打开时补跑一次
          </span>
        </div>
        <button onClick={onRefresh}
          className="ml-auto inline-flex items-center gap-1.5 text-xs font-semibold px-3 py-1.5 rounded-xl"
          style={{ background: T.soft, color: T.ink }}>
          <RefreshCw size={12} /> 重新扫描
        </button>
      </div>

      {msg && (
        <div className="mb-3 px-3.5 py-2 rounded-xl text-xs" style={{ background: T.indigoSoft, color: T.indigoDeep }}>{msg}</div>
      )}

      <Card className="p-4 mb-4">
        <div className="text-[10px] font-semibold tracking-widest mb-2" style={{ color: T.faint }}>定义目录(先到先得)</div>
        <div className="space-y-1.5">
          {roots.map((r, i) => (
            <div key={r} className="flex items-center gap-2 text-[11.5px]">
              <FolderOpen size={12} style={{ color: T.sub }} />
              <Tag tone={i === 0 ? "ind" : undefined}>{i === 0 ? "仓库级" : "个人级"}</Tag>
              <code className="text-[11px]" style={{ color: T.sub }}>{r}/&lt;name&gt;/WORKFLOW.md</code>
            </div>
          ))}
          <div className="flex items-center gap-2 text-[11.5px]">
            <FolderOpen size={12} style={{ color: T.sub }} />
            <Tag>自有仓库与产物</Tag>
            <code className="text-[11px]" style={{ color: T.sub }}>{data?.runs_root ?? "~/.muster/workflow-runs"}/&lt;name&gt;/{"{repo,out}"}</code>
          </div>
          <div className="text-[10.5px]" style={{ color: T.faint }}>
            PDF:{data?.chrome ? "可用(检测到本机 Chrome / Edge)" : "不可用——没找到 Chrome / Chromium / Edge,可设 MUSTER_CHROME 指向可执行文件;届时只出 HTML"}
          </div>
        </div>
      </Card>

      {flows.length === 0 ? (
        <Card className="p-5">
          <div className="text-[13px] font-semibold">还没有工作流</div>
          <div className="text-[11.5px] mt-1.5 leading-relaxed" style={{ color: T.sub }}>
            在上面任一目录下建 <code>&lt;name&gt;/WORKFLOW.md</code>(目录名必须与 name 一致)。frontmatter 写定时、技能、
            允许的外部主机与产物,正文用自然语言写步骤。仓库自带一个示例 daily-finance-brief。
          </div>
          <pre className="mt-3 p-3 rounded-xl text-[11px] leading-relaxed overflow-x-auto"
            style={{ background: T.panel, border: `1px solid ${T.line}`, color: T.ink }}>{EXAMPLE}</pre>
        </Card>
      ) : (
        <div className="space-y-4">
          {flows.map((w) => (
            <Card key={w.name} className="p-5">
              <div className="flex items-center gap-2 flex-wrap">
                <span className="text-[10px] font-extrabold tracking-widest" style={{ color: T.indigo }}>WORKFLOW</span>
                <Tag tone={w.origin === "workspace" ? "ind" : undefined}>{w.origin === "workspace" ? "仓库级" : "个人级"}</Tag>
                <Tag><Clock size={10} /> {w.schedule_text}</Tag>
                {w.sensitivity && (
                  <Tag tone={w.sensitivity === "restricted" ? "red" : w.sensitivity === "internal" ? "amb" : undefined}>
                    <Lock size={10} /> {w.sensitivity}
                  </Tag>
                )}
                {w.running && <Tag tone="grn">运行中</Tag>}
                <span className="ml-auto">
                  <button onClick={() => runNow(w.name)} disabled={busy === w.name || w.running}
                    className="inline-flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
                    style={{ background: T.indigo, color: "#fff", opacity: busy === w.name || w.running ? 0.5 : 1 }}>
                    <Play size={12} /> 立即运行
                  </button>
                </span>
              </div>
              <div className="mt-2.5 text-[15px]"><b>{w.name}</b></div>
              <div className="text-[11.5px] mt-1 leading-relaxed" style={{ color: T.sub }}>{w.description}</div>

              <div className="grid grid-cols-2 gap-x-6 gap-y-1.5 mt-3 text-[11px]" style={{ color: T.sub }}>
                <div>下次:<b style={{ color: T.ink }}>{w.next_due_ms ? fmt(w.next_due_ms) : "手动"}</b></div>
                <div>发往:<b style={{ color: T.ink }}>{w.channel ?? "personal"}</b></div>
                <div className="flex items-center gap-1 flex-wrap">
                  <Sparkles size={11} /> 技能:{w.skills.length ? w.skills.map((s) => <Tag key={s}>{s}</Tag>) : <span style={{ color: T.faint }}>无</span>}
                </div>
                <div className="flex items-center gap-1 flex-wrap">
                  产物:{w.outputs.map((o) => <Tag key={o} tone={o === "pptx" ? "amb" : undefined}>{o}</Tag>)}
                </div>
                <div className="col-span-2 flex items-center gap-1 flex-wrap">
                  <Globe size={11} /> 外部主机:{w.egress.length ? w.egress.map((h) => <Tag key={h}>{h}</Tag>) : <span style={{ color: T.faint }}>不联网</span>}
                </div>
              </div>

              <div className="mt-3 text-[10px] font-semibold tracking-widest" style={{ color: T.faint }}>最近运行</div>
              {w.runs.length === 0 ? (
                <div className="text-[11px] mt-1" style={{ color: T.faint }}>还没跑过。</div>
              ) : (
                <div className="mt-1 space-y-1.5">
                  {w.runs.map((r) => (
                    <div key={r.run_id} className="rounded-xl px-3 py-2 text-[11px]" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
                      <div className="flex items-center gap-2 flex-wrap">
                        <span style={{ color: T.sub }}>{fmt(r.ts_ms)}</span>
                        <Tag>{r.trigger === "manual" ? "手动" : "定时"}</Tag>
                        {outcomeTag(r)}
                        <code style={{ color: T.faint }}>{r.run_id}</code>
                        {r.merged_commit && <span style={{ color: T.faint }}>合入 {r.merged_commit.slice(0, 8)}</span>}
                      </div>
                      {r.artifacts.length > 0 && (
                        <div className="mt-1 flex gap-1.5 flex-wrap">
                          {r.artifacts.map(([kind, hash]) => (
                            <Tag key={kind + hash} tone={kind === "pdf" ? "ind" : undefined}>{kind} · {hash.slice(7, 15)}</Tag>
                          ))}
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              )}
              <div className="mt-2 text-[10.5px] break-all" style={{ color: T.faint }}>
                定义 {w.path} · 产物目录 {w.out_dir}
              </div>
            </Card>
          ))}
        </div>
      )}

      {problems.length > 0 && (
        <Card className="p-4 mt-4" style={{ borderColor: T.amber }}>
          <div className="flex items-center gap-1.5 mb-2">
            <AlertTriangle size={12} style={{ color: T.amber }} />
            <span className="text-[10px] font-semibold tracking-widest" style={{ color: T.faint }}>没能加载的工作流</span>
            <span className="ml-auto text-[10px] font-bold" style={{ color: T.amber }}>{problems.length} 项</span>
          </div>
          <div className="space-y-1.5">
            {problems.map((p) => (
              <div key={p} className="text-[11px] leading-relaxed break-all" style={{ color: T.sub }}>{p}</div>
            ))}
          </div>
        </Card>
      )}

      <div className="mt-5 text-[10.5px] leading-relaxed" style={{ color: T.faint }}>
        每次运行是一条普通的审计链:workflow.trigger 记定义哈希,跑的过程与普通任务一样(run.start → model.call → …),
        http_get 的每次抓取落 tool.egress 并计入演习报告的外发字节,workflow.output 记产物哈希与合入 commit。
        产出自动合入工作流自有仓库(里面没有人的代码,所以不走审批);要改你的代码仓库,那是任务,不是工作流。
      </div>
    </div>
  );
}
