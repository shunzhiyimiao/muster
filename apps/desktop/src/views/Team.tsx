/* 团队模块:频道(真实聊天+概念叙事卡)/ 编制 / 会议室 / 能力库 */
import { useEffect, useState } from "react";
import {
  BadgeCheck, Bot, Cast, Check, Eye, GitBranch, Lock, Mic, MicOff, Monitor,
  Phone, Play, Plus, Radio, Search, Share2, Sparkles, Video,
} from "lucide-react";
import { T } from "../theme";
import { Card, IBtn, LvTag, RouteTag, Tag } from "../ui";
import { AuditRow, CapsuleOut, Channel, DOWNGRADE_ZH, ForgeableRun, ORIGIN_ZH, PendingApprovalOut, RosterEntryOut, fmtDate, fmtTime } from "../api";
import { ChatPane, ChatState } from "../chat";
import { CAPS, CAPTIONS, ROSTER, RosterEntry, TICON } from "../data";
import { DiffPanel } from "./Diff";
import { ApprovalsPanel } from "./Approvals";

/* ==================== 频道视图 ==================== */

export function ChannelView({
  channel,
  chat,
  auditRows,
  streamed,
  introduced,
  setIntroduced,
  openConvo,
  goMeeting,
  approvals,
  onApprovalsChanged,
}: {
  channel: Channel;
  chat: ChatState;
  auditRows: AuditRow[];
  streamed: boolean;
  introduced: boolean;
  setIntroduced: (v: boolean) => void;
  openConvo: () => void;
  goMeeting: () => void;
  approvals: PendingApprovalOut[];
  onApprovalsChanged: () => void;
}) {
  const isPlatform = channel.id === "platform";
  const start = chat.lastStart?.channel_id === channel.id ? chat.lastStart : null;
  const fail = chat.lastFail?.channel_id === channel.id ? chat.lastFail : null;

  const conceptCards = isPlatform ? (
    <>
      {streamed && (
        <div className="ml-10 max-w-xl rounded-2xl overflow-hidden fade" style={{ background: T.indigoSoft, border: `1px solid ${T.indigo}` }}>
          <div className="p-4">
            <div className="flex items-center gap-1.5 text-[11px] font-semibold" style={{ color: T.indigo }}>
              <Cast size={12} /> 个人会话串流
              <span className="flex items-center gap-1 ml-1 px-1.5 py-0.5 rounded-md text-[10px]" style={{ background: T.red, color: "#fff" }}>
                <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: "#fff" }} />LIVE
              </span>
            </div>
            <div className="text-sm font-bold mt-1.5">Alice 正在串流 · 与小七的私有会话</div>
            <div className="flex items-center gap-2 mt-1.5 text-[10.5px] flex-wrap" style={{ color: T.sub }}>
              与小七(A-007) · <RouteTag /> · <Tag tone="amb">internal</Tag> · 串流通道 v1.x(演示)
            </div>
            <div className="mt-2.5 rounded-xl px-3 py-2 text-[11.5px] leading-relaxed" style={{ background: "#fff", color: "#454A5C" }}>
              <b style={{ color: T.indigo }}>小七:</b> 兼容层 diff 已备好,灰度期双写…
              <span className="ml-1.5 inline-flex gap-0.5 align-middle">
                {[0, 1, 2].map((i) => (
                  <span key={i} className="wv rounded-full" style={{ width: 3, height: 9, background: T.indigo, animationDelay: `${i * 0.14}s` }} />
                ))}
              </span>
            </div>
          </div>
          <div className="flex items-center gap-2 px-4 py-2.5" style={{ borderTop: "1px solid rgba(91,91,245,.25)" }}>
            <button className="flex items-center gap-1.5 text-xs font-semibold px-3 py-1.5 rounded-lg" style={{ background: T.indigo, color: "#fff" }}>
              <Eye size={12} /> 加入围观
            </button>
            <span className="text-[10.5px]" style={{ color: T.sub }}>12 人围观中 · 只读投屏,接手需 Alice 授权</span>
          </div>
        </div>
      )}

      <div className="ml-10 max-w-xl rounded-2xl p-4" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
        <div className="flex items-center gap-1.5 text-[11px]" style={{ color: T.sub }}>
          <Share2 size={12} /> Alice 分享了一段与 Agent-007 的对话(概念示例)
        </div>
        <div className="text-sm font-bold mt-1.5">支付重试幂等性讨论</div>
        <div className="flex items-center gap-2 mt-1.5 text-[10.5px] flex-wrap" style={{ color: T.faint }}>
          14 条消息 · <RouteTag local /> · 外发 0 B · <Tag tone="amb">internal</Tag>
        </div>
        <div className="flex items-center gap-2 mt-3">
          <button onClick={openConvo} className="text-xs font-semibold px-3 py-1.5 rounded-lg" style={{ background: "#fff", border: `1px solid ${T.line}` }}>
            展开对话
          </button>
          <span className="text-[10px]" style={{ color: T.faint }}>对话可整段引用,来源可溯源</span>
        </div>
      </div>

      <div className="ml-10 max-w-xl rounded-2xl overflow-hidden" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
        <div className="p-4">
          <div className="flex items-center gap-2">
            <span className="text-[10px] font-extrabold tracking-widest" style={{ color: T.indigo }}>CAPSULE</span>
            <Tag>来自 支付组</Tag>
            <Tag>P4 概念示例</Tag>
          </div>
          <div className="font-bold mt-1.5">
            Release Checklist <span className="text-[11px] font-normal" style={{ color: T.sub }}>v1.2</span>
          </div>
          <div className="text-xs mt-1" style={{ color: T.sub }}>发布前检查 → 依赖冻结 → 回滚脚本 → 灰度放量建议</div>
          <div className="flex items-center gap-2.5 mt-2.5 text-[10.5px] flex-wrap" style={{ color: T.faint }}>
            <span className="flex items-center gap-1 font-semibold" style={{ color: T.green }}>
              <BadgeCheck size={12} />验真 96% · 32 次影子重放
            </span>
            · 支付组已使用 41 次 · <Tag tone="amb">internal</Tag>
          </div>
        </div>
        <div className="flex items-center gap-2 px-4 py-3" style={{ borderTop: `1px solid ${T.line}` }}>
          {introduced ? (
            <span className="flex items-center gap-1.5 text-xs font-semibold fade" style={{ color: T.green }}>
              <Check size={13} /> 已引入平台组能力库 · 密级与验真记录随包迁移
            </span>
          ) : (
            <IBtn onClick={() => setIntroduced(true)}>
              <Plus size={12} /> 引入到平台组能力库
            </IBtn>
          )}
          <button className="ml-auto flex items-center gap-1 text-xs px-3 py-1.5 rounded-lg" style={{ background: "#fff", border: `1px solid ${T.line}`, color: T.sub }}>
            <Play size={11} /> 直接运行
          </button>
        </div>
      </div>
    </>
  ) : undefined;

  return (
    <div className="px-7 pt-1 pb-6 flex gap-4" style={{ height: "calc(100% - 78px)" }}>
      <div className="flex-1 min-w-0 flex flex-col">
        <div className="flex items-center gap-2 pb-2">
          {isPlatform && (
            <button onClick={goMeeting} className="inline-flex items-center gap-1.5 text-[11.5px] font-semibold px-3 py-1.5 rounded-full"
              style={{ background: T.greenSoft, color: T.green }}>
              <Radio size={11} className="wv" /> 语音房间 · 3 人 + 007 · 加入
            </button>
          )}
          <span className="ml-auto flex items-center gap-1.5">
            <Tag tone={channel.level === "restricted" ? "red" : channel.level === "internal" ? "amb" : undefined}>
              频道密级 {channel.level}
            </Tag>
          </span>
        </div>
        <Card className="flex-1 min-h-0 flex flex-col overflow-hidden">
          <ChatPane channel={channel} chat={chat} header={conceptCards} />
        </Card>
      </div>

      {/* 频道资产 + 真实审批/路由/Diff/审计栏 */}
      <div className="w-72 shrink-0 flex flex-col gap-3 overflow-y-auto">
        <ApprovalsPanel pending={approvals} onDecided={onApprovalsChanged} />
        <DiffPanel diff={chat.lastDiff} />
        {isPlatform && (
          <Card className="p-4">
            <div className="text-[10px] font-semibold tracking-widest mb-2.5" style={{ color: T.faint }}>本频道 · 置顶能力</div>
            <PinCap name="Code Review" ver="v2.0" rate={98} />
            {introduced && <PinCap name="Release Checklist" ver="v1.2" rate={96} from="支付组" fresh />}
          </Card>
        )}

        <Card className="p-4">
          <div className="text-[10px] font-semibold tracking-widest mb-2" style={{ color: T.faint }}>路由决策</div>
          {fail && !start ? (
            <div className="rounded-xl p-3 text-[11.5px] leading-relaxed" style={{ background: T.redSoft, color: "#8F2B2E" }}>
              <b>⛔ {fail.run_id} 被拒绝</b>
              <div className="mt-1 break-all">{fail.message}</div>
            </div>
          ) : start ? (
            <div className="space-y-2">
              <div className="flex items-center gap-2">
                <RouteTag local={start.plan.primary_locality.toLowerCase() !== "cloud"} />
                <b className="text-[12.5px]">{start.provider.display_name}</b>
                <span className="text-[10.5px]" style={{ color: T.sub }}>{start.provider.model}</span>
              </div>
              <div className="flex items-center justify-between text-[11px]" style={{ color: T.sub }}>
                有效密级 <LvTag level={start.plan.effective} />
              </div>
              {start.plan.deciders.length > 0 ? (
                start.plan.deciders.map((d, i) => (
                  <div key={i} className="flex items-center gap-1.5 text-[10.5px] rounded-lg px-2 py-1.5" style={{ background: T.panel, color: T.sub }}>
                    <Tag>{ORIGIN_ZH[d.origin] ?? d.origin}</Tag>
                    {d.subject}
                  </div>
                ))
              ) : (
                <div className="text-[10.5px]" style={{ color: T.faint }}>无标签来源,默认 open</div>
              )}
              {start.plan.downgraded && (
                <div className="rounded-lg px-2.5 py-2 text-[10.5px] leading-relaxed" style={{ background: T.amberSoft, color: T.amber }}>
                  {DOWNGRADE_ZH[start.plan.downgraded.reason] ?? start.plan.downgraded.reason}
                </div>
              )}
              {chat.lastDone && (
                <div className="text-[10.5px]" style={{ color: T.faint }}>
                  {chat.lastDone.latency_ms} ms · in {chat.lastDone.prompt_tokens ?? "—"} / out {chat.lastDone.completion_tokens ?? "—"} tokens
                </div>
              )}
            </div>
          ) : (
            <div className="text-[10.5px] leading-relaxed" style={{ color: T.faint }}>发一条消息后,这里显示"为什么落在这里"。</div>
          )}
        </Card>

        <Card className="p-4">
          <div className="text-[10px] font-semibold tracking-widest mb-2" style={{ color: T.faint }}>最近审计</div>
          <div className="space-y-1.5">
            {auditRows.slice(0, 6).map((r) => (
              <div key={r.event_id} className="flex items-center gap-1.5 text-[10px]">
                <span className="font-bold shrink-0" style={{ color: T.indigoDeep }}>{r.event_type}</span>
                <span className="truncate" style={{ color: T.sub }}>{r.run_id ?? ""}</span>
                <span className="ml-auto shrink-0" style={{ color: T.faint }}>{fmtTime(r.ts_ms)}</span>
              </div>
            ))}
            {auditRows.length === 0 && <div className="text-[10.5px]" style={{ color: T.faint }}>暂无事件</div>}
          </div>
        </Card>

        <Card className="p-4">
          <div className="text-[10px] font-semibold tracking-widest mb-2" style={{ color: T.faint }}>频道密级</div>
          <div className="flex items-center gap-2 text-xs" style={{ color: T.sub }}>
            <LvTag level={channel.level} /> 分享入本频道的内容不得低配此级
          </div>
        </Card>
      </div>
    </div>
  );
}

