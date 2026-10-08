/* 首页横幅 + 顶栏铃铛:只汇总真实发生、需要人出手的事。 */
import { useEffect, useRef } from "react";
import { AlertTriangle, Bell, ChevronRight, GitMerge } from "lucide-react";
import { T } from "../theme";
import { WorkflowAlert } from "./alertModel";

const fmt = (ms: number) => new Date(ms).toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });

/** 首页顶部的横幅。没事就什么都不渲染——横幅常亮会被当成壁纸。 */
export function AlertBanner({
  alerts,
  pending,
  onGoWorkflows,
  onGoApprovals,
}: {
  alerts: WorkflowAlert[];
  pending: number;
  onGoWorkflows: () => void;
  onGoApprovals: () => void;
}) {
  if (alerts.length === 0 && pending === 0) return null;
  return (
    <div className="flex flex-col gap-2 mb-3">
      {alerts.map((a) => (
        <div key={a.name} className="rounded-2xl px-4 py-2.5 flex items-center gap-2 flex-wrap text-[11.5px] fade"
          style={{ background: T.redSoft, border: "1px solid #F5C9CA", color: "#8F2B2E" }}>
          <AlertTriangle size={13} className="shrink-0" />
          <span>
            工作流 <b>{a.name}</b> 已连续失败 <b>{a.capped ? `${a.consecutive}+` : a.consecutive}</b> 次
            <span style={{ opacity: 0.85 }}> · 最近 {fmt(a.lastTs)} · {a.reason}</span>
          </span>
          <button onClick={onGoWorkflows} className="ml-auto inline-flex items-center gap-1 text-[11px] font-semibold px-2.5 py-1 rounded-lg"
            style={{ background: "#fff", color: "#8F2B2E" }}>
            去看工作流 <ChevronRight size={11} />
          </button>
        </div>
      ))}
      {pending > 0 && (
        <div className="rounded-2xl px-4 py-2.5 flex items-center gap-2 flex-wrap text-[11.5px] fade"
          style={{ background: T.amberSoft, border: "1px solid #F3DCB0", color: "#7A4B00" }}>
          <GitMerge size={13} className="shrink-0" />
          <span>有 <b>{pending}</b> 项合入申请等你裁决,改动内容在审批面板里可以直接看。</span>
          <button onClick={onGoApprovals} className="ml-auto inline-flex items-center gap-1 text-[11px] font-semibold px-2.5 py-1 rounded-lg"
            style={{ background: "#fff", color: "#7A4B00" }}>
            去审批 <ChevronRight size={11} />
          </button>
        </div>
      )}
    </div>
  );
}

/** 铃铛下拉:同一份事实,给在别的页面里干活的人看。点外面 / Esc 关。 */
export function BellMenu({
  alerts,
  pending,
  onClose,
  onGoWorkflows,
  onGoApprovals,
}: {
  alerts: WorkflowAlert[];
  pending: number;
  onClose: () => void;
  onGoWorkflows: () => void;
  onGoApprovals: () => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);
  const empty = alerts.length === 0 && pending === 0;
  return (
    <div ref={ref} role="menu" className="absolute right-0 top-11 w-80 z-40 rounded-2xl overflow-hidden fade"
      style={{ background: "#fff", border: `1px solid ${T.line}`, boxShadow: "0 16px 40px rgba(23,24,28,.14)" }}>
      <div className="px-3.5 py-2.5 flex items-center gap-2" style={{ background: T.panel, borderBottom: `1px solid ${T.line}` }}>
        <Bell size={13} style={{ color: T.sub }} />
        <b className="text-[12px]">需要你出手的事</b>
        <span className="ml-auto text-[10px]" style={{ color: T.faint }}>{empty ? "暂无" : `${alerts.length + (pending > 0 ? 1 : 0)} 项`}</span>
      </div>
      {empty ? (
        <div className="px-3.5 py-4 text-[11px]" style={{ color: T.faint }}>没有需要注意的事。工作流连续失败、待裁决的合入申请会出现在这里。</div>
      ) : (
        <div className="py-1">
          {alerts.map((a) => (
            <button key={a.name} role="menuitem" onClick={() => { onClose(); onGoWorkflows(); }}
              className="w-full text-left px-3.5 py-2 flex items-start gap-2 hover:bg-[#F7F8FB]">
              <span className="mt-1.5 w-2 h-2 rounded-full shrink-0" style={{ background: T.red }} />
              <span className="min-w-0">
                <div className="text-[11.5px] font-semibold truncate">工作流 {a.name} 连续失败 {a.capped ? `${a.consecutive}+` : a.consecutive} 次</div>
                <div className="text-[10.5px]" style={{ color: T.sub }}>{fmt(a.lastTs)} · {a.reason}</div>
              </span>
            </button>
          ))}
          {pending > 0 && (
            <button role="menuitem" onClick={() => { onClose(); onGoApprovals(); }}
              className="w-full text-left px-3.5 py-2 flex items-start gap-2 hover:bg-[#F7F8FB]">
              <span className="mt-1.5 w-2 h-2 rounded-full shrink-0" style={{ background: T.amber }} />
              <span className="min-w-0">
                <div className="text-[11.5px] font-semibold">{pending} 项合入申请待裁决</div>
                <div className="text-[10.5px]" style={{ color: T.sub }}>审批面板里能直接看改动</div>
              </span>
            </button>
          )}
        </div>
      )}
      <div className="px-3.5 py-2 text-[9.5px]" style={{ color: T.faint, borderTop: `1px solid ${T.line}` }}>
        只汇总真实发生的事,不做提醒订阅;要通知到手机得接服务端。
      </div>
    </div>
  );
}
