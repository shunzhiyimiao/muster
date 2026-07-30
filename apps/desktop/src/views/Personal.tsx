/* 个人模块:我的工作台 / Agent 档案(真实统计 + 真实热力图) */
import {
  BadgeCheck, Bot, Cast, ChevronRight, Eye, Hash, MessageSquare, Pencil, Shield, Sparkles,
  StopCircle, Brain,
} from "lucide-react";
import { T, LV } from "../theme";
import { Card, ChipDd, IBtn, RouteTag, Tag } from "../ui";
import { AgentStats, Channel, HomeStats, fmtBytes, fmtDate } from "../api";
import { Msg } from "../chat";
import { MEMO } from "../data";

export function PersonalHome({
  personalMsgs,
  agent,
  home,
  streamed,
  allMsgs,
  channels,
  onStream,
  onStop,
  goAgent,
  goChat,
  goChannel,
  openConvo,
  onOpenChannel,
}: {
  personalMsgs: Msg[];
  agent: AgentStats | null;
  home: HomeStats | null;
  streamed: boolean;
  allMsgs: Record<string, Msg[]>;
  channels: Channel[];
  onStream: () => void;
  onStop: () => void;
  goAgent: () => void;
  goChat: () => void;
  goChannel: () => void;
  openConvo: () => void;
  onOpenChannel: (c: Channel) => void;
}) {
  const lastUser = [...personalMsgs].reverse().find((m) => m.role === "user");
  const lastAgent = [...personalMsgs].reverse().find((m) => m.role === "agent" && m.text);
  const hasConvo = personalMsgs.length > 0;

  return (
    <div className="px-7 pb-8 pt-2" style={{ display: "grid", gridTemplateColumns: "1.6fr 1fr", gap: 16 }}>
      <div className="flex flex-col gap-4">
        {/* 进行中会话(真实私有会话) */}
        <Card className="p-5" style={streamed ? { borderColor: T.indigo, boxShadow: "0 8px 22px rgba(91,91,245,.16)" } : {}}>
          <div className="flex items-center gap-2">
            <Tag tone="teal">{hasConvo ? "进行中" : "私有空间"}</Tag>
            <span className="text-[11px]" style={{ color: T.sub }}>私有会话 · 未进入任何频道</span>
            {streamed && (
              <span className="ml-auto flex items-center gap-1.5 text-[11px] font-semibold" style={{ color: T.red }}>
                <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />LIVE
              </span>
            )}
          </div>
          <div className="text-[17px] font-bold mt-2">{hasConvo ? "与小七的私有会话" : "还没有进行中的会话"}</div>
          <div className="flex items-center gap-2 mt-1.5 text-[11px] flex-wrap" style={{ color: T.faint }}>
            与 <b style={{ color: T.indigo }}>小七</b>
            {hasConvo && <> · {personalMsgs.length} 条</>} · <RouteTag /> kimi-k3 · <Tag>open</Tag>
          </div>

          {hasConvo ? (
            <div className="mt-3.5 rounded-xl p-3.5 space-y-2.5" style={{ background: T.panel }}>
              {lastUser && <PSnip who="Alice" text={lastUser.text} />}
              {lastAgent && <PSnip who="小七" bot text={lastAgent.text} />}
            </div>
          ) : (
            <div className="mt-3.5 rounded-xl p-3.5 text-xs leading-relaxed" style={{ background: T.panel, color: T.sub }}>
              在这里与小七讨论、推演、跑任务——内容默认不进团队,不出现在任何频道与检索里。
            </div>
          )}

          <div className="flex items-center gap-2 mt-3.5">
            {streamed ? (
              <>
                <button onClick={onStop} className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.redSoft, color: T.red }}>
                  <StopCircle size={13} /> 停止串流
                </button>
                <button onClick={goChannel} className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.soft }}>
                  <Hash size={13} /> 去 #platform 看围观
                </button>
                <span className="ml-auto flex items-center gap-1 text-[11px]" style={{ color: T.sub }}>
                  <Eye size={12} /> 12 人围观中
                </span>
              </>
            ) : (
              <>
                <IBtn onClick={goChat}>
                  <MessageSquare size={13} /> {hasConvo ? "继续对话" : "开始对话"}
                </IBtn>
                <button onClick={onStream} className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.soft }}>
                  <Cast size={13} /> 串流到团队
                </button>
                <button onClick={openConvo} className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.soft }}>
                  串流=实时投屏 · 快照=定格存档
                </button>
              </>
            )}
          </div>
        </Card>

        {/* 近期对话(C1 持久化,真实数据) */}
        <Card className="px-5 pt-4 pb-2">
          <div className="flex items-center">
            <b className="text-[15px]">近期对话</b>
            <span className="ml-auto text-[10.5px]" style={{ color: T.faint }}>C1 · 重启不丢</span>
          </div>
          {(() => {
            const rows = channels
              .map((c) => {
                const list = allMsgs[c.id] ?? [];
                const last = list[list.length - 1];
                return last ? { c, count: list.length, last } : null;
              })
              .filter((x): x is { c: Channel; count: number; last: Msg } => x !== null)
              .sort((a, b) => (b.last.ts ?? 0) - (a.last.ts ?? 0))
              .slice(0, 5);
            if (rows.length === 0)
              return <div className="py-4 text-xs" style={{ color: T.sub }}>还没有对话记录。</div>;
            return rows.map(({ c, count, last }) => (
              <button key={c.id} onClick={() => onOpenChannel(c)} className="w-full flex items-center gap-3 py-2.5 text-left"
                style={{ borderTop: `1px solid ${T.line}` }}>
                <div className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0" style={{ background: T.soft, color: "#5A5E70" }}>
                  <MessageSquare size={14} />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="text-[13px] font-semibold">{c.personal ? "与小七的私有会话" : `#${c.name}`}</div>
                  <div className="text-[11px] mt-0.5 truncate" style={{ color: T.sub }}>
                    {count} 条 · {last.text.replace(/\n/g, " ").slice(0, 42)}
                  </div>
                </div>
                <span className="ml-auto flex items-center gap-1.5 shrink-0">
                  {last.ts && <span className="text-[10px]" style={{ color: T.faint }}>{fmtDate(last.ts)}</span>}
                  <Tag tone={c.level === "open" ? undefined : c.level === "restricted" ? "red" : "amb"}>{c.level}</Tag>
                </span>
              </button>
            ));
          })()}
        </Card>
      </div>

      <div className="flex flex-col gap-4">
        {/* 我的 Agent(真实统计) */}
        <Card className="p-5">
          <div className="flex items-center gap-3">
            <div className="w-12 h-12 rounded-2xl flex items-center justify-center" style={{ background: T.indigoSoft, color: T.indigo }}>
              <Bot size={24} />
            </div>
            <div>
              <div className="text-[15px] font-bold">小七</div>
              <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>编制 A-007 · 代码评审员</div>
            </div>
            <span className="ml-auto"><RouteTag /></span>
          </div>
          <div className="grid grid-cols-3 gap-2 mt-4">
            {[
              [agent ? String(agent.hired_days) : "—", "入职天数"],
              [agent ? String(agent.total_runs) : "—", "累计 Runs"],
              [agent ? fmtBytes(agent.total_egress_bytes) : "—", "累计外发"],
            ].map(([v, l]) => (
              <div key={l} className="rounded-xl py-2.5 text-center" style={{ background: T.soft }}>
                <div className="text-[15px] font-extrabold">{v}</div>
                <div className="text-[10px] mt-0.5" style={{ color: T.sub }}>{l}</div>
              </div>
            ))}
          </div>
          <button onClick={goAgent} className="w-full mt-3.5 py-2 rounded-xl text-xs font-semibold flex items-center justify-center gap-1.5"
            style={{ background: T.indigoSoft, color: T.indigo }}>
            查看完整档案 <ChevronRight size={13} />
          </button>
        </Card>

        {/* 我的任务(真实 run.finish) */}
        <Card className="px-5 pt-4 pb-2">
          <div className="flex items-center">
            <b className="text-[15px]">我的任务</b>
            <span className="ml-auto text-[11px]" style={{ color: T.sub }}>最近 run.finish</span>
          </div>
          {!home || home.recent_runs.length === 0 ? (
            <div className="py-4 text-xs" style={{ color: T.sub }}>
              还没有任务运行——在频道或私有会话里用「▶ 任务」发起。
            </div>
          ) : (
            home.recent_runs.slice(0, 4).map((r) => (
              <div key={r.run_id + r.ts_ms} className="flex items-center gap-3 py-2.5" style={{ borderTop: `1px solid ${T.line}` }}>
                <span className="w-1 rounded-full" style={{ height: 30, background: r.outcome === "success" ? T.green : T.red }} />
                <div>
                  <div className="text-[13px] font-semibold">{r.run_id}</div>
                  <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>
                    {r.outcome} · {(r.duration_ms / 1000).toFixed(1)}s · {fmtDate(r.ts_ms)}
                  </div>
                </div>
              </div>
            ))
          )}
        </Card>

        {/* 个人 ↔ 团队 边界说明 */}
        <Card className="p-4" style={{ background: T.panel }}>
          <div className="flex items-center gap-1.5 text-[11px] font-semibold">
            <Shield size={13} style={{ color: T.indigo }} /> 个人空间边界
          </div>
          <div className="text-[11px] mt-1.5 leading-relaxed" style={{ color: T.sub }}>
            个人会话默认<b style={{ color: T.ink }}>不进团队</b>,不出现在任何频道与检索里。串流或分享是唯一的出口,且受密级约束、全程留痕。
          </div>
        </Card>
      </div>
    </div>
  );
}

