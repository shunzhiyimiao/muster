import React, { useEffect, useState } from "react";
import {
  Home, Users, Video, Hash, Library, Bot, ShieldAlert, Shield, Settings,
  Search, Plus, Bell, ChevronRight, ChevronDown, Check, X, Lock,
  Mic, MicOff, Monitor, Phone, Radio, Sparkles, BadgeCheck, Play, GitBranch,
  TrendingUp, Zap, Cloud, HardDrive, Share2, FileText, AlertTriangle, User,
  LayoutDashboard, Terminal, Calendar, Puzzle, Network, Pencil, Brain,
  MessageSquare, Clock, Cast, Eye, BookOpen, Link2, StopCircle, LineChart
} from "lucide-react";

/* ============================================================
   Muster 概念稿 v4 · 双层侧栏 + 个人/团队模块分离
   - 图标轨(8 模块):控制台 / 工作(个人) / 团队 / 设置 /
     模块扩展 / 计划表 / 终端 / 远程连接
   - 二级栏随模块切换;个人模块含 Agent 档案页(热力图+记忆时间线)
   - 串流:个人会话 → 选频道(受密级约束)→ 团队频道出现 LIVE 卡
   ============================================================ */

const T = {
  canvas: "#E9EAF2", shell: "#FFFFFF", panel: "#F7F8FB", rail: "#F1F2F7",
  soft: "#F1F2F7", line: "#ECEDF3",
  ink: "#17181C", sub: "#8B8FA3", faint: "#B9BCCB",
  indigo: "#5B5BF5", indigoDeep: "#4747E0", indigoSoft: "#EEEEFE",
  green: "#16A34A", greenSoft: "#E4F6EC", red: "#E5484D", redSoft: "#FDE9E9",
  amber: "#D97706", amberSoft: "#FCF1DF", teal: "#0EA5A5", tealSoft: "#E2F5F5",
  barGray: "#E3E4EC", black: "#17181C",
};
const LV = ["#EFF0F5", "#D9DBFA", "#AFB2F8", "#8385F2", "#5B5BF5"];

const TEAMS = [
  { id: "platform", name: "平台组", people: 2, agents: 2, channels: [
    { id: "platform", label: "platform", level: "internal" },
    { id: "code-review", label: "code-review", level: "internal", unread: true },
    { id: "general", label: "general", level: "open" },
  ]},
  { id: "pay", name: "支付组", people: 1, agents: 1, channels: [
    { id: "payments", label: "payments", level: "internal" },
    { id: "release-train", label: "release-train", level: "internal" },
  ]},
  { id: "sec", name: "安全组", people: 1, agents: 1, channels: [
    { id: "sec-ops", label: "sec-ops", level: "restricted" },
  ]},
];

const BARS = { days: ["一","二","三","四","五","六","日"],
  cloud: [34,46,20,44,16,30,22], local: [26,40,14,38,12,26,18], tipAt: 3 };

const CAPTIONS = [
  ["Alice","重试幂等键还是放在网关层统一生成吧,业务侧太散了"],
  ["Bob","同意。业务侧只透传,别各写各的"],
  ["Carol","那回滚脚本谁来出?上次就是回滚卡住的"],
  ["Agent-007","我可以在会后跑 Release Checklist,顺带产出回滚脚本草稿"],
  ["Alice","好,这条行动项记你头上"],
];

/* ---------- 原子件 ---------- */

const Card = ({ children, className = "", style = {} }) => (
  <div className={`rounded-2xl ${className}`} style={{ background: T.shell, border: `1px solid ${T.line}`, ...style }}>{children}</div>
);
const Tag = ({ children, tone, style = {} }) => {
  const m = { ind: [T.indigoSoft, T.indigo], red: [T.redSoft, T.red], grn: [T.greenSoft, T.green], amb: [T.amberSoft, T.amber], teal: [T.tealSoft, T.teal] };
  const [bg, fg] = tone ? m[tone] : [T.soft, T.sub];
  return <span className="inline-flex items-center gap-1 text-[10.5px] px-2 py-0.5 rounded-lg" style={{ background: bg, color: fg, ...style }}>{children}</span>;
};
const Pct = ({ up, hero, children }) => (
  <span className="inline-flex text-[10.5px] font-semibold px-2 py-0.5 rounded-full"
    style={hero ? { background: "rgba(255,255,255,.2)", color: "#fff" } : { background: up ? T.greenSoft : T.redSoft, color: up ? T.green : T.red }}>
    {children}
  </span>
);
const RouteTag = ({ local }) => (
  <span className="inline-flex items-center gap-1 text-[10.5px] font-semibold px-2 py-0.5 rounded-full"
    style={{ background: local ? T.tealSoft : T.indigoSoft, color: local ? T.teal : T.indigo }}>
    {local ? <HardDrive size={11} /> : <Cloud size={11} />}{local ? "本地" : "云端"}
  </span>
);
const ChipDd = ({ children }) => (
  <span className="inline-flex items-center gap-1 text-[11px] px-2.5 py-1 rounded-full" style={{ border: `1px solid ${T.line}`, color: "#5A5E70" }}>
    {children} <ChevronDown size={12} />
  </span>
);
const IBtn = ({ children, onClick, className = "" }) => (
  <button onClick={onClick} className={`inline-flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl ${className}`}
    style={{ background: T.indigo, color: "#fff" }}>{children}</button>
);
const SideSec = ({ children }) => (
  <div className="mt-4 mb-1.5 px-2 text-[10px] font-semibold tracking-widest" style={{ color: "#B9BCCB" }}>{children}</div>
);
const SideItem = ({ icon, label, active, onClick, extra }) => (
  <button onClick={onClick} className="w-full flex items-center gap-2.5 px-3 py-2 rounded-xl text-left text-[13px]"
    style={{ background: active ? T.indigo : "transparent", color: active ? "#fff" : "#5A5E70", fontWeight: active ? 600 : 500, boxShadow: active ? "0 6px 14px rgba(91,91,245,.28)" : "none" }}>
    {icon}{label}{extra}
  </button>
);
const CollapseSec = ({ label, open, onToggle, children }) => (
  <>
    <button onClick={onToggle} className="w-full flex items-center px-2 mt-4 mb-1.5">
      <span className="text-[10px] font-semibold tracking-widest" style={{ color: "#B9BCCB" }}>{label}</span>
      <ChevronDown size={12} className="ml-auto" style={{ color: "#B9BCCB", transform: open ? "none" : "rotate(-90deg)", transition: "transform .15s" }} />
    </button>
    {open && <div className="fade">{children}</div>}
  </>
);
const Bub = ({ children, fresh }) => (
  <div className={`flex gap-2 ${fresh ? "fade" : ""}`}>
    <div className="w-6 h-6 rounded-lg flex items-center justify-center shrink-0" style={{ background: T.indigo, color: "#fff" }}><Bot size={12} /></div>
    <div className="rounded-xl px-3 py-2 leading-relaxed" style={{ background: T.soft }}>{children}</div>
  </div>
);
const CB = ({ who, bot, children }) => (
  <div className="flex gap-2.5">
    <div className="w-7 h-7 rounded-lg flex items-center justify-center shrink-0 text-xs font-bold"
      style={{ background: bot ? T.indigoSoft : "#E4E6EF", color: bot ? T.indigo : "#5A5E70" }}>{bot ? <Bot size={14} /> : who[0]}</div>
    <div className="min-w-0">
      <div className="text-[11px] font-semibold" style={{ color: bot ? T.indigo : T.ink }}>{who}</div>
      <div className="mt-0.5 leading-relaxed rounded-xl px-3 py-2" style={{ background: T.soft, color: "#454A5C" }}>{children}</div>
    </div>
  </div>
);

/* ==================== App ==================== */

const RAIL = [
  { id: "console", icon: LayoutDashboard, label: "控制台", ok: true },
  { id: "personal", icon: User, label: "工作", ok: true },
  { id: "team", icon: Users, label: "团队", ok: true },
  { id: "setting", icon: Settings, label: "设置" },
  { id: "ext", icon: Puzzle, label: "模块扩展" },
  { id: "plan", icon: Calendar, label: "计划表" },
  { id: "term", icon: Terminal, label: "终端" },
  { id: "remote", icon: Network, label: "远程连接" },
];

