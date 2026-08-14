/* 真实聊天/任务状态机:与后端事件通道对接(task-start/delta/done/refused/failed) */
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Bot, Play, Plus, Send } from "lucide-react";
import { api, Channel, DiffPayload, DonePayload, FailPayload, StartPayload, StoredMsg, ThreadInfo } from "./api";
import { LvTag, Tag } from "./ui";
import { T } from "./theme";

export interface Msg {
  key: string;
  role: "user" | "agent" | "system";
  text: string;
  runId?: string;
  status: "streaming" | "done" | "failed" | "refused";
  ts?: number;
}

export interface ChatState {
  msgs: Record<string, Msg[]>;
  busy: Record<string, boolean>;
  lastStart: StartPayload | null;
  lastDone: DonePayload | null;
  lastFail: FailPayload | null;
  /// P1-04:最近一次任务的真实代码变更(worktree 模式)。
  lastDiff: DiffPayload | null;
  send: (channelId: string, text: string, asTask: boolean, threadId?: string | null) => void;
  /// C1:把持久化历史灌进尚未有内容的频道(避免与本次会话消息重复)。
  hydrate: (rows: StoredMsg[]) => void;
  /** 强制重读某个频道(hydrate 只填空频道,刷新不了已有内容的) */
  reload: (channelId: string, threadId?: string | null) => Promise<void>;
  /** C2:服务端推来的一条(别人发的,或自己在别的客户端发的) */
  pushRemote: (channelId: string, role: string, text: string, ts: number) => void;
}

