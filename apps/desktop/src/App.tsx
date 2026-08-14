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
  CapsuleOut, ForgeableRun, PendingApprovalOut, RemoteMeeting, RemoteStatus, RosterEntryOut,
  TeamCount, WhoAmI, fmtBytes, fmtTime,
} from "./api";
import { useChat, ChatPane } from "./chat";
import { Bub, Card, CB, CollapseSec, RouteTag, SideItem, SideSec, Tag } from "./ui";
import { ConsoleHome, AuditCenter } from "./views/Console";
import { PersonalHome, AgentProfile } from "./views/Personal";
import { ChannelView, RosterView, MeetingView, CapsView } from "./views/Team";
import { MeetingRoom, type TranscriptLine } from "./views/Meeting";
import { DiffPanel } from "./views/Diff";
import { ApprovalsPanel } from "./views/Approvals";

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
  const [rosterLive, setRosterLive] = useState<RosterEntryOut[]>([]);
  const [teamCounts, setTeamCounts] = useState<TeamCount[]>([]);
  /* C1:服务端连接。null = 还没查;connected=false = 单机模式 */
  const [remote, setRemote] = useState<RemoteStatus | null>(null);
  const [remoteChans, setRemoteChans] = useState<Channel[]>([]);
  const [loginOpen, setLoginOpen] = useState(false);
  /* C3:当前所在的会议(null = 不在会里);转写由 SSE 推来 */
  const [meeting, setMeeting] = useState<RemoteMeeting | null>(null);
  const [transcript, setTranscript] = useState<TranscriptLine[]>([]);
  /* 连上服务器后仍要能看演示形态——否则想给人演示时反而找不到它 */
  const [showDemo, setShowDemo] = useState(false);
  const [approvals, setApprovals] = useState<PendingApprovalOut[]>([]);
  const [capsules, setCapsules] = useState<CapsuleOut[]>([]);
  const [forgeable, setForgeable] = useState<ForgeableRun[]>([]);
  const [me, setMe] = useState<WhoAmI | null>(null);
  const [drillOn, setDrillOn] = useState(false);
  const [drillId, setDrillId] = useState<string | null>(null);
  const [drillReport, setDrillReport] = useState<DrillReportOut | null>(null);

  const refreshAll = () => {
    api.auditTail(50).then(setAudit).catch(() => {});
    api.verifyChain().then(setChain).catch(() => {});
    api.homeStats().then(setHome).catch(() => {});
    api.agentStats().then(setAgent).catch(() => {});
    api.rosterStats().then(setRosterLive).catch(() => {});
    api.rosterCounts().then(setTeamCounts).catch(() => {});
    api.approvalsPending().then(setApprovals).catch(() => {});
    api.capsulesList().then(setCapsules).catch(() => {});
    api.forgeableRuns().then(setForgeable).catch(() => {});
    api.whoami().then(setMe).catch(() => {});
    api.remoteRestore().then((r) => {
      setRemote(r);
      if (r.connected) api.remoteChannels().then(setRemoteChans).catch(() => setRemoteChans([]));
      else setRemoteChans([]);
    }).catch(() => {});
  };
  const chat = useChat(refreshAll);

  useEffect(() => {
    api
      .bootstrap()
      .then((b) => {
        setBoot(b);
        setDrillOn(b.egress_locked);
        refreshAll();
        api.historyBulk(600).then(chat.hydrate).catch(() => {});
      })
      .catch((e) => setBootErr(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 连上服务端就用它的团队频道;**个人频道始终来自本地**——
  // 它不上服务端(界面上承诺过"不进团队、不出现在任何频道与检索里")
  const localChannels = boot?.channels ?? [];
  const channels =
    remote?.connected && remoteChans.length > 0
      ? [...remoteChans, ...localChannels.filter((c) => c.personal)]
      : localChannels;
  const teamChannels = useMemo(() => channels.filter((c) => !c.personal), [channels]);

  /* 频道列表换了(比如刚连上服务端)就校正选中项。
     连上之后本地那套 demo 频道 id 全都不存在于服务端了,再拿着旧 id 去请求
     只会得到「找不到:频道 xxx」——而那句话完全看不出根因是"你选的频道是
     上一套里的"。这里按列表校正,而不是只修某一个 id。 */
  useEffect(() => {
    if (channels.length === 0) return;
    const cur = channels.find((c) => c.id === channelId);
    if (cur) {
      // 频道还在,但所属团队可能变了(服务端的 team_id 与本地不同)
      if (!cur.personal && cur.team_id !== team) setTeam(cur.team_id);
      return;
    }
    const first = teamChannels[0] ?? channels[0];
    if (first) {
      setChannelId(first.id);
      setTeam(first.team_id);
      setExpanded((e) => ({ ...e, [first.team_id]: true }));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [channels]);
  const personalChannel = useMemo(() => channels.find((c) => c.personal) ?? null, [channels]);
  const activeChannel = teamChannels.find((c) => c.id === channelId) ?? null;
  const teams = TEAM_ORDER.map((tid) => ({
    id: tid,
    name: teamChannels.find((c) => c.team_id === tid)?.team ?? tid,
    channels: teamChannels.filter((c) => c.team_id === tid),
  })).filter((t) => t.channels.length > 0);

  /* C2:实时通道。一条 SSE 复用所有频道,**断线重连与 Last-Event-ID 由
     EventSource 自带**——这正是从 WebSocket 换过来的理由,不必自己写补拉。 */
  useEffect(() => {
    if (!remote?.connected || !remote.base) return;
    let es: EventSource | null = null;
    let stop = false;
    api.remoteToken().then((tok) => {
      if (!tok || stop) return;
      es = new EventSource(api.eventsUrl(remote.base!, tok));
      es.onmessage = (e) => {
        try {
          const ev = JSON.parse(e.data);
          if (ev.type === "message") {
            chat.pushRemote(ev.channel_id, ev.role, ev.body, ev.ts_ms);
          } else if (ev.type === "transcript") {
            // 会议纪要行:Agent 转写落库后广播过来
            setTranscript((t) => [...t, { speaker: ev.speaker_id, text: ev.text, ts: ev.ts_ms }]);
          }
        } catch {
          /* 单条解析失败不该拖垮整条流 */
        }
      };
      // onerror 不用手动重连:EventSource 会自己退避重试并带上 Last-Event-ID
    });
    return () => {
      stop = true;
      es?.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [remote?.connected, remote?.base]);

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
    refreshAll(); // 编制是活数据,进页即刷
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
    // 链断是一种**有出路**的启动失败:后端用前缀标记它,这里给出封存入口。
    const broken = bootErr.includes("AUDIT_CHAIN_BROKEN");
    const detail = broken ? bootErr.split("|").slice(1).join("|") : bootErr;
    return (
      <div className="w-full h-screen flex items-center justify-center" style={{ background: T.canvas }}>
        <Card className="p-6 max-w-lg">
          <b className="text-sm">
            {broken ? "审计链校验失败,拒绝启动(fail-closed)" : "启动失败(fail-fast,按设计炸响)"}
          </b>
          <pre className="mt-3 p-3 rounded-xl text-[11px] whitespace-pre-wrap" style={{ background: T.redSoft, color: T.red }}>{detail}</pre>
          {broken ? (
            <>
              <div className="text-xs mt-3 leading-relaxed" style={{ color: T.sub }}>
                封存**不会删除**任何东西:坏掉的那份改名留在 <code>~/.muster/</code> 原地供取证,
                新链的第一条会记下它断在哪、被挪到哪去了。
              </div>
              <div className="flex items-center gap-2 mt-3">
                <button
                  onClick={() =>
                    api
                      .auditArchiveBroken()
                      .then((to) => {
                        setBootErr(null);
                        setNotice(`旧链已封存至 ${to};新链已重开并记下断裂位置`);
                        api.bootstrap().then((b) => { setBoot(b); setDrillOn(b.egress_locked); refreshAll(); })
                          .catch((e) => setBootErr(String(e)));
                      })
                      .catch((e) => setBootErr(String(e)))
                  }
                  className="px-4 py-2 rounded-xl text-xs font-semibold"
                  style={{ background: T.red, color: "#fff" }}
                >
                  封存旧链并重开
                </button>
                <span className="text-[11px]" style={{ color: T.faint }}>
                  想先取证就别点,直接去 ~/.muster/ 查那个文件
                </span>
              </div>
            </>
          ) : (
            <div className="text-xs mt-2" style={{ color: T.sub }}>常见原因:环境变量 KIMI_API_KEY 未设置。请在启动终端 export 后重开应用。</div>
          )}
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
              <ConnBar remote={remote} onClick={() => setLoginOpen(true)} />
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
                <button onClick={toggleDrill} disabled={!(me?.can.toggle_drill ?? true)}
                  title={me?.can.toggle_drill ? "" : "需组织所有者/管理员角色"}
                  className="mt-3 w-full py-2 rounded-xl text-xs font-semibold"
                  style={{ background: drillOn ? T.red : T.indigo, opacity: (me?.can.toggle_drill ?? true) ? 1 : 0.45 }}>
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
                    <div className="col-span-2 text-[9px] leading-relaxed mt-0.5 text-left" style={{ color: "#B9BCCB" }}>
                      口径:外发只统计模型调用。任务里执行的构建/测试命令跑的是工作区代码,
                      其出网在进程外,本报告测不到——不要读成「本次零外发」。
                    </div>
                  </div>
                )}
              </div>
            </>
          )}

          {module === "personal" && (
            <>
              <ConnBar remote={remote} onClick={() => setLoginOpen(true)} />
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
                  {streamed ? "串流(未实现)" : "串流到团队"}
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
              <ConnBar remote={remote} onClick={() => setLoginOpen(true)} />
              <SideSec>团队</SideSec>
              {teams.map((t) => {
                const open = !!expanded[t.id];
                const isActiveTeam = team === t.id && (view === "channel" || view === "roster");
                const rosterOn = view === "roster" && team === t.id;
                const meta = teamCounts.find((c) => c.team === t.name) ?? { people: 0, agents: 0 };
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
                          // 同上:这个 live 标记不代表任何真实的串流
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
                <SideItem icon={<Library size={16} />} label="能力库" active={view === "caps"} onClick={() => { setView("caps"); setNotice(""); refreshAll(); }}
                  extra={<span className="ml-auto text-[9px] font-bold px-1.5 py-0.5 rounded-md" style={{ background: view === "caps" ? "rgba(255,255,255,.22)" : T.indigoSoft, color: view === "caps" ? "#fff" : T.indigo }}>P4</span>} />
                <SideItem icon={<Shield size={16} />} label="审计中心" onClick={() => { setModule("console"); setView("audit"); setNotice(""); refreshAll(); }} />
              </CollapseSec>
            </>
          )}
        </aside>

        {/* ===== 主区 ===== */}
        <main className="flex-1 min-w-0 overflow-y-auto flex flex-col">
          <TopBar module={module} view={view} channelName={activeChannel?.name ?? channelId} teamName={activeChannel?.team ?? ""} streamed={streamed} drillOn={drillOn} me={me} />
          {notice && (
            <div className="mx-7 mt-2 px-3.5 py-2 rounded-xl text-xs flex items-center gap-2 fade" style={{ background: T.indigoSoft, color: T.indigoDeep }}>
              <Sparkles size={13} /> {notice}
              <button className="ml-auto" onClick={() => setNotice("")}><X size={13} /></button>
            </div>
          )}
          <div className="flex-1 min-h-0">
            {view === "home" && (
              <ConsoleHome home={home} pending={approvals}
                onGoApprovals={() => { setModule("team"); setView("channel"); }} />
            )}
            {view === "audit" && <AuditCenter rows={audit} chain={chain} onRefresh={refreshAll} channels={channels} />}
            {view === "phome" && (
              <PersonalHome me={me} personalMsgs={personalMsgs} agent={agent} home={home} streamed={streamed}
                allMsgs={chat.msgs} channels={channels}
                onStream={() => setPicker(true)} onStop={() => setStreamed(false)}
                goAgent={() => setView("agent")} goChat={goPersonalChat}
                goChannel={() => goChannel("platform", "platform")} openConvo={() => setConvo("open")}
                onOpenChannel={(c) => (c.personal ? goPersonalChat() : goChannel(c.team_id, c.id))} />
            )}
            {view === "agent" && <AgentProfile agent={agent} streamed={streamed} onStream={() => setPicker(true)} goChat={goPersonalChat} />}
            {view === "pchat" && personalChannel && (
              <div className="px-7 pt-1 pb-6 flex gap-4" style={{ height: "calc(100% - 8px)" }}>
                <Card className="flex-1 min-w-0 flex flex-col overflow-hidden">
                  <ChatPane channel={personalChannel} chat={chat} />
                </Card>
                <div className="w-72 shrink-0 overflow-y-auto flex flex-col gap-3">
                  <ApprovalsPanel pending={approvals} onDecided={refreshAll} canApprove={me?.can.approve_merge ?? true} />
                  <DiffPanel diff={chat.lastDiff} />
                </div>
              </div>
            )}
            {view === "channel" && activeChannel && (
              <ChannelView channel={activeChannel} chat={chat} auditRows={audit} streamed={streamed}
                introduced={introduced} setIntroduced={setIntroduced}
                openConvo={() => setConvo("open")} goMeeting={() => setView("meeting")}
                approvals={approvals} onApprovalsChanged={refreshAll} canApprove={me?.can.approve_merge ?? true} />
            )}
            {view === "roster" && (
              <RosterView filter={filter} setFilter={setFilter} team={team} live={rosterLive} />
            )}
            {view === "meeting" &&
              (meeting ? (
                <MeetingRoom
                  meeting={meeting}
                  transcript={transcript}
                  onLeave={() => setMeeting(null)}
                />
              ) : remote?.connected && !showDemo ? (
                <MeetingLobby
                  channelId={channelId}
                  onEnter={(m) => {
                    setTranscript([]);
                    setMeeting(m);
                  }}
                  onDemo={() => setShowDemo(true)}
                />
              ) : (
                <>
                  {remote?.connected && (
                    <div className="px-7 pt-3">
                      <button onClick={() => setShowDemo(false)}
                        className="text-[11.5px] font-semibold px-3 py-1.5 rounded-xl"
                        style={{ background: T.soft, color: T.sub }}>
                        ← 回到真实会议
                      </button>
                    </div>
                  )}
                  <MeetingView />
                </>
              ))}
            {view === "caps" && (
              <CapsView trace={trace} setTrace={setTrace} introduced={introduced}
                live={capsules} forgeable={forgeable}
                onForge={(runId, goal) => {
                  api.capsuleForge(runId, goal, "team")
                    .then((msg) => { setNotice(msg); refreshAll(); })
                    .catch((e) => setNotice(`锻造失败:${e}`));
                }}
                canRun={me?.can.create_task ?? true}
                onRun={(capsuleId) => {
                  setNotice(`正在用能力 ${capsuleId} 执行任务…`);
                  api.capsuleRun(capsuleId, channelId)
                    .then((runId) => { setNotice(`已发起 ${runId},产出将进入审批`); refreshAll(); })
                    .catch((e) => setNotice(`${e}`));
                }}
                onAdopt={(capsuleId) => {
                  const toTeam = teams.find((t) => t.id === team)?.name ?? "平台组";
                  api.capsuleAdopt(capsuleId, toTeam)
                    .then((msg) => { setNotice(msg); refreshAll(); })
                    .catch((e) => setNotice(`${e}`));
                }}
                onVerify={(capsuleId) => {
                  setNotice(`正在影子重放 ${capsuleId}…`);
                  api.capsuleVerify(capsuleId)
                    .then((msg) => { setNotice(msg); refreshAll(); })
                    .catch((e) => { setNotice(`${e}`); refreshAll(); });
                }} />
            )}
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
                <b>这条通道尚未实现</b>——点下去只会改变本机的界面状态,
                <b>不会有任何内容发送到任何频道</b>。
                <br />
                做出来之后的形态:只读投屏,队友可围观与提问,接手需你授权,全程计入审计。
              </div>
            </div>
          </div>
        </div>
      )}

      {/* ===== C1 服务端连接 ===== */}
      {loginOpen && (
        <LoginDialog
          current={remote}
          onClose={() => setLoginOpen(false)}
          onDone={(r) => {
            setRemote(r);
            setLoginOpen(false);
            if (r.connected) {
              api.remoteChannels().then(setRemoteChans).catch(() => setRemoteChans([]));
              setNotice(`已连接 ${r.base}(${r.display_name})——团队频道来自服务端,私有会话仍只在本机`);
            } else {
              setRemoteChans([]);
              setNotice("已断开服务端,回到单机模式");
            }
          }}
        />
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
              <button disabled title="对话引用为概念示例,尚未实现" className="text-xs font-semibold px-3 py-1.5 rounded-lg" style={{ background: T.indigoSoft, color: T.indigo , opacity: 0.45, cursor: "not-allowed"}}>引用到 #platform</button>
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
  me,
}: {
  module: string;
  view: string;
  channelName: string;
  teamName: string;
  streamed: boolean;
  drillOn: boolean;
  me: WhoAmI | null;
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
        {/* **串流尚未实现,后端一行都没有。** 原来这里写「串流中 → #platform」
            并配一个闪烁红点,频道名还是硬编码的——看起来像正在发生的事。
            对话框里确实写了「当前仅 UI 状态」,但那句话点完就没了,
            而顶栏这条会一直亮着。
            界面上的东西要么是真的,要么明说自己不是。 */}
        {streamed && (
          <span className="flex items-center gap-1.5 text-[11px] font-semibold px-3 py-1.5 rounded-full"
                style={{ background: T.soft, color: T.sub }}
                title="串流通道尚未实现:这只是界面状态,没有任何内容被发送到任何频道。">
            <span className="w-1.5 h-1.5 rounded-full" style={{ background: T.faint }} />
            串流(未实现·仅界面状态)
          </span>
        )}
        <button disabled title="全局搜索尚未实现" className="w-9 h-9 rounded-full flex items-center justify-center"
          style={{ border: `1px solid ${T.line}`, color: "#5A5E70", opacity: 0.4, cursor: "not-allowed" }}><Search size={15} /></button>
        <button disabled title="通知中心尚未实现" className="w-9 h-9 rounded-full flex items-center justify-center"
          style={{ border: `1px solid ${T.line}`, color: "#5A5E70", opacity: 0.4, cursor: "not-allowed" }}><Bell size={15} /></button>
        <div className="flex items-center gap-2 ml-1" title={me ? `身份来自 MUSTER_ROLE/MUSTER_USER;接 OIDC 后改由 iss+sub 解析` : ""}>
          <div className="w-9 h-9 rounded-full flex items-center justify-center font-bold" style={{ background: T.indigoSoft, color: T.indigo }}>
            {(me?.display_name ?? "?").slice(0, 1).toUpperCase()}
          </div>
          <div>
            <div className="text-[13px] font-semibold">{me?.display_name ?? "…"}</div>
            <div className="text-[10px]" style={{ color: T.sub }}>
              {me ? `${me.scope} · ${me.role_zh}` : "身份加载中"}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

/* 连接状态条。**单机不是"没连上",是一种正常形态**——所以文案不用
   "离线""未连接"这种带缺陷意味的词,而是如实说"单机模式"。 */
function ConnBar({ remote, onClick }: { remote: RemoteStatus | null; onClick: () => void }) {
  const on = !!remote?.connected;
  return (
    <button onClick={onClick} className="w-full flex items-center gap-2 px-2.5 py-2 rounded-xl mb-1 text-left"
      style={{ background: on ? T.greenSoft : T.soft }}>
      <span className="w-1.5 h-1.5 rounded-full shrink-0" style={{ background: on ? T.green : T.faint }} />
      <span className="text-[11.5px] font-semibold" style={{ color: on ? T.green : T.sub }}>
        {on ? "已连接团队服务器" : "单机模式"}
      </span>
      <span className="ml-auto text-[10px] truncate" style={{ color: T.faint, maxWidth: 90 }}>
        {on ? remote?.display_name : "点此连接"}
      </span>
    </button>
  );
}

/* ===== C1:服务端登录 =====
   未连接时是单机点将台,一切照旧;连上之后团队频道来自服务端,
   **私有会话仍然只在本机**——界面上承诺过它不进团队,连上服务器也不能破。 */
function LoginDialog({
  current,
  onClose,
  onDone,
}: {
  current: RemoteStatus | null;
  onClose: () => void;
  onDone: (r: RemoteStatus) => void;
}) {
  const [base, setBase] = useState(current?.base ?? "http://localhost:8787");
  const [id, setId] = useState(current?.account_id ?? "");
  const [pw, setPw] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const submit = () => {
    if (!base.trim() || !id.trim() || !pw) {
      setErr("服务器地址、账号、口令都要填");
      return;
    }
    setBusy(true);
    setErr(null);
    api
      .remoteLogin(base.trim(), id.trim(), pw)
      .then(onDone)
      .catch((e) => setErr(String(e)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" style={{ background: "rgba(23,24,28,.35)" }}>
      <Card className="w-[420px] p-6">
        <b className="text-sm">连接团队服务器</b>
        <div className="text-[11.5px] mt-1.5 leading-relaxed" style={{ color: T.sub }}>
          连上之后,团队频道与消息来自服务端;<b>私有会话仍然只存在这台机器上</b>。
          任务执行、审计链、worktree 也都留在本机——服务端不持有源码。
        </div>

        {current?.connected ? (
          <div className="mt-4">
            <div className="text-xs" style={{ color: T.sub }}>
              当前已连接 <b style={{ color: T.indigoDeep }}>{current.base}</b>
              <br />身份:{current.display_name}({current.account_id})
            </div>
            <div className="flex gap-2 mt-4">
              <button
                onClick={() =>
                  api.remoteLogout().then(() =>
                    onDone({ connected: false, base: null, account_id: null, display_name: null })
                  )
                }
                className="px-4 py-2 rounded-xl text-xs font-semibold"
                style={{ background: T.redSoft, color: T.red }}
              >
                断开,回到单机模式
              </button>
              <button onClick={onClose} className="px-4 py-2 rounded-xl text-xs" style={{ background: T.soft, color: T.sub }}>
                关闭
              </button>
            </div>
          </div>
        ) : (
          <>
            <div className="mt-4 space-y-2">
              {[
                ["服务器地址", base, setBase, "text"],
                ["账号", id, setId, "text"],
                ["口令", pw, setPw, "password"],
              ].map(([label, val, set, type]) => (
                <label key={label as string} className="block">
                  <span className="text-[11px]" style={{ color: T.faint }}>{label as string}</span>
                  <input
                    type={type as string}
                    value={val as string}
                    onChange={(e) => (set as (v: string) => void)(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && submit()}
                    className="w-full mt-1 px-3 py-2 rounded-xl text-xs outline-none"
                    style={{ background: T.panel, border: `1px solid ${T.line}` }}
                  />
                </label>
              ))}
            </div>
            {err && (
              <div className="mt-3 px-3 py-2 rounded-xl text-[11.5px]" style={{ background: T.redSoft, color: T.red }}>
                {err}
              </div>
            )}
            <div className="flex gap-2 mt-4">
              <button disabled={busy} onClick={submit} className="px-4 py-2 rounded-xl text-xs font-semibold"
                style={{ background: T.indigo, color: "#fff", opacity: busy ? 0.5 : 1 }}>
                {busy ? "连接中…" : "连接"}
              </button>
              <button onClick={onClose} className="px-4 py-2 rounded-xl text-xs" style={{ background: T.soft, color: T.sub }}>
                取消
              </button>
            </div>
          </>
        )}
      </Card>
    </div>
  );
}

/* C3:会议大厅。**只在连了服务端时出现**——会议本就是多人的事,
   单机模式下没有"别人"可开会,那时显示的仍是概念稿(并标着"演示叙事")。 */
function MeetingLobby({
  channelId,
  onEnter,
  onDemo,
}: {
  channelId: string;
  onEnter: (m: RemoteMeeting) => void;
  onDemo: () => void;
}) {
  const [list, setList] = useState<RemoteMeeting[]>([]);
  const [title, setTitle] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const load = () => {
    api.remoteMeetings(channelId).then(setList).catch((e) => setErr(String(e)));
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(load, [channelId]);

  const start = () => {
    const t = title.trim();
    if (!t) {
      setErr("先给会议起个名字");
      return;
    }
    setBusy(true);
    api
      .remoteMeetingStart(channelId, t)
      .then((m) => {
        setTitle("");
        onEnter(m);
      })
      .catch((e) => setErr(String(e)))
      .finally(() => setBusy(false));
  };

  const live = list.filter((m) => !m.ended_ms);
  return (
    <div className="px-7 pb-8 pt-2 flex flex-col gap-4">
      <Card className="p-5">
        <b className="text-[15px]">开会</b>
        <div className="text-[11.5px] mt-1 leading-relaxed" style={{ color: T.sub }}>
          会议密级继承本频道——否则"把话题挪进会议"就成了绕过密级的办法。
          转写由会议 Agent 走<b>本地</b> whisper 完成,音频不出内网。
        </div>
        <div className="flex items-center gap-2 mt-3.5">
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && start()}
            placeholder="会议主题,例如「平台组周会」"
            className="flex-1 px-3.5 py-2 rounded-xl text-xs outline-none"
            style={{ background: T.panel, border: `1px solid ${T.line}` }}
          />
          <button disabled={busy} onClick={start} className="text-xs font-semibold px-4 py-2 rounded-xl"
            style={{ background: T.indigo, color: "#fff", opacity: busy ? 0.5 : 1 }}>
            {busy ? "创建中…" : "发起会议"}
          </button>
        </div>
        {err && (
          <div className="mt-3 px-3 py-2 rounded-xl text-[11.5px]" style={{ background: T.redSoft, color: T.red }}>{err}</div>
        )}
        <button onClick={onDemo} className="mt-3 text-[11px] font-semibold"
          style={{ color: T.indigo }}>
          看演示形态 →
        </button>
      </Card>

      <Card className="px-5 pt-4 pb-2">
        <div className="flex items-center">
          <b className="text-[13px]">进行中</b>
          <span className="ml-auto text-[10.5px]" style={{ color: T.faint }}>本频道 · 共 {list.length} 场</span>
        </div>
        {live.length === 0 ? (
          <div className="py-5 text-[11.5px]" style={{ color: T.sub }}>当前没有进行中的会议。</div>
        ) : (
          live.map((m) => (
            <button key={m.id} onClick={() => onEnter(m)} className="w-full flex items-center gap-2.5 py-2.5 text-left"
              style={{ borderTop: `1px solid ${T.line}` }}>
              <Video size={14} style={{ color: T.indigo }} />
              <span className="text-[13px] font-semibold">{m.title}</span>
              <Tag tone={(m.level === "restricted" ? "red" : m.level === "internal" ? "amb" : undefined) as never}>{m.level}</Tag>
              <span className="ml-auto text-[10.5px]" style={{ color: T.faint }}>{fmtTime(m.started_ms)} 开始</span>
            </button>
          ))
        )}
      </Card>
    </div>
  );
}
