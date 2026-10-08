/* P5 审批:合入申请的人工裁决。批准与拒绝都写审计。
   裁决前先看得到改动:每条申请自动加载分支 diff,并标明是否与申请时链上记的哈希一致。 */
import { useEffect, useState } from "react";
import { Check, GitBranch, GitMerge, ShieldAlert, X } from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { ApprovalDiffOut, PendingApprovalOut, api, fmtDate } from "../api";
import { DiffBody } from "./Diff";

type LoadedDiff = { diff: ApprovalDiffOut } | { error: string };

export function ApprovalsPanel({
  pending,
  onDecided,
  canApprove = true,
}: {
  pending: PendingApprovalOut[];
  onDecided: () => void;
  /** P2:当前身份是否有裁决权;无权时不让人白点,直接说明原因 */
  canApprove?: boolean;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [result, setResult] = useState<string | null>(null);
  // 每条待裁决申请的改动。合入前看不到改了什么,审批就是盲签——所以一出现就加载,不等人点
  const [diffs, setDiffs] = useState<Record<string, LoadedDiff>>({});
  const runIds = pending.map((p) => p.run_id ?? "").join(",");
  useEffect(() => {
    pending.forEach((p) => {
      const rid = p.run_id ?? "";
      if (!rid || diffs[rid]) return;
      api
        .approvalsDiff(rid)
        .then((diff) => setDiffs((m) => ({ ...m, [rid]: { diff } })))
        .catch((e) => setDiffs((m) => ({ ...m, [rid]: { error: String(e) } })));
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runIds]);

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
            <div className="mt-2">
              {(() => {
                const d = diffs[p.run_id ?? ""];
                if (!d) return <div className="text-[10px]" style={{ color: T.faint }}>正在读取改动…</div>;
                if ("error" in d) {
                  return (
                    <div className="rounded-lg px-2 py-1.5 text-[10px]" style={{ background: T.amberSoft, color: T.amber }}>
                      读不到改动:{d.error}
                    </div>
                  );
                }
                return (
                  <>
                    <DiffBody diff={d.diff} emptyText="这条分支上没有任何改动。" />
                    <div className="mt-1.5 text-[9.5px] break-all" title={`申请时记录的哈希 ${p.command_hash}`}
                      style={{ color: d.diff.matches_request === false ? T.amber : d.diff.matches_request ? T.green : T.faint }}>
                      {d.diff.matches_request === true && "✓ 与申请时记录的哈希一致,你看到的就是它申请合入的那份"}
                      {d.diff.matches_request === false && "⚠ 与申请时记录的哈希不一致——分支在申请后又动过,以这里显示的为准"}
                      {d.diff.matches_request === null && `内容哈希 ${p.command_hash.slice(0, 24)}…`}
                    </div>
                  </>
                );
              })()}
            </div>

            {!p.worktree_exists && (
              <div className="mt-2 rounded-lg px-2 py-1.5 text-[10px]" style={{ background: T.amberSoft, color: T.amber }}>
                隔离工作区已被保留策略回收,无法再合入,只能拒绝归档。
              </div>
            )}

            {!canApprove && (
              <div className="mt-2 rounded-lg px-2 py-1.5 text-[10px]" style={{ background: T.soft, color: T.sub }}>
                当前身份无裁决权——裁决需覆盖本频道的审批人或管理员角色,且必须是人类。
              </div>
            )}
            <div className="flex gap-2 mt-2.5">
              <button
                onClick={() => act(p.run_id ?? "", true)}
                disabled={busy !== null || !p.worktree_exists || !canApprove}
                className="flex-1 inline-flex items-center justify-center gap-1 py-1.5 rounded-lg text-[11px] font-semibold"
                style={{
                  background: T.indigo,
                  color: "#fff",
                  opacity: busy !== null || !p.worktree_exists || !canApprove ? 0.45 : 1,
                }}
              >
                <GitMerge size={11} /> {busy === p.run_id ? "处理中…" : "批准合入"}
              </button>
              <button
                onClick={() => act(p.run_id ?? "", false)}
                disabled={busy !== null || !canApprove}
                className="inline-flex items-center justify-center gap-1 px-3 py-1.5 rounded-lg text-[11px] font-semibold"
                style={{ background: T.soft, color: T.sub, opacity: busy !== null || !canApprove ? 0.45 : 1 }}
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