const PinCap = ({ name, ver, rate, from, fresh }: { name: string; ver: string; rate: number; from?: string; fresh?: boolean }) => (
  <div className={`rounded-xl p-3 mb-2 ${fresh ? "fade" : ""}`} style={{ background: T.panel, border: `1px solid ${T.line}` }}>
    <div className="flex items-center gap-1.5">
      <span className="text-[9px] font-extrabold tracking-widest" style={{ color: T.indigo }}>CAPSULE</span>
      {from && <Tag>来自{from}</Tag>}
    </div>
    <div className="text-[13px] font-bold mt-1">
      {name} <span className="text-[10px] font-normal" style={{ color: T.sub }}>{ver}</span>
    </div>
    <div className="flex items-center mt-2">
      <span className="text-[10px] font-semibold flex items-center gap-1" style={{ color: T.green }}>
        <BadgeCheck size={10} />验真 {rate}%
      </span>
      <button className="ml-auto text-[11px] font-semibold px-2.5 py-1 rounded-lg flex items-center gap-1" style={{ background: T.indigoSoft, color: T.indigo }}>
        <Play size={10} /> 运行
      </button>
    </div>
  </div>
);

/* ==================== 编制管理(D6 概念) ==================== */

export function RosterView({
  approved,
  filter,
  setFilter,
  team,
  live,
}: {
  approved: boolean;
  filter: string;
  setFilter: (f: string) => void;
  team: string;
  live: RosterEntryOut[];
}) {
  // 真实编制:审计链里干过活的 actor(system 类不算人手,不入册)
  const real = live.filter((r) => r.actor_kind !== "system");
  const realFiltered = real.filter((r) =>
    filter === "全部" ? true
      : filter === "人类" ? r.actor_kind === "human"
      : filter === "Agent" ? r.actor_kind === "agent"
      : r.pending_approvals > 0
  );
  const inTeam = ROSTER.filter((r) => r.team === team);
  const conceptFiltered = inTeam.filter((r) =>
    filter === "全部" ? true : filter === "人类" ? r.kind === "human" : filter === "Agent" ? r.kind === "agent"
      : r.tiles.some((t) => (t.hot && !approved) || t.hot2)
  );

  return (
    <div className="px-7 pb-8 pt-2">
      <div className="flex items-center gap-2.5 mb-4">
        <div className="flex items-center gap-2 flex-1 px-3.5 py-2 rounded-xl text-[13px]" style={{ background: T.soft, color: T.faint, maxWidth: 320 }}>
          <Search size={14} /> 搜索本团队成员或 Agent…
        </div>
        <span className="text-[11px]" style={{ color: T.sub }}>
          在册 {real.length}(审计链实证)· 概念 {inTeam.length}
        </span>
        <div className="ml-auto flex items-center gap-2">
          {["全部", "人类", "Agent", "待审批"].map((f) => (
            <button key={f} onClick={() => setFilter(f)} className="text-xs px-3.5 py-2 rounded-xl"
              style={{ background: filter === f ? T.indigo : T.soft, color: filter === f ? "#fff" : T.sub, fontWeight: filter === f ? 600 : 500 }}>
              {f}
            </button>
          ))}
          <IBtn><Plus size={13} /> 新增编制</IBtn>
        </div>
      </div>

      {/* ---- 在册编制(真数据) ---- */}
      <div className="flex items-center gap-2 mb-2.5">
        <b className="text-[13px]">在册编制</b>
        <span className="text-[10.5px]" style={{ color: T.sub }}>
          编制 = 审计链里真实干过活的 actor;数字全部来自 SQL
        </span>
      </div>
      {realFiltered.length === 0 ? (
        <Card className="p-5 text-center text-xs mb-6" style={{ color: T.sub }}>
          {real.length === 0
            ? "还没有任何 actor 在审计链里留下记录——发起一次对话或任务后,这里会出现真实编制。"
            : "当前筛选下没有在册条目。"}
        </Card>
      ) : (
        <div className="grid grid-cols-3 gap-4 mb-6">
          {realFiltered.map((r) => <LiveCard key={r.actor_kind + r.actor_id} r={r} />)}
        </div>
      )}

      {/* ---- 概念编制(演示叙事) ---- */}
      <div className="flex items-center gap-2 mb-2.5">
        <b className="text-[13px]">概念编制</b>
        <Tag>演示叙事 · 非真实数据</Tag>
        <span className="text-[10.5px]" style={{ color: T.faint }}>
          多人多 Agent 的组织形态待 P2(账号与权限)落地后接真
        </span>
      </div>
      {conceptFiltered.length === 0 ? (
        <Card className="p-5 text-center text-xs" style={{ color: T.sub }}>本团队当前筛选下没有概念条目</Card>
      ) : (
        <div className="grid grid-cols-3 gap-4">
          {conceptFiltered.map((p) => <PersonCard key={p.name} p={p} approved={approved} />)}
        </div>
      )}
    </div>
  );
}

