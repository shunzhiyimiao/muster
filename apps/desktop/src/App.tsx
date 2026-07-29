import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./app.css";

type Sensitivity = "open" | "internal" | "restricted";

interface Channel {
  id: string;
  name: string;
  team: string;
  level: Sensitivity;
  level_note: string;
  desc: string;
}
interface ProviderCard {
  id: string;
  display_name: string;
  model: string;
  locality: string;
}
interface Bootstrap {
  channels: Channel[];
  providers: ProviderCard[];
  policy_cloud_max: Sensitivity;
  audit_db: string;
  egress_locked: boolean;
}
interface DrillReportOut {
  model_calls: number;
  egress_bytes: number;
  unmetered_calls: number;
  local_calls: number;
  cloud_calls: number;
  ok: boolean;
}
interface DrillStatus {
  on: boolean;
  drill_id: string | null;
  report: DrillReportOut | null;
}
interface Decider {
  origin: string;
  level: Sensitivity;
  subject: string;
}
interface Plan {
  effective: Sensitivity;
  deciders: Decider[];
  primary: string;
  primary_locality: string;
  fallbacks: string[];
  downgraded: { from: string | null; reason: string } | null;
  policy_cloud_max: Sensitivity;
  policy_egress_locked: boolean;
}
interface StartPayload {
  run_id: string;
  channel_id: string;
  plan: Plan;
  provider: ProviderCard;
  attempts: string[];
}
interface DonePayload {
  run_id: string;
  latency_ms: number;
  finish: string;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  chars: number;
}
interface FailPayload {
  run_id: string;
  channel_id: string;
  message: string;
}
interface AuditRow {
  event_id: string;
  ts_ms: number;
  event_type: string;
  actor: string;
  run_id: string | null;
  channel: string | null;
  label: string | null;
  locality: string | null;
}
interface ChainStatus {
  ok: boolean;
  rows: number;
  detail: string;
}

interface Msg {
  key: string;
  role: "user" | "agent";
  text: string;
  runId?: string;
  status: "streaming" | "done" | "failed" | "refused";
}

const DOWNGRADE_ZH: Record<string, string> = {
  egress_locked: "主权演习进行中:全组织外联已切断,任务强制本地执行",
  restricted_data: "数据密级为 restricted:已强制本地执行,云端选项不可用",
  policy_ceiling: "组织策略:该密级不允许云端处理,已路由至本地",
};
const ORIGIN_ZH: Record<string, string> = {
  channel: "频道",
  repo: "仓库",
  manual: "手动",
  session_lock: "会话锁",
};

const NAV = [
  { key: "home", label: "首页", enabled: false, hint: "P1.x" },
  { key: "roster", label: "编制管理", enabled: false, hint: "D6" },
  { key: "channels", label: "频道消息", enabled: true, hint: "" },
  { key: "meeting", label: "会议室", enabled: false, hint: "v1.x" },
  { key: "skills", label: "能力库", enabled: false, hint: "P4" },
  { key: "audit", label: "审计中心", enabled: true, hint: "右栏" },
];

