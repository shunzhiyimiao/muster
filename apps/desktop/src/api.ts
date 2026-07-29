/* 后端契约:所有 invoke 与事件载荷的类型,单一出处 */
import { invoke } from "@tauri-apps/api/core";

export type Sensitivity = "open" | "internal" | "restricted";

export interface Channel {
  id: string;
  name: string;
  team_id: string;
  team: string;
  level: Sensitivity;
  level_note: string;
  desc: string;
  personal: boolean;
}
export interface ProviderCard {
  id: string;
  display_name: string;
  model: string;
  locality: string;
}
export interface Bootstrap {
  channels: Channel[];
  providers: ProviderCard[];
  policy_cloud_max: Sensitivity;
  audit_db: string;
  egress_locked: boolean;
}
export interface Decider {
  origin: string;
  level: Sensitivity;
  subject: string;
}
export interface Plan {
  effective: Sensitivity;
  deciders: Decider[];
  primary: string;
  primary_locality: string;
  fallbacks: string[];
  downgraded: { from: string | null; reason: string } | null;
  policy_cloud_max: Sensitivity;
  policy_egress_locked: boolean;
}
export interface StartPayload {
  run_id: string;
  channel_id: string;
  plan: Plan;
  provider: ProviderCard;
  attempts: string[];
}
export interface DonePayload {
  run_id: string;
  latency_ms: number;
  finish: string;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  chars: number;
}
export interface FailPayload {
  run_id: string;
  channel_id: string;
  message: string;
}
export interface AuditRow {
  event_id: string;
  ts_ms: number;
  event_type: string;
  actor: string;
  run_id: string | null;
  channel: string | null;
  label: string | null;
  locality: string | null;
}
export interface ChainStatus {
  ok: boolean;
  rows: number;
  detail: string;
}
export interface DayBar {
  date: string;
  weekday: string;
  local: number;
  cloud: number;
}
export interface DrillLast {
  ts_ms: number;
  drill_id: string;
  egress_bytes: number;
  unmetered_calls: number;
  ok: boolean;
}
export interface DowngradeItem {
  ts_ms: number;
  run_id: string | null;
  text: string;
}
export interface RunItem {
  ts_ms: number;
  run_id: string;
  outcome: string;
  duration_ms: number;
}
export interface HomeStats {
  runs_week: number;
  runs_prev_week: number;
  egress_week_bytes: number;
  egress_prev_week_bytes: number;
  unmetered_week: number;
  cloud_calls_week: number;
  local_calls_week: number;
  pending_approvals: number;
  drill_last: DrillLast | null;
  throughput: DayBar[];
  downgrades: DowngradeItem[];
  recent_runs: RunItem[];
}
export interface DrillReportOut {
  model_calls: number;
  egress_bytes: number;
  unmetered_calls: number;
  local_calls: number;
  cloud_calls: number;
  ok: boolean;
}
export interface DrillStatus {
  on: boolean;
  drill_id: string | null;
  report: DrillReportOut | null;
}
export interface AgentStats {
  badge: string;
  first_seen_ms: number | null;
  hired_days: number;
  total_runs: number;
  total_egress_bytes: number;
  heat: DayBar[];
}

export const api = {
  bootstrap: () => invoke<Bootstrap>("bootstrap"),
  send: (channelId: string, text: string) => invoke<string>("send_message", { channelId, text }),
  runTask: (channelId: string, text: string) =>
    invoke<string>("run_workspace_task", { channelId, text }),
  auditTail: (limit: number) => invoke<AuditRow[]>("audit_tail", { limit }),
  verifyChain: () => invoke<ChainStatus>("verify_chain"),
  toggleDrill: (on: boolean) => invoke<DrillStatus>("toggle_drill", { on }),
  homeStats: () => invoke<HomeStats>("home_stats"),
  agentStats: () => invoke<AgentStats>("agent_stats"),
};

export const DOWNGRADE_ZH: Record<string, string> = {
  egress_locked: "主权演习进行中:全组织外联已切断,任务强制本地执行",
  restricted_data: "数据密级为 restricted:已强制本地执行,云端选项不可用",
  policy_ceiling: "组织策略:该密级不允许云端处理,已路由至本地",
};
export const ORIGIN_ZH: Record<string, string> = {
  channel: "频道",
  repo: "仓库",
  manual: "手动",
  session_lock: "会话锁",
};

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
export function fmtTime(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
export function fmtDate(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