/// 真实编制卡:每个数字都能在审计表里查到。
function LiveCard({ r }: { r: RosterEntryOut }) {
  const isAgent = r.actor_kind === "agent";
  const local = r.last_locality === "local";
  const tiles: [string, string, boolean][] = [
    ["参与 Runs", String(r.runs), false],
    ["待审批", String(r.pending_approvals), r.pending_approvals > 0],
    ["被拒路由", String(r.refusals), r.refusals > 0],
  ];
  return (
    <div className="rounded-2xl flex flex-col" style={{ background: "#fff", border: `1px solid ${T.line}`, padding: 18 }}>
      <div className="flex items-center gap-3">
        <div className="w-11 h-11 rounded-2xl flex items-center justify-center font-bold text-lg"
          style={isAgent ? { background: T.indigoSoft, color: T.indigo } : { background: `${T.teal}18`, color: T.teal }}>
          {isAgent ? <Bot size={22} /> : r.display_name[0]}
        </div>
        <div className="min-w-0">
          <div className="text-[14.5px] font-bold truncate">{r.display_name}</div>
          <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>{r.role}</div>
        </div>
        <div className="ml-auto flex flex-col items-end gap-1">
          <Tag tone="ind">{isAgent ? `编制 ${r.actor_id}` : r.actor_id}</Tag>
          {r.last_locality && <RouteTag local={local} />}
        </div>
      </div>

      <div className="flex gap-1.5 mt-3 flex-wrap">
        <Tag tone="grn">在册 · 审计实证</Tag>
        <Tag>{r.events} 条事件</Tag>
        {r.cloud_calls > 0 && <Tag tone="ind">云端 {r.cloud_calls}</Tag>}
        {r.local_calls > 0 && <Tag tone="teal">本地 {r.local_calls}</Tag>}
      </div>

      <div className="grid grid-cols-3 gap-2 mt-3.5">
        {tiles.map(([label, v, hot]) => (
          <div key={label} className="rounded-xl px-1.5 py-2.5 text-center" style={{ background: T.soft }}>
            <div className="text-[10px]" style={{ color: T.sub }}>{label}</div>
            <div className="text-[13px] font-bold mt-0.5" style={{ color: hot ? T.red : T.ink }}>{v}</div>
          </div>
        ))}
      </div>

      <div className="mt-3.5 text-[10.5px]" style={{ color: T.faint }}>
        入职 {fmtDate(r.first_seen_ms)} · 最近活跃 {fmtDate(r.last_seen_ms)}
      </div>
    </div>
  );
}

