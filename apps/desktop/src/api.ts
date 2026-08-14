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
export interface WhoAmI {
  id: string;
  display_name: string;
  kind: string;
  role: string;
  role_zh: string;
  scope: string;
  /** 当前身份能做什么——UI 据此禁用按钮,而不是点了才报错 */
  can: Record<string, boolean>;
}
export interface CapsuleOut {
  capsule_id: string;
  name: string;
  version: string;
  scope: string;
  source_run_id: string;
  forged_ms: number;
  forged_by: string;
  /** 密级:跨团队引入时随包迁移,不可降密 */
  label: string | null;
  owner_team: string | null;
  verify_passed: number;
  verify_total: number;
  /** null = 尚未验真(与"验真失败"必须区分) */
  verified_rate: number | null;
  adopted: number;
}
export interface ForgeableRun {
  run_id: string;
  ts_ms: number;
  output_hash: string;
  duration_ms: number;
}
/** 侧栏「N人·M AI」:与在册编制同一口径——审计链里真干过活的 actor */
export interface TeamCount {
  team: string;
  people: number;
  agents: number;
}
/** 正文存储(desktop-state.db)的体量与保留期 */
export interface TranscriptStats {
  messages: number;
  text_bytes: number;
  oldest_ts_ms: number | null;
  /** null = 未开启保留期,正文永久保留 */
  keep_days: number | null;
  export_dir: string;
}
/** C1:服务端连接状态。未连接 = 单机模式,一切行为与从前一致 */
export interface RemoteStatus {
  connected: boolean;
  base: string | null;
  account_id: string | null;
  display_name: string | null;
}
/** C3:服务端上的会议 */
export interface RemoteMeeting {
  id: string;
  channel_id: string;
  title: string;
  level: string;
  room: string;
  started_ms: number;
  ended_ms: number | null;
  /** 是否请了 Agent。**只是意愿**——它到没到看参会者列表 */
  wants_agent: boolean;
}
/** 会议行动项:**提案,不是任务**。见 muster-server/src/action.rs。 */
export interface ActionItem {
  id: string;
  meeting_id: string;
  text: string;
  owner_hint: string | null;
  /** 出处原话——人要能核对"它是不是听岔了" */
  source_quote: string | null;
  status: "proposed" | "confirmed" | "rejected";
  decided_by: string | null;
  run_id: string | null;
  created_ms: number;
}

export interface ForkResult {
  thread_id: string;
  forked_from: string;
  inherited: number;
  /** 被切掉的那条提问,回到输入框等你改 */
  reopened_prompt: string | null;
}

export interface ThreadInfo {
  id: string;
  title: string;
  forked_from: string | null;
  inherited_count: number;
  persistence: string;
  created_ms: number;
}

export interface JoinInfo {
  url: string;
  token: string;
  room: string;
  level: string;
  /** 由服务端 can() 判定;前端照着显示,不自己判 */
  can_publish: boolean;
  can_record: boolean;
}
export interface PendingApprovalOut {
  approval_id: string;
  ts_ms: number;
  actor_id: string;
  run_id: string | null;
  channel: string | null;
  requested_capability: string;
  reason: string;
  command_hash: string;
  branch: string;
  worktree_path: string;
  worktree_exists: boolean;
}
export interface RosterEntryOut {
  actor_kind: string;
  actor_id: string;
  display_name: string;
  role: string;
  first_seen_ms: number;
  last_seen_ms: number;
  runs: number;
  local_calls: number;
  cloud_calls: number;
  refusals: number;
  events: number;
  pending_approvals: number;
  last_locality: string | null;
}
export interface FileChange {
  path: string;
  status: string;
  added: number;
  removed: number;
}
export interface DiffPayload {
  run_id: string;
  branch: string;
  files_changed: number;
  insertions: number;
  deletions: number;
  files: FileChange[];
  patch: string;
}
export interface StoredMsg {
  channel_id: string;
  role: string;
  text: string;
  run_id: string | null;
  status: string;
  ts_ms: number;
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
  historyBulk: (limit: number) => invoke<StoredMsg[]>("history_bulk", { limit }),
  rosterStats: (team?: string) => invoke<RosterEntryOut[]>("roster_stats", { team: team ?? null }),
  rosterCounts: () => invoke<TeamCount[]>("roster_counts_cmd"),
  /** 封存断裂的审计链并重开一条;返回封存后的路径 */
  auditArchiveBroken: () => invoke<string>("audit_archive_broken"),
  remoteStatus: () => invoke<RemoteStatus>("remote_status"),
  /** 启动时恢复上次连接;探不通就返回未连接,不假装还连着 */
  remoteRestore: () => invoke<RemoteStatus>("remote_restore"),
  remoteLogin: (base: string, id: string, password: string) =>
    invoke<RemoteStatus>("remote_login", { base, id, password }),
  remoteLogout: () => invoke<void>("remote_logout"),
  remoteToken: () => invoke<string | null>("remote_token"),
  remoteMeetings: (channelId: string) => invoke<RemoteMeeting[]>("remote_meetings", { channelId }),
  remoteMeetingStart: (channelId: string, title: string) =>
    invoke<RemoteMeeting>("remote_meeting_start", { channelId, title }),
  remoteMeetingJoin: (meetingId: string) => invoke<JoinInfo>("remote_meeting_join", { meetingId }),
  /** 从第 nth 条用户提问**之前**分叉。父会话不动,新会话新 id(照抄 codex)。 */
  forkConversation: (channelId: string, threadId: string | null, nthUserMessage: number, persistence: "copied" | "referenced") =>
    invoke<ForkResult>("fork_conversation", { channelId, threadId, nthUserMessage, persistence }),
  /** 把团队频道的一段对话拉到个人空间。**会抬升个人会话的密级**(E3 棘轮)。 */
  forkToPersonal: (channelId: string, threadId: string | null, nthUserMessage: number) =>
    invoke<ForkResult>("fork_to_personal", { channelId, threadId, nthUserMessage }),
  listThreads: (channelId: string) => invoke<ThreadInfo[]>("list_threads", { channelId }),
  threadHistory: (threadId: string) => invoke<StoredMsg[]>("thread_history", { threadId }),

