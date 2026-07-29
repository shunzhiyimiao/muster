/* Muster 点将台 · v4 壳(双层侧栏 + 个人/团队分离)
   概念稿:docs/Muster-概念稿-v4.html;真实后端接线处均标注。 */
import { useEffect, useMemo, useState } from "react";
import {
  Bell, Bot, Brain, BookOpen, Calendar, Cast, ChevronDown, Clock, Hash, Home,
  LayoutDashboard, Library, LineChart, Link2, Lock, MessageSquare, Network, Puzzle, Search,
  Settings, Shield, ShieldAlert, Sparkles, StopCircle, Terminal, User, Users, Video, X,
} from "lucide-react";
import { T } from "./theme";
import {
  api, AgentStats, AuditRow, Bootstrap, ChainStatus, Channel, DrillReportOut, HomeStats,
  fmtBytes,
} from "./api";
import { useChat, ChatPane } from "./chat";
import { Bub, Card, CB, CollapseSec, IBtn, RouteTag, SideItem, SideSec, Tag } from "./ui";
import { ConsoleHome, AuditCenter } from "./views/Console";
import { PersonalHome, AgentProfile } from "./views/Personal";
import { ChannelView, RosterView, MeetingView, CapsView } from "./views/Team";
import { TEAM_META } from "./data";

const RAIL = [
  { id: "console", icon: LayoutDashboard, label: "控制台", ok: true },
  { id: "personal", icon: User, label: "工作", ok: true },
  { id: "team", icon: Users, label: "团队", ok: true },
  { id: "setting", icon: Settings, label: "设置", ok: false },
  { id: "ext", icon: Puzzle, label: "模块扩展", ok: false },
  { id: "plan", icon: Calendar, label: "计划表", ok: false },
  { id: "term", icon: Terminal, label: "终端", ok: false },
  { id: "remote", icon: Network, label: "远程连接", ok: false },
];

const TEAM_ORDER = ["platform", "pay", "sec"];