function PersonCard({ p, approved }: { p: RosterEntry; approved: boolean }) {
  const feat = p.feat, isA = p.kind === "agent";
  return (
    <div className="rounded-2xl flex flex-col" style={feat
      ? { background: T.indigo, color: "#fff", boxShadow: "0 10px 24px rgba(91,91,245,.3)", padding: 18 }
      : { background: "#fff", border: `1px solid ${T.line}`, padding: 18 }}>
      <div className="flex items-center gap-3">
        <div className="w-11 h-11 rounded-2xl flex items-center justify-center font-bold text-lg"
          style={feat ? { background: "rgba(255,255,255,.16)" } : isA ? { background: T.indigoSoft, color: T.indigo } : { background: `${p.tone}18`, color: p.tone }}>
          {isA ? <Bot size={22} /> : p.init}
        </div>
        <div>
          <div className="text-[14.5px] font-bold">{p.name}</div>
          <div className="text-[11px] mt-0.5" style={{ color: feat ? "#C9C9FB" : T.sub }}>{isA ? p.role : "成员"}</div>
        </div>
        <div className="ml-auto flex items-start gap-1.5">
          <Tag tone={feat ? undefined : "ind"} style={feat ? { background: "rgba(255,255,255,.18)", color: "#fff" } : {}}>{p.grade}</Tag>
          <span style={{ color: feat ? "#C9C9FB" : T.faint }}>⋮</span>
        </div>
      </div>
      <div className="flex gap-1.5 mt-3 flex-wrap">
        {p.tags.map((t) => (
          <Tag key={t} style={feat ? { background: "rgba(255,255,255,.13)", color: "#DCDCFE" } : {}}>{t}</Tag>
        ))}
      </div>
      <div className="grid grid-cols-3 gap-2 mt-3.5">
        {p.tiles.map((t, i) => {
          const Ic = TICON[t.i];
          const hot = (t.hot && !approved) || t.hot2;
          const v = t.hot && approved ? "0 项" : t.v;
          return (
            <div key={i} className="rounded-xl px-1.5 py-2.5 text-center" style={{ background: feat ? "rgba(255,255,255,.13)" : T.soft }}>
              <div className="flex justify-center" style={{ color: feat ? "#DCDCFE" : hot ? T.red : "#5A5E70" }}><Ic size={14} /></div>
              <div className="text-[10px] mt-1" style={{ color: feat ? "#C9C9FB" : T.sub }}>{t.l}</div>
              <div className="text-[11.5px] font-bold mt-0.5" style={{ color: feat ? "#fff" : hot ? T.red : T.ink }}>{v}</div>
            </div>
          );
        })}
      </div>
      <div className="flex items-center mt-3.5 text-[10.5px]" style={{ color: feat ? "#BDBDF9" : T.faint }}>
        {p.foot}
        <button className="ml-auto text-[11.5px] font-semibold px-3 py-1.5 rounded-lg"
          style={feat ? { background: "#fff", color: T.indigo } : { background: T.indigoSoft, color: T.indigo }}>
          详情
        </button>
      </div>
    </div>
  );
}