  remoteActionItems: (meetingId: string) =>
    invoke<ActionItem[]>("remote_action_items", { meetingId }),
  /** 批准 / 驳回。**判定在服务端**:它要 CreateTask 权限,且不许 Agent 裁决自己提的。 */
  remoteDecideAction: (id: string, confirm: boolean) =>
    invoke<ActionItem>("remote_decide_action", { id, confirm }),
  remoteMeetingEnd: (meetingId: string) => invoke<void>("remote_meeting_end", { meetingId }),
  /** 请 Agent 来 / 请它离开;认领由服务器上的 agent-daemon 完成 */
  remoteMeetingAgent: (meetingId: string, want: boolean) =>
    invoke<void>("remote_meeting_agent", { meetingId, want }),
  /** 组织的频道(登录后);个人频道不在其中——它不上服务端 */
  remoteChannels: () => invoke<Channel[]>("remote_channels"),
  remoteHistory: (channelId: string) => invoke<StoredMsg[]>("remote_history", { channelId }),
  /** C2:实时通道地址。SSE 的 EventSource 由前端直连服务端,不经 Tauri 命令
      ——浏览器的自动重连与 Last-Event-ID 是白送的,绕一圈反而要自己重写 */
  eventsUrl: (base: string, token: string) =>
    `${base.replace(/\/$/, "")}/events?token=${encodeURIComponent(token)}`,
  transcriptStats: () => invoke<TranscriptStats>("transcript_stats"),
  /** kind: all | channel | run | older_than_days */
  transcriptExport: (kind: string, value?: string) =>
    invoke<string>("transcript_export", { kind, value: value ?? null }),
  transcriptPurge: (kind: string, value?: string) =>
    invoke<number>("transcript_purge", { kind, value: value ?? null }),
  approvalsPending: () => invoke<PendingApprovalOut[]>("approvals_pending"),
  approvalsDecide: (runId: string, granted: boolean, note?: string) =>
    invoke<string>("approvals_decide", { runId, granted, note: note ?? null }),
  capsulesList: () => invoke<CapsuleOut[]>("capsules_list"),
  forgeableRuns: () => invoke<ForgeableRun[]>("forgeable_runs"),
  capsuleForge: (runId: string, goal: string, visibility: string) =>
    invoke<string>("capsule_forge", { runId, goal, visibility }),
  capsuleVerify: (capsuleId: string) => invoke<string>("capsule_verify", { capsuleId }),
  capsuleAdopt: (capsuleId: string, toTeam: string) =>
    invoke<string>("capsule_adopt", { capsuleId, toTeam }),
  whoami: () => invoke<WhoAmI>("whoami"),
  capsuleRun: (capsuleId: string, channelId: string, context?: string) =>
    invoke<string>("capsule_run", { capsuleId, channelId, context: context ?? null }),
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