export function useChat(onActivity: () => void): ChatState {
  const [msgs, setMsgs] = useState<Record<string, Msg[]>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [lastStart, setLastStart] = useState<StartPayload | null>(null);
  const [lastDone, setLastDone] = useState<DonePayload | null>(null);
  const [lastFail, setLastFail] = useState<FailPayload | null>(null);
  const [lastDiff, setLastDiff] = useState<DiffPayload | null>(null);
  const runIndex = useRef<Record<string, { channelId: string; msgKey: string }>>({});
  const pending = useRef<Record<string, string[]>>({});
  const activity = useRef(onActivity);
  activity.current = onActivity;

  useEffect(() => {
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
        setLastDiff(null);
      }),
      listen<DiffPayload>("task-diff", (e) => setLastDiff(e.payload)),
      listen<{ run_id: string; text: string }>("task-delta", (e) => {
        patch(e.payload.run_id, (m) => ({ ...m, text: m.text + e.payload.text }));
      }),
      listen<DonePayload>("task-done", (e) => {
        patch(e.payload.run_id, (m) => ({ ...m, status: "done" }));
        const idx = runIndex.current[e.payload.run_id];
        if (idx) setBusy((b) => ({ ...b, [idx.channelId]: false }));
        setLastDone(e.payload);
        activity.current();
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
        patch(e.payload.run_id, (m) => ({ ...m, status: "failed", text: m.text + `\n\n⚠️ ${e.payload.message}` }));
        setBusy((b) => ({ ...b, [e.payload.channel_id]: false }));
        setLastFail(e.payload);
        activity.current();
      }),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((un) => un()));
    };
  }, []);

  const send = (channelId: string, text: string, asTask: boolean, threadId?: string | null) => {
    const t = text.trim();
    if (!t || busy[channelId]) return;
    const userKey = `u-${Date.now()}`;
    const agentKey = `a-${Date.now()}`;
    setMsgs((prev) => ({
      ...prev,
      [channelId]: [
        ...(prev[channelId] ?? []),
        { key: userKey, role: "user", text: asTask ? `▶ 任务:${t}` : t, status: "done" },
        { key: agentKey, role: "agent", text: "", status: "streaming" },
      ],
    }));
    pending.current[channelId] = [...(pending.current[channelId] ?? []), agentKey];
    setBusy((b) => ({ ...b, [channelId]: true }));
    (asTask ? api.runTask(channelId, t, threadId) : api.send(channelId, t, threadId)).catch((e) => {
      setMsgs((prev) => ({
        ...prev,
        [channelId]: (prev[channelId] ?? []).map((m) => (m.key === agentKey ? { ...m, status: "failed", text: `⚠️ ${e}` } : m)),
      }));
      setBusy((b) => ({ ...b, [channelId]: false }));
    });
  };

  /// 库里的行 → 界面的消息。**只此一处**——hydrate 与 reload 各写一份的话,
  /// 迟早在某一边漏掉 system,而那时"来源分隔线"会显示成小七说的话。
  const toMsg = (r: StoredMsg, i: number): Msg => ({
    key: `db-${i}-${r.ts_ms}`,
    // system 要保住:它是来源分隔线,归成 agent 就成了小七说的话
    role: r.role === "user" ? "user" : r.role === "system" ? "system" : "agent",
    text: r.text,
    runId: r.run_id ?? undefined,
    status: (["done", "failed", "refused"].includes(r.status) ? r.status : "done") as Msg["status"],
    ts: r.ts_ms,
  });

  const hydrate = (rows: StoredMsg[]) => {
    setMsgs((prev) => {
      const next = { ...prev };
      const grouped: Record<string, Msg[]> = {};
      rows.forEach((r, i) => (grouped[r.channel_id] ??= []).push(toMsg(r, i)));
      for (const [chan, list] of Object.entries(grouped)) {
        // 只填空频道:启动时的载入不该盖掉正在流式的消息
        if (!next[chan] || next[chan].length === 0) next[chan] = list;
      }
      return next;
    });
  };

  /// **强制重读某个频道。**
  ///
  /// hydrate 只填空频道(见上),于是一个已有内容的频道没有任何路径能刷新——
  /// "拉到个人空间"写进了库,界面却永远看不见,要重启应用才出现。
  /// 这条是那个缺口的补丁,所以它必须是替换而不是合并。
  const reload = async (channelId: string, threadId?: string | null) => {
    // 指定了对话就读那一条;否则读整个频道(启动与团队频道走这条)
    const rows = threadId
      ? await api.threadHistory(threadId)
      : (await api.historyBulk(600)).filter((r) => r.channel_id === channelId);
    setMsgs((prev) => ({ ...prev, [channelId]: rows.map(toMsg) }));
  };

  /// 服务端推来的消息。**按 (ts, text) 去重**:自己发的那条既走了 HTTP 响应
  /// 又会从 SSE 推回来,不去重就会看见两遍。
  const pushRemote = (channelId: string, role: string, text: string, ts: number) => {
    setMsgs((m) => {
      const cur = m[channelId] ?? [];
      if (cur.some((x) => x.text === text && Math.abs((x.ts ?? 0) - ts) < 2000)) return m;
      const msg: Msg = {
        key: `remote-${ts}-${cur.length}`,
        role: role === "agent" ? "agent" : "user",
        text,
        status: "done",
        ts,
      };
      return { ...m, [channelId]: [...cur, msg] };
    });
  };

  return { msgs, busy, lastStart, lastDone, lastFail, lastDiff, send, hydrate, reload, pushRemote };
}

/* ---------- 真实消息流 + 输入区(v4 皮肤) ---------- */