function fmtTime(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export default function App() {
  const [boot, setBoot] = useState<Bootstrap | null>(null);
  const [bootErr, setBootErr] = useState<string | null>(null);
  const [active, setActive] = useState<string>("general");
  const [msgs, setMsgs] = useState<Record<string, Msg[]>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [lastStart, setLastStart] = useState<StartPayload | null>(null);
  const [lastDone, setLastDone] = useState<DonePayload | null>(null);
  const [lastFail, setLastFail] = useState<FailPayload | null>(null);
  const [audit, setAudit] = useState<AuditRow[]>([]);
  const [chain, setChain] = useState<ChainStatus | null>(null);
  const [draft, setDraft] = useState("");
  const [drillOn, setDrillOn] = useState(false);
  const [drillId, setDrillId] = useState<string | null>(null);
  const [drillReport, setDrillReport] = useState<DrillReportOut | null>(null);

  // run_id → { channelId, msgKey };task-start 早于 invoke 返回,用挂起队列衔接。
  const runIndex = useRef<Record<string, { channelId: string; msgKey: string }>>({});
  const pending = useRef<Record<string, string[]>>({});
  const scroller = useRef<HTMLDivElement | null>(null);

  const refreshAudit = () => {
    invoke<AuditRow[]>("audit_tail", { limit: 14 }).then(setAudit).catch(() => {});
    invoke<ChainStatus>("verify_chain").then(setChain).catch(() => {});
  };

  useEffect(() => {
    invoke<Bootstrap>("bootstrap")
      .then((b) => {
        setBoot(b);
        setDrillOn(b.egress_locked);
        refreshAudit();
      })
      .catch((e) => setBootErr(String(e)));

    const attach = (runId: string, channelId: string): string | undefined => {
      const q = pending.current[channelId] ?? [];
      const msgKey = q.shift();
      pending.current[channelId] = q;
      if (msgKey) runIndex.current[runId] = { channelId, msgKey };
      return msgKey;
    };
    const patch = (runId: string, fn: (m: Msg) => Msg) => {
      const idx = runIndex.current[runId];
      if (!idx) return;
      setMsgs((prev) => ({
        ...prev,
        [idx.channelId]: (prev[idx.channelId] ?? []).map((m) => (m.key === idx.msgKey ? fn(m) : m)),
      }));
    };

    const unlisteners = [
      listen<StartPayload>("task-start", (e) => {
        attach(e.payload.run_id, e.payload.channel_id);
        patch(e.payload.run_id, (m) => ({ ...m, runId: e.payload.run_id }));
        setLastStart(e.payload);
        setLastDone(null);
        setLastFail(null);
      }),
      listen<{ run_id: string; text: string }>("task-delta", (e) => {
        patch(e.payload.run_id, (m) => ({ ...m, text: m.text + e.payload.text }));
      }),
      listen<DonePayload>("task-done", (e) => {
        patch(e.payload.run_id, (m) => ({ ...m, status: "done" }));
        const idx = runIndex.current[e.payload.run_id];
        if (idx) setBusy((b) => ({ ...b, [idx.channelId]: false }));
        setLastDone(e.payload);
        refreshAudit();
      }),
      listen<FailPayload>("task-refused", (e) => {
        const key = attach(e.payload.run_id, e.payload.channel_id);
        if (key)
          patch(e.payload.run_id, (m) => ({
            ...m,
            status: "refused",
            text: `⛔ 路由拒绝(fail-closed,绝不静默升云)\n${e.payload.message}`,
          }));
        setBusy((b) => ({ ...b, [e.payload.channel_id]: false }));
        setLastFail(e.payload);
        setLastStart(null);
        setLastDone(null);
      }),
      listen<FailPayload>("task-failed", (e) => {
        patch(e.payload.run_id, (m) => ({
          ...m,
          status: "failed",
          text: m.text + `\n\n⚠️ ${e.payload.message}`,
        }));
        setBusy((b) => ({ ...b, [e.payload.channel_id]: false }));
        setLastFail(e.payload);
        refreshAudit();
      }),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((un) => un()));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight });
  }, [msgs, active]);

  const toggleDrill = () => {
    invoke<DrillStatus>("toggle_drill", { on: !drillOn })
      .then((s) => {
        setDrillOn(s.on);
        setDrillId(s.drill_id);
        setDrillReport(s.on ? null : s.report);
        refreshAudit();
      })
      .catch(() => {});
  };

  const send = (asTask = false) => {
    const text = draft.trim();
    if (!text || !boot || busy[active]) return;
    setDraft("");
    const userKey = `u-${Date.now()}`;
    const agentKey = `a-${Date.now()}`;
    setMsgs((prev) => ({
      ...prev,
      [active]: [
        ...(prev[active] ?? []),
        { key: userKey, role: "user", text: asTask ? `▶ 任务:${text}` : text, status: "done" },
        { key: agentKey, role: "agent", text: "", status: "streaming" },
      ],
    }));
    pending.current[active] = [...(pending.current[active] ?? []), agentKey];
    setBusy((b) => ({ ...b, [active]: true }));
    invoke<string>(asTask ? "run_workspace_task" : "send_message", { channelId: active, text }).catch((e) => {
      setMsgs((prev) => ({
        ...prev,
        [active]: (prev[active] ?? []).map((m) =>
          m.key === agentKey ? { ...m, status: "failed", text: `⚠️ ${e}` } : m
        ),
      }));
      setBusy((b) => ({ ...b, [active]: false }));
    });
  };

  if (bootErr) {
    return (
      <div className="shell boot-err">
        <div>
          <h2>启动失败(fail-fast,按设计炸响)</h2>
          <pre>{bootErr}</pre>
          <p>常见原因:环境变量 KIMI_API_KEY 未设置。请在启动终端 export 后重开应用。</p>
        </div>
      </div>
    );
  }
  if (!boot) return <div className="shell boot-loading">初始化中…</div>;

  const channel = boot.channels.find((c) => c.id === active)!;
  const list = msgs[active] ?? [];

  return (
    <div className="shell">
      {/* ---------------- 侧栏 ---------------- */}
      <aside className="side">
        <div className="brand">
          <span className="brand-mark">M</span>
          <div>
            <b>Muster 点将台</b>
            <small>本地部署 · Agent 协作</small>
          </div>
        </div>
        <div className="nav-title">菜单</div>
        <nav>
          {NAV.map((n) => (
            <button key={n.key} className={"nav-item" + (n.key === "channels" ? " active" : "") + (n.enabled ? "" : " disabled")} title={n.enabled ? "" : `未启用(${n.hint})`}>
              {n.label}
              {!n.enabled && <em>{n.hint}</em>}
            </button>
          ))}
        </nav>
        <div className={"drill-card" + (drillOn ? " on" : "")}>
          <b>主权演习{drillOn && " · 进行中"}</b>
          {drillOn ? (
            <p className="live">
              全组织外联已切断,任务强制本地执行
              <br />
              {drillId ?? ""}
            </p>
          ) : (
            <p>季度合规窗口:切断外联,验证全组织本地执行能力</p>
          )}
          <button onClick={toggleDrill}>{drillOn ? "结束演习并出报告" : "启动演习"}</button>
          {drillReport && !drillOn && (
            <div className="drill-report">
              <div>
                <b>{drillReport.egress_bytes} B</b>
                <small>窗口外发</small>
              </div>
              <div>
                <b>{drillReport.model_calls}</b>
                <small>模型调用</small>
              </div>
              <div>
                <b>
                  {drillReport.local_calls}/{drillReport.cloud_calls}
                </b>
                <small>本地/云端</small>
              </div>
              <div>
                <b>{drillReport.ok ? "✓ 达标" : "✗ 不达标"}</b>
                <small>unmetered {drillReport.unmetered_calls}</small>
              </div>
            </div>
          )}
        </div>
        <div className="side-foot">
          <div>策略:cloud_max = {boot.policy_cloud_max}</div>
          <div title={boot.audit_db}>审计库:~/.muster/…</div>
        </div>
      </aside>

      {/* ---------------- 频道列 ---------------- */}
      <section className="channels">
        <header>频道消息</header>
        {boot.channels.map((c) => (
          <button key={c.id} className={"chan" + (c.id === active ? " active" : "")} onClick={() => setActive(c.id)}>
            <span className={`dot lv-${c.level}`} />
            <div className="chan-text">
              <b>{c.name}</b>
              <small>{c.desc}</small>
            </div>
            <span className={`lv-chip lv-${c.level}`}>{c.level}</span>
          </button>
        ))}
        <div className="providers">
          <div className="nav-title">模型编制</div>
          {boot.providers.map((p) => (
            <div key={p.id} className="prov">
              <span className={`loc-chip loc-${p.locality}`}>{p.locality === "cloud" ? "云端" : "本地"}</span>
              <div className="chan-text">
                <b>{p.display_name}</b>
                <small>{p.model}</small>
              </div>
            </div>
          ))}
        </div>
      </section>

      {/* ---------------- 会话区 ---------------- */}
      <main className="main">
        <header className="chat-head">
          <div>
            <b>{channel.name}</b>
            <span className={`lv-chip lv-${channel.level}`} title={channel.level_note}>
              {channel.level}
            </span>
            {drillOn && <span className="drill-chip">演习中 · 外联切断</span>}
          </div>
          <small>{channel.level_note}</small>
        </header>
        <div className="chat-body" ref={scroller}>
          {list.length === 0 && (
            <div className="empty">
              <b>在「{channel.name}」发起对话</b>
              <p>
                消息将经 E2 路由决策(当前频道密级 {channel.level})选择落点,全过程写入审计哈希链。
                {channel.level === "restricted" && " 本频道为 restricted:本地通道不可用时将被拒绝——这是产品行为。"}
              </p>
            </div>
          )}
          {list.map((m) => (
            <div key={m.key} className={`msg ${m.role} st-${m.status}`}>
              <div className="msg-meta">
                {m.role === "user" ? "你" : `Agent A-007${m.runId ? ` · ${m.runId}` : ""}`}
              </div>
              <div className="bubble">
                {m.text || (m.status === "streaming" ? "…" : "")}
                {m.status === "streaming" && m.text && <span className="caret" />}
              </div>
            </div>
          ))}
        </div>
        <footer className="composer">
          <textarea
            value={draft}
            placeholder={busy[active] ? "任务执行中…" : `发消息到 ${channel.name}(Enter 发送,Shift+Enter 换行)`}
            disabled={busy[active]}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send();
              }
            }}
          />
          <button
            className="task-btn"
            onClick={() => send(true)}
            disabled={busy[active] || !draft.trim()}
            title="B1 任务模式:在工作区 ~/muster 上运行只读工具循环(list_dir/read_file/grep)"
          >
            ▶ 任务
          </button>
          <button onClick={() => send()} disabled={busy[active] || !draft.trim()}>
            发送
          </button>
        </footer>
      </main>

      {/* ---------------- 右栏:任务/路由/审计 ---------------- */}
      <aside className="right">
        <div className="card">
          <div className="card-title">路由决策</div>
          {lastFail && !lastStart ? (
            <div className="refused-box">
              <b>⛔ {lastFail.run_id} 被拒绝</b>
              <p>{lastFail.message}</p>
            </div>
          ) : lastStart ? (
            <>
              <div className="route-line">
                <span className={`loc-chip loc-${lastStart.plan.primary_locality.toLowerCase()}`}>
                  {lastStart.plan.primary_locality.toLowerCase() === "cloud" ? "云端" : "本地"}
                </span>
                <b>{lastStart.provider.display_name}</b>
                <small>{lastStart.provider.model}</small>
              </div>
              <div className="kv">
                <span>有效密级</span>
                <span className={`lv-chip lv-${lastStart.plan.effective}`}>{lastStart.plan.effective}</span>
              </div>
              {lastStart.plan.deciders.length > 0 ? (
                <div className="deciders">
                  {lastStart.plan.deciders.map((d, i) => (
                    <div key={i} className="decider">
                      <span className="origin">{ORIGIN_ZH[d.origin] ?? d.origin}</span>
                      <span>{d.subject}</span>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="hint">无标签来源,默认 open</div>
              )}
              {lastStart.plan.downgraded && (
                <div className="downgrade">{DOWNGRADE_ZH[lastStart.plan.downgraded.reason] ?? lastStart.plan.downgraded.reason}</div>
              )}
              {lastStart.plan.fallbacks.length > 0 && (
                <div className="hint">降落带(仅本地):{lastStart.plan.fallbacks.join(" → ")}</div>
              )}
              {lastStart.attempts.length > 0 && <div className="hint warn">前序尝试失败:{lastStart.attempts.length} 次</div>}
            </>
          ) : (
            <div className="hint">发送一条消息后,这里显示"为什么落在这里"。</div>
          )}
        </div>

        <div className="card">
          <div className="card-title">用量与时延</div>
          {lastDone ? (
            <div className="usage">
              <div>
                <b>{lastDone.latency_ms} ms</b>
                <small>端到端</small>
              </div>
              <div>
                <b>{lastDone.prompt_tokens ?? "—"}</b>
                <small>输入 tokens</small>
              </div>
              <div>
                <b>{lastDone.completion_tokens ?? "—"}</b>
                <small>输出 tokens(含思考)</small>
              </div>
              <div>
                <b>{lastDone.finish}</b>
                <small>finish</small>
              </div>
            </div>
          ) : (
            <div className="hint">完成一次调用后显示(计量来自厂商回报,供 E4 对账)。</div>
          )}
        </div>

        <div className="card audit-card">
          <div className="card-title">
            审计中心
            <button className="mini" onClick={refreshAudit}>
              刷新
            </button>
          </div>
          {chain && (
            <div className={"chain " + (chain.ok ? "ok" : "bad")}>
              {chain.ok ? `✓ ${chain.detail}` : `✗ 哈希链校验失败:${chain.detail}`}
            </div>
          )}
          <div className="audit-list">
            {audit.length === 0 && <div className="hint">暂无事件。</div>}
            {audit.map((r) => (
              <div key={r.event_id} className="audit-row">
                <span className="etype">{r.event_type}</span>
                <span className="erun">{r.run_id ?? "—"}</span>
                {r.label && <span className={`lv-chip lv-${r.label}`}>{r.label}</span>}
                {r.locality && (
                  <span className={`loc-chip loc-${r.locality}`}>{r.locality === "cloud" ? "云" : "本"}</span>
                )}
                <span className="ets">{fmtTime(r.ts_ms)}</span>
              </div>
            ))}
          </div>
        </div>
      </aside>
    </div>
  );
}
