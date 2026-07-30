/* 控制台:中控台(真实数据版 HomeView)+ 审计中心(真实哈希链) */
import { useState } from "react";
import {
  AlertTriangle, BadgeCheck, Cloud, HardDrive, RefreshCw, Shield, ShieldCheck, Zap,
} from "lucide-react";
import { T } from "../theme";
import { ApRow, Card, ChipDd, Kpi, LegRow, Pct, Tag, TodoRow } from "../ui";
import { AuditRow, ChainStatus, HomeStats, PendingApprovalOut, fmtBytes, fmtDate, fmtTime } from "../api";

const WEEKDAY_ZH = ["日", "一", "二", "三", "四", "五", "六"];

export function ConsoleHome({
  home,
  pending,
  onGoApprovals,
}: {
  home: HomeStats | null;
  /** 真实待裁决申请(与频道右栏「待我审批」同一来源) */
  pending: PendingApprovalOut[];
  onGoApprovals: () => void;
}) {
  const [hover, setHover] = useState<number | null>(null);
  if (!home) {
    return (
      <div className="px-7 pt-4 text-xs" style={{ color: T.sub }}>
        统计加载中…
      </div>
    );
  }
  const runsDiff = home.runs_week - home.runs_prev_week;
  const egressDiff = home.egress_week_bytes - home.egress_prev_week_bytes;
  const maxV = Math.max(1, ...home.throughput.flatMap((d) => [d.local, d.cloud]));
  const today = home.throughput[home.throughput.length - 1] ?? { local: 0, cloud: 0, date: "", weekday: "0" };
  const todayTotal = today.local + today.cloud;
  const frLocal = todayTotal ? today.local / todayTotal : 0;
  const frCloud = todayTotal ? today.cloud / todayTotal : 0;
  // 第三环也要是今日的:downgrades 是 7 日窗口,直接拿它的条数当今日占比
  // 会把两个窗口混在一张图里。按当天零点过滤后再算。
  const dayStart = new Date(new Date().setHours(0, 0, 0, 0)).getTime();
  const downToday = home.downgrades.filter((d) => d.ts_ms >= dayStart).length;
  const frDown = todayTotal ? Math.min(1, downToday / todayTotal) : 0;

  const arc = (r: number, fr: number, col: string, rot = -90) => {
    const C = 2 * Math.PI * r;
    return (
      <g key={`${r}-${col}`}>
        <circle cx="70" cy="70" r={r} fill="none" stroke="#F1F2F7" strokeWidth="9" />
        <circle
          cx="70" cy="70" r={r} fill="none" stroke={col} strokeWidth="9" strokeLinecap="round"
          strokeDasharray={`${(C * Math.max(0.001, fr)).toFixed(1)} ${C.toFixed(1)}`}
          transform={`rotate(${rot} 70 70)`}
        />
      </g>
    );
  };

  // 待办:全部由真实状态推导,推不出就是空。
  const todos: { tone: string; t: string; n: string }[] = [];
  if (home.pending_approvals > 0)
    todos.push({ tone: T.red, t: `待审批 ${home.pending_approvals} 项`, n: "Agent 越权与发布申请" });
  if (home.unmetered_week > 0)
    todos.push({ tone: T.red, t: `本周 ${home.unmetered_week} 次外发未计量`, n: "按违规计,需排查 A2 计量链路" });
  if (!home.drill_last)
    todos.push({ tone: T.amber, t: "季度主权演习尚未执行", n: "侧栏「启动演习」可随时开窗验证" });
  const lastFailed = home.recent_runs.find((r) => r.outcome !== "success");
  if (lastFailed)
    todos.push({ tone: T.indigo, t: `复盘 ${lastFailed.run_id}(${lastFailed.outcome})`, n: `${fmtDate(lastFailed.ts_ms)} · 审计链可溯源` });

  return (
    <div className="px-7 pb-8 pt-2" style={{ display: "grid", gridTemplateColumns: "1.55fr 1fr", gap: 16 }}>
      <div className="flex flex-col gap-4">
        <div className="grid grid-cols-2 gap-4">
          <Kpi hero icon={<Zap size={16} />}
            pct={<Pct hero>{runsDiff >= 0 ? `+${runsDiff}` : `${runsDiff}`}</Pct>}
            label="本周任务(Runs)" val={home.runs_week} cap="较上周 · 全组织" />
          <Kpi icon={<Shield size={16} />}
            pct={home.pending_approvals === 0 ? <Pct up>已清零</Pct> : <Pct>+{home.pending_approvals}</Pct>}
            label="待我审批" val={home.pending_approvals} cap="有产出待裁决即在此计数" />
          <Kpi icon={<Cloud size={16} />}
            pct={<Pct up={egressDiff <= 0}>{egressDiff === 0 ? "持平" : `${egressDiff > 0 ? "+" : "−"}${fmtBytes(Math.abs(egressDiff))}`}</Pct>}
            label="云端外发流量 · 7日" val={fmtBytes(home.egress_week_bytes)} cap="较上周 · 越少越好" />
          <Kpi icon={<BadgeCheck size={16} />}
            pct={home.drill_last ? <Pct up={home.drill_last.ok}>{home.drill_last.ok ? "100%" : "未达标"}</Pct> : undefined}
            label="最近演习" val={home.drill_last ? (home.drill_last.ok ? "达标" : "未达标") : "—"}
            cap={home.drill_last ? `${fmtDate(home.drill_last.ts_ms)} · 外发 ${fmtBytes(home.drill_last.egress_bytes)}` : "尚未演习 · 侧栏可启动"} />
        </div>

        <Card className="p-5">
          <div className="flex items-center">
            <div>
              <b className="text-[15px]">任务吞吐</b>
              <div className="text-[11.5px] mt-0.5" style={{ color: T.sub }}>近 7 日 · 本地 vs 云端执行(model.call)</div>
            </div>
            <div className="ml-auto flex items-center gap-3">
              <span className="flex gap-3 text-[11px]" style={{ color: T.sub }}>
                <span><i className="inline-block w-2 h-2 rounded-full mr-1.5" style={{ background: T.indigo }} />云端</span>
                <span><i className="inline-block w-2 h-2 rounded-full mr-1.5" style={{ background: T.teal }} />本地</span>
              </span>
              <ChipDd>本周</ChipDd>
            </div>
          </div>
          <div className="flex items-end mt-4" style={{ height: 150 }}>
            {home.throughput.map((d, i) => {
              const isLast = i === home.throughput.length - 1;
              return (
                <div key={d.date} className="flex-1" onMouseEnter={() => setHover(i)} onMouseLeave={() => setHover(null)}>
                  <div className="relative flex items-end justify-center gap-1" style={{ height: 130 }}>
                    {hover === i && (
                      <div className="absolute text-[11px] text-white rounded-xl px-3 py-2 whitespace-nowrap z-10"
                        style={{ bottom: "calc(100% - 42px)", left: "50%", transform: "translateX(-50%)", background: T.black, boxShadow: "0 8px 20px rgba(23,24,28,.25)" }}>
                        <div><span className="inline-block w-1.5 h-1.5 rounded-full mr-1.5" style={{ background: "#9DA1B5" }} />{d.cloud} 次 · 云端</div>
                        <div className="mt-0.5"><span className="inline-block w-1.5 h-1.5 rounded-full mr-1.5" style={{ background: T.teal }} />{d.local} 次 · 本地</div>
                        <div className="absolute left-1/2 top-full" style={{ transform: "translateX(-50%)", border: "6px solid transparent", borderTopColor: T.black }} />
                      </div>
                    )}
                    <div className="relative rounded-t-lg" style={{ width: 13, height: Math.max(d.cloud > 0 ? 6 : 0, (d.cloud / maxV) * 118), background: T.indigo, borderRadius: "7px 7px 4px 4px" }}>
                      {d.cloud > 0 && <span className="absolute -top-4 left-1/2 -translate-x-1/2 text-[9.5px] font-semibold" style={{ color: T.sub }}>{d.cloud}</span>}
                    </div>
                    <div className="relative rounded-t-lg" style={{ width: 13, height: Math.max(d.local > 0 ? 6 : 0, (d.local / maxV) * 118), background: T.teal, borderRadius: "7px 7px 4px 4px" }}>
                      {d.local > 0 && <span className="absolute -top-4 left-1/2 -translate-x-1/2 text-[9.5px] font-semibold" style={{ color: T.sub }}>{d.local}</span>}
                    </div>
                  </div>
                  <div className="text-center text-[10.5px] mt-2" style={{ color: isLast ? T.indigoDeep : T.faint, fontWeight: isLast ? 700 : 400 }}>
                    {isLast ? "今天" : `周${WEEKDAY_ZH[+d.weekday]}`}
                  </div>
                </div>
              );
            })}
          </div>
        </Card>

        <Card className="px-5 pt-4 pb-2">
          <div className="flex items-center">
            <b className="text-[15px]">待办事项</b>
            {todos.length > 0 && <span className="text-[11px] font-semibold ml-2" style={{ color: T.red }}>{todos.length} 项</span>}
            <span className="ml-auto text-[10.5px]" style={{ color: T.faint }}>由审计状态实时推导</span>
          </div>
          {todos.length === 0 ? (
            <div className="py-4 text-xs" style={{ color: T.sub }}>当前没有系统级待办。</div>
          ) : (
            todos.map((x) => <TodoRow key={x.t} tone={x.tone} t={x.t} n={x.n} />)
          )}
        </Card>
      </div>

      <div className="flex flex-col gap-4">
        <Card className="p-5">
          <div className="flex items-center">
            <div>
              <b className="text-[15px]">路由统计</b>
              <div className="text-[11.5px] mt-0.5" style={{ color: T.sub }}>模型调用去向</div>
            </div>
            <span className="ml-auto"><ChipDd>今日</ChipDd></span>
          </div>
          <div className="flex items-center gap-4 mt-3">
            <svg width="140" height="140" viewBox="0 0 140 140">
              {arc(58, frCloud, T.indigo)}
              {arc(44, frLocal, T.teal, 30)}
              {arc(30, frDown, T.amber, -40)}
            </svg>
            <div>
              <div className="text-2xl font-extrabold">{todayTotal}</div>
              <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>今日调用次数</div>
            </div>
          </div>
          <div className="mt-2">
            <LegRow icon={<Cloud size={14} />} l="云端" v={today.cloud} />
            <LegRow icon={<HardDrive size={14} />} l="本地" v={today.local} />
            <LegRow icon={<AlertTriangle size={14} />} l="降级落地 · 7日" v={home.downgrades.length} />
          </div>
        </Card>

        <Card className="px-5 pt-4 pb-2">
          <div className="flex items-center">
            <b className="text-[15px]">审批监控</b>
            <span className="ml-auto"><ChipDd>实时</ChipDd></span>
          </div>
          {pending.length === 0 && home.downgrades.length === 0 ? (
            <div className="py-5 text-center text-[11.5px]" style={{ color: T.sub, borderTop: `1px solid ${T.line}` }}>
              当前没有待裁决的申请。
              <br />
              Agent 产出代码变更后会在这里出现。
            </div>
          ) : (
            <>
              {pending.slice(0, 3).map((p) => (
                <ApRow
                  key={p.approval_id}
                  bg={T.indigo}
                  nm={p.actor_id}
                  sb={`${p.run_id ?? p.approval_id} · ${p.requested_capability}`}
                  right={
                    <button
                      onClick={onGoApprovals}
                      className="text-[11px] font-semibold px-3 py-1.5 rounded-full"
                      style={{ background: T.indigo, color: "#fff" }}
                    >
                      去裁决
                    </button>
                  }
                />
              ))}
              {home.downgrades[0] && (
                <ApRow bg="#9DA1B5" icon={<Shield size={15} />} nm="路由中心"
                  sb={home.downgrades[0].text} right={<Tag tone="ind">通知</Tag>} />
              )}
            </>
          )}
          {pending.length > 3 && (
            <div className="pb-2 text-[10.5px]" style={{ color: T.faint }}>
              另有 {pending.length - 3} 项未列出——「去裁决」可看全部
            </div>
          )}
        </Card>
      </div>
    </div>
  );
}

