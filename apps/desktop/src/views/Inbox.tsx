/* 待我处理:派到我这儿的派发单(块 3)。
 *
 * 只有被派到的本人能接受或驳回——主持人、管理员都不能替我点,所以这一页只有我自己的。
 * 驳回必须写理由:它会回到提出者的视野,没理由就没法改。接受 = 我自己做
 * (「交给私有 Agent」是设计里的第 3 步,这里没有)。
 */
import { useState } from "react";
import { Check, Inbox as InboxIcon, X } from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { api, InboxItem, fmtTime } from "../api";

export function InboxView({
  items,
  onChanged,
  onOpenMeeting,
}: {
  items: InboxItem[];
  onChanged: () => void;
  onOpenMeeting?: (meetingId: string) => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [rejecting, setRejecting] = useState<string | null>(null);
  const [reason, setReason] = useState("");
  const [err, setErr] = useState<string | null>(null);

  const decide = async (id: string, confirm: boolean) => {
    setErr(null);
    if (!confirm && !reason.trim()) {
      setErr("驳回要写理由");
      return;
    }
    setBusy(id);
    try {
      await api.remoteDecideAction(id, confirm, confirm ? undefined : reason.trim());
      setRejecting(null);
      setReason("");
      onChanged();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  const left = (ms: number | null) => {
    if (ms == null) return "";
    const h = Math.max(0, Math.round((ms - Date.now()) / 3600000));
    return h >= 24 ? `${Math.round(h / 24)} 天后过期` : `${h} 小时后过期`;
  };

  return (
    <div className="px-7 pb-8 pt-2 flex flex-col gap-4">
      <Card className="p-5">
        <div className="flex items-center gap-2">
          <InboxIcon size={15} style={{ color: T.indigo }} />
          <b className="text-[15px]">待我处理</b>
          <span className="ml-auto text-[10.5px]" style={{ color: T.faint }}>
            {items.length} 条 · 会上派给我的事,接受 = 我自己做;驳回要写理由
          </span>
        </div>
        {err && (
          <div className="mt-3 px-3 py-2 rounded-xl text-[11.5px]" style={{ background: T.redSoft, color: T.red }}>{err}</div>
        )}
        {items.length === 0 ? (
          <div className="py-8 text-[12px] leading-relaxed" style={{ color: T.sub }}>
            没有派给你的事。会议里值班 Agent 听到"某某负责做什么"会提一条派发单,派到你名下就出现在这里。
          </div>
        ) : (
          <div className="mt-3 flex flex-col gap-2.5">
            {items.map((it) => (
              <div key={it.id} className="rounded-2xl px-4 py-3" style={{ border: `1px solid ${T.line}` }}>
                <div className="flex items-center gap-2 text-[10.5px]" style={{ color: T.faint }}>
                  <button onClick={() => onOpenMeeting?.(it.meeting_id)} className="font-semibold" style={{ color: T.indigoDeep }}>
                    {it.meeting_title}
                  </button>
                  <span>· #{it.channel_id}</span>
                  {it.sensitivity && (
                    <Tag tone={(it.sensitivity === "restricted" ? "red" : it.sensitivity === "internal" ? "amb" : undefined) as never}>{it.sensitivity}</Tag>
                  )}
                  <span className="ml-auto">{fmtTime(it.created_ms)} 提出 · {left(it.expires_ms)}</span>
                </div>
                <div className="text-[13px] mt-1.5 leading-relaxed">
                  {it.text}
                  {it.owner_hint && <span style={{ color: T.sub }}> — 会上说的是「{it.owner_hint}」</span>}
                </div>
                {it.source_quote && (
                  <div className="text-[11px] mt-1 leading-relaxed" style={{ color: T.faint }}>原话:{it.source_quote}</div>
                )}
                {rejecting === it.id ? (
                  <div className="mt-2.5 flex flex-col gap-2">
                    <textarea
                      value={reason}
                      onChange={(e) => setReason(e.target.value)}
                      placeholder="为什么驳回(必填):不该是我 / 听岔了 / 已经做过…"
                      rows={2}
                      className="w-full px-3 py-2 rounded-xl text-[12px] outline-none"
                      style={{ background: T.panel, border: `1px solid ${T.line}` }}
                    />
                    <div className="flex items-center gap-2">
                      <button onClick={() => decide(it.id, false)} disabled={busy === it.id}
                        className="flex items-center gap-1 text-[11px] font-semibold px-3 py-1.5 rounded-lg" style={{ background: T.red, color: "#fff" }}>
                        <X size={11} /> 确认驳回
                      </button>
                      <button onClick={() => { setRejecting(null); setReason(""); }} className="text-[11px] font-semibold px-3 py-1.5 rounded-lg" style={{ background: T.soft, color: T.sub }}>
                        取消
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="flex items-center gap-2 mt-2.5">
                    <button onClick={() => decide(it.id, true)} disabled={busy === it.id}
                      className="flex items-center gap-1 text-[11px] font-semibold px-3 py-1.5 rounded-lg" style={{ background: T.green, color: "#fff", opacity: busy === it.id ? 0.5 : 1 }}>
                      <Check size={11} /> 接受,我来做
                    </button>
                    <button onClick={() => { setRejecting(it.id); setReason(""); }} disabled={busy === it.id}
                      className="flex items-center gap-1 text-[11px] font-semibold px-3 py-1.5 rounded-lg" style={{ background: T.soft, color: T.sub }}>
                      <X size={11} /> 驳回
                    </button>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </Card>
    </div>
  );
}