/* ==================== 会议室(v1.x 概念) ==================== */

export function MeetingView() {
  const [sec, setSec] = useState(24 * 60 + 31);
  const [ci, setCi] = useState(0);
  const [asked, setAsked] = useState(false);
  const [mic, setMic] = useState(true);
  useEffect(() => {
    const a = setInterval(() => setSec((s) => s + 1), 1000);
    const b = setInterval(() => setCi((i) => (i + 1) % CAPTIONS.length), 3000);
    return () => {
      clearInterval(a);
      clearInterval(b);
    };
  }, []);
  const mm = String(Math.floor(sec / 60)).padStart(2, "0"), ss = String(sec % 60).padStart(2, "0");
  const cap: [string, string] =
    asked && ci === 0 ? ["Agent-007", "结论复述:幂等键由网关层统一生成;回滚脚本由我会后产出,今天内给草稿。"] : CAPTIONS[ci];

  return (
    <div className="px-7 pb-6 pt-1 flex gap-4" style={{ height: "calc(100% - 78px)" }}>
      <div className="flex-1 min-w-0 flex flex-col gap-3.5">
        <div className="flex items-center gap-2.5">
          <span className="text-base font-bold">平台组周会</span>
          <span className="text-[12.5px] font-bold px-2.5 py-1 rounded-full" style={{ background: T.greenSoft, color: T.green }}>{mm}:{ss}</span>
          <Tag tone="red">● 录制中 · 本地存储</Tag>
          <Tag>会议系统 v1.x 概念</Tag>
          <span className="ml-auto flex items-center gap-1.5 text-[11.5px]" style={{ color: T.sub }}>实时转写 <RouteTag local /></span>
        </div>
        <div className="grid grid-cols-2 gap-3.5 flex-1 min-h-0">
          <Seat n="Alice" tone={T.indigo} speaking />
          <Seat n="Bob" tone={T.teal} />
          <Seat n="Carol" tone={T.amber} muted />
          <div className="relative rounded-2xl flex flex-col items-center justify-center gap-2.5" style={{ background: T.indigoSoft, border: `2px solid ${T.indigo}` }}>
            <div className="w-14 h-14 rounded-2xl flex items-center justify-center" style={{ background: "#fff", color: T.indigo }}><Bot size={26} /></div>
            <div className="flex items-end gap-1" style={{ height: 18 }}>
              {[0, 1, 2, 3, 4].map((i) => (
                <span key={i} className="wv rounded-full" style={{ width: 4, height: 16, background: T.indigo, animationDelay: `${i * 0.12}s` }} />
              ))}
            </div>
            <div className="absolute left-3 bottom-2.5 flex items-center gap-1.5">
              <span className="text-[13px] font-bold">Agent-007</span>
              <Tag tone="ind" style={{ background: "#fff" }}>编制 A-007</Tag>
            </div>
            <span className="absolute right-3 top-3"><RouteTag local /></span>
          </div>
        </div>
        <Card className="flex items-center gap-3 px-4 py-3">
          <Tag>字幕</Tag>
          <span className="text-[13px] truncate">
            <b style={{ color: cap[0] === "Agent-007" ? T.indigo : T.ink }}>{cap[0]}:</b>
            <span style={{ color: "#454A5C" }}> {cap[1]}</span>
          </span>
          <button onClick={() => setAsked(true)} disabled={asked}
            className="ml-auto shrink-0 text-[11.5px] font-semibold px-3.5 py-2 rounded-full"
            style={{ background: asked ? T.soft : T.indigo, color: asked ? T.faint : "#fff" }}>
            {asked ? "已复述" : "语音:@007 复述结论"}
          </button>
        </Card>
        <div className="flex items-center justify-center gap-2.5">
          <Ctl on={mic} onClick={() => setMic((m) => !m)}>{mic ? <Mic size={16} /> : <MicOff size={16} />}</Ctl>
          <Ctl on><Video size={16} /></Ctl>
          <Ctl on><Monitor size={16} /></Ctl>
          <button className="flex items-center gap-1.5 text-[12.5px] font-semibold px-4 py-2.5 rounded-full" style={{ background: T.indigoSoft, color: T.indigo }}>
            <Bot size={14} /> 007 参会中
          </button>
          <button className="flex items-center gap-1.5 text-[12.5px] font-semibold px-4 py-2.5 rounded-full" style={{ background: T.red, color: "#fff" }}>
            <Phone size={14} /> 结束会议
          </button>
        </div>
      </div>
      <Card className="w-[300px] shrink-0 flex flex-col overflow-hidden">
        <div className="px-4 py-3.5 flex items-center gap-1.5" style={{ borderBottom: `1px solid ${T.line}` }}>
          <Sparkles size={14} style={{ color: T.indigo }} /><b className="text-[13.5px]">实时纪要</b>
          <span className="text-[11px]" style={{ color: T.sub }}>· Agent-021</span>
        </div>
        <div className="flex-1 overflow-y-auto px-4 py-1">
          <Note k="议题">支付重试的幂等性方案与发布回滚流程</Note>
          <Note k="决定">幂等键收敛到网关层统一生成,业务侧只透传</Note>
          <Note k="决定">回滚脚本纳入 Release Checklist 强制项</Note>
          <Note k="行动" who="Agent-007" ai>会后执行 Release Checklist,产出回滚脚本草稿</Note>
          {asked && <Note k="行动" who="Agent-007" ai fresh>复述结论已确认,今日内交付草稿</Note>}
          <Note k="行动" who="Bob">网关幂等键设计文档,周四评审</Note>
        </div>
        <div className="px-4 py-3 text-[10.5px]" style={{ color: T.sub, borderTop: `1px solid ${T.line}` }}>
          会后自动发布到 #platform · 行动项进任务看板
        </div>
      </Card>
    </div>
  );
}
const Seat = ({ n, tone, speaking, muted }: { n: string; tone: string; speaking?: boolean; muted?: boolean }) => (
  <div className="relative rounded-2xl flex items-center justify-center" style={{ background: T.panel, border: `2px solid ${speaking ? T.indigo : "transparent"}` }}>
    <div className="w-14 h-14 rounded-2xl flex items-center justify-center text-[22px] font-bold" style={{ background: `${tone}18`, color: tone }}>{n[0]}</div>
    <div className="absolute left-3 bottom-2.5 text-[13px] font-bold">
      {n}
      {speaking && <span style={{ color: T.indigo }}> · 发言中</span>}
    </div>
    {muted && <span className="absolute right-3 top-3" style={{ color: T.faint }}><MicOff size={13} /></span>}
  </div>
);
const Ctl = ({ children, on, onClick }: { children: React.ReactNode; on?: boolean; onClick?: () => void }) => (
  <button onClick={onClick} className="w-10 h-10 rounded-full flex items-center justify-center" style={{ background: on ? T.soft : T.redSoft, color: on ? T.ink : T.red }}>
    {children}
  </button>
);
const Note = ({ k, who, ai, fresh, children }: { k: string; who?: string; ai?: boolean; fresh?: boolean; children: React.ReactNode }) => (
  <div className={`py-2.5 text-xs ${fresh ? "fade" : ""}`} style={{ borderTop: `1px solid ${T.line}` }}>
    <div className="flex items-center">
      <Tag tone={k === "行动" ? "ind" : undefined}>{k}</Tag>
      {who && <span className="ml-auto text-[10.5px] font-semibold" style={{ color: ai ? T.indigo : T.sub }}>{who}</span>}
    </div>
    <div className="mt-1.5 leading-relaxed" style={{ color: "#454A5C" }}>{children}</div>
    {ai && (
      <div className="flex items-center gap-1 text-[10px] font-semibold mt-1.5" style={{ color: T.green }}>
        <Check size={10} /> 已转任务 RUN-2231 · 本地执行
      </div>
    )}
  </div>
);