export function ChatPane({
  channel,
  chat,
  agentName = "小七",
  header,
}: {
  channel: Channel;
  chat: ChatState;
  agentName?: string;
  header?: React.ReactNode;
}) {
  const [draft, setDraft] = useState("");
  const [hint, setHint] = useState<string | null>(null);
  const scroller = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const list = chat.msgs[channel.id] ?? [];
  const isBusy = !!chat.busy[channel.id];

  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight });
  }, [list.length, list[list.length - 1]?.text]);

  const doSend = (asTask: boolean) => {
    if (isBusy) return;
    // 空输入时不能"点了没反应"——按钮该引导,不该沉默
    if (!draft.trim()) {
      setHint(asTask ? "先描述要做什么,再交给 Agent 执行" : "先输入内容再发送");
      inputRef.current?.focus();
      setTimeout(() => setHint(null), 2600);
      return;
    }
    setHint(null);
    chat.send(channel.id, draft, asTask, active);
    setDraft("");
  };

  /// 这条用户消息是第几条**用户**消息。分叉切的是用户消息边界,
  /// 不是行号——切在助手回合中间会留下有调用没结果的工具调用。
  const nthUserBefore = (all: typeof list, idx: number) =>
    all.slice(0, idx).filter((x) => x.role === "user").length;

  const [forkNote, setForkNote] = useState<string | null>(null);

  /* ---------------------------------------------------------------- 多对话
   *
   * **只在个人空间。** 团队频道的历史在服务端,一个频道就是一条流——
   * 本地再分几条对话,只会与所有人看到的那份分岔,而别人永远看不到你的分支。
   */
  const [threads, setThreads] = useState<ThreadInfo[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const multi = channel.personal;

  const refreshThreads = async () => {
    if (!multi) return;
    try {
      setThreads(await api.listThreads(channel.id));
    } catch {
      /* 列不出来就只用主对话,不该因此发不出消息 */
    }
  };
  useEffect(() => {
    setActive(null);
    void refreshThreads();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [channel.id]);

  const switchTo = async (id: string | null) => {
    setActive(id);
    await chat.reload(channel.id, id);
  };

  const addConversation = async () => {
    try {
      const t = await api.newConversation(channel.id);
      await refreshThreads();
      await switchTo(t.id);
      setForkNote("已开一条新对话。它是空的,与其他对话互不影响。");
      setTimeout(() => setForkNote(null), 5000);
    } catch (e) {
      setForkNote(`新建失败:${e}`);
      setTimeout(() => setForkNote(null), 6000);
    }
  };

  /// 拉到个人空间。
  ///
  /// **切点比「从这里分叉」多一格,因为意图相反。**
  /// 分叉是"回到这一问之前重写",所以切在它之前;拉到个人空间是"把这段讨论
  /// 带走接着深挖",切在之前会恰好把你选中的那条排除掉——点在唯一一条消息上
  /// 会得到"已拉取 0 条",而那看起来像功能坏了。
  ///
  /// 所以传 nth+1:含选中这一问及它的回答。
  const toPersonal = async (nth: number) => {
    try {
      const r = await api.forkToPersonal(channel.id, null, nth);
      // 写进库不等于看得见:个人频道已有内容,hydrate 填不进去
      await chat.reload("personal");
      await refreshThreads();
      // **抬升必须说出来。** 悄悄把个人空间锁到 restricted,人下次发现是
      // "为什么我的私人会话突然不能用云模型了",而那时已经找不到原因
      const raised =
        channel.level === "open"
          ? ""
          : ` 个人会话的密级已抬升到 ${channel.level}——只升不降,要回到 open 只能开新会话。`;
      setForkNote(
        r.inherited === 0
          ? "这一问之前没有内容,没有可拉取的历史。"
          : `已拉到个人空间:${r.inherited} 条。${raised}`
      );
      setTimeout(() => setForkNote(null), 12000);
    } catch (e) {
      setForkNote(`拉取失败:${e}`);
      setTimeout(() => setForkNote(null), 8000);
    }
  };

  const forkAt = async (nth: number, prompt: string) => {
    try {
      const r = await api.forkConversation(channel.id, active, nth, "copied");
      await refreshThreads();
      await switchTo(r.thread_id);
      // 把被切掉的那条提问放回输入框——codex 的 Esc Esc 就是干这个的
      setDraft(r.reopened_prompt ?? prompt);
      setForkNote(`已分叉:继承 ${r.inherited} 条,原会话未改动。改完这句再发。`);
      setTimeout(() => setForkNote(null), 6000);
    } catch (e) {
      setForkNote(`分叉失败:${e}`);
      setTimeout(() => setForkNote(null), 6000);
    }
  };

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      {/* 对话切换条。分叉出来的、拉过来的、新建的,在使用者眼里是同一种东西
          ——**左边列表里的一行**,所以放在一起。 */}
      {multi && threads.length > 0 && (
        <div className="flex items-center gap-1.5 px-5 pt-3 flex-wrap">
          {threads.map((t) => {
            const id = t.id.startsWith("main:") ? null : t.id;
            const on = active === id;
            return (
              <button
                key={t.id}
                onClick={() => switchTo(id)}
                className="text-[11px] font-semibold px-2.5 py-1 rounded-lg whitespace-nowrap"
                style={{
                  background: on ? T.indigo : T.soft,
                  color: on ? "#fff" : T.sub,
                }}
                title={
                  t.forked_from
                    ? `继承 ${t.inherited_count} 条,来自 ${t.forked_from}`
                    : t.id.startsWith("main:")
                      ? "这个频道原本的那条对话"
                      : "新开的空对话"
                }
              >
                {t.title}
                {t.inherited_count > 0 && (
                  <span style={{ opacity: 0.7 }}> ·{t.inherited_count}</span>
                )}
              </button>
            );
          })}
          <button
            onClick={addConversation}
            className="flex items-center gap-1 text-[11px] font-semibold px-2 py-1 rounded-lg"
            style={{ border: `1px dashed ${T.line}`, color: T.faint }}
            title="开一条空对话,与其他对话互不影响"
          >
            <Plus size={11} /> 新对话
          </button>
        </div>
      )}

      {forkNote && (
        <div className="mx-5 mt-3 px-3 py-2 rounded-xl text-[11.5px]" style={{ background: T.indigoSoft, color: T.indigoDeep }}>
          {forkNote}
        </div>
      )}
      <div ref={scroller} className="flex-1 overflow-y-auto p-5 space-y-4">
        {header}
        {list.length === 0 && !header && (
          <div className="max-w-md mx-auto mt-14 text-center">
            <div className="text-[13.5px] font-bold">{channel.personal ? "与小七开始私有会话" : `在 #${channel.name} 发起协作`}</div>
            <div className="text-xs mt-2 leading-relaxed" style={{ color: T.sub }}>
              {channel.level_note}
              {channel.level === "restricted" && " —— 本地通道不可用时将被拒绝,这是产品行为。"}
            </div>
          </div>
        )}
        {list.map((m, i) =>
          /* 来源分隔线。**必须单独渲染**——落到 agent 分支的话,
             "以下 N 条来自 #platform" 会显示成小七说的话,那是伪造来源。 */
          m.role === "system" ? (
            <div key={m.key} className="flex items-center gap-2.5 py-1">
              <div className="flex-1 h-px" style={{ background: T.line }} />
              <span className="text-[10.5px] px-2 py-0.5 rounded-md whitespace-nowrap"
                    style={{ background: T.soft, color: T.sub }}>
                {m.text}
              </span>
              <div className="flex-1 h-px" style={{ background: T.line }} />
            </div>
          ) : m.role === "user" ? (
            <div key={m.key} className="flex gap-2.5 max-w-2xl group">
              <div className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0 text-xs font-bold" style={{ background: `${T.indigo}18`, color: T.indigo }}>
                A
              </div>
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <div className="text-[13px] font-bold">Alice</div>
                  {/* 从这一问之前分叉。**父线程不动**——所以是分支不是改写。
                      分叉后这条提问回到输入框里等你改,那才是这个功能的意义,
                      不是"复制一份对话"。 */}
                  {/* 分叉只在个人空间。团队频道里分叉会建一条**只有你看得见的
                      本地分支**,而团队频道的意义就是彼此看得见——
                      那边要的是「拉到个人空间」。 */}
                  {multi && (
                    <button
                      onClick={() => forkAt(nthUserBefore(list, i), m.text)}
                      className="opacity-0 group-hover:opacity-100 transition-opacity text-[10px] font-semibold px-1.5 py-0.5 rounded-md"
                      style={{ background: T.soft, color: T.sub }}
                      title="从这一问之前分叉出一条新对话,并把它放回输入框重写"
                    >
                      从这里分叉
                    </button>
                  )}
                  {/* 团队 → 个人。个人空间里没有这个按钮:它已经是终点了。
                      密级会跟着搬过去(E3 棘轮),下面的提示条会说清楚。 */}
                  {!channel.personal && (
                    <button
                      onClick={() => toPersonal(nthUserBefore(list, i) + 1)}
                      className="opacity-0 group-hover:opacity-100 transition-opacity text-[10px] font-semibold px-1.5 py-0.5 rounded-md"
                      style={{ background: T.soft, color: T.sub }}
                      title="把到这一问为止的对话(含这一问和它的回答)拉到个人空间接着深挖。个人会话的密级会被抬升到本频道的级别。"
                    >
                      拉到个人空间
                    </button>
                  )}
                </div>
                <div className="text-[13px] mt-0.5 leading-relaxed whitespace-pre-wrap break-words" style={{ color: "#454A5C" }}>
                  {m.text}
                </div>
              </div>
            </div>
          ) : (
            <div key={m.key} className="flex gap-2.5 max-w-2xl">
              <div className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0" style={{ background: T.indigoSoft, color: T.indigo }}>
                <Bot size={16} />
              </div>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="text-[13px] font-bold">{agentName}</span>
                  <Tag tone="ind">编制 A-007</Tag>
                  {m.runId && (
                    <span className="text-[10px]" style={{ color: T.faint }}>
                      {m.runId}
                    </span>
                  )}
                </div>
                <div
                  className="mt-1.5 rounded-2xl px-4 py-3 text-[13px] leading-relaxed whitespace-pre-wrap break-words"
                  style={{
                    background: m.status === "refused" ? T.redSoft : m.status === "failed" ? T.amberSoft : T.panel,
                    border: `1px solid ${m.status === "refused" ? "#F5C9CA" : T.line}`,
                    color: m.status === "refused" ? "#8F2B2E" : "#454A5C",
                  }}
                >
                  {m.text || (m.status === "streaming" ? "…" : "")}
                  {m.status === "streaming" && m.text && <span className="caret" />}
                </div>
              </div>
            </div>
          )
        )}
      </div>
      <div className="px-4 pb-4">
        <div className="flex items-end gap-2 px-4 py-2.5 rounded-xl" style={{ background: T.soft }}>
          <textarea
            ref={inputRef}
            value={draft}
            disabled={isBusy}
            placeholder={
              isBusy ? "执行中…" : channel.personal ? `对${agentName}说点什么…(Enter 发送)` : `给 #${channel.name} 发消息…可 ▶ 交给 ${agentName} 执行任务`
            }
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                doSend(false);
              }
            }}
            className="flex-1 bg-transparent resize-none outline-none text-[13px] leading-relaxed"
            rows={1}
            style={{ color: T.ink, maxHeight: 96 }}
          />
          <button
            onClick={() => doSend(true)}
            disabled={isBusy}
            title="任务模式:Agent 在隔离分支上真改代码,产出 diff 后需人工批准才合入"
            className="inline-flex items-center gap-1 text-[11.5px] font-semibold px-3 py-1.5 rounded-lg shrink-0"
            style={{ background: T.indigo, color: "#fff", opacity: isBusy ? 0.5 : draft.trim() ? 1 : 0.7 }}
          >
            <Play size={11} /> 任务
          </button>
          <button
            onClick={() => doSend(false)}
            disabled={isBusy}
            className="inline-flex items-center gap-1 text-[11.5px] font-semibold px-3 py-1.5 rounded-lg shrink-0"
            style={{ background: T.soft, color: T.sub, opacity: isBusy ? 0.5 : draft.trim() ? 1 : 0.7 }}
          >
            <Send size={11} /> 仅对话
          </button>
        </div>
        <div className="mt-1.5 px-1 flex items-center gap-2 text-[10px]" style={{ color: hint ? T.amber : T.faint }}>
          {hint ? (
            <b>{hint}</b>
          ) : (
            <>
              <LvTag level={channel.level} />
              <span>▶ 任务 = Agent 真改代码(隔离分支 + 审批);仅对话 = 只聊天,无工具</span>
            </>
          )}
          {channel.personal ? "私有会话默认不进团队;串流/分享是唯一出口" : "消息经 E2 路由决策,全程写入审计哈希链"}
        </div>
      </div>
    </div>
  );
}
