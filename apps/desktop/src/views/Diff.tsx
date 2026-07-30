/* P1-04:真实代码变更面板。数据来自 worktree 的 git diff,不是示意。 */
import { useState } from "react";
import { FileCode, GitBranch, Minus, Plus } from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { DiffPayload } from "../api";

const STATUS_ZH: Record<string, string> = { A: "新增", M: "修改", D: "删除", R: "重命名" };

export function DiffPanel({ diff }: { diff: DiffPayload | null }) {
  const [open, setOpen] = useState(false);
  if (!diff) {
    return (
      <Card className="p-4">
        <div className="text-[10px] font-semibold tracking-widest mb-2" style={{ color: T.faint }}>代码变更</div>
        <div className="text-[10.5px] leading-relaxed" style={{ color: T.faint }}>
          用「▶ 任务」发起后,Agent 在隔离分支上的改动会显示在这里。
        </div>
      </Card>
    );
  }
  const none = diff.files_changed === 0;
  return (
    <Card className="p-4">
      <div className="flex items-center gap-1.5 mb-2">
        <span className="text-[10px] font-semibold tracking-widest" style={{ color: T.faint }}>代码变更</span>
        {!none && (
          <span className="ml-auto flex items-center gap-1.5 text-[10px] font-semibold">
            <span style={{ color: T.green }}>+{diff.insertions}</span>
            <span style={{ color: T.red }}>−{diff.deletions}</span>
          </span>
        )}
      </div>

      <div className="flex items-center gap-1.5 text-[10px] mb-2.5" style={{ color: T.sub }}>
        <GitBranch size={11} />
        <span className="truncate" title={diff.branch}>{diff.branch}</span>
      </div>

      {none ? (
        <div className="text-[10.5px]" style={{ color: T.faint }}>本次运行没有产生代码改动。</div>
      ) : (
        <>
          <div className="space-y-1">
            {diff.files.map((f) => (
              <div key={f.path} className="flex items-center gap-1.5 text-[10.5px] rounded-lg px-2 py-1.5"
                style={{ background: T.panel }}>
                <FileCode size={11} style={{ color: T.sub, flex: "none" }} />
                <span className="truncate flex-1" title={f.path}>{f.path}</span>
                <Tag tone={f.status === "A" ? "grn" : f.status === "D" ? "red" : undefined}>
                  {STATUS_ZH[f.status] ?? f.status}
                </Tag>
                {f.added > 0 && (
                  <span className="flex items-center" style={{ color: T.green }}><Plus size={9} />{f.added}</span>
                )}
                {f.removed > 0 && (
                  <span className="flex items-center" style={{ color: T.red }}><Minus size={9} />{f.removed}</span>
                )}
              </div>
            ))}
          </div>

          <button onClick={() => setOpen((o) => !o)}
            className="w-full mt-2.5 py-1.5 rounded-lg text-[11px] font-semibold"
            style={{ background: T.indigoSoft, color: T.indigo }}>
            {open ? "收起 diff" : "查看完整 diff"}
          </button>
          {open && (
            <pre className="mt-2 rounded-lg p-2.5 overflow-x-auto text-[10px] leading-relaxed fade"
              style={{ background: "#17181C", color: "#D6D8E3", maxHeight: 320 }}>
              {diff.patch.split("\n").map((line, i) => (
                <div key={i} style={{
                  color: line.startsWith("+") && !line.startsWith("+++") ? "#7EE2A8"
                    : line.startsWith("-") && !line.startsWith("---") ? "#F5A3A5"
                    : line.startsWith("@@") ? "#8385F2" : "#D6D8E3",
                }}>
                  {line || " "}
                </div>
              ))}
            </pre>
          )}
          <div className="mt-2 text-[9.5px] leading-relaxed" style={{ color: T.faint }}>
            改动在隔离分支上,主仓未被触碰。合入与推送需单独授权(P5),Runner 不代劳。
          </div>
        </>
      )}
    </Card>
  );
}