/* ---------------- 审计中心(真实) ---------------- */

export function AuditCenter({
  rows,
  chain,
  onRefresh,
}: {
  rows: AuditRow[];
  chain: ChainStatus | null;
  onRefresh: () => void;
}) {
  return (
    <div className="px-7 pb-8 pt-2 flex flex-col gap-4">
      <div className="flex items-center gap-2.5">
        {chain && (
          <span className="inline-flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
            style={{ background: chain.ok ? T.greenSoft : T.redSoft, color: chain.ok ? T.green : T.red }}>
            <ShieldCheck size={14} />
            {chain.ok ? `SHA-256 哈希链完整 · ${chain.rows} 行` : `哈希链校验失败:${chain.detail}`}
          </span>
        )}
        <span className="text-[11px]" style={{ color: T.sub }}>append-only · 只存哈希不存正文 · 每个数字可 SQL 复查</span>
        <button onClick={onRefresh} className="ml-auto inline-flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl"
          style={{ background: T.indigoSoft, color: T.indigo }}>
          <RefreshCw size={13} /> 刷新
        </button>
      </div>
      <Card className="overflow-hidden">
        <div className="grid text-[10.5px] font-semibold px-4 py-2.5" style={{ gridTemplateColumns: "150px 1fr 90px 90px 80px 70px", color: T.faint, borderBottom: `1px solid ${T.line}`, letterSpacing: ".04em" }}>
          <span>事件类型</span><span>RUN</span><span>密级</span><span>落点</span><span>工牌</span><span>时间</span>
        </div>
        <div className="max-h-[560px] overflow-y-auto">
          {rows.length === 0 && <div className="px-4 py-6 text-xs" style={{ color: T.sub }}>暂无事件。</div>}
          {rows.map((r) => (
            <div key={r.event_id} className="grid items-center px-4 py-2 text-[11.5px]"
              style={{ gridTemplateColumns: "150px 1fr 90px 90px 80px 70px", borderBottom: `1px solid ${T.line}` }}>
              <span className="font-bold" style={{ color: T.indigoDeep }}>{r.event_type}</span>
              <span className="truncate" style={{ color: T.sub }}>{r.run_id ?? "—"}{r.channel ? ` · #${r.channel}` : ""}</span>
              <span>{r.label ? <Tag tone={r.label === "restricted" ? "red" : r.label === "internal" ? "amb" : undefined}>{r.label}</Tag> : "—"}</span>
              <span>{r.locality ? <Tag tone={r.locality === "cloud" ? "ind" : "teal"}>{r.locality === "cloud" ? "云端" : "本地"}</Tag> : "—"}</span>
              <span style={{ color: T.sub }}>{r.actor.includes("A-007") ? "A-007" : r.actor.includes("Human") || r.actor.includes("human") ? "人类" : "系统"}</span>
              <span style={{ color: T.faint }}>{fmtTime(r.ts_ms)}</span>
            </div>
          ))}
        </div>
      </Card>
    </div>
  );
}
