/* 真实聊天/任务状态机:与后端事件通道对接(task-start/delta/done/refused/failed) */
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Bot, Play, Send } from "lucide-react";
import { api, Channel, DiffPayload, DonePayload, FailPayload, StartPayload, StoredMsg } from "./api";
import { LvTag, Tag } from "./ui";
import { T } from "./theme";

export interface Msg {
  key: string;
  role: "user" | "agent";
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
  send: (channelId: string, text: string, asTask: boolean) => void;
  /// C1:把持久化历史灌进尚未有内容的频道(避免与本次会话消息重复)。
  hydrate: (rows: StoredMsg[]) => void;
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

  const send = (channelId: string, text: string, asTask: boolean) => {
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
    (asTask ? api.runTask(channelId, t) : api.send(channelId, t)).catch((e) => {
      setMsgs((prev) => ({
        ...prev,
        [channelId]: (prev[channelId] ?? []).map((m) => (m.key === agentKey ? { ...m, status: "failed", text: `⚠️ ${e}` } : m)),
      }));
      setBusy((b) => ({ ...b, [channelId]: false }));
    });
  };

  const hydrate = (rows: StoredMsg[]) => {
    setMsgs((prev) => {
      const next = { ...prev };
      const grouped: Record<string, Msg[]> = {};
      rows.forEach((r, i) => {
        (grouped[r.channel_id] ??= []).push({
          key: `db-${i}-${r.ts_ms}`,
          role: r.role === "user" ? "user" : "agent",
          text: r.text,
          runId: r.run_id ?? undefined,
          status: (["done", "failed", "refused"].includes(r.status) ? r.status : "done") as Msg["status"],
          ts: r.ts_ms,
        });
      });
      for (const [chan, list] of Object.entries(grouped)) {
        if (!next[chan] || next[chan].length === 0) next[chan] = list;
      }
      return next;
    });
  };

  return { msgs, busy, lastStart, lastDone, lastFail, lastDiff, send, hydrate };
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
    chat.send(channel.id, draft, asTask);
    setDraft("");
  };

  return (
    <div className="flex-1 min-h-0 flex flex-col">
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
        {list.map((m) =>
          m.role === "user" ? (
            <div key={m.key} className="flex gap-2.5 max-w-2xl">
              <div className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0 text-xs font-bold" style={{ background: `${T.indigo}18`, color: T.indigo }}>
                A
              </div>
              <div className="min-w-0">
                <div className="text-[13px] font-bold">Alice</div>
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
