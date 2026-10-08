/* 技能:SKILL.md 约定的过程性知识包(与 codex 同一格式)。
   这一页只回答两件事:任务模式会把哪些技能交给 Agent、为什么有的没加载。
   正文不在这里展示——Agent 在任务里经 read_skill 按需读取,审计只记哈希。 */
import { AlertTriangle, FolderOpen, Lock, RefreshCw, Sparkles } from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { SkillsOut } from "../api";

const EXAMPLE = `---
name: release-checklist
description: 发版前的检查清单:跑测试、核对变更日志、确认回滚脚本。
metadata:
  sensitivity: internal   # 可选:open / internal / restricted
---

# 发版检查清单
1. 跑 cargo test --workspace,基线不绿不发。
2. …`;

export function SkillsView({ data, onRefresh }: { data: SkillsOut | null; onRefresh: () => void }) {
  const skills = data?.skills ?? [];
  const problems = data?.problems ?? [];
  const roots = data?.roots ?? [];

  return (
    <div className="px-7 pb-8 pt-2">
      <div className="flex items-center mb-4">
        <div className="flex items-center gap-2">
          <Sparkles size={16} style={{ color: T.indigo }} />
          <b className="text-[15px]">技能</b>
          <span className="text-[11px]" style={{ color: T.sub }}>
            已发现 {skills.length} 个 · 任务模式会原样交给 Agent
          </span>
        </div>
        <button onClick={onRefresh}
          className="ml-auto inline-flex items-center gap-1.5 text-xs font-semibold px-3 py-1.5 rounded-xl"
          style={{ background: T.soft, color: T.ink }}>
          <RefreshCw size={12} /> 重新扫描
        </button>
      </div>

      <Card className="p-4 mb-4">
        <div className="text-[10px] font-semibold tracking-widest mb-2" style={{ color: T.faint }}>发现目录(先到先得)</div>
        <div className="space-y-1.5">
          {roots.map((r, i) => {
            // 顺序由后端定(即优先级):仓库级两处在前,个人级两处在后
            const note = [
              "随代码提交,团队共用;与 Codex 的 .agents/skills 同一份",
              "Codex 的另一个项目级目录,同一仓库里的 Codex 用户放这里的也认",
              "只有这台机器的你能用",
              "Codex 用户的个人技能目录,两边共用,不必复制",
            ][i] ?? "";
            const repoLevel = i < 2;
            return (
              <div key={r} className="flex items-center gap-2 text-[11.5px]">
                <FolderOpen size={12} style={{ color: T.sub }} />
                <Tag tone={repoLevel ? "ind" : undefined}>{repoLevel ? "仓库级" : "个人级"}</Tag>
                <code className="text-[11px]" style={{ color: T.sub }}>{r}/&lt;name&gt;/SKILL.md</code>
                <span className="text-[10.5px]" style={{ color: T.faint }}>{note}</span>
              </div>
            );
          })}
        </div>
      </Card>

      {skills.length === 0 ? (
        <Card className="p-5">
          <div className="text-[13px] font-semibold">还没有技能</div>
          <div className="text-[11.5px] mt-1.5 leading-relaxed" style={{ color: T.sub }}>
            在上面任一目录下建 <code>&lt;name&gt;/SKILL.md</code>(目录名必须与 name 一致),
            frontmatter 里 name 与 description 必填。只从本地目录发现,不联网下载。
          </div>
          <pre className="mt-3 p-3 rounded-xl text-[11px] leading-relaxed overflow-x-auto"
            style={{ background: T.panel, border: `1px solid ${T.line}`, color: T.ink }}>{EXAMPLE}</pre>
        </Card>
      ) : (
        <div className="grid grid-cols-2 gap-4">
          {skills.map((s) => (
            <Card key={s.name} className="p-5">
              <div className="flex items-center gap-2">
                <span className="text-[10px] font-extrabold tracking-widest" style={{ color: T.indigo }}>SKILL</span>
                <Tag tone={s.origin === "workspace" ? "ind" : undefined}>{s.origin === "workspace" ? "仓库级" : "个人级"}</Tag>
                <span className="ml-auto flex gap-1.5">
                  {s.sensitivity && (
                    <Tag tone={s.sensitivity === "restricted" ? "red" : s.sensitivity === "internal" ? "amb" : undefined}>
                      <Lock size={10} /> {s.sensitivity}
                    </Tag>
                  )}
                </span>
              </div>
              <div className="mt-2.5 text-[15px]"><b>{s.name}</b></div>
              <div className="text-[11.5px] mt-1.5 leading-relaxed" style={{ color: T.sub }}>{s.description}</div>
              <div className="mt-3 text-[10.5px] break-all" style={{ color: T.faint }}>{s.path}</div>
              <div className="mt-1 text-[10.5px]" style={{ color: T.faint }}>
                {s.content_hash.slice(0, 19)}… · 审计 skill.offer / skill.read 记的就是这个哈希
                {s.sensitivity ? " · 用它的任务会把会话密级抬到 " + s.sensitivity : ""}
              </div>
            </Card>
          ))}
        </div>
      )}

      {problems.length > 0 && (
        <Card className="p-4 mt-4" style={{ borderColor: T.amber }}>
          <div className="flex items-center gap-1.5 mb-2">
            <AlertTriangle size={12} style={{ color: T.amber }} />
            <span className="text-[10px] font-semibold tracking-widest" style={{ color: T.faint }}>没能加载的技能</span>
            <span className="ml-auto text-[10px] font-bold" style={{ color: T.amber }}>{problems.length} 项</span>
          </div>
          <div className="space-y-1.5">
            {problems.map((p) => (
              <div key={p} className="text-[11px] leading-relaxed break-all" style={{ color: T.sub }}>{p}</div>
            ))}
          </div>
        </Card>
      )}

      <div className="mt-5 text-[10.5px] leading-relaxed" style={{ color: T.faint }}>
        技能与能力库的分工:技能是人写的"该怎么做"(没有出处运行、没有验真);能力库里的 Capsule 锻造自一次成功的运行,带重放引用与验真记录。
        Agent 在任务里只看到技能目录(名字 + 一句描述),需要时才读全文;读之前会重新校验哈希,发现后被改过的文件一个字都不给。
      </div>
    </div>
  );
}