/* ==================== 能力库(P4 概念) ==================== */

export function CapsView({
  trace, setTrace, introduced, live, forgeable, onForge,
}: {
  trace: boolean;
  setTrace: (fn: (t: boolean) => boolean) => void;
  introduced: boolean;
  live: CapsuleOut[];
  forgeable: ForgeableRun[];
  onForge: (runId: string, goal: string) => void;
}) {
  return (
    <div className="px-7 pb-8 pt-2">
      <div className="flex items-center mb-4">
        <div className="flex items-center gap-2 flex-1 px-3.5 py-2 rounded-xl text-[13px]" style={{ background: T.soft, color: T.faint, maxWidth: 360 }}>
          <Search size={14} /> 搜索能力…
        </div>
        <span className="ml-3 text-[11px]" style={{ color: T.sub }}>
          已锻造 {live.length} · 可锻造运行 {forgeable.length}
        </span>
      </div>

      <ForgeSection live={live} forgeable={forgeable} onForge={onForge} />

      <div className="flex items-center gap-2 mb-2.5 mt-6">
        <b className="text-[13px]">概念能力</b>
        <Tag>演示叙事 · 非真实数据</Tag>
        <span className="text-[10.5px]" style={{ color: T.faint }}>
          验真率、影子重放次数等为示例值;真实 Capsule 见上方
        </span>
      </div>
      <div className="grid grid-cols-2 gap-4">
        {CAPS.map((c) => (
          <Card key={c.name} className="p-5">
            <div className="flex items-center gap-2">
              <span className="text-[10px] font-extrabold tracking-widest" style={{ color: T.indigo }}>CAPSULE</span>
              <Tag>{c.team}</Tag>
              <span className="ml-auto flex gap-1.5">
                {c.restricted && <Tag tone="red"><Lock size={10} /> restricted</Tag>}
                {c.hot && introduced && <Tag tone="grn"><Check size={10} /> 已引入平台组</Tag>}
                {c.hot && <Tag tone="ind">高复用</Tag>}
              </span>
            </div>
            <div className="mt-2.5 text-[15px]">
              <b>{c.name}</b> <span className="text-[11.5px]" style={{ color: T.sub }}>{c.ver}</span>
            </div>
            <div className="flex gap-1.5 mt-2 flex-wrap">{c.tags.map((t) => <Tag key={t}>{t}</Tag>)}</div>
            <div className="flex items-center gap-3 mt-3.5 text-[11.5px]">
              {c.verifying ? (
                <span className="flex items-center gap-2" style={{ color: T.amber }}>
                  验真中 {c.rate}%
                  <span className="rounded-full overflow-hidden" style={{ width: 64, height: 6, background: T.soft }}>
                    <span className="block h-full" style={{ width: `${c.rate}%`, background: T.amber }} />
                  </span>
                </span>
              ) : (
                <span className="flex items-center gap-1 font-semibold" style={{ color: T.green }}>
                  <BadgeCheck size={13} /> 验真 {c.rate}% · {c.runs} 次影子重放
                </span>
              )}
              <span style={{ color: T.sub }}>使用 {c.used} 次</span>
              <Tag>{c.scope}</Tag>
            </div>
            <div className="flex gap-2 mt-4">
              <IBtn className="px-3.5 py-2"><Play size={12} /> 运行</IBtn>
              <button onClick={() => c.hot && setTrace((t) => !t)} className="flex items-center gap-1.5 text-xs px-3.5 py-2 rounded-xl" style={{ background: T.soft }}>
                <GitBranch size={12} /> 溯源
              </button>
            </div>
            {trace && c.hot && (
              <div className="mt-4 rounded-xl p-4 text-xs fade" style={{ background: T.panel }}>
                <TStep t="锻造自成功运行 RUN-1893" d="支付组 · 07-02 · 全事件链归档" first />
                <TStep t="参数化 3 项" d="目标仓库 / 发布分支 / 灰度比例" />
                <TStep t="影子重放验真 32/33" d="环境夹具:仓库快照 + 依赖锁定" tone={T.green} />
                <TStep t="Promote:支付组 → 跨团队" d="管理员批准 · 密级随包迁移" tone={T.indigo} last />
              </div>
            )}
          </Card>
        ))}
      </div>
    </div>
  );
}
const TStep = ({ t, d, tone, first, last }: { t: string; d: string; tone?: string; first?: boolean; last?: boolean }) => (
  <div className="flex gap-3">
    <div className="flex flex-col items-center">
      {!first && <span style={{ width: 1, height: 8, background: T.line }} />}
      <span className="rounded-full shrink-0" style={{ width: 8, height: 8, background: tone || T.faint }} />
      {!last && <span className="flex-1" style={{ width: 1, background: T.line }} />}
    </div>
    <div className="pb-3">
      <div className="font-semibold" style={{ color: tone || T.ink }}>{t}</div>
      <div className="text-[10px] mt-0.5" style={{ color: T.faint }}>{d}</div>
    </div>
  </div>
);

