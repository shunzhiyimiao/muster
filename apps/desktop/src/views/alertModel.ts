/* 需要有人知道的事(纯逻辑):工作流连续失败、待裁决的合入申请。
   Kimi 欠费 11 天没人发现,因为每天的失败只是个人空间里多一张卡——这里把它抬到首页与铃铛上。 */
import { WorkflowOut, WorkflowRunOut, WorkflowsOut } from "../api";

/** 把 workflow.output 的 outcome 翻成人话。 */
export function reasonZh(outcome: string | null): string {
  if (outcome === null) return "未收尾(还在跑,或进程中途退出)";
  switch (outcome) {
    case "success":
      return "成功";
    case "no_output":
      return "跑完了但没有产物";
    case "failed:quota":
      return "模型账户配额用尽或已暂停,请充值或换一个 provider";
    case "failed:stream":
      return "模型调用失败(限流或中断)";
    case "failed:setup":
      return "定义或环境有问题,没能启动";
    case "max_turns":
    case "failed:max_turns":
      return "回合耗尽,Agent 没做完";
    case "refused":
      return "路由拒绝(fail-closed)";
    default:
      return outcome;
  }
}

export interface WorkflowHealth {
  /** 从最近一次已收尾的运行往前数,连续失败了几次;0 = 最近一次是成功的(或从没跑过) */
  consecutive: number;
  /** 列表只带最近 8 次,全是失败时真实次数可能更多 */
  capped: boolean;
  /** 最近一次已收尾的运行 */
  last: WorkflowRunOut | null;
}

/** 正在跑的那次(未收尾)不算数;成功 = outcome === "success",其余都算失败。 */
export function workflowHealth(w: WorkflowOut): WorkflowHealth {
  const finished = w.runs.filter((r) => r.outcome !== null);
  let n = 0;
  for (const r of finished) {
    if (r.outcome === "success") break;
    n++;
  }
  return { consecutive: n, capped: n > 0 && n === finished.length && w.runs.length >= 8, last: finished[0] ?? null };
}

export interface WorkflowAlert {
  name: string;
  consecutive: number;
  capped: boolean;
  lastTs: number;
  lastOutcome: string;
  reason: string;
}

/**
 * 值得打扰人的失败:连续两次及以上(一次可能只是网络抖动),或者最近一次是配额用尽
 * (它不会自己好,一次就该说)。
 */
export function workflowAlerts(data: WorkflowsOut | null): WorkflowAlert[] {
  if (!data) return [];
  const out: WorkflowAlert[] = [];
  for (const w of data.workflows) {
    const h = workflowHealth(w);
    if (!h.last || h.consecutive === 0) continue;
    const quota = h.last.outcome === "failed:quota";
    if (h.consecutive >= 2 || quota) {
      out.push({
        name: w.name,
        consecutive: h.consecutive,
        capped: h.capped,
        lastTs: h.last.ts_ms,
        lastOutcome: h.last.outcome ?? "",
        reason: reasonZh(h.last.outcome),
      });
    }
  }
  return out;
}