export default function MusterV4() {
  const [module, setModule] = useState("personal");
  const [view, setView] = useState("phome");
  const [team, setTeam] = useState("platform");
  const [channel, setChannel] = useState("platform");
  const [notice, setNotice] = useState("");
  const [approved, setApproved] = useState(false);
  const [modal, setModal] = useState(false);
  const [introduced, setIntroduced] = useState(false);
  const [convo, setConvo] = useState("closed");
  const [fab, setFab] = useState(false);
  const [fabAsked, setFabAsked] = useState(false);
  const [trace, setTrace] = useState(false);
  const [filter, setFilter] = useState("全部");
  const [expanded, setExpanded] = useState({ platform: true });
  const [agentOpen, setAgentOpen] = useState(true);
  const [picker, setPicker] = useState(false);
  const [streamed, setStreamed] = useState(false);

  const activeTeam = TEAMS.find(t => t.id === team);
  const soft = (label) => setNotice(`「${label}」为完整形态占位模块,本稿聚焦控制台 / 个人工作台 / 团队协作`);

  const goRail = (r) => {
    if (!r.ok) { soft(r.label); return; }
    setNotice(""); setModule(r.id);
    setView(r.id === "console" ? "home" : r.id === "personal" ? "phome" : "channel");
  };
  const goChannel = (tid, cid) => {
    setTeam(tid); setChannel(cid); setModule("team"); setView("channel"); setNotice("");
    setExpanded(e => ({ ...e, [tid]: true }));
  };
  const goRoster = (tid) => {
    setTeam(tid); setModule("team"); setView("roster"); setNotice(""); setFilter("全部");
    setExpanded(e => ({ ...e, [tid]: true }));
  };
  const doStream = () => { setStreamed(true); setPicker(false); };

  return (
    <div className="w-full h-screen overflow-hidden" style={{ background: T.canvas }}>
      <style>{`
        @import url('https://fonts.googleapis.com/css2?family=Inter:wght@500;600;700;800&display=swap');
        *{font-family:'Inter','Noto Sans SC','PingFang SC',system-ui,sans-serif}
        ::-webkit-scrollbar{width:8px}::-webkit-scrollbar-thumb{background:#D6D8E3;border-radius:4px}
        @keyframes wave{0%,100%{transform:scaleY(.35)}50%{transform:scaleY(1)}}
        .wv{animation:wave 1.1s ease-in-out infinite;transform-origin:center}
        @keyframes fadeUp{0%{opacity:0;transform:translateY(4px)}100%{opacity:1;transform:none}}
        .fade{animation:fadeUp .3s ease-out both}
        @keyframes pulse{0%,100%{opacity:1;transform:scale(1)}50%{opacity:.45;transform:scale(.82)}}
        .lv{animation:pulse 1.4s ease-in-out infinite}
        @media(prefers-reduced-motion:reduce){.wv,.fade,.lv{animation:none!important}}
        button:focus-visible{outline:2px solid ${T.indigo};outline-offset:2px}
      `}</style>

      <div className="absolute rounded-3xl flex overflow-hidden" style={{ inset: 14, background: T.shell, boxShadow: "0 12px 40px rgba(23,24,28,.08)" }}>

        {/* ===== 一级:图标轨 ===== */}
        <div className="w-14 shrink-0 flex flex-col items-center py-4 gap-1.5" style={{ background: T.rail, borderRight: `1px solid ${T.line}` }}>
          <div className="w-9 h-9 rounded-xl flex items-center justify-center font-extrabold text-sm mb-2" style={{ background: T.indigo, color: "#fff" }}>M</div>
          {RAIL.map(r => {
            const Ic = r.icon, on = module === r.id;
            return (
              <button key={r.id} onClick={() => goRail(r)} title={r.label}
                className="w-10 h-10 rounded-xl flex items-center justify-center relative"
                style={{ background: on ? T.indigo : "transparent", color: on ? "#fff" : "#8B8FA3", boxShadow: on ? "0 6px 14px rgba(91,91,245,.28)" : "none" }}>
                <Ic size={18} />
                {r.id === "personal" && streamed && !on &&
                  <span className="absolute top-1.5 right-1.5 w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />}
              </button>
            );
          })}
          <div className="mt-auto w-9 h-9 rounded-full flex items-center justify-center text-xs font-bold" style={{ background: T.indigoSoft, color: T.indigo }}>A</div>
        </div>

        {/* ===== 二级:随模块切换 ===== */}
        <aside className="w-56 shrink-0 flex flex-col px-3 py-4 overflow-y-auto" style={{ background: T.panel, borderRight: `1px solid ${T.line}` }}>
          <div className="px-2 pb-2 text-[13px] font-bold">
            {module === "console" ? "控制台" : module === "personal" ? "我的工作台" : "团队"}
          </div>

          {module === "console" && (
            <>
              <SideSec>总览</SideSec>
              <SideItem icon={<Home size={16} />} label="中控台" active={view === "home"} onClick={() => { setView("home"); setNotice(""); }} />
              <SideItem icon={<Shield size={16} />} label="审计中心" onClick={() => soft("审计中心")} />
              <SideItem icon={<LineChart size={16} />} label="数据分析" onClick={() => soft("数据分析")} />
              <div className="mt-auto rounded-2xl p-4 text-white" style={{ background: T.black }}>
                <div className="w-8 h-8 rounded-lg flex items-center justify-center mb-2.5" style={{ background: "rgba(255,255,255,.14)" }}><ShieldAlert size={15} /></div>
                <b className="text-sm">主权演习</b>
                <p className="text-[11px] mt-1 leading-relaxed" style={{ color: "#9FA3B5" }}>季度合规窗口:切断外联,验证全组织本地执行能力</p>
                <button onClick={() => soft("主权演习")} className="mt-3 w-full py-2 rounded-xl text-xs font-semibold" style={{ background: T.indigo }}>启动演习 →</button>
              </div>
            </>
          )}

          {module === "personal" && (
            <>
              <SideSec>我的</SideSec>
              <SideItem icon={<Home size={16} />} label="首页" active={view === "phome"} onClick={() => { setView("phome"); setNotice(""); }} />
              <SideItem icon={<Bot size={16} />} label="Agent 档案" active={view === "agent"} onClick={() => { setView("agent"); setNotice(""); }}
                extra={<span className="ml-auto text-[10px]" style={{ color: view === "agent" ? "#DCDCFE" : T.faint }}>小七</span>} />
              <SideItem icon={<MessageSquare size={16} />} label="对话" onClick={() => soft("对话")} />
              <SideItem icon={<Clock size={16} />} label="任务" onClick={() => soft("任务")} />
              <SideSec>积累</SideSec>
              <SideItem icon={<Brain size={16} />} label="记忆" onClick={() => soft("记忆")} />
              <SideItem icon={<Sparkles size={16} />} label="技能" onClick={() => soft("技能")} />
              <SideItem icon={<BookOpen size={16} />} label="知识库" onClick={() => soft("知识库")} />
              <SideItem icon={<Link2 size={16} />} label="连接器" onClick={() => soft("连接器")} />
              <SideItem icon={<Lock size={16} />} label="权限" onClick={() => soft("权限")} />
              <div className="mt-auto rounded-2xl p-3.5" style={{ background: streamed ? T.indigo : "#fff", border: `1px solid ${streamed ? T.indigo : T.line}`, color: streamed ? "#fff" : T.ink }}>
                <div className="flex items-center gap-1.5 text-[11px] font-semibold">
                  <Cast size={13} />{streamed ? "串流进行中" : "串流到团队"}
                </div>
                {streamed ? (
                  <>
                    <div className="text-[11px] mt-1.5 leading-relaxed" style={{ color: "#DCDCFE" }}>
                      重构支付重试幂等性 · 与小七<br />→ 平台组 #platform · 12 人围观
                    </div>
                    <button onClick={() => setStreamed(false)}
                      className="mt-2.5 w-full py-1.5 rounded-lg text-[11px] font-semibold flex items-center justify-center gap-1"
                      style={{ background: "rgba(255,255,255,.18)" }}><StopCircle size={12} /> 停止串流</button>
                  </>
                ) : (
                  <div className="text-[11px] mt-1 leading-relaxed" style={{ color: T.sub }}>
                    把你与 Agent 的会话实时投到频道,队友可围观、可接手
                  </div>
                )}
              </div>
            </>
          )}

          {module === "team" && (
            <>
              <SideItem icon={<Video size={16} />} label="会议室" active={view === "meeting"} onClick={() => { setView("meeting"); setNotice(""); }}
                extra={<span className="ml-auto inline-flex items-center gap-1 text-[10px] font-semibold" style={{ color: view === "meeting" ? "#fff" : T.green }}><Radio size={10} className="wv" />进行中</span>} />
              <SideSec>团队</SideSec>
              {TEAMS.map(t => {
                const open = !!expanded[t.id];
                const isActiveTeam = team === t.id && (view === "channel" || view === "roster");
                const rosterOn = view === "roster" && team === t.id;
                return (
                  <div key={t.id} className="mb-0.5">
                    <button onClick={() => setExpanded(e => ({ ...e, [t.id]: !open }))}
                      className="w-full flex items-center gap-2 px-2 py-2 rounded-xl text-left">
                      <ChevronDown size={12} style={{ color: T.faint, transform: open ? "none" : "rotate(-90deg)", transition: "transform .15s" }} />
                      <span className="w-6 h-6 rounded-lg flex items-center justify-center text-[10px] font-bold"
                        style={{ background: isActiveTeam ? T.indigo : "#E4E6EF", color: isActiveTeam ? "#fff" : "#5A5E70" }}>{t.name[0]}</span>
                      <span className="text-[13px] font-semibold" style={{ color: isActiveTeam ? T.indigoDeep : "#5A5E70" }}>{t.name}</span>
                      <span className="ml-auto text-[10px]" style={{ color: T.faint }}>{t.people}人·{t.agents}AI</span>
                    </button>
                    {open && (
                      <div className="ml-3.5 pl-2 fade" style={{ borderLeft: `1px solid ${T.line}` }}>
                        {t.channels.map(c => {
                          const on = view === "channel" && team === t.id && channel === c.id;
                          const live = streamed && t.id === "platform" && c.id === "platform";
                          return (
                            <button key={c.id} onClick={() => goChannel(t.id, c.id)}
                              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-lg text-left text-[12.5px]"
                              style={{ background: on ? T.indigo : "transparent", color: on ? "#fff" : "#5A5E70", fontWeight: on ? 600 : 400 }}>
                              <Hash size={12} style={{ opacity: .7 }} /> {c.label}
                              {c.level === "restricted" && <Lock size={10} style={{ color: on ? "#fff" : T.red }} />}
                              {live && <span className="ml-auto w-1.5 h-1.5 rounded-full lv" style={{ background: on ? "#fff" : T.red }} />}
                              {c.unread && !on && !live && <span className="ml-auto w-1.5 h-1.5 rounded-full" style={{ background: T.indigo }} />}
                            </button>
                          );
                        })}
                        <button onClick={() => goRoster(t.id)}
                          className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-lg text-left text-[12.5px]"
                          style={{ background: rosterOn ? T.indigo : "transparent", color: rosterOn ? "#fff" : "#5A5E70", fontWeight: rosterOn ? 600 : 400 }}>
                          <Users size={12} style={{ opacity: .8 }} /> 编制
                          <span className="ml-auto text-[9px] font-bold px-1.5 py-0.5 rounded-md"
                            style={{ background: rosterOn ? "rgba(255,255,255,.22)" : T.soft, color: rosterOn ? "#fff" : T.sub }}>
                            {t.people + t.agents}
                          </span>
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
              <CollapseSec label="AGENT" open={agentOpen} onToggle={() => setAgentOpen(o => !o)}>
                <SideItem icon={<Library size={16} />} label="能力库" active={view === "caps"} onClick={() => { setView("caps"); setNotice(""); }}
                  extra={<span className="ml-auto text-[9px] font-bold px-1.5 py-0.5 rounded-md" style={{ background: view === "caps" ? "rgba(255,255,255,.22)" : T.indigoSoft, color: view === "caps" ? "#fff" : T.indigo }}>NEW</span>} />
                <SideItem icon={<Shield size={16} />} label="审计中心" onClick={() => soft("审计中心")} />
              </CollapseSec>
            </>
          )}
        </aside>

        {/* ===== 主区 ===== */}
        <main className="flex-1 min-w-0 overflow-y-auto">
          <TopBar module={module} view={view} channel={channel} teamName={activeTeam.name} streamed={streamed} />
          {notice && (
            <div className="mx-7 mt-2 px-3.5 py-2 rounded-xl text-xs flex items-center gap-2 fade" style={{ background: T.indigoSoft, color: T.indigoDeep }}>
              <Sparkles size={13} /> {notice}
              <button className="ml-auto" onClick={() => setNotice("")}><X size={13} /></button>
            </div>
          )}
          {view === "home" && <HomeView approved={approved} onApprove={() => setModal(true)} />}
          {view === "phome" && (
            <PersonalHome streamed={streamed} onStream={() => setPicker(true)} onStop={() => setStreamed(false)}
              goAgent={() => setView("agent")} goChannel={() => goChannel("platform", "platform")} openConvo={() => setConvo("open")} />
          )}
          {view === "agent" && <AgentProfile streamed={streamed} onStream={() => setPicker(true)} />}
          {view === "channel" && (
            <ChannelView channel={channel} introduced={introduced} setIntroduced={setIntroduced} streamed={streamed}
              openConvo={() => setConvo("open")} goMeeting={() => setView("meeting")} />
          )}
          {view === "roster" && <RosterView approved={approved} filter={filter} setFilter={setFilter} team={team} />}
          {view === "meeting" && <MeetingView />}
          {view === "caps" && <CapsView trace={trace} setTrace={setTrace} introduced={introduced} />}
        </main>
      </div>

      {/* ===== FAB ===== */}
      <button onClick={() => setFab(f => !f)} className="fixed bottom-8 right-8 w-14 h-14 rounded-2xl flex items-center justify-center z-40"
        style={{ background: T.indigo, color: "#fff", boxShadow: "0 10px 26px rgba(91,91,245,.4)" }}>
        <Bot size={24} />
      </button>
      {fab && (
        <div className="fixed bottom-24 right-8 w-80 z-40 rounded-2xl overflow-hidden fade" style={{ background: "#fff", border: `1px solid ${T.line}`, boxShadow: "0 16px 40px rgba(23,24,28,.14)" }}>
          <div className="px-3.5 py-3 flex items-center gap-2" style={{ background: T.indigoSoft }}>
            <div className="w-8 h-8 rounded-lg flex items-center justify-center" style={{ background: T.indigo, color: "#fff" }}><Bot size={15} /></div>
            <b className="text-[13px]">小七</b>
            <Tag tone="ind" style={{ background: "#fff" }}>编制 A-007</Tag>
            <span className="ml-auto"><RouteTag local /></span>
          </div>
          <div className="p-3 space-y-2 text-xs" style={{ maxHeight: 210, overflowY: "auto" }}>
            <Bub>我在。周会纪要已发布到 #platform,回滚脚本草稿在 RUN-2231 排队执行。</Bub>
            {fabAsked && <Bub fresh>你名下待审批 2 项:<br />① 我申请执行 rm -rf .cache/fixtures(越权)<br />② Agent-012 的 Release v1.3 跨团队发布</Bub>}
          </div>
          <div className="px-3 pb-2 flex gap-1.5 flex-wrap">
            {["下达任务", "查我的审批", "串流到频道"].map(q => (
              <button key={q} onClick={() => { if (q === "查我的审批") setFabAsked(true); if (q === "串流到频道") { setFab(false); setPicker(true); } }}
                className="text-[11px] px-2.5 py-1 rounded-lg" style={{ background: T.soft }}>{q}</button>
            ))}
          </div>
          <div className="m-3 mt-1 px-3 py-2 rounded-xl text-xs flex items-center" style={{ background: T.soft, color: T.faint }}>
            对小七说点什么…<span className="ml-auto">⏎</span>
          </div>
        </div>
      )}

      {/* ===== 串流选择频道 ===== */}
      {picker && (
        <div className="fixed inset-0 z-50 flex items-center justify-center" style={{ background: "rgba(23,24,28,.35)" }}>
          <div className="w-[420px] rounded-2xl overflow-hidden fade" style={{ background: "#fff", boxShadow: "0 24px 60px rgba(23,24,28,.25)" }}>
            <div className="px-5 py-3.5 flex items-center gap-2 text-[13px] font-semibold" style={{ background: T.indigoSoft, color: T.indigoDeep }}>
              <Cast size={15} /> 串流会话到频道
              <button className="ml-auto" onClick={() => setPicker(false)}><X size={14} /></button>
            </div>
            <div className="px-5 py-4">
              <div className="rounded-xl p-3" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
                <div className="text-[13px] font-bold">重构支付重试幂等性 · 与小七</div>
                <div className="flex items-center gap-2 mt-1.5 text-[10.5px] flex-wrap" style={{ color: T.faint }}>
                  进行中 · <RouteTag local /> · 外发 0 B · <Tag tone="amb">internal</Tag>
                </div>
              </div>
              <div className="text-[11px] mt-3.5 mb-2" style={{ color: T.sub }}>选择目标频道 · 密级跟着会话走</div>
              <div className="space-y-1.5">
                {[
                  { t: "平台组", c: "platform", lv: "internal", ok: true },
                  { t: "平台组", c: "code-review", lv: "internal", ok: true },
                  { t: "平台组", c: "general", lv: "open", ok: false },
                  { t: "支付组", c: "payments", lv: "internal", ok: true },
                ].map(o => (
                  <button key={o.c} disabled={!o.ok} onClick={doStream}
                    className="w-full flex items-center gap-2 px-3 py-2.5 rounded-xl text-left text-[13px]"
                    style={{ background: o.ok ? T.soft : "#FBFBFD", color: o.ok ? T.ink : T.faint, cursor: o.ok ? "pointer" : "not-allowed" }}>
                    <Hash size={13} style={{ opacity: .7 }} /> {o.c}
                    <span className="text-[10.5px]" style={{ color: T.faint }}>· {o.t}</span>
                    <span className="ml-auto flex items-center gap-1.5">
                      <Tag tone={o.lv === "open" ? undefined : "amb"}>{o.lv}</Tag>
                      {!o.ok && <span className="flex items-center gap-1 text-[10px]" style={{ color: T.red }}><Lock size={10} />低于会话密级</span>}
                    </span>
                  </button>
                ))}
              </div>
              <div className="mt-3.5 text-[10.5px] leading-relaxed" style={{ color: T.faint }}>
                串流为只读投屏:队友可围观与提问,接手需你授权。全程计入审计,本地会话外发仍为 0 B。
              </div>
            </div>
          </div>
        </div>
      )}

      {/* ===== 审批弹窗 ===== */}
      {modal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center" style={{ background: "rgba(23,24,28,.35)" }}>
          <div className="w-[392px] rounded-2xl overflow-hidden fade" style={{ background: "#fff", boxShadow: "0 24px 60px rgba(23,24,28,.25)" }}>
            <div className="px-5 py-3.5 flex items-center gap-2 text-[13px] font-semibold" style={{ background: T.indigoSoft, color: T.indigoDeep }}>
              <ShieldAlert size={15} /> 审批请求
            </div>
            <div className="p-5 space-y-3.5">
              <div className="flex items-center gap-3">
                <div className="w-11 h-11 rounded-2xl flex items-center justify-center" style={{ background: T.indigoSoft, color: T.indigo }}><Bot size={20} /></div>
                <div>
                  <div className="text-sm font-bold">Agent-007 <span className="font-normal text-[11.5px]" style={{ color: T.sub }}>· 代码评审员</span></div>
                  <div className="text-[11.5px] mt-0.5" style={{ color: T.sub }}>权限:只读仓库 / 发评论 / 跑测试</div>
                </div>
                <span className="ml-auto"><Tag tone="ind">编制 A-007</Tag></span>
              </div>
              <div className="text-[13.5px] leading-relaxed">
                申请执行 <code className="text-[11.5px] px-2 py-1 rounded-lg" style={{ background: T.redSoft, color: T.red }}>rm -rf .cache/fixtures</code>
                <div className="text-[11.5px] mt-1.5" style={{ color: T.sub }}>该操作超出其岗位权限。批准与拒绝都会写入审计。</div>
              </div>
              <div className="flex gap-2 pt-0.5">
                <IBtn onClick={() => { setModal(false); setApproved(true); }} className="px-5 py-2.5">批准执行</IBtn>
                <button onClick={() => setModal(false)} className="px-5 py-2.5 rounded-xl text-xs font-medium" style={{ background: T.soft, color: T.sub }}>拒绝</button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* ===== 对话快照弹窗 ===== */}
      {convo !== "closed" && (
        <div className="fixed inset-0 z-50 flex items-center justify-center" style={{ background: "rgba(23,24,28,.35)" }}>
          <div className="w-[480px] max-h-[80vh] rounded-2xl overflow-hidden flex flex-col fade" style={{ background: "#fff", boxShadow: "0 24px 60px rgba(23,24,28,.25)" }}>
            <div className="px-4 py-3 flex items-center gap-2" style={{ borderBottom: `1px solid ${T.line}` }}>
              <Share2 size={14} style={{ color: T.sub }} />
              <b className="text-sm">对话快照 · 支付重试幂等性讨论</b>
              <button className="ml-auto" onClick={() => setConvo("closed")} style={{ color: T.sub }}><X size={15} /></button>
            </div>
            <div className="px-4 py-2 flex items-center gap-2 text-[10.5px] flex-wrap" style={{ borderBottom: `1px solid ${T.line}`, color: T.sub }}>
              Alice ↔ 小七(A-007) · 14 条 · 07-21 · <RouteTag local /> qwen3:8b · 外发 0 B · <Tag tone="amb">internal</Tag>
            </div>
            <div className="flex-1 overflow-y-auto p-4 space-y-3 text-[12.5px]">
              <CB who="Alice">支付重试如果网络抖动重复提交,现在的幂等保障够吗?</CB>
              <CB who="小七" bot>查了 gateway/retry.rs:幂等键由业务侧各自拼接,三处实现不一致,存在重复扣款窗口。建议网关层统一生成,业务侧只透传。</CB>
              <CB who="Alice">迁移成本呢?</CB>
              <CB who="小七" bot>两个服务需要改造,预计各 0.5 天;我可以先产出兼容层 diff,灰度期双写。</CB>
              <div className="text-center text-[10px]" style={{ color: T.faint }}>—— 其余 10 条已折叠 ——</div>
            </div>
            {convo === "blocked" && (
              <div className="mx-4 mb-3 p-3 rounded-xl text-xs fade" style={{ background: T.redSoft }}>
                <div className="flex items-center gap-1.5 font-semibold" style={{ color: T.red }}><ShieldAlert size={13} /> 分享被策略阻止</div>
                <div className="mt-1 leading-relaxed" style={{ color: "#8A4A4D" }}>
                  该对话密级为 <b>internal</b>,目标频道 <b>#general</b> 为 open。<b>密级跟着对话走</b>:向低密级频道分享需发起降密审批(v1.1),或改分享到同级频道。
                </div>
              </div>
            )}
            <div className="px-4 py-3 flex gap-2 items-center" style={{ borderTop: `1px solid ${T.line}` }}>
              <button className="text-xs font-semibold px-3 py-1.5 rounded-lg" style={{ background: T.indigoSoft, color: T.indigo }}>引用到 #platform</button>
              <button onClick={() => setConvo("blocked")} className="text-xs px-3 py-1.5 rounded-lg" style={{ background: T.soft, color: T.sub }}>
                分享到 #general(open)
              </button>
              <span className="ml-auto text-[10px]" style={{ color: T.faint }}>快照只读 · 引用可溯源</span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/* ==================== 顶栏 ==================== */

function TopBar({ module, view, channel, teamName, streamed }) {
  const titles = {
    home: ["中控台", "2026年7月22日 星期三 · 全组织实时态势"],
    phome: ["我的工作台", "个人空间 · 与 Agent 的私有会话,默认不进团队"],
    agent: ["Agent 档案 · 小七", "编制 A-007 · 代码评审员 · 由我日常使用"],
    channel: [`#${channel}`, `${teamName} · 频道协作 · 共享对话与工作流`],
    roster: [`编制 · ${teamName}`, "团队内的人与 Agent · 点将、授权与审计"],
    meeting: ["会议室", "平台组周会 · Agent-007 / 021 在席"],
    caps: ["能力库", "组织的 Capsule 资产池"],
  };
  const [t, s] = titles[view];
  return (
    <div className="flex items-center px-7 pt-5 pb-1.5">
      <div>
        <div className="flex items-center gap-2">
          <span className="text-[21px] font-bold tracking-tight">{t}</span>
          <span className="text-[10px] font-semibold px-2 py-0.5 rounded-md"
            style={{ background: module === "personal" ? T.tealSoft : module === "team" ? T.indigoSoft : T.soft, color: module === "personal" ? T.teal : module === "team" ? T.indigo : T.sub }}>
            {module === "personal" ? "个人" : module === "team" ? "团队" : "组织"}
          </span>
        </div>
        <div className="text-xs mt-0.5" style={{ color: T.sub }}>{s}</div>
      </div>
      <div className="ml-auto flex items-center gap-2.5">
        {streamed && (
          <span className="flex items-center gap-1.5 text-[11px] font-semibold px-3 py-1.5 rounded-full" style={{ background: T.redSoft, color: T.red }}>
            <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />串流中 → #platform
          </span>
        )}
        <button className="w-9 h-9 rounded-full flex items-center justify-center" style={{ border: `1px solid ${T.line}`, color: "#5A5E70" }}><Search size={15} /></button>
        <button className="w-9 h-9 rounded-full flex items-center justify-center" style={{ border: `1px solid ${T.line}`, color: "#5A5E70" }}><Bell size={15} /></button>
        <div className="flex items-center gap-2 ml-1">
          <div className="w-9 h-9 rounded-full flex items-center justify-center font-bold" style={{ background: T.indigoSoft, color: T.indigo }}>A</div>
          <div><div className="text-[13px] font-semibold">Alice</div><div className="text-[10px]" style={{ color: T.sub }}>平台组 · 组长</div></div>
        </div>
      </div>
    </div>
  );
}

/* ==================== 个人工作台 ==================== */

function PersonalHome({ streamed, onStream, onStop, goAgent, goChannel, openConvo }) {
  return (
    <div className="px-7 pb-8 pt-2" style={{ display: "grid", gridTemplateColumns: "1.6fr 1fr", gap: 16 }}>
      <div className="flex flex-col gap-4">
        {/* 进行中会话 */}
        <Card className="p-5" style={streamed ? { borderColor: T.indigo, boxShadow: "0 8px 22px rgba(91,91,245,.16)" } : {}}>
          <div className="flex items-center gap-2">
            <Tag tone="teal">进行中</Tag>
            <span className="text-[11px]" style={{ color: T.sub }}>私有会话 · 未进入任何频道</span>
            {streamed && <span className="ml-auto flex items-center gap-1.5 text-[11px] font-semibold" style={{ color: T.red }}>
              <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />LIVE</span>}
          </div>
          <div className="text-[17px] font-bold mt-2">重构支付重试幂等性</div>
          <div className="flex items-center gap-2 mt-1.5 text-[11px] flex-wrap" style={{ color: T.faint }}>
            与 <b style={{ color: T.indigo }}>小七</b> · 14 条 · <RouteTag local /> qwen3:8b · 外发 0 B · <Tag tone="amb">internal</Tag>
          </div>

          <div className="mt-3.5 rounded-xl p-3.5 space-y-2.5" style={{ background: T.panel }}>
            <CB who="Alice">三处幂等键实现不一致,先给我一版收敛方案</CB>
            <CB who="小七" bot>建议网关层统一生成,业务侧只透传。兼容层 diff 已备好,灰度期双写。</CB>
          </div>

          <div className="flex items-center gap-2 mt-3.5">
            {streamed ? (
              <>
                <button onClick={onStop} className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.redSoft, color: T.red }}>
                  <StopCircle size={13} /> 停止串流
                </button>
                <button onClick={goChannel} className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.soft }}>
                  <Hash size={13} /> 去 #platform 看围观
                </button>
                <span className="ml-auto flex items-center gap-1 text-[11px]" style={{ color: T.sub }}><Eye size={12} /> 12 人围观中</span>
              </>
            ) : (
              <>
                <IBtn onClick={onStream}><Cast size={13} /> 串流到团队频道</IBtn>
                <button onClick={openConvo} className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.soft }}>
                  <Share2 size={13} /> 分享快照
                </button>
                <span className="ml-auto text-[10.5px]" style={{ color: T.faint }}>串流=实时投屏 · 快照=定格存档</span>
              </>
            )}
          </div>
        </Card>

        {/* 近期对话 */}
        <Card className="px-5 pt-4 pb-2">
          <div className="flex items-center">
            <b className="text-[15px]">近期对话</b>
            <button className="ml-auto text-[11.5px] font-semibold" style={{ color: T.indigo }}>全部</button>
          </div>
          {[
            { t: "网关限流策略推演", n: "小七 · 昨天 · 8 条", lv: "internal", local: true },
            { t: "SQLite 写放大排查", n: "小七 · 07-20 · 21 条", lv: "internal", local: true },
            { t: "英文发布说明润色", n: "小七 · 07-19 · 5 条", lv: "open", local: false },
          ].map(c => (
            <div key={c.t} className="flex items-center gap-3 py-2.5" style={{ borderTop: `1px solid ${T.line}` }}>
              <div className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0" style={{ background: T.soft, color: "#5A5E70" }}><MessageSquare size={14} /></div>
              <div className="min-w-0">
                <div className="text-[13px] font-semibold">{c.t}</div>
                <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>{c.n}</div>
              </div>
              <span className="ml-auto flex items-center gap-1.5 shrink-0">
                <RouteTag local={c.local} />
                <Tag tone={c.lv === "open" ? undefined : "amb"}>{c.lv}</Tag>
              </span>
            </div>
          ))}
        </Card>
      </div>

      <div className="flex flex-col gap-4">
        {/* 我的 Agent */}
        <Card className="p-5">
          <div className="flex items-center gap-3">
            <div className="w-12 h-12 rounded-2xl flex items-center justify-center" style={{ background: T.indigoSoft, color: T.indigo }}><Bot size={24} /></div>
            <div>
              <div className="text-[15px] font-bold">小七</div>
              <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>编制 A-007 · 代码评审员</div>
            </div>
            <span className="ml-auto"><RouteTag local /></span>
          </div>
          <div className="grid grid-cols-3 gap-2 mt-4">
            {[["43", "入职天数"], ["86", "累计 Runs"], ["0 B", "累计外发"]].map(([v, l]) => (
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

        {/* 我的任务 */}
        <Card className="px-5 pt-4 pb-2">
          <div className="flex items-center"><b className="text-[15px]">我的任务</b>
            <span className="ml-auto text-[11px] font-semibold" style={{ color: T.red }}>2 项待办</span></div>
          {[
            { t: "确认回滚脚本草稿合入", n: "RUN-2231 · 小七已产出", tone: T.red },
            { t: "网关幂等键设计文档", n: "周四评审 · 来自周会行动项", tone: T.indigo },
            { t: "季度主权演习", n: "周五 15:00 · 全员参与", tone: T.amber },
          ].map(x => (
            <div key={x.t} className="flex items-center gap-3 py-2.5" style={{ borderTop: `1px solid ${T.line}` }}>
              <span className="w-1 rounded-full" style={{ height: 30, background: x.tone }} />
              <div><div className="text-[13px] font-semibold">{x.t}</div>
                <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>{x.n}</div></div>
              <span className="ml-auto w-7 h-7 rounded-full flex items-center justify-center" style={{ background: T.soft, color: "#5A5E70" }}><ChevronRight size={14} /></span>
            </div>
          ))}
        </Card>

        {/* 个人 ↔ 团队 边界说明 */}
        <Card className="p-4" style={{ background: T.panel }}>
          <div className="flex items-center gap-1.5 text-[11px] font-semibold"><Shield size={13} style={{ color: T.indigo }} /> 个人空间边界</div>
          <div className="text-[11px] mt-1.5 leading-relaxed" style={{ color: T.sub }}>
            个人会话默认<b style={{ color: T.ink }}>不进团队</b>,不出现在任何频道与检索里。串流或分享是唯一的出口,且受密级约束、全程留痕。
          </div>
        </Card>
      </div>
    </div>
  );
}

/* ==================== Agent 档案页 ==================== */

const cellLv = (r, c) => {
  if (c === 44 && r === 2) return 4;
  if (c < 39) return (c * 7 + r * 13) % 29 === 0 ? 1 : 0;
  const v = (c * 5 + r * 3) % 9;
  return [0, 1, 1, 2, 2, 3, 3, 4, 4][v];
};

const MEMO = [
  { t: "架构偏好 · 记忆固化", d: "36 天前", s: "增量交付、约定优先、测试先行" },
  { t: "技能:change-validation-planner", d: "36 天前", s: "改动影响面推演与验证计划" },
  { t: "技能:code-review", d: "36 天前", s: "锻造自 RUN-1893 · 验真 98%" },
  { t: "Capsule:Release Checklist", d: "13 天前", s: "引入自支付组 · 密级随包迁移" },
];

function AgentProfile({ streamed, onStream }) {
  const months = ["6月","7月","8月","9月","10月","11月","12月","1月","2月","3月","4月","5月"];
  return (
    <div className="px-7 pb-8 pt-2 flex flex-col gap-4">
      {/* 档案头 */}
      <Card className="p-6 flex items-start gap-6">
        <div className="relative shrink-0" style={{ transform: "rotate(-3deg)" }}>
          <div className="w-32 h-36 rounded-2xl flex items-center justify-center" style={{ background: T.indigoSoft, border: `1px solid ${T.line}`, boxShadow: "0 10px 24px rgba(23,24,28,.12)" }}>
            <Bot size={56} style={{ color: T.indigo }} />
          </div>
          <div className="absolute left-2 bottom-2 text-[10px] font-semibold px-1.5 py-0.5 rounded" style={{ background: "#fff", color: T.sub }}>ID: a7f0-007</div>
          <div className="absolute -right-3 -bottom-3 w-9 h-9 rounded-full flex items-center justify-center" style={{ background: "#fff", border: `1px solid ${T.line}`, color: T.indigo }}>
            <BadgeCheck size={18} />
          </div>
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2.5">
            <span className="text-2xl font-extrabold">小七</span>
            <Tag tone="ind">编制 A-007 · 代码评审员</Tag>
            <button className="flex items-center gap-1 text-[11px] px-2 py-1 rounded-lg" style={{ background: T.soft, color: T.sub }}><Pencil size={11} /> 编辑</button>
          </div>
          <div className="flex items-center gap-3 mt-2 text-[11.5px]" style={{ color: T.sub }}>
            <span className="flex items-center gap-1.5"><span className="w-1.5 h-1.5 rounded-full" style={{ background: T.green }} />在线</span>
            <span>入职时间:2026年6月9日</span>
            <RouteTag local />
          </div>
          <div className="text-[12.5px] mt-2.5 leading-relaxed" style={{ color: "#454A5C" }}>
            负责代码评审与变更验证:静态审查、跑测试、产出修复 diff 与评审意见。遵循增量交付、约定优先、测试先行的工程习惯。默认只读仓库权限,越权操作一律走审批。
          </div>
          <div className="flex items-center gap-2 mt-3">
            {streamed
              ? <span className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.redSoft, color: T.red }}>
                  <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />会话串流中 · #platform</span>
              : <IBtn onClick={onStream}><Cast size={13} /> 串流当前会话</IBtn>}
            <button className="flex items-center gap-1.5 text-xs font-semibold px-3.5 py-2 rounded-xl" style={{ background: T.soft }}><MessageSquare size={13} /> 继续对话</button>
          </div>
        </div>
      </Card>

      {/* 工作记录 */}
      <Card className="p-5">
        <div className="flex items-center gap-2">
          <b className="text-[15px]">工作记录</b>
          <span className="text-[11px] px-2 py-1 rounded-lg font-medium" style={{ background: T.indigoSoft, color: T.indigo }}>时间线视图</span>
          <span className="text-[11px] px-2 py-1 rounded-lg" style={{ color: T.sub }}>对话任务</span>
          <span className="text-[11px] px-2 py-1 rounded-lg" style={{ color: T.sub }}>自动任务</span>
          <span className="ml-auto"><ChipDd>近 12 个月</ChipDd></span>
        </div>
        <div className="grid grid-cols-4 gap-3 mt-4">
          {[["43", "入职天数"], ["86", "累计 Runs"], ["4", "已掌握技能"], ["0 B", "累计外发"]].map(([v, l]) => (
            <div key={l} className="rounded-xl p-3.5" style={{ background: T.panel }}>
              <div className="text-[22px] font-extrabold">{v}</div>
              <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>{l}</div>
            </div>
          ))}
        </div>

        {/* 贡献热力图 */}
        <div className="mt-5 overflow-x-auto">
          <div className="flex gap-[3px] ml-8 mb-1.5">
            {months.map((m, i) => <div key={i} className="text-[10px] shrink-0" style={{ color: T.faint, width: 48 - 3 }}>{m}</div>)}
          </div>
          <div className="flex gap-2">
            <div className="flex flex-col gap-[3px] justify-around text-[10px] pt-0.5" style={{ color: T.faint, width: 24 }}>
              <span>周一</span><span>周三</span><span>周五</span>
            </div>
            <div className="flex flex-col gap-[3px]">
              {Array.from({ length: 7 }).map((_, r) => (
                <div key={r} className="flex gap-[3px]">
                  {Array.from({ length: 48 }).map((_, c) => (
                    <span key={c} className="rounded-[2px]" style={{ width: 9, height: 9, background: LV[cellLv(r, c)] }} />
                  ))}
                </div>
              ))}
            </div>
          </div>
          <div className="flex items-center gap-1.5 mt-3 text-[10px]" style={{ color: T.faint }}>
            少 {LV.map((c, i) => <span key={i} className="rounded-[2px]" style={{ width: 9, height: 9, background: c, display: "inline-block" }} />)} 多
            <span className="ml-3">入职初期以学习与影子重放为主,7 月起进入常态评审</span>
          </div>
        </div>
      </Card>

      {/* 记忆与积累 */}
      <Card className="p-5">
        <div className="flex items-center gap-2">
          <Brain size={15} style={{ color: T.indigo }} />
          <b className="text-[15px]">记忆与积累</b>
          <button className="text-[11.5px] font-semibold flex items-center gap-1" style={{ color: T.indigo }}>查看完整记忆 <ChevronRight size={12} /></button>
          <span className="ml-auto text-[11px]" style={{ color: T.sub }}>全部本地存储 · 可导出、可清除</span>
        </div>
        <div className="relative mt-6 mb-2" style={{ height: 168 }}>
          <div className="absolute left-0 right-0" style={{ top: 84, height: 1, background: T.line }} />
          <div className="flex justify-between relative">
            {MEMO.map((m, i) => {
              const up = i % 2 === 1;
              return (
                <div key={m.t} className="flex-1 flex flex-col items-center">
                  {up && (
                    <div className="text-center mb-2" style={{ height: 72 }}>
                      <div className="text-[10.5px]" style={{ color: T.faint }}>{m.d}</div>
                      <div className="text-[12.5px] font-semibold mt-0.5">{m.t}</div>
                      <div className="text-[10.5px] mt-0.5" style={{ color: T.sub }}>{m.s}</div>
                    </div>
                  )}
                  {!up && <div style={{ height: 72 }} />}
                  <div className="w-9 h-9 rounded-full flex items-center justify-center shrink-0" style={{ background: T.indigoSoft, border: `2px solid #fff`, color: T.indigo }}>
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
/* ==================== 首页 · 中控台 ==================== */

function HomeView({ approved, onApprove }) {
  const arc = (r, fr, col, rot = -90) => {
    const C = 2 * Math.PI * r;
    return (
      <g key={`${r}-${col}`}>
        <circle cx="70" cy="70" r={r} fill="none" stroke="#F1F2F7" strokeWidth="9" />
        <circle cx="70" cy="70" r={r} fill="none" stroke={col} strokeWidth="9" strokeLinecap="round"
          strokeDasharray={`${(C * fr).toFixed(1)} ${C.toFixed(1)}`} transform={`rotate(${rot} 70 70)`} />
      </g>
    );
  };
  return (
    <div className="px-7 pb-8 pt-2" style={{ display: "grid", gridTemplateColumns: "1.55fr 1fr", gap: 16 }}>
      <div className="flex flex-col gap-4">
        <div className="grid grid-cols-2 gap-4">
          <Kpi hero icon={<Zap size={16} />} pct={<Pct hero>+12,4%</Pct>} label="本周任务(Runs)" val="128" cap="较上周 · 全组织" />
          <Kpi icon={<Shield size={16} />} pct={approved ? <Pct up>已清零</Pct> : <Pct>+1 紧急</Pct>} label="待我审批" val={approved ? "1" : "2"} cap="Agent 越权与发布申请" />
          <Kpi icon={<Cloud size={16} />} pct={<Pct up>-32%</Pct>} label="云端外发流量" val="1.8 GB" cap="较上周 · 越少越好" />
          <Kpi icon={<BadgeCheck size={16} />} pct={<Pct up>100%</Pct>} label="季度演习达标" val="1 / 1" cap="演习窗口外发 0 B" />
        </div>

        <Card className="p-5">
          <div className="flex items-center">
            <div><b className="text-[15px]">任务吞吐</b><div className="text-[11.5px] mt-0.5" style={{ color: T.sub }}>近 7 日 · 本地 vs 云端执行</div></div>
            <div className="ml-auto flex items-center gap-3">
              <span className="flex gap-3 text-[11px]" style={{ color: T.sub }}>
                <span><i className="inline-block w-2 h-2 rounded-full mr-1.5" style={{ background: T.barGray }} />云端</span>
                <span><i className="inline-block w-2 h-2 rounded-full mr-1.5" style={{ background: T.indigo }} />本地</span>
              </span>
              <ChipDd>本周</ChipDd>
            </div>
          </div>
          <div className="flex items-end mt-4" style={{ height: 150 }}>
            {BARS.days.map((d, i) => (
              <div key={d} className="flex-1">
                <div className="relative flex items-end justify-center gap-1" style={{ height: 130 }}>
                  {i === BARS.tipAt && (
                    <>
                      <div className="absolute text-[11px] text-white rounded-xl px-3 py-2 whitespace-nowrap"
                        style={{ bottom: "calc(100% + 10px)", left: "50%", transform: "translateX(-50%)", background: T.black, boxShadow: "0 8px 20px rgba(23,24,28,.25)" }}>
                        <div><span className="inline-block w-1.5 h-1.5 rounded-full mr-1.5" style={{ background: "#9DA1B5" }} />{BARS.cloud[i]} 次 · 云端</div>
                        <div className="mt-0.5"><span className="inline-block w-1.5 h-1.5 rounded-full mr-1.5" style={{ background: T.indigo }} />{BARS.local[i]} 次 · 本地</div>
                        <div className="absolute left-1/2 top-full" style={{ transform: "translateX(-50%)", border: "6px solid transparent", borderTopColor: T.black }} />
                      </div>
                      <div className="absolute w-2 h-2 rounded-full" style={{ top: -14, left: "50%", transform: "translateX(-50%)", background: T.black, border: "2px solid #fff" }} />
                    </>
                  )}
                  <div className="rounded-t-lg" style={{ width: 13, height: BARS.cloud[i] * 2.4, background: T.barGray, borderRadius: "7px 7px 4px 4px" }} />
                  <div className="rounded-t-lg" style={{ width: 13, height: BARS.local[i] * 2.4, background: T.indigo, borderRadius: "7px 7px 4px 4px" }} />
                </div>
                <div className="text-center text-[10.5px] mt-2" style={{ color: T.faint }}>周{d}</div>
              </div>
            ))}
          </div>
        </Card>

        <Card className="px-5 pt-4 pb-2">
          <div className="flex items-center">
            <b className="text-[15px]">待办事项</b><span className="text-[11px] font-semibold ml-2" style={{ color: T.red }}>3 项</span>
            <button className="ml-auto text-[11.5px] font-semibold" style={{ color: T.indigo }}>查看全部</button>
          </div>
          <TodoRow tone={T.red} t="确认回滚脚本草稿合入" n="RUN-2231 已由 Agent-007 产出 · 今天" />
          <TodoRow tone={T.indigo} t="平台组周会纪要确认" n="Agent-021 已生成 · 3 决定 2 行动项" />
          <TodoRow tone={T.amber} t="季度主权演习" n="周五 15:00 · 断外联 30 分钟" />
        </Card>
      </div>

      <div className="flex flex-col gap-4">
        <Card className="p-5">
          <div className="flex items-center">
            <div><b className="text-[15px]">路由统计</b><div className="text-[11.5px] mt-0.5" style={{ color: T.sub }}>模型调用去向</div></div>
            <span className="ml-auto"><ChipDd>今日</ChipDd></span>
          </div>
          <div className="flex items-center gap-4 mt-3">
            <svg width="140" height="140" viewBox="0 0 140 140">
              {arc(58, .68, T.indigo)}
              {arc(44, .27, "#C9CBDA", 30)}
              {arc(30, .06, T.red, -40)}
            </svg>
            <div>
              <div className="text-2xl font-extrabold">342</div>
              <div className="text-[11px] mt-0.5" style={{ color: T.sub }}>今日调用次数</div>
              <div className="mt-1.5"><Pct up>+5,3%</Pct></div>
            </div>
          </div>
          <div className="mt-2">
            <LegRow icon={<HardDrive size={14} />} l="本地" v="233" p={<Pct up>+1,8%</Pct>} />
            <LegRow icon={<Cloud size={14} />} l="云端" v="92" p={<Pct up>+2,3%</Pct>} />
            <LegRow icon={<AlertTriangle size={14} />} l="降级落地" v="17" p={<Pct>-1,0%</Pct>} />
          </div>
        </Card>

        <Card className="px-5 pt-4 pb-2">
          <div className="flex items-center"><b className="text-[15px]">审批监控</b><span className="ml-auto"><ChipDd>实时</ChipDd></span></div>
          <ApRow bg={T.indigo} nm="Agent-007" sb="rm -rf .cache/fixtures · 越权"
            right={approved ? <Pct up>已批准</Pct> :
              <button onClick={onApprove} className="text-[11px] font-semibold px-3 py-1.5 rounded-full" style={{ background: T.indigo, color: "#fff" }}>立即审批</button>} />
          <ApRow bg="#8A8DF0" nm="Agent-012" sb="Release v1.3 · 跨团队发布"
            right={<button className="text-[11px] font-semibold px-3 py-1.5 rounded-full" style={{ background: T.indigoSoft, color: T.indigo }}>审批</button>} />
          <ApRow bg="#9DA1B5" icon={<Shield size={15} />} nm="路由中心" sb="demo-repo → restricted · 已降落本地"
            right={<Tag tone="ind">通知</Tag>} />
        </Card>
      </div>
    </div>
  );
}

const Kpi = ({ hero, icon, pct, label, val, cap }) => (
  <div className="rounded-2xl p-4" style={hero
    ? { background: T.indigo, color: "#fff", boxShadow: "0 10px 24px rgba(91,91,245,.3)" }
    : { background: "#fff", border: `1px solid ${T.line}` }}>
    <div className="flex items-center">
      <div className="w-9 h-9 rounded-xl flex items-center justify-center" style={{ background: hero ? "#fff" : T.soft, color: hero ? T.indigo : T.ink }}>{icon}</div>
      <span className="ml-auto">{pct}</span>
    </div>
    <div className="text-xs mt-3.5" style={{ color: hero ? "#DCDCFE" : T.sub }}>{label}</div>
    <div className="text-[26px] font-extrabold mt-0.5 tracking-tight">{val}</div>
    <div className="text-[10.5px] mt-0.5" style={{ color: hero ? "#BDBDF9" : T.faint }}>{cap}</div>
  </div>
);
const TodoRow = ({ tone, t, n }) => (
  <div className="flex items-center gap-3 py-2.5" style={{ borderTop: `1px solid ${T.line}` }}>
    <span className="w-1 rounded-full" style={{ height: 34, background: tone }} />
    <div><div className="text-[13px] font-semibold">{t}</div><div className="text-[11px] mt-0.5" style={{ color: T.sub }}>{n}</div></div>
    <span className="ml-auto w-7 h-7 rounded-full flex items-center justify-center" style={{ background: T.soft, color: "#5A5E70" }}><ChevronRight size={14} /></span>
  </div>
);
const LegRow = ({ icon, l, v, p }) => (
  <div className="flex items-center gap-2.5 py-2" style={{ borderTop: `1px solid ${T.line}` }}>
    <div className="w-7 h-7 rounded-lg flex items-center justify-center" style={{ background: T.soft, color: "#5A5E70" }}>{icon}</div>
    <span className="text-[12.5px] font-medium">{l}</span>
    <span className="ml-auto text-[13px] font-bold">{v}</span>{p}
  </div>
);
const ApRow = ({ bg, icon, nm, sb, right }) => (
  <div className="flex items-center gap-2.5 py-2.5" style={{ borderTop: `1px solid ${T.line}` }}>
    <div className="w-8 h-8 rounded-full flex items-center justify-center shrink-0" style={{ background: bg, color: "#fff" }}>{icon || <Bot size={15} />}</div>
    <div className="min-w-0"><div className="text-[13px] font-semibold">{nm}</div>
      <div className="text-[11px] truncate" style={{ color: T.sub, maxWidth: 150 }}>{sb}</div></div>
    <span className="ml-auto shrink-0">{right}</span>
  </div>
);

/* ==================== 频道视图(vision 功能 · v2 皮肤) ==================== */

function ChannelView({ channel, introduced, setIntroduced, openConvo, goMeeting, streamed }) {
  if (channel !== "platform") {
    return (
      <div className="px-7 pt-2 pb-8">
        <Card className="p-5 max-w-xl">
          <div className="flex items-center gap-2 text-sm"><Hash size={15} style={{ color: T.sub }} /><b>{channel}</b></div>
          <div className="text-xs mt-2 leading-relaxed" style={{ color: T.sub }}>
            此频道为占位内容。完整剧本(共享对话 / 跨团队 Capsule / 会议纪要卡)在 <b style={{ color: T.indigo }}>平台组 → #platform</b>。
          </div>
        </Card>
      </div>
    );
  }
  return (
    <div className="px-7 pt-1 pb-6 flex gap-4" style={{ height: "calc(100% - 78px)" }}>
      <div className="flex-1 min-w-0 flex flex-col">
        <div className="flex items-center gap-2 pb-2">
          <button onClick={goMeeting} className="inline-flex items-center gap-1.5 text-[11.5px] font-semibold px-3 py-1.5 rounded-full"
            style={{ background: T.greenSoft, color: T.green }}>
            <Radio size={11} className="wv" /> 语音房间 · 3 人 + 007 · 加入
          </button>
          <span className="ml-auto"><Tag tone="amb">频道密级 internal</Tag></span>
        </div>

        <Card className="flex-1 min-h-0 flex flex-col overflow-hidden">
          <div className="flex-1 overflow-y-auto p-5 space-y-4">
            <Msg who="Bob" tone={T.teal} time="09:58">周会 10 点,老地方。007 也拉进来,让它把 Release Checklist 的事当面说清楚。</Msg>

            {/* 个人会话串流卡 */}
            {streamed && (
              <div className="ml-10 max-w-xl rounded-2xl overflow-hidden fade" style={{ background: T.indigoSoft, border: `1px solid ${T.indigo}` }}>
                <div className="p-4">
                  <div className="flex items-center gap-1.5 text-[11px] font-semibold" style={{ color: T.indigo }}>
                    <Cast size={12} /> 个人会话串流
                    <span className="flex items-center gap-1 ml-1 px-1.5 py-0.5 rounded-md text-[10px]" style={{ background: T.red, color: "#fff" }}>
                      <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: "#fff" }} />LIVE
                    </span>
                  </div>
                  <div className="text-sm font-bold mt-1.5">Alice 正在串流 · 重构支付重试幂等性</div>
                  <div className="flex items-center gap-2 mt-1.5 text-[10.5px] flex-wrap" style={{ color: T.sub }}>
                    与小七(A-007) · <RouteTag local /> · 外发 0 B · <Tag tone="amb">internal</Tag>
                  </div>
                  <div className="mt-2.5 rounded-xl px-3 py-2 text-[11.5px] leading-relaxed" style={{ background: "#fff", color: "#454A5C" }}>
                    <b style={{ color: T.indigo }}>小七:</b> 兼容层 diff 已备好,灰度期双写…
                    <span className="ml-1.5 inline-flex gap-0.5 align-middle">
                      {[0, 1, 2].map(i => <span key={i} className="wv rounded-full" style={{ width: 3, height: 9, background: T.indigo, animationDelay: `${i * .14}s` }} />)}
                    </span>
                  </div>
                </div>
                <div className="flex items-center gap-2 px-4 py-2.5" style={{ borderTop: `1px solid rgba(91,91,245,.25)` }}>
                  <button className="flex items-center gap-1.5 text-xs font-semibold px-3 py-1.5 rounded-lg" style={{ background: T.indigo, color: "#fff" }}>
                    <Eye size={12} /> 加入围观
                  </button>
                  <span className="text-[10.5px]" style={{ color: T.sub }}>12 人围观中 · 只读投屏,接手需 Alice 授权</span>
                </div>
              </div>
            )}

            {/* 共享对话卡 */}
            <div className="ml-10 max-w-xl rounded-2xl p-4 fade" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
              <div className="flex items-center gap-1.5 text-[11px]" style={{ color: T.sub }}>
                <Share2 size={12} /> Alice 分享了一段与 Agent-007 的对话
              </div>
              <div className="text-sm font-bold mt-1.5">支付重试幂等性讨论</div>
              <div className="flex items-center gap-2 mt-1.5 text-[10.5px] flex-wrap" style={{ color: T.faint }}>
                14 条消息 · <RouteTag local /> · 外发 0 B · <Tag tone="amb">internal</Tag>
              </div>
              <div className="flex items-center gap-2 mt-3">
                <button onClick={openConvo} className="text-xs font-semibold px-3 py-1.5 rounded-lg" style={{ background: "#fff", border: `1px solid ${T.line}` }}>展开对话</button>
                <span className="text-[10px]" style={{ color: T.faint }}>对话可整段引用,来源可溯源</span>
              </div>
            </div>

            {/* 跨团队 Capsule 卡 */}
            <div className="ml-10 max-w-xl rounded-2xl overflow-hidden fade" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
              <div className="p-4">
                <div className="flex items-center gap-2">
                  <span className="text-[10px] font-extrabold tracking-widest" style={{ color: T.indigo }}>CAPSULE</span>
                  <Tag>来自 支付组</Tag>
                </div>
                <div className="font-bold mt-1.5">Release Checklist <span className="text-[11px] font-normal" style={{ color: T.sub }}>v1.2</span></div>
                <div className="text-xs mt-1" style={{ color: T.sub }}>发布前检查 → 依赖冻结 → 回滚脚本 → 灰度放量建议</div>
                <div className="flex items-center gap-2.5 mt-2.5 text-[10.5px] flex-wrap" style={{ color: T.faint }}>
                  <span className="flex items-center gap-1 font-semibold" style={{ color: T.green }}><BadgeCheck size={12} />验真 96% · 32 次影子重放</span>
                  · 支付组已使用 41 次 · <Tag tone="amb">internal</Tag>
                </div>
              </div>
              <div className="flex items-center gap-2 px-4 py-3" style={{ borderTop: `1px solid ${T.line}` }}>
                {introduced ? (
                  <span className="flex items-center gap-1.5 text-xs font-semibold fade" style={{ color: T.green }}>
                    <Check size={13} /> 已引入平台组能力库 · 密级与验真记录随包迁移
                  </span>
                ) : (
                  <IBtn onClick={() => setIntroduced(true)}><Plus size={12} /> 引入到平台组能力库</IBtn>
                )}
                <button className="ml-auto flex items-center gap-1 text-xs px-3 py-1.5 rounded-lg" style={{ background: "#fff", border: `1px solid ${T.line}`, color: T.sub }}>
                  <Play size={11} /> 直接运行
                </button>
              </div>
            </div>

            {/* 007 会议纪要卡 */}
            <div className="flex gap-2.5 max-w-2xl fade">
              <div className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0" style={{ background: T.indigoSoft, color: T.indigo }}><Bot size={16} /></div>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="text-[13px] font-bold">Agent-007</span>
                  <Tag tone="ind">编制 A-007</Tag>
                  <RouteTag local />
                  <span className="text-[10px]" style={{ color: T.faint }}>10:32</span>
                </div>
                <div className="mt-1.5 rounded-2xl px-4 py-3" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
                  <div className="flex items-center gap-1.5 text-xs font-semibold"><Sparkles size={12} style={{ color: T.indigo }} />平台组周会 · 纪要(本地转写)</div>
                  <div className="text-xs mt-2 space-y-1.5" style={{ color: T.sub }}>
                    <div>· 决定:幂等键收敛到网关层统一生成</div>
                    <div>· 决定:回滚脚本纳入 Release Checklist 强制项</div>
                    <div className="flex items-center gap-1 flex-wrap">· 行动:<b style={{ color: T.indigo }}>Agent-007</b> 会后执行 Release Checklist
                      <span className="text-[10px] font-semibold" style={{ color: T.green }}>已排队 →</span></div>
                  </div>
                </div>
              </div>
            </div>
          </div>
          <div className="px-4 pb-4">
            <div className="flex items-center gap-2 px-4 py-2.5 rounded-xl text-[13px]" style={{ background: T.soft, color: T.faint }}>
              给 #platform 发消息…可 @Agent-007,可 /capsule 分享工作流
              <span className="ml-auto text-[11px]">⏎</span>
            </div>
          </div>
        </Card>
      </div>

      {/* 频道资产栏 */}
      <div className="w-72 shrink-0 flex flex-col gap-3 overflow-y-auto">
        <Card className="p-4">
          <div className="text-[10px] font-semibold tracking-widest mb-2.5" style={{ color: T.faint }}>本频道 · 置顶能力</div>
          <PinCap name="Code Review" ver="v2.0" rate={98} />
          {introduced && <PinCap name="Release Checklist" ver="v1.2" rate={96} from="支付组" fresh />}
        </Card>
        <Card className="p-4">
          <div className="text-[10px] font-semibold tracking-widest mb-2.5" style={{ color: T.faint }}>最近共享</div>
          <div className="space-y-2 text-xs" style={{ color: T.sub }}>
            <div className="flex items-center gap-2"><Share2 size={12} /> 对话 · 支付重试幂等性讨论</div>
            <div className="flex items-center gap-2"><FileText size={12} /> 纪要 · 平台组周会 07-22</div>
            <div className="flex items-center gap-2"><GitBranch size={12} /> Capsule · Release Checklist v1.2</div>
          </div>
        </Card>
        <Card className="p-4">
          <div className="text-[10px] font-semibold tracking-widest mb-2" style={{ color: T.faint }}>频道密级</div>
          <div className="flex items-center gap-2 text-xs" style={{ color: T.sub }}>
            <Tag tone="amb">internal</Tag> 分享入本频道的内容不得低配此级
          </div>
        </Card>
      </div>
    </div>
  );
}

const Msg = ({ who, tone, time, children }) => (
  <div className="flex gap-2.5 max-w-2xl">
    <div className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0 text-xs font-bold" style={{ background: `${tone}18`, color: tone }}>{who[0]}</div>
    <div className="min-w-0">
      <div className="flex items-baseline gap-2"><span className="text-[13px] font-bold">{who}</span><span className="text-[10px]" style={{ color: T.faint }}>{time}</span></div>
      <div className="text-[13px] mt-0.5 leading-relaxed" style={{ color: "#454A5C" }}>{children}</div>
    </div>
  </div>
);
const PinCap = ({ name, ver, rate, from, fresh }) => (
  <div className={`rounded-xl p-3 mb-2 ${fresh ? "fade" : ""}`} style={{ background: T.panel, border: `1px solid ${T.line}` }}>
    <div className="flex items-center gap-1.5">
      <span className="text-[9px] font-extrabold tracking-widest" style={{ color: T.indigo }}>CAPSULE</span>
      {from && <Tag>来自{from}</Tag>}
    </div>
    <div className="text-[13px] font-bold mt-1">{name} <span className="text-[10px] font-normal" style={{ color: T.sub }}>{ver}</span></div>
    <div className="flex items-center mt-2">
      <span className="text-[10px] font-semibold flex items-center gap-1" style={{ color: T.green }}><BadgeCheck size={10} />验真 {rate}%</span>
      <button className="ml-auto text-[11px] font-semibold px-2.5 py-1 rounded-lg flex items-center gap-1" style={{ background: T.indigoSoft, color: T.indigo }}>
        <Play size={10} /> 运行
      </button>
    </div>
  </div>
);

/* ==================== 编制管理 ==================== */

const ROSTER = [
  { kind: "agent", feat: true, team: "platform", name: "Agent-007", grade: "编制 A-007", role: "代码评审员",
    tags: ["只读仓库","可跑测试","可发评论"],
    tiles: [{ i: "play", l: "执行中", v: "RUN-2231" }, { i: "shield", l: "待审批", v: "1 项", hot: true }, { i: "hdd", l: "当前路由", v: "本地" }],
    foot: "最近运行 10:32 · 周会纪要已发布" },
  { kind: "human", team: "platform", name: "Alice", grade: "组长", init: "A", tone: T.indigo,
    tags: ["平台组","审批人","架构评审"],
    tiles: [{ i: "trend", l: "在办任务", v: "2" }, { i: "shield", l: "本周审批", v: "5" }, { i: "video", l: "今日会议", v: "1 场" }],
    foot: "最近活跃 10:32 · 周会中" },
  { kind: "human", team: "platform", name: "Bob", grade: "工程师", init: "B", tone: T.teal,
    tags: ["平台组","网关方向","值周"],
    tiles: [{ i: "trend", l: "在办任务", v: "3" }, { i: "shield", l: "本周审批", v: "2" }, { i: "video", l: "今日会议", v: "1 场" }],
    foot: "最近活跃 10:29" },
  { kind: "agent", team: "platform", name: "Agent-021", grade: "编制 A-021", role: "会议书记员",
    tags: ["实时转写","纪要发布","行动项跟踪"],
    tiles: [{ i: "play", l: "执行中", v: "转写中" }, { i: "shield", l: "待审批", v: "0" }, { i: "hdd", l: "当前路由", v: "本地" }],
    foot: "最近运行 进行中 · 平台组周会" },
  { kind: "human", team: "pay", name: "Carol", grade: "工程师", init: "C", tone: T.amber,
    tags: ["支付组","发布负责人"],
    tiles: [{ i: "trend", l: "在办任务", v: "1" }, { i: "shield", l: "本周审批", v: "0" }, { i: "video", l: "今日会议", v: "1 场" }],
    foot: "最近活跃 10:31" },
  { kind: "agent", team: "pay", name: "Agent-012", grade: "编制 A-012", role: "发布管理员",
    tags: ["Release Checklist","灰度放量","回滚"],
    tiles: [{ i: "play", l: "执行中", v: "—" }, { i: "shield", l: "待审批", v: "1 项", hot2: true }, { i: "cloud", l: "当前路由", v: "云端" }],
    foot: "最近运行 昨日 18:04" },
  { kind: "human", team: "sec", name: "林小安", grade: "安全员", init: "林", tone: T.red,
    tags: ["安全组","脱敏策略","审计对接"],
    tiles: [{ i: "trend", l: "在办任务", v: "1" }, { i: "shield", l: "本周审批", v: "1" }, { i: "video", l: "今日会议", v: "0 场" }],
    foot: "最近活跃 09:12" },
  { kind: "agent", team: "sec", name: "Agent-033", grade: "编制 A-033", role: "脱敏巡检员",
    tags: ["仅本地","敏感字段扫描","整改清单"],
    tiles: [{ i: "play", l: "执行中", v: "每日巡检" }, { i: "shield", l: "待审批", v: "0" }, { i: "hdd", l: "当前路由", v: "本地" }],
    foot: "最近运行 06:00 · 定时巡检" },
];
const TICON = { play: Play, shield: Shield, hdd: HardDrive, cloud: Cloud, trend: TrendingUp, video: Video };

function RosterView({ approved, filter, setFilter, team }) {
  const inTeam = ROSTER.filter(r => r.team === team);
  const list = inTeam.filter(r =>
    filter === "全部" ? true : filter === "人类" ? r.kind === "human" : filter === "Agent" ? r.kind === "agent"
      : r.tiles.some(t => (t.hot && !approved) || t.hot2));
  const humans = inTeam.filter(r => r.kind === "human").length;
  const agents = inTeam.filter(r => r.kind === "agent").length;
  return (
    <div className="px-7 pb-8 pt-2">
      <div className="flex items-center gap-2.5 mb-4">
        <div className="flex items-center gap-2 flex-1 px-3.5 py-2 rounded-xl text-[13px]" style={{ background: T.soft, color: T.faint, maxWidth: 320 }}>
          <Search size={14} /> 搜索本团队成员或 Agent…
        </div>
        <span className="text-[11px]" style={{ color: T.sub }}>{humans} 人 · {agents} AI · 共 {inTeam.length} 编制</span>
        <div className="ml-auto flex items-center gap-2">
          {["全部", "人类", "Agent", "待审批"].map(f => (
            <button key={f} onClick={() => setFilter(f)} className="text-xs px-3.5 py-2 rounded-xl font-medium"
              style={{ background: filter === f ? T.indigo : T.soft, color: filter === f ? "#fff" : T.sub, fontWeight: filter === f ? 600 : 500 }}>{f}</button>
          ))}
          <IBtn><Plus size={13} /> 新增编制</IBtn>
        </div>
      </div>
      {list.length === 0 ? (
        <Card className="p-6 text-center text-xs" style={{ color: T.sub }}>本团队当前筛选下没有编制条目</Card>
      ) : (
        <div className="grid grid-cols-3 gap-4">
          {list.map(p => <PersonCard key={p.name} p={p} approved={approved} />)}
        </div>
      )}
    </div>
  );
}

function PersonCard({ p, approved }) {
  const feat = p.feat, isA = p.kind === "agent";
  return (
    <div className="rounded-2xl p-4.5 flex flex-col" style={feat
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
        {p.tags.map(t => <Tag key={t} style={feat ? { background: "rgba(255,255,255,.13)", color: "#DCDCFE" } : {}}>{t}</Tag>)}
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
          style={feat ? { background: "#fff", color: T.indigo } : { background: T.indigoSoft, color: T.indigo }}>详情</button>
      </div>
    </div>
  );
}

/* ==================== 会议室 ==================== */

function MeetingView() {
  const [sec, setSec] = useState(24 * 60 + 31);
  const [ci, setCi] = useState(0);
  const [asked, setAsked] = useState(false);
  const [mic, setMic] = useState(true);
  useEffect(() => {
    const a = setInterval(() => setSec(s => s + 1), 1000);
    const b = setInterval(() => setCi(i => (i + 1) % CAPTIONS.length), 3000);
    return () => { clearInterval(a); clearInterval(b); };
  }, []);
  const mm = String(Math.floor(sec / 60)).padStart(2, "0"), ss = String(sec % 60).padStart(2, "0");
  const cap = asked && ci === 0 ? ["Agent-007", "结论复述:幂等键由网关层统一生成;回滚脚本由我会后产出,今天内给草稿。"] : CAPTIONS[ci];

  return (
    <div className="px-7 pb-6 pt-1 flex gap-4" style={{ height: "calc(100% - 78px)" }}>
      <div className="flex-1 min-w-0 flex flex-col gap-3.5">
        <div className="flex items-center gap-2.5">
          <span className="text-base font-bold">平台组周会</span>
          <span className="text-[12.5px] font-bold px-2.5 py-1 rounded-full" style={{ background: T.greenSoft, color: T.green }}>{mm}:{ss}</span>
          <Tag tone="red">● 录制中 · 本地存储</Tag>
          <span className="ml-auto flex items-center gap-1.5 text-[11.5px]" style={{ color: T.sub }}>实时转写 <RouteTag local /></span>
        </div>
        <div className="grid grid-cols-2 gap-3.5 flex-1 min-h-0">
          <Seat n="Alice" tone={T.indigo} speaking />
          <Seat n="Bob" tone={T.teal} />
          <Seat n="Carol" tone={T.amber} muted />
          <div className="relative rounded-2xl flex flex-col items-center justify-center gap-2.5" style={{ background: T.indigoSoft, border: `2px solid ${T.indigo}` }}>
            <div className="w-14 h-14 rounded-2xl flex items-center justify-center" style={{ background: "#fff", color: T.indigo }}><Bot size={26} /></div>
            <div className="flex items-end gap-1" style={{ height: 18 }}>
              {[0, 1, 2, 3, 4].map(i => (
                <span key={i} className="wv rounded-full" style={{ width: 4, height: 16, background: T.indigo, animationDelay: `${i * .12}s` }} />
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
          <Ctl on={mic} onClick={() => setMic(m => !m)}>{mic ? <Mic size={16} /> : <MicOff size={16} />}</Ctl>
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
const Seat = ({ n, tone, speaking, muted }) => (
  <div className="relative rounded-2xl flex items-center justify-center" style={{ background: T.panel, border: `2px solid ${speaking ? T.indigo : "transparent"}` }}>
    <div className="w-14 h-14 rounded-2xl flex items-center justify-center text-[22px] font-bold" style={{ background: `${tone}18`, color: tone }}>{n[0]}</div>
    <div className="absolute left-3 bottom-2.5 text-[13px] font-bold">{n}{speaking && <span style={{ color: T.indigo }}> · 发言中</span>}</div>
    {muted && <span className="absolute right-3 top-3" style={{ color: T.faint }}><MicOff size={13} /></span>}
  </div>
);
const Ctl = ({ children, on, onClick }) => (
  <button onClick={onClick} className="w-10 h-10 rounded-full flex items-center justify-center"
    style={{ background: on ? T.soft : T.redSoft, color: on ? T.ink : T.red }}>{children}</button>
);
const Note = ({ k, who, ai, fresh, children }) => (
  <div className={`py-2.5 text-xs ${fresh ? "fade" : ""}`} style={{ borderTop: `1px solid ${T.line}` }}>
    <div className="flex items-center">
      <Tag tone={k === "行动" ? "ind" : undefined}>{k}</Tag>
      {who && <span className="ml-auto text-[10.5px] font-semibold" style={{ color: ai ? T.indigo : T.sub }}>{who}</span>}
    </div>
    <div className="mt-1.5 leading-relaxed" style={{ color: "#454A5C" }}>{children}</div>
    {ai && <div className="flex items-center gap-1 text-[10px] font-semibold mt-1.5" style={{ color: T.green }}><Check size={10} /> 已转任务 RUN-2231 · 本地执行</div>}
  </div>
);

/* ==================== 能力库 ==================== */

const CAPS = [
  { name: "Release Checklist", ver: "v1.2", team: "支付组", tags: ["发布检查","回滚脚本","灰度建议"], rate: 96, runs: 32, used: 41, scope: "跨团队", hot: true },
  { name: "Code Review", ver: "v2.0", team: "平台组", tags: ["静态审查","跑测试","修复 diff"], rate: 98, runs: 57, used: 126, scope: "全组织" },
  { name: "数据脱敏巡检", ver: "v1.0", team: "安全组", tags: ["敏感字段","整改清单"], rate: 100, runs: 18, used: 12, scope: "团队内", restricted: true },
  { name: "周报汇总", ver: "v0.9", team: "平台组", tags: ["纪要聚合","周报草稿"], rate: 61, runs: 9, used: 3, scope: "验真中", verifying: true },
];

function CapsView({ trace, setTrace, introduced }) {
  return (
    <div className="px-7 pb-8 pt-2">
      <div className="flex items-center mb-4">
        <div className="flex items-center gap-2 flex-1 px-3.5 py-2 rounded-xl text-[13px]" style={{ background: T.soft, color: T.faint, maxWidth: 360 }}>
          <Search size={14} /> 搜索能力…
        </div>
        <span className="ml-auto"><IBtn><Plus size={13} /> 从运行锻造</IBtn></span>
      </div>
      <div className="grid grid-cols-2 gap-4">
        {CAPS.map(c => (
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
            <div className="mt-2.5 text-[15px]"><b>{c.name}</b> <span className="text-[11.5px]" style={{ color: T.sub }}>{c.ver}</span></div>
            <div className="flex gap-1.5 mt-2 flex-wrap">{c.tags.map(t => <Tag key={t}>{t}</Tag>)}</div>
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
              <button onClick={() => c.hot && setTrace(t => !t)} className="flex items-center gap-1.5 text-xs px-3.5 py-2 rounded-xl" style={{ background: T.soft }}>
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
const TStep = ({ t, d, tone, first, last }) => (
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