function PSnip({ who, bot, text }: { who: string; bot?: boolean; text: string }) {
  const short = text.length > 110 ? text.slice(0, 110) + "…" : text;
  return (
    <div className="flex gap-2.5">
      <div className="w-7 h-7 rounded-lg flex items-center justify-center shrink-0 text-xs font-bold"
        style={{ background: bot ? T.indigoSoft : "#E4E6EF", color: bot ? T.indigo : "#5A5E70" }}>
        {bot ? <Bot size={14} /> : who[0]}
      </div>
      <div className="min-w-0">
        <div className="text-[11px] font-semibold" style={{ color: bot ? T.indigo : T.ink }}>{who}</div>
        <div className="mt-0.5 text-[12px] leading-relaxed rounded-xl px-3 py-2 whitespace-pre-wrap break-words" style={{ background: T.soft, color: "#454A5C" }}>
          {short}
        </div>
      </div>
    </div>
  );
}

/* ==================== Agent 档案页 ==================== */

export function AgentProfile({
  agent,
  streamed,
  onStream,
  goChat,
}: {
  agent: AgentStats | null;
  streamed: boolean;
  onStream: () => void;
  goChat: () => void;
}) {
  // 真实热力图:heat 按日期升序(336 天);行 = 周一..周日,列 = 周。
  const heat = agent?.heat ?? [];
  const offset = heat.length ? (parseInt(heat[0].weekday, 10) + 6) % 7 : 0;
  const weeks = Math.ceil((heat.length + offset) / 7) || 48;
  const grid: number[][] = Array.from({ length: 7 }, () => Array(weeks).fill(-1));
  const monthMarks: { col: number; label: string }[] = [];
  let lastMonth = "";
  heat.forEach((d, i) => {
    const row = (parseInt(d.weekday, 10) + 6) % 7;
    const col = Math.floor((i + offset) / 7);
    grid[row][col] = d.local + d.cloud;
    const month = d.date.slice(5, 7);
    if (month !== lastMonth) {
      lastMonth = month;
      if (!monthMarks.length || col > monthMarks[monthMarks.length - 1].col + 1)
        monthMarks.push({ col, label: `${parseInt(month, 10)}月` });
    }
  });
  const lv = (n: number) => (n <= 0 ? 0 : n === 1 ? 1 : n <= 3 ? 2 : n <= 6 ? 3 : 4);
  const firstSeen = agent?.first_seen_ms ? fmtDate(agent.first_seen_ms).split(" ")[0] : null;

  return (
    <div className="px-7 pb-8 pt-2 flex flex-col gap-4">
      {/* 档案头 */}
      <Card className="p-6 flex items-start gap-6">
        <div className="relative shrink-0" style={{ transform: "rotate(-3deg)" }}>
          <div className="w-32 h-36 rounded-2xl flex items-center justify-center"
            style={{ background: T.indigoSoft, border: `1px solid ${T.line}`, boxShadow: "0 10px 24px rgba(23,24,28,.12)" }}>
            <Bot size={56} style={{ color: T.indigo }} />
          </div>
          <div className="absolute left-2 bottom-2 text-[10px] font-semibold px-1.5 py-0.5 rounded" style={{ background: "#fff", color: T.sub }}>
            ID: a7f0-007
          </div>
          <div className="absolute -right-3 -bottom-3 w-9 h-9 rounded-full flex items-center justify-center"
            style={{ background: "#fff", border: `1px solid ${T.line}`, color: T.indigo }}>
            <BadgeCheck size={18} />
          </div>
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2.5">
            <span className="text-2xl font-extrabold">小七</span>
            <Tag tone="ind">编制 A-007 · 代码评审员</Tag>
            <button disabled title="Agent 档案编辑尚未实现" className="flex items-center gap-1 text-[11px] px-2 py-1 rounded-lg" style={{ background: T.soft, color: T.sub , opacity: 0.45, cursor: "not-allowed"}}>
              <Pencil size={11} /> 编辑
            </button>
          </div>
          <div className="flex items-center gap-3 mt-2 text-[11.5px]" style={{ color: T.sub }}>
            <span className="flex items-center gap-1.5"><span className="w-1.5 h-1.5 rounded-full" style={{ background: T.green }} />在线</span>
            <span>入职时间:{firstSeen ?? "以首条审计事件为准(暂无)"}</span>
            <RouteTag />
          </div>
          <div className="text-[12.5px] mt-2.5 leading-relaxed" style={{ color: "#454A5C" }}>
            负责代码评审与变更验证:静态审查、跑测试、产出修复 diff 与评审意见。当前具备只读工具(list_dir / read_file / grep),工作区圈禁,越权操作一律走审批(P5)。
          </div>
          <div className="flex items-center gap-2 mt-3">
            {streamed ? (
              <span className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.redSoft, color: T.red }}>
                <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />会话串流中 · #platform
              </span>
            ) : (
              <IBtn onClick={onStream}><Cast size={13} /> 串流当前会话</IBtn>
            )}
            <button onClick={goChat} className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.soft }}>
              <MessageSquare size={13} /> 继续对话
            </button>
          </div>
        </div>
      </Card>

      {/* 工作记录(真实) */}
      <Card className="p-5">
        <div className="flex items-center gap-2">
          <b className="text-[15px]">工作记录</b>
          <span className="text-[11px] px-2 py-1 rounded-lg font-medium" style={{ background: T.indigoSoft, color: T.indigo }}>审计链实时</span>
          <span className="ml-auto"><ChipDd>近 48 周</ChipDd></span>
        </div>
        <div className="grid grid-cols-4 gap-3 mt-4">
          {[
            [agent ? String(agent.hired_days) : "—", "入职天数"],
            [agent ? String(agent.total_runs) : "—", "累计 Runs"],
            ["3", "只读工具"],
            [agent ? fmtBytes(agent.total_egress_bytes) : "—", "累计外发"],
          ].map(([v, l]) => (
            <div key={l} className="rounded-xl p-3.5" style={{ background: T.panel }}>
              <div className="text-[22px] font-extrabold">{v}</div>
              <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>{l}</div>
            </div>
          ))}
        </div>

        {/* 贡献热力图(真实:model.call 按日) */}
        <div className="mt-5 overflow-x-auto">
          <div className="relative ml-8 mb-1.5" style={{ height: 14 }}>
            {monthMarks.map((m) => (
              <span key={m.col} className="absolute text-[10px]" style={{ color: T.faint, left: m.col * 12 }}>{m.label}</span>
            ))}
          </div>
          <div className="flex gap-2">
            <div className="flex flex-col justify-around text-[10px] pt-0.5" style={{ color: T.faint, width: 24 }}>
              <span>周一</span><span>周三</span><span>周五</span>
            </div>
            <div className="flex flex-col gap-[3px]">
              {grid.map((row, r) => (
                <div key={r} className="flex gap-[3px]">
                  {row.map((n, c) => (
                    <span key={c} className="rounded-[2px]" title={n >= 0 ? `${n} 次调用` : ""}
                      style={{ width: 9, height: 9, background: n < 0 ? "transparent" : LV[lv(n)] }} />
                  ))}
                </div>
              ))}
            </div>
          </div>
          <div className="flex items-center gap-1.5 mt-3 text-[10px]" style={{ color: T.faint }}>
            少 {LV.map((c, i) => (
              <span key={i} className="rounded-[2px]" style={{ width: 9, height: 9, background: c, display: "inline-block" }} />
            ))} 多
            <span className="ml-3">数据 = 审计链 model.call 逐日计数(day_throughput SQL)</span>
          </div>
        </div>
      </Card>

      {/* 记忆与积累(概念示意) */}
      <Card className="p-5">
        <div className="flex items-center gap-2">
          <Brain size={15} style={{ color: T.indigo }} />
          <b className="text-[15px]">记忆与积累</b>
          <Tag>概念示意 · 记忆系统 v1.x</Tag>
          <span className="ml-auto text-[11px]" style={{ color: T.sub }}>全部本地存储 · 可导出、可清除</span>
        </div>
        <div className="relative mt-6 mb-2" style={{ height: 168 }}>
          <div className="absolute left-0 right-0" style={{ top: 84, height: 1, background: T.line }} />
          <div className="flex justify-between relative">
            {MEMO.map((m, i) => {
              const up = i % 2 === 1;
              return (
                <div key={m.t} className="flex-1 flex flex-col items-center">
                  {up ? (
                    <div className="text-center mb-2" style={{ height: 72 }}>
                      <div className="text-[10.5px]" style={{ color: T.faint }}>{m.d}</div>
                      <div className="text-[12.5px] font-semibold mt-0.5">{m.t}</div>
                      <div className="text-[10.5px] mt-0.5" style={{ color: T.sub }}>{m.s}</div>
                    </div>
                  ) : (
                    <div style={{ height: 72 }} />
                  )}
                  <div className="w-9 h-9 rounded-full flex items-center justify-center shrink-0"
                    style={{ background: T.indigoSoft, border: "2px solid #fff", color: T.indigo }}>
                    <Sparkles size={15} />
                  </div>
                  {!up && (
                    <div className="text-center mt-2">
                      <div className="text-[10.5px]" style={{ color: T.faint }}>{m.d}</div>
                      <div className="text-[12.5px] font-semibold mt-0.5">{m.t}</div>
                      <div className="text-[10.5px] mt-0.5" style={{ color: T.sub }}>{m.s}</div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      </Card>
    </div>
  );
}