export default function App() {
  /* ---- 概念稿交互态 ---- */
  const [module, setModule] = useState("personal");
  const [view, setView] = useState("phome");
  const [team, setTeam] = useState("platform");
  const [channelId, setChannelId] = useState("platform");
  const [notice, setNotice] = useState("");
  const [approved, setApproved] = useState(false);
  const [modal, setModal] = useState(false);
  const [introduced, setIntroduced] = useState(false);
  const [convo, setConvo] = useState<"closed" | "open" | "blocked">("closed");
  const [fab, setFab] = useState(false);
  const [fabAsked, setFabAsked] = useState(false);
  const [trace, setTrace] = useState(false);
  const [filter, setFilter] = useState("全部");
  const [expanded, setExpanded] = useState<Record<string, boolean>>({ platform: true });
  const [agentOpen, setAgentOpen] = useState(true);
  const [picker, setPicker] = useState(false);
  const [streamed, setStreamed] = useState(false);

  /* ---- 真实后端态 ---- */
  const [boot, setBoot] = useState<Bootstrap | null>(null);
  const [bootErr, setBootErr] = useState<string | null>(null);
  const [audit, setAudit] = useState<AuditRow[]>([]);
  const [chain, setChain] = useState<ChainStatus | null>(null);
  const [home, setHome] = useState<HomeStats | null>(null);
  const [agent, setAgent] = useState<AgentStats | null>(null);
  const [drillOn, setDrillOn] = useState(false);
  const [drillId, setDrillId] = useState<string | null>(null);
  const [drillReport, setDrillReport] = useState<DrillReportOut | null>(null);

  const refreshAll = () => {
    api.auditTail(50).then(setAudit).catch(() => {});
    api.verifyChain().then(setChain).catch(() => {});
    api.homeStats().then(setHome).catch(() => {});
    api.agentStats().then(setAgent).catch(() => {});
  };
  const chat = useChat(refreshAll);

  useEffect(() => {
    api
      .bootstrap()
      .then((b) => {
        setBoot(b);
        setDrillOn(b.egress_locked);
        refreshAll();
      })
      .catch((e) => setBootErr(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const channels = boot?.channels ?? [];
  const teamChannels = useMemo(() => channels.filter((c) => !c.personal), [channels]);
  const personalChannel = useMemo(() => channels.find((c) => c.personal) ?? null, [channels]);
  const activeChannel = teamChannels.find((c) => c.id === channelId) ?? null;
  const teams = TEAM_ORDER.map((tid) => ({
    id: tid,
    name: teamChannels.find((c) => c.team_id === tid)?.team ?? tid,
    channels: teamChannels.filter((c) => c.team_id === tid),
  })).filter((t) => t.channels.length > 0);

  const soft = (label: string) =>
    setNotice(`「${label}」为完整形态占位模块,当前聚焦 控制台 / 个人工作台 / 团队协作`);

  const goRail = (r: (typeof RAIL)[number]) => {
    if (!r.ok) {
      soft(r.label);
      return;
    }
    setNotice("");
    setModule(r.id);
    setView(r.id === "console" ? "home" : r.id === "personal" ? "phome" : "channel");
  };
  const goChannel = (tid: string, cid: string) => {
    setTeam(tid);
    setChannelId(cid);
    setModule("team");
    setView("channel");
    setNotice("");
    setExpanded((e) => ({ ...e, [tid]: true }));
  };
  const goRoster = (tid: string) => {
    setTeam(tid);
    setModule("team");
    setView("roster");
    setNotice("");
    setFilter("全部");
    setExpanded((e) => ({ ...e, [tid]: true }));
  };
  const goPersonalChat = () => {
    setModule("personal");
    setView("pchat");
    setNotice("");
  };

  /* E6 演习:真实开关(路由层 set_egress_locked + drill.start/end + SQL 报告) */
  const toggleDrill = () => {
    api
      .toggleDrill(!drillOn)
      .then((s) => {
        setDrillOn(s.on);
        setDrillId(s.drill_id);
        setDrillReport(s.on ? null : s.report);
        refreshAll();
      })
      .catch(() => {});
  };

  if (bootErr) {
    return (
      <div className="w-full h-screen flex items-center justify-center" style={{ background: T.canvas }}>
        <Card className="p-6 max-w-lg">
          <b className="text-sm">启动失败(fail-fast,按设计炸响)</b>
          <pre className="mt-3 p-3 rounded-xl text-[11px] whitespace-pre-wrap" style={{ background: T.redSoft, color: T.red }}>{bootErr}</pre>
          <div className="text-xs mt-2" style={{ color: T.sub }}>常见原因:环境变量 KIMI_API_KEY 未设置。请在启动终端 export 后重开应用。</div>
        </Card>
      </div>
    );
  }

  const personalMsgs = chat.msgs["personal"] ?? [];

  return (
    <div className="w-full h-screen overflow-hidden" style={{ background: T.canvas }}>
      <div className="absolute rounded-3xl flex overflow-hidden" style={{ inset: 14, background: T.shell, boxShadow: "0 12px 40px rgba(23,24,28,.08)" }}>
        {/* ===== 一级:图标轨 ===== */}
        <div className="w-14 shrink-0 flex flex-col items-center py-4 gap-1.5" style={{ background: T.rail, borderRight: `1px solid ${T.line}` }}>
          <div className="w-9 h-9 rounded-xl flex items-center justify-center font-extrabold text-sm mb-2" style={{ background: T.indigo, color: "#fff" }}>M</div>
          {RAIL.map((r) => {
            const Ic = r.icon;
            const on = module === r.id;
            return (
              <button key={r.id} onClick={() => goRail(r)} title={r.label}
                className="w-10 h-10 rounded-xl flex items-center justify-center relative"
                style={{ background: on ? T.indigo : "transparent", color: on ? "#fff" : "#8B8FA3", boxShadow: on ? "0 6px 14px rgba(91,91,245,.28)" : "none" }}>
                <Ic size={18} />
                {r.id === "personal" && streamed && !on && (
                  <span className="absolute top-1.5 right-1.5 w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />
                )}
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
              <SideItem icon={<Home size={16} />} label="中控台" active={view === "home"} onClick={() => { setView("home"); setNotice(""); refreshAll(); }} />
              <SideItem icon={<Shield size={16} />} label="审计中心" active={view === "audit"} onClick={() => { setView("audit"); setNotice(""); refreshAll(); }} />
              <SideItem icon={<LineChart size={16} />} label="数据分析" onClick={() => soft("数据分析")} />
              {/* E6 主权演习:真实开关 */}
              <div className="mt-auto rounded-2xl p-4 text-white" style={{ background: drillOn ? "#2B0E10" : T.black, boxShadow: drillOn ? `0 0 0 1.5px ${T.red} inset` : "none" }}>
                <div className="w-8 h-8 rounded-lg flex items-center justify-center mb-2.5" style={{ background: "rgba(255,255,255,.14)" }}>
                  <ShieldAlert size={15} />
                </div>
                <b className="text-sm" style={{ color: drillOn ? "#FFB3B5" : "#fff" }}>主权演习{drillOn ? " · 进行中" : ""}</b>
                <p className="text-[11px] mt-1 leading-relaxed" style={{ color: drillOn ? "#FFB3B5" : "#9FA3B5" }}>
                  {drillOn ? `全组织外联已切断,任务强制本地执行\n${drillId ?? ""}` : "季度合规窗口:切断外联,验证全组织本地执行能力"}
                </p>
                <button onClick={toggleDrill} className="mt-3 w-full py-2 rounded-xl text-xs font-semibold" style={{ background: drillOn ? T.red : T.indigo }}>
                  {drillOn ? "结束演习并出报告" : "启动演习 →"}
                </button>
                {drillReport && !drillOn && (
                  <div className="mt-3 grid grid-cols-2 gap-1.5 text-center">
                    {[
                      [fmtBytes(drillReport.egress_bytes), "窗口外发"],
                      [String(drillReport.model_calls), "模型调用"],
                      [`${drillReport.local_calls}/${drillReport.cloud_calls}`, "本地/云端"],
                      [drillReport.ok ? "✓ 达标" : "✗ 不达标", `unmetered ${drillReport.unmetered_calls}`],
                    ].map(([v, l]) => (
                      <div key={l} className="rounded-lg py-1.5" style={{ background: "rgba(255,255,255,.08)" }}>
                        <div className="text-[11.5px] font-bold">{v}</div>
                        <div className="text-[9px]" style={{ color: "#B9BCCB" }}>{l}</div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </>
          )}

          {module === "personal" && (
            <>
              <SideSec>我的</SideSec>
              <SideItem icon={<Home size={16} />} label="首页" active={view === "phome"} onClick={() => { setView("phome"); setNotice(""); }} />
              <SideItem icon={<Bot size={16} />} label="Agent 档案" active={view === "agent"} onClick={() => { setView("agent"); setNotice(""); refreshAll(); }}
                extra={<span className="ml-auto text-[10px]" style={{ color: view === "agent" ? "#DCDCFE" : T.faint }}>小七</span>} />
              <SideItem icon={<MessageSquare size={16} />} label="对话" active={view === "pchat"} onClick={goPersonalChat} />
              <SideItem icon={<Clock size={16} />} label="任务" onClick={() => soft("任务")} />
              <SideSec>积累</SideSec>
              <SideItem icon={<Brain size={16} />} label="记忆" onClick={() => soft("记忆")} />
              <SideItem icon={<Sparkles size={16} />} label="技能" onClick={() => soft("技能")} />
              <SideItem icon={<BookOpen size={16} />} label="知识库" onClick={() => soft("知识库")} />
              <SideItem icon={<Link2 size={16} />} label="连接器" onClick={() => soft("连接器")} />
              <SideItem icon={<Lock size={16} />} label="权限" onClick={() => soft("权限")} />
              <div className="mt-auto rounded-2xl p-3.5" style={{ background: streamed ? T.indigo : "#fff", border: `1px solid ${streamed ? T.indigo : T.line}`, color: streamed ? "#fff" : T.ink }}>
                <div className="flex items-center gap-1.5 text-[11px] font-semibold">
                  <Cast size={13} />
                  {streamed ? "串流进行中" : "串流到团队"}
                </div>
                {streamed ? (
                  <>
                    <div className="text-[11px] mt-1.5 leading-relaxed" style={{ color: "#DCDCFE" }}>
                      与小七的私有会话
                      <br />→ 平台组 #platform · 12 人围观
                    </div>
                    <button onClick={() => setStreamed(false)} className="mt-2.5 w-full py-1.5 rounded-lg text-[11px] font-semibold flex items-center justify-center gap-1"
                      style={{ background: "rgba(255,255,255,.18)" }}>
                      <StopCircle size={12} /> 停止串流
                    </button>
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
                extra={<span className="ml-auto inline-flex items-center gap-1 text-[10px] font-semibold" style={{ color: view === "meeting" ? "#fff" : T.green }}>
                  <span className="wv inline-block w-1 h-2.5 rounded-full" style={{ background: "currentColor" }} />进行中
                </span>} />
              <SideSec>团队</SideSec>
              {teams.map((t) => {
                const open = !!expanded[t.id];
                const isActiveTeam = team === t.id && (view === "channel" || view === "roster");
                const rosterOn = view === "roster" && team === t.id;
                const meta = TEAM_META[t.id] ?? { people: 0, agents: 0 };
                return (
                  <div key={t.id} className="mb-0.5">
                    <button onClick={() => setExpanded((e) => ({ ...e, [t.id]: !open }))} className="w-full flex items-center gap-2 px-2 py-2 rounded-xl text-left">
                      <ChevronDown size={12} style={{ color: T.faint, transform: open ? "none" : "rotate(-90deg)", transition: "transform .15s" }} />
                      <span className="w-6 h-6 rounded-lg flex items-center justify-center text-[10px] font-bold"
                        style={{ background: isActiveTeam ? T.indigo : "#E4E6EF", color: isActiveTeam ? "#fff" : "#5A5E70" }}>{t.name[0]}</span>
                      <span className="text-[13px] font-semibold" style={{ color: isActiveTeam ? T.indigoDeep : "#5A5E70" }}>{t.name}</span>
                      <span className="ml-auto text-[10px]" style={{ color: T.faint }}>{meta.people}人·{meta.agents}AI</span>
                    </button>
                    {open && (
                      <div className="ml-3.5 pl-2 fade" style={{ borderLeft: `1px solid ${T.line}` }}>
                        {t.channels.map((c) => {
                          const on = view === "channel" && channelId === c.id;
                          const live = streamed && c.id === "platform";
                          return (
                            <button key={c.id} onClick={() => goChannel(t.id, c.id)}
                              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-lg text-left text-[12.5px]"
                              style={{ background: on ? T.indigo : "transparent", color: on ? "#fff" : "#5A5E70", fontWeight: on ? 600 : 400 }}>
                              <Hash size={12} style={{ opacity: 0.7 }} /> {c.name}
                              {c.level === "restricted" && <Lock size={10} style={{ color: on ? "#fff" : T.red }} />}
                              {live && <span className="ml-auto w-1.5 h-1.5 rounded-full lv" style={{ background: on ? "#fff" : T.red }} />}
                            </button>
                          );
                        })}
                        <button onClick={() => goRoster(t.id)} className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-lg text-left text-[12.5px]"
                          style={{ background: rosterOn ? T.indigo : "transparent", color: rosterOn ? "#fff" : "#5A5E70", fontWeight: rosterOn ? 600 : 400 }}>
                          <Users size={12} style={{ opacity: 0.8 }} /> 编制
                          <span className="ml-auto text-[9px] font-bold px-1.5 py-0.5 rounded-md"
                            style={{ background: rosterOn ? "rgba(255,255,255,.22)" : T.soft, color: rosterOn ? "#fff" : T.sub }}>
                            {meta.people + meta.agents}
                          </span>
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
              <CollapseSec label="AGENT" open={agentOpen} onToggle={() => setAgentOpen((o) => !o)}>
                <SideItem icon={<Library size={16} />} label="能力库" active={view === "caps"} onClick={() => { setView("caps"); setNotice(""); }}
                  extra={<span className="ml-auto text-[9px] font-bold px-1.5 py-0.5 rounded-md" style={{ background: view === "caps" ? "rgba(255,255,255,.22)" : T.indigoSoft, color: view === "caps" ? "#fff" : T.indigo }}>P4</span>} />
                <SideItem icon={<Shield size={16} />} label="审计中心" onClick={() => { setModule("console"); setView("audit"); setNotice(""); refreshAll(); }} />
              </CollapseSec>
            </>
          )}
        </aside>

        {/* ===== 主区 ===== */}
        <main className="flex-1 min-w-0 overflow-y-auto flex flex-col">
          <TopBar module={module} view={view} channelName={activeChannel?.name ?? channelId} teamName={activeChannel?.team ?? ""} streamed={streamed} drillOn={drillOn} />
          {notice && (
            <div className="mx-7 mt-2 px-3.5 py-2 rounded-xl text-xs flex items-center gap-2 fade" style={{ background: T.indigoSoft, color: T.indigoDeep }}>
              <Sparkles size={13} /> {notice}
              <button className="ml-auto" onClick={() => setNotice("")}><X size={13} /></button>
            </div>
          )}
          <div className="flex-1 min-h-0">
            {view === "home" && <ConsoleHome home={home} approved={approved} onApprove={() => setModal(true)} />}
            {view === "audit" && <AuditCenter rows={audit} chain={chain} onRefresh={refreshAll} />}
            {view === "phome" && (
              <PersonalHome personalMsgs={personalMsgs} agent={agent} home={home} streamed={streamed}
                onStream={() => setPicker(true)} onStop={() => setStreamed(false)}
                goAgent={() => setView("agent")} goChat={goPersonalChat}
                goChannel={() => goChannel("platform", "platform")} openConvo={() => setConvo("open")} />
            )}
            {view === "agent" && <AgentProfile agent={agent} streamed={streamed} onStream={() => setPicker(true)} goChat={goPersonalChat} />}
            {view === "pchat" && personalChannel && (
              <div className="px-7 pt-1 pb-6" style={{ height: "calc(100% - 8px)" }}>
                <Card className="h-full flex flex-col overflow-hidden">
                  <ChatPane channel={personalChannel} chat={chat} />
                </Card>
              </div>
            )}
            {view === "channel" && activeChannel && (
              <ChannelView channel={activeChannel} chat={chat} auditRows={audit} streamed={streamed}
                introduced={introduced} setIntroduced={setIntroduced}
                openConvo={() => setConvo("open")} goMeeting={() => setView("meeting")} />
            )}
            {view === "roster" && <RosterView approved={approved} filter={filter} setFilter={setFilter} team={team} />}
            {view === "meeting" && <MeetingView />}
            {view === "caps" && <CapsView trace={trace} setTrace={setTrace} introduced={introduced} />}
          </div>
        </main>
      </div>

      {/* ===== FAB 小七 ===== */}
      <button onClick={() => setFab((f) => !f)} className="fixed bottom-8 right-8 w-14 h-14 rounded-2xl flex items-center justify-center z-40"
        style={{ background: T.indigo, color: "#fff", boxShadow: "0 10px 26px rgba(91,91,245,.4)" }}>
        <Bot size={24} />
      </button>
      {fab && (
        <div className="fixed bottom-24 right-8 w-80 z-40 rounded-2xl overflow-hidden fade" style={{ background: "#fff", border: `1px solid ${T.line}`, boxShadow: "0 16px 40px rgba(23,24,28,.14)" }}>
          <div className="px-3.5 py-3 flex items-center gap-2" style={{ background: T.indigoSoft }}>
            <div className="w-8 h-8 rounded-lg flex items-center justify-center" style={{ background: T.indigo, color: "#fff" }}><Bot size={15} /></div>
            <b className="text-[13px]">小七</b>
            <Tag tone="ind" style={{ background: "#fff" }}>编制 A-007</Tag>
            <span className="ml-auto"><RouteTag /></span>
          </div>
          <div className="p-3 space-y-2 text-xs" style={{ maxHeight: 210, overflowY: "auto" }}>
            <Bub>
              我在。累计 {agent?.total_runs ?? "—"} 个 Runs,累计外发 {agent ? fmtBytes(agent.total_egress_bytes) : "—"};审计链
              {chain ? (chain.ok ? `完整(${chain.rows} 行)` : "校验异常!") : "…"}。
            </Bub>
            {fabAsked && (
              <Bub fresh>
                你名下待审批 {home?.pending_approvals ?? 0} 项。
                {(home?.pending_approvals ?? 0) === 0 && <><br />审批流于 P5 接入,当前没有真实审批事件。</>}
              </Bub>
            )}
          </div>
          <div className="px-3 pb-2 flex gap-1.5 flex-wrap">
            {["下达任务", "查我的审批", "串流到频道"].map((q) => (
              <button key={q}
                onClick={() => {
                  if (q === "查我的审批") setFabAsked(true);
                  if (q === "串流到频道") { setFab(false); setPicker(true); }
                  if (q === "下达任务") { setFab(false); goPersonalChat(); }
                }}
                className="text-[11px] px-2.5 py-1 rounded-lg" style={{ background: T.soft }}>
                {q}
              </button>
            ))}
          </div>
          <button onClick={() => { setFab(false); goPersonalChat(); }} className="m-3 mt-1 px-3 py-2 rounded-xl text-xs flex items-center w-[calc(100%-24px)]" style={{ background: T.soft, color: T.faint }}>
            对小七说点什么…<span className="ml-auto">⏎</span>
          </button>
        </div>
      )}

      {/* ===== 串流选择频道(通道 v1.x 演示;密级规则为真实口径) ===== */}
      {picker && (
        <div className="fixed inset-0 z-50 flex items-center justify-center" style={{ background: "rgba(23,24,28,.35)" }}>
          <div className="w-[420px] rounded-2xl overflow-hidden fade" style={{ background: "#fff", boxShadow: "0 24px 60px rgba(23,24,28,.25)" }}>
            <div className="px-5 py-3.5 flex items-center gap-2 text-[13px] font-semibold" style={{ background: T.indigoSoft, color: T.indigoDeep }}>
              <Cast size={15} /> 串流会话到频道
              <button className="ml-auto" onClick={() => setPicker(false)}><X size={14} /></button>
            </div>
            <div className="px-5 py-4">
              <div className="rounded-xl p-3" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
                <div className="text-[13px] font-bold">与小七的私有会话</div>
                <div className="flex items-center gap-2 mt-1.5 text-[10.5px] flex-wrap" style={{ color: T.faint }}>
                  {personalMsgs.length > 0 ? "进行中" : "空会话"} · <RouteTag /> · <Tag>open</Tag> · 会话被 E3 棘轮抬升后,低密级频道将被禁投
                </div>
              </div>
              <div className="text-[11px] mt-3.5 mb-2" style={{ color: T.sub }}>选择目标频道 · 密级跟着会话走</div>
              <div className="space-y-1.5">
                {teamChannels.map((o: Channel) => {
                  const ok = true; // 会话当前 open,任何频道不低于它;抬升后此处生效
                  return (
                    <button key={o.id} disabled={!ok} onClick={() => { setStreamed(true); setPicker(false); }}
                      className="w-full flex items-center gap-2 px-3 py-2.5 rounded-xl text-left text-[13px]"
                      style={{ background: ok ? T.soft : "#FBFBFD", color: ok ? T.ink : T.faint, cursor: ok ? "pointer" : "not-allowed" }}>
                      <Hash size={13} style={{ opacity: 0.7 }} /> {o.name}
                      <span className="text-[10.5px]" style={{ color: T.faint }}>· {o.team}</span>
                      <span className="ml-auto flex items-center gap-1.5">
                        <Tag tone={o.level === "open" ? undefined : o.level === "restricted" ? "red" : "amb"}>{o.level}</Tag>
                      </span>
                    </button>
                  );
                })}
              </div>
              <div className="mt-3.5 text-[10.5px] leading-relaxed" style={{ color: T.faint }}>
                串流为只读投屏:队友可围观与提问,接手需你授权。全程计入审计。<b>串流通道为 v1.x 演示,当前仅 UI 状态。</b>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* ===== 审批弹窗(概念示例,P5 真实化) ===== */}
      {modal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center" style={{ background: "rgba(23,24,28,.35)" }}>
          <div className="w-[392px] rounded-2xl overflow-hidden fade" style={{ background: "#fff", boxShadow: "0 24px 60px rgba(23,24,28,.25)" }}>
            <div className="px-5 py-3.5 flex items-center gap-2 text-[13px] font-semibold" style={{ background: T.indigoSoft, color: T.indigoDeep }}>
              <ShieldAlert size={15} /> 审批请求 <Tag>概念示例</Tag>
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
                <div className="text-[11.5px] mt-1.5" style={{ color: T.sub }}>该操作超出其岗位权限。批准与拒绝都会写入审计(approval.* 事件已在 A9 就绪)。</div>
              </div>
              <div className="flex gap-2 pt-0.5">
                <IBtn onClick={() => { setModal(false); setApproved(true); }} className="px-5 py-2.5">批准执行</IBtn>
                <button onClick={() => setModal(false)} className="px-5 py-2.5 rounded-xl text-xs font-medium" style={{ background: T.soft, color: T.sub }}>拒绝</button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* ===== 对话快照弹窗(概念示例) ===== */}
      {convo !== "closed" && (
        <div className="fixed inset-0 z-50 flex items-center justify-center" style={{ background: "rgba(23,24,28,.35)" }}>
          <div className="w-[480px] max-h-[80vh] rounded-2xl overflow-hidden flex flex-col fade" style={{ background: "#fff", boxShadow: "0 24px 60px rgba(23,24,28,.25)" }}>
            <div className="px-4 py-3 flex items-center gap-2" style={{ borderBottom: `1px solid ${T.line}` }}>
              <b className="text-sm">对话快照 · 支付重试幂等性讨论</b>
              <Tag>概念示例</Tag>
              <button className="ml-auto" onClick={() => setConvo("closed")} style={{ color: T.sub }}><X size={15} /></button>
            </div>
            <div className="px-4 py-2 flex items-center gap-2 text-[10.5px] flex-wrap" style={{ borderBottom: `1px solid ${T.line}`, color: T.sub }}>
              Alice ↔ 小七(A-007) · 14 条 · <RouteTag local /> qwen3:8b · 外发 0 B · <Tag tone="amb">internal</Tag>
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
                <div className="flex items-center gap-1.5 font-semibold" style={{ color: T.red }}>
                  <ShieldAlert size={13} /> 分享被策略阻止
                </div>
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

function TopBar({
  module,
  view,
  channelName,
  teamName,
  streamed,
  drillOn,
}: {
  module: string;
  view: string;
  channelName: string;
  teamName: string;
  streamed: boolean;
  drillOn: boolean;
}) {
  const titles: Record<string, [string, string]> = {
    home: ["中控台", "全组织实时态势 · 每个数字 = 审计表一条 SQL"],
    audit: ["审计中心", "append-only 证据层 · SHA-256 哈希链逐行可验"],
    phome: ["我的工作台", "个人空间 · 与 Agent 的私有会话,默认不进团队"],
    agent: ["Agent 档案 · 小七", "编制 A-007 · 代码评审员 · 由我日常使用"],
    pchat: ["对话 · 小七", "私有会话 · 真实路由与审计,内容不进团队"],
    channel: [`#${channelName}`, `${teamName} · 频道协作 · 共享对话与工作流`],
    roster: [`编制 · ${teamName || "团队"}`, "团队内的人与 Agent · 点将、授权与审计"],
    meeting: ["会议室", "平台组周会 · Agent-007 / 021 在席(概念)"],
    caps: ["能力库", "组织的 Capsule 资产池(P4 概念)"],
  };
  const [t, s] = titles[view] ?? ["Muster", ""];
  return (
    <div className="flex items-center px-7 pt-5 pb-1.5 shrink-0">
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
        {drillOn && (
          <span className="flex items-center gap-1.5 text-[11px] font-semibold px-3 py-1.5 rounded-full" style={{ background: T.redSoft, color: T.red }}>
            <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />演习中 · 外联切断
          </span>
        )}
        {streamed && (
          <span className="flex items-center gap-1.5 text-[11px] font-semibold px-3 py-1.5 rounded-full" style={{ background: T.redSoft, color: T.red }}>
            <span className="w-1.5 h-1.5 rounded-full lv" style={{ background: T.red }} />串流中 → #platform
          </span>
        )}
        <button className="w-9 h-9 rounded-full flex items-center justify-center" style={{ border: `1px solid ${T.line}`, color: "#5A5E70" }}><Search size={15} /></button>
        <button className="w-9 h-9 rounded-full flex items-center justify-center" style={{ border: `1px solid ${T.line}`, color: "#5A5E70" }}><Bell size={15} /></button>
        <div className="flex items-center gap-2 ml-1">
          <div className="w-9 h-9 rounded-full flex items-center justify-center font-bold" style={{ background: T.indigoSoft, color: T.indigo }}>A</div>
          <div>
            <div className="text-[13px] font-semibold">Alice</div>
            <div className="text-[10px]" style={{ color: T.sub }}>平台组 · 组长</div>
          </div>
        </div>
      </div>
    </div>
  );
}
