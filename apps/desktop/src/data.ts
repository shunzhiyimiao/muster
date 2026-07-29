/* 概念稿静态数据(编制卡 / 能力库 / 会议字幕 / 记忆时间线)。
   这些是 v4 的产品叙事占位,真实化排期:编制=D6、能力库=P4、会议=v1.x。 */
import { Cloud, HardDrive, Play, Shield, TrendingUp, Video } from "lucide-react";

export const TEAM_META: Record<string, { people: number; agents: number }> = {
  platform: { people: 2, agents: 2 },
  pay: { people: 1, agents: 1 },
  sec: { people: 1, agents: 1 },
};

export interface RosterTile {
  i: keyof typeof TICON;
  l: string;
  v: string;
  hot?: boolean;
  hot2?: boolean;
}
export interface RosterEntry {
  kind: "agent" | "human";
  feat?: boolean;
  team: string;
  name: string;
  grade: string;
  role?: string;
  init?: string;
  tone?: string;
  tags: string[];
  tiles: RosterTile[];
  foot: string;
}

export const TICON = { play: Play, shield: Shield, hdd: HardDrive, cloud: Cloud, trend: TrendingUp, video: Video };

export const ROSTER: RosterEntry[] = [
  { kind: "agent", feat: true, team: "platform", name: "Agent-007", grade: "编制 A-007", role: "代码评审员",
    tags: ["只读仓库", "可跑测试", "可发评论"],
    tiles: [{ i: "play", l: "执行中", v: "RUN-2231" }, { i: "shield", l: "待审批", v: "1 项", hot: true }, { i: "hdd", l: "当前路由", v: "本地" }],
    foot: "最近运行 10:32 · 周会纪要已发布" },
  { kind: "human", team: "platform", name: "Alice", grade: "组长", init: "A", tone: "#5B5BF5",
    tags: ["平台组", "审批人", "架构评审"],
    tiles: [{ i: "trend", l: "在办任务", v: "2" }, { i: "shield", l: "本周审批", v: "5" }, { i: "video", l: "今日会议", v: "1 场" }],
    foot: "最近活跃 10:32 · 周会中" },
  { kind: "human", team: "platform", name: "Bob", grade: "工程师", init: "B", tone: "#0EA5A5",
    tags: ["平台组", "网关方向", "值周"],
    tiles: [{ i: "trend", l: "在办任务", v: "3" }, { i: "shield", l: "本周审批", v: "2" }, { i: "video", l: "今日会议", v: "1 场" }],
    foot: "最近活跃 10:29" },
  { kind: "agent", team: "platform", name: "Agent-021", grade: "编制 A-021", role: "会议书记员",
    tags: ["实时转写", "纪要发布", "行动项跟踪"],
    tiles: [{ i: "play", l: "执行中", v: "转写中" }, { i: "shield", l: "待审批", v: "0" }, { i: "hdd", l: "当前路由", v: "本地" }],
    foot: "最近运行 进行中 · 平台组周会" },
  { kind: "human", team: "pay", name: "Carol", grade: "工程师", init: "C", tone: "#D97706",
    tags: ["支付组", "发布负责人"],
    tiles: [{ i: "trend", l: "在办任务", v: "1" }, { i: "shield", l: "本周审批", v: "0" }, { i: "video", l: "今日会议", v: "1 场" }],
    foot: "最近活跃 10:31" },
  { kind: "agent", team: "pay", name: "Agent-012", grade: "编制 A-012", role: "发布管理员",
    tags: ["Release Checklist", "灰度放量", "回滚"],
    tiles: [{ i: "play", l: "执行中", v: "—" }, { i: "shield", l: "待审批", v: "1 项", hot2: true }, { i: "cloud", l: "当前路由", v: "云端" }],
    foot: "最近运行 昨日 18:04" },
  { kind: "human", team: "sec", name: "林小安", grade: "安全员", init: "林", tone: "#E5484D",
    tags: ["安全组", "脱敏策略", "审计对接"],
    tiles: [{ i: "trend", l: "在办任务", v: "1" }, { i: "shield", l: "本周审批", v: "1" }, { i: "video", l: "今日会议", v: "0 场" }],
    foot: "最近活跃 09:12" },
  { kind: "agent", team: "sec", name: "Agent-033", grade: "编制 A-033", role: "脱敏巡检员",
    tags: ["仅本地", "敏感字段扫描", "整改清单"],
    tiles: [{ i: "play", l: "执行中", v: "每日巡检" }, { i: "shield", l: "待审批", v: "0" }, { i: "hdd", l: "当前路由", v: "本地" }],
    foot: "最近运行 06:00 · 定时巡检" },
];

export interface Cap {
  name: string;
  ver: string;
  team: string;
  tags: string[];
  rate: number;
  runs: number;
  used: number;
  scope: string;
  hot?: boolean;
  restricted?: boolean;
  verifying?: boolean;
}
export const CAPS: Cap[] = [
  { name: "Release Checklist", ver: "v1.2", team: "支付组", tags: ["发布检查", "回滚脚本", "灰度建议"], rate: 96, runs: 32, used: 41, scope: "跨团队", hot: true },
  { name: "Code Review", ver: "v2.0", team: "平台组", tags: ["静态审查", "跑测试", "修复 diff"], rate: 98, runs: 57, used: 126, scope: "全组织" },
  { name: "数据脱敏巡检", ver: "v1.0", team: "安全组", tags: ["敏感字段", "整改清单"], rate: 100, runs: 18, used: 12, scope: "团队内", restricted: true },
  { name: "周报汇总", ver: "v0.9", team: "平台组", tags: ["纪要聚合", "周报草稿"], rate: 61, runs: 9, used: 3, scope: "验真中", verifying: true },
];

export const MEMO = [
  { t: "架构偏好 · 记忆固化", d: "36 天前", s: "增量交付、约定优先、测试先行" },
  { t: "技能:change-validation-planner", d: "36 天前", s: "改动影响面推演与验证计划" },
  { t: "技能:code-review", d: "36 天前", s: "锻造自 RUN-1893 · 验真 98%" },
  { t: "Capsule:Release Checklist", d: "13 天前", s: "引入自支付组 · 密级随包迁移" },
];

export const CAPTIONS: [string, string][] = [
  ["Alice", "重试幂等键还是放在网关层统一生成吧,业务侧太散了"],
  ["Bob", "同意。业务侧只透传,别各写各的"],
  ["Carol", "那回滚脚本谁来出?上次就是回滚卡住的"],
  ["Agent-007", "我可以在会后跑 Release Checklist,顺带产出回滚脚本草稿"],
  ["Alice", "好,这条行动项记你头上"],
];