/* Msg 原子(概念示例消息) */
export const ConceptMsg = ({ who, tone, time, children }: { who: string; tone: string; time: string; children: React.ReactNode }) => (
  <div className="flex gap-2.5 max-w-2xl">
    <div className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0 text-xs font-bold" style={{ background: `${tone}18`, color: tone }}>{who[0]}</div>
    <div className="min-w-0">
      <div className="flex items-baseline gap-2">
        <span className="text-[13px] font-bold">{who}</span>
        <span className="text-[10px]" style={{ color: T.faint }}>{time}</span>
      </div>
      <div className="text-[13px] mt-0.5 leading-relaxed" style={{ color: "#454A5C" }}>{children}</div>
    </div>
  </div>
);

/* ==================== P4 真实 Capsule(锻造区) ==================== */

function ForgeSection({
  live,
  forgeable,
  onForge,
}: {
  live: CapsuleOut[];
  forgeable: ForgeableRun[];
  onForge: (runId: string, goal: string) => void;
}) {
  const [picking, setPicking] = useState<string | null>(null);
  const [goal, setGoal] = useState("");

  return (
    <>
      <div className="flex items-center gap-2 mb-2.5">
        <b className="text-[13px]">已锻造能力</b>
        <span className="text-[10.5px]" style={{ color: T.sub }}>
          锻造自成功运行,带着出处与重放引用;验真率由 capsule.verify 事件算出
        </span>
      </div>

      {live.length === 0 ? (
        <Card className="p-5 text-xs mb-4" style={{ color: T.sub }}>
          还没有锻造过能力。一次**成功完成且经审批**的任务运行就可以被固化成 Capsule——
          它复制的不是提示词,而是那次运行的完整重放引用(仓库快照 / 依赖锁 / 模型 / 工具环境)。
        </Card>
      ) : (
        <div className="grid grid-cols-2 gap-4 mb-4">
          {live.map((c) => (
            <Card key={c.capsule_id} className="p-5">
              <div className="flex items-center gap-2">
                <span className="text-[10px] font-extrabold tracking-widest" style={{ color: T.indigo }}>CAPSULE</span>
                <Tag tone="grn">真实锻造</Tag>
                <span className="ml-auto"><Tag>{c.scope}</Tag></span>
              </div>
              <div className="mt-2.5 text-[15px]">
                <b>{c.name}</b> <span className="text-[11.5px]" style={{ color: T.sub }}>v{c.version}</span>
              </div>
              <div className="flex items-center gap-2 mt-2 text-[10.5px] flex-wrap" style={{ color: T.faint }}>
                <GitBranch size={11} /> 源运行 {c.source_run_id} · 锻造于 {fmtDate(c.forged_ms)} · {c.forged_by}
              </div>
              <div className="flex items-center gap-3 mt-3 text-[11.5px]">
                {c.verified_rate === null ? (
                  <span className="flex items-center gap-1.5" style={{ color: T.sub }}>
                    <Tag tone="amb">尚未验真</Tag>
                    <span style={{ color: T.faint }}>影子重放后才有验真率</span>
                  </span>
                ) : (
                  <span className="flex items-center gap-1 font-semibold" style={{ color: c.verified_rate >= 0.9 ? T.green : T.amber }}>
                    <BadgeCheck size={13} /> 验真 {(c.verified_rate * 100).toFixed(0)}% · {c.verify_passed}/{c.verify_total} 次重放
                  </span>
                )}
                {c.adopted > 0 && <Tag tone="ind">已被引入 {c.adopted} 次</Tag>}
              </div>
            </Card>
          ))}
        </div>
      )}

      <div className="flex items-center gap-2 mb-2.5">
        <b className="text-[13px]">可锻造的运行</b>
        <span className="text-[10.5px]" style={{ color: T.sub }}>
          仅限成功结束、留有重放引用、且尚未锻造过的运行
        </span>
      </div>
      {forgeable.length === 0 ? (
        <Card className="p-5 text-xs" style={{ color: T.sub }}>
          暂无可锻造的运行。用「▶ 任务」跑一个成功的任务后,它会出现在这里。
        </Card>
      ) : (
        <Card className="p-4">
          {forgeable.map((r) => (
            <div key={r.run_id} className="py-2.5" style={{ borderTop: `1px solid ${T.line}` }}>
              <div className="flex items-center gap-3">
                <div className="min-w-0 flex-1">
                  <div className="text-[13px] font-semibold">{r.run_id}</div>
                  <div className="text-[10.5px] mt-0.5" style={{ color: T.sub }}>
                    {fmtDate(r.ts_ms)} · 耗时 {(r.duration_ms / 1000).toFixed(1)}s
                    {r.output_hash && ` · 产出 ${r.output_hash.slice(7, 19)}`}
                  </div>
                </div>
                <button
                  onClick={() => { setPicking(picking === r.run_id ? null : r.run_id); setGoal(""); }}
                  className="text-[11.5px] font-semibold px-3 py-1.5 rounded-lg"
                  style={{ background: T.indigoSoft, color: T.indigo }}
                >
                  <Plus size={11} className="inline" /> 锻造
                </button>
              </div>
              {picking === r.run_id && (
                <div className="mt-2.5 rounded-xl p-3 fade" style={{ background: T.panel }}>
                  <div className="text-[10.5px] mb-1.5" style={{ color: T.sub }}>
                    这个能力解决什么问题?(会写进 Capsule 定义,可日后改写)
                  </div>
                  <input
                    value={goal}
                    onChange={(e) => setGoal(e.target.value)}
                    placeholder="例:修复算术运算符写反的 bug"
                    className="w-full px-3 py-2 rounded-lg text-[12.5px] outline-none"
                    style={{ background: "#fff", border: `1px solid ${T.line}` }}
                  />
                  <div className="flex gap-2 mt-2">
                    <button
                      onClick={() => { onForge(r.run_id, goal.trim() || `运行 ${r.run_id} 的能力`); setPicking(null); }}
                      className="text-[11.5px] font-semibold px-3.5 py-1.5 rounded-lg"
                      style={{ background: T.indigo, color: "#fff" }}
                    >
                      确认锻造
                    </button>
                    <button onClick={() => setPicking(null)} className="text-[11.5px] px-3 py-1.5 rounded-lg" style={{ background: T.soft, color: T.sub }}>
                      取消
                    </button>
                  </div>
                </div>
              )}
            </div>
          ))}
        </Card>
      )}
    </>
  );
}
