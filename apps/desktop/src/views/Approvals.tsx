/* P5 审批:合入申请的人工裁决。批准与拒绝都写审计。 */
import { useState } from "react";
import { Check, GitBranch, GitMerge, ShieldAlert, X } from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { PendingApprovalOut, api, fmtDate } from "../api";

export function ApprovalsPanel({
  pending,
  onDecided,
  compact,
}: {
  pending: PendingApprovalOut[];
  onDecided: () => void;
  compact?: boolean;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [result, setResult] = useState<string | null>(null);

  const act = (runId: string, granted: boolean) => {
    setBusy(runId);
    setResult(null);
    api
      .approvalsDecide(runId, granted, granted ? "已复核 diff,同意合入" : "不采纳本次改动")
      .then((detail) => {
        setResult(detail);
        onDecided();
      })
      .catch((e) => setResult(`失败:${e}`))
      .finally(() => setBusy(null));
  };

  if (pending.length === 0) {
    return (
      <Card className="p-4">
        <div className="text-[10px] font-semibold tracking-widest mb-2" style={{ color: T.faint }}>待我审批</div>
        <div className="text-[10.5px] leading-relaxed" style={{ color: T.faint }}>
          没有待裁决的申请。Agent 在隔离分支上产出改动后,会在这里申请合入。
        </div>
        {result && <div className="mt-2 text-[10.5px]" style={{ color: T.green }}>{result}</div>}
      </Card>
    );
  }

  return (
    <Card className="p-4">
      <div className="flex items-center gap-1.5 mb-2.5">
        <ShieldAlert size={12} style={{ color: T.red }} />
        <span className="text-[10px] font-semibold tracking-widest" style={{ color: T.faint }}>待我审批</span>
        <span className="ml-auto text-[10px] font-bold" style={{ color: T.red }}>{pending.length} 项</span>
      </div>

      <div className="space-y-2.5">
        {pending.map((p) => (
          <div key={p.approval_id} className="rounded-xl p-3" style={{ background: T.panel, border: `1px solid ${T.line}` }}>
            <div className="flex items-center gap-1.5 flex-wrap">
              <Tag tone="ind">{p.actor_id}</Tag>
              <Tag tone="red">{p.requested_capability}</Tag>
              <span className="ml-auto text-[9.5px]" style={{ color: T.faint }}>{fmtDate(p.ts_ms)}</span>
            </div>

            <div className="text-[11.5px] mt-2 leading-relaxed" style={{ color: "#454A5C" }}>{p.reason}</div>

            <div className="flex items-center gap-1 mt-2 text-[9.5px]" style={{ color: T.faint }}>
              <GitBranch size={10} />
              <span className="truncate" title={p.branch}>{p.branch}</span>
            </div>
            {!compact && (
              <div className="mt-1 text-[9.5px] break-all" style={{ color: T.faint }}>
                内容哈希 {p.command_hash.slice(0, 24)}…
              </div>
            )}

            {!p.worktree_exists && (
              <div className="mt-2 rounded-lg px-2 py-1.5 text-[10px]" style={{ background: T.amberSoft, color: T.amber }}>
                隔离工作区已被保留策略回收,无法再合入,只能拒绝归档。
              </div>
            )}

            <div className="flex gap-2 mt-2.5">
              <button
                onClick={() => act(p.run_id ?? "", true)}
                disabled={busy !== null || !p.worktree_exists}
                className="flex-1 inline-flex items-center justify-center gap-1 py-1.5 rounded-lg text-[11px] font-semibold"
                style={{
                  background: T.indigo,
                  color: "#fff",
                  opacity: busy !== null || !p.worktree_exists ? 0.45 : 1,
                }}
              >
                <GitMerge size={11} /> {busy === p.run_id ? "处理中…" : "批准合入"}
              </button>
              <button
                onClick={() => act(p.run_id ?? "", false)}
                disabled={busy !== null}
                className="inline-flex items-center justify-center gap-1 px-3 py-1.5 rounded-lg text-[11px] font-semibold"
                style={{ background: T.soft, color: T.sub, opacity: busy !== null ? 0.45 : 1 }}
              >
                <X size={11} /> 拒绝
              </button>
            </div>
          </div>
        ))}
      </div>

      {result && (
        <div className="mt-2.5 flex items-start gap-1.5 text-[10.5px] leading-relaxed"
          style={{ color: result.startsWith("失败") ? T.red : T.green }}>
          <Check size={11} className="mt-0.5 shrink-0" />
          <span>{result}</span>
        </div>
      )}
      <div className="mt-2 text-[9.5px] leading-relaxed" style={{ color: T.faint }}>
        批准与拒绝都会写入审计链(approval.decision),不可事后抹除。
      </div>
    </Card>
  );
}
