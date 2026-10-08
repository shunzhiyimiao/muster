/* P1-04:真实代码变更面板。数据来自 worktree 的 git diff,不是示意。
   完整 diff 在弹出窗口里看,默认左右对比(改动前 | 改动后,行行对齐,词级高亮),
   也可切成单栏。侧栏只有 288px 宽,塞一个小黑框等于让人用钥匙孔审代码。 */
import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Columns2, FileCode, GitBranch, Maximize2, Minus, Plus, Rows3, X } from "lucide-react";
import { T } from "../theme";
import { Card, Tag } from "../ui";
import { DiffPayload, FileChange } from "../api";
import { buildSplitRows, parsePatch, Seg, Side } from "./diffModel";

const STATUS_ZH: Record<string, string> = { A: "新增", M: "修改", D: "删除", R: "重命名" };
const MONO = 'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace';

/* Diffchecker 式配色:浅底,删除偏红、新增偏绿,行内改动的词再深一档 */
const C = {
  gutterBg: "#F6F8FA",
  gutterFg: "#8B8FA3",
  line: "#E8EAF0",
  delRow: "#FFEBE9",
  delWord: "#FFC2C0",
  addRow: "#E6FFEC",
  addWord: "#ABF2BC",
  emptyRow: "#F3F4F8",
  hunkBg: "#EEEEFE",
  hunkFg: "#5B5BF5",
  text: "#1F2328",
};

function Segs({ segs, text, tone }: { segs?: Seg[]; text: string; tone: "add" | "del" | "none" }) {
  if (!segs) return <>{text || " "}</>;
  const bg = tone === "add" ? C.addWord : C.delWord;
  return (
    <>
      {segs.map((s, i) =>
        s.changed ? (
          <mark key={i} style={{ background: bg, color: "inherit", borderRadius: 2, padding: "0 1px" }}>{s.text}</mark>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </>
  );
}

function sideBg(k: Side["kind"]): string {
  return k === "del" ? C.delRow : k === "add" ? C.addRow : k === "empty" ? C.emptyRow : "#fff";
}

/* ------------------------------------------------------------------ 弹出窗口 */

function DiffModal({ diff, focus, onClose }: { diff: DiffPayload; focus: string | null; onClose: () => void }) {
  const chunks = useMemo(() => parsePatch(diff.patch), [diff.patch]);
  const byPath = useMemo(() => new Map(diff.files.map((f) => [f.path, f] as [string, FileChange])), [diff.files]);
  const refs = useRef<Record<string, HTMLElement | null>>({});
  const [mode, setMode] = useState<"split" | "unified">("split");
  const [wrap, setWrap] = useState(true);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    if (focus) refs.current[focus]?.scrollIntoView({ block: "start" });
  }, [focus, mode]);

  const jump = (path: string) => refs.current[path]?.scrollIntoView({ block: "start", behavior: "smooth" });
  const textStyle: React.CSSProperties = {
    whiteSpace: wrap ? "pre-wrap" : "pre",
    wordBreak: wrap ? "break-all" : "normal",
    color: C.text,
  };
  const gutter: React.CSSProperties = {
    background: C.gutterBg,
    color: C.gutterFg,
    textAlign: "right",
    padding: "0 8px",
    whiteSpace: "nowrap",
    userSelect: "none",
    width: 1,
    verticalAlign: "top",
  };

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-5"
      style={{ background: "rgba(23,24,28,.48)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="完整代码变更"
        className="flex flex-col rounded-2xl overflow-hidden fade"
        style={{ width: "min(1440px, 96vw)", height: "90vh", background: "#fff", boxShadow: "0 24px 64px rgba(23,24,28,.32)" }}
      >
        {/* 头部 */}
        <div className="flex items-center gap-3 px-5 py-3 shrink-0 flex-wrap" style={{ borderBottom: `1px solid ${T.line}` }}>
          <span className="text-[13px] font-bold">代码变更</span>
          <span className="inline-flex items-center gap-1 text-[11px] min-w-0" style={{ color: T.sub }}>
            <GitBranch size={12} />
            <span className="truncate" title={diff.branch}>{diff.branch}</span>
          </span>
          <span className="text-[11px]" style={{ color: T.sub }}>{diff.files_changed} 个文件</span>
          <span className="inline-flex items-center gap-1.5 text-[11px] font-semibold">
            <span style={{ color: T.green }}>+{diff.insertions}</span>
            <span style={{ color: T.red }}>−{diff.deletions}</span>
          </span>

          <div className="ml-auto inline-flex rounded-lg p-0.5 text-[11px] font-semibold" style={{ background: T.soft }}>
            {(
              [
                ["split", "左右对比", Columns2],
                ["unified", "单栏", Rows3],
              ] as const
            ).map(([m, label, Icon]) => (
              <button
                key={m}
                onClick={() => setMode(m)}
                className="inline-flex items-center gap-1 px-2.5 py-1 rounded-md"
                style={{ background: mode === m ? "#fff" : "transparent", color: mode === m ? T.ink : T.sub, boxShadow: mode === m ? "0 1px 3px rgba(23,24,28,.12)" : "none" }}
              >
                <Icon size={12} /> {label}
              </button>
            ))}
          </div>
          <label className="inline-flex items-center gap-1.5 text-[11px] select-none" style={{ color: T.sub }}>
            <input id="diff-wrap" type="checkbox" checked={wrap} onChange={(e) => setWrap(e.target.checked)} />
            长行折行
          </label>
          <button
            onClick={onClose}
            title="关闭(Esc)"
            aria-label="关闭"
            className="w-8 h-8 rounded-lg inline-flex items-center justify-center"
            style={{ background: T.soft, color: T.sub }}
          >
            <X size={14} />
          </button>
        </div>

        {/* 主体:左侧文件导航,右侧 diff */}
        <div className="flex flex-1 min-h-0">
          <nav className="w-60 shrink-0 overflow-y-auto py-2" style={{ borderRight: `1px solid ${T.line}`, background: T.panel }}>
            <div className="px-4 pb-1.5 text-[10px] font-semibold tracking-widest" style={{ color: T.faint }}>文件</div>
            {chunks.map((c) => {
              const f = byPath.get(c.path);
              return (
                <button
                  key={c.path}
                  onClick={() => jump(c.path)}
                  className="w-full text-left px-4 py-1.5 flex items-center gap-1.5 text-[11px] hover:bg-[#ECEDF3]"
                  title={c.path}
                >
                  <FileCode size={11} style={{ color: T.sub, flex: "none" }} />
                  <span className="truncate flex-1">{c.path}</span>
                  {f && (
                    <span className="inline-flex items-center gap-1 text-[10px] shrink-0">
                      {f.added > 0 && <span style={{ color: T.green }}>+{f.added}</span>}
                      {f.removed > 0 && <span style={{ color: T.red }}>−{f.removed}</span>}
                    </span>
                  )}
                </button>
              );
            })}
          </nav>

          <div className="flex-1 min-w-0 overflow-auto" style={{ background: "#fff" }}>
            {chunks.map((c) => {
              const f = byPath.get(c.path);
              return (
                <section key={c.path} ref={(el) => { refs.current[c.path] = el; }} style={{ borderBottom: `1px solid ${C.line}` }}>
                  {/* 文件标题(吸顶) */}
                  <div
                    className="sticky top-0 z-10 flex items-center gap-2 px-4 py-2 text-[11.5px]"
                    style={{ background: "#FBFBFD", borderBottom: `1px solid ${C.line}` }}
                  >
                    <FileCode size={12} style={{ color: T.sub }} />
                    <span className="font-semibold" style={{ fontFamily: MONO, color: C.text }}>{c.path}</span>
                    {f && <Tag tone={f.status === "A" ? "grn" : f.status === "D" ? "red" : undefined}>{STATUS_ZH[f.status] ?? f.status}</Tag>}
                    {f && (
                      <span className="ml-auto inline-flex items-center gap-1.5 text-[10.5px] font-semibold">
                        {f.added > 0 && <span style={{ color: T.green }}>+{f.added}</span>}
                        {f.removed > 0 && <span style={{ color: T.red }}>−{f.removed}</span>}
                      </span>
                    )}
                  </div>

                  {mode === "split" ? (
                    <table style={{ width: "100%", tableLayout: wrap ? "fixed" : "auto", borderCollapse: "collapse", fontFamily: MONO, fontSize: 12, lineHeight: 1.65 }}>
                      <colgroup>
                        <col style={{ width: 52 }} />
                        <col />
                        <col style={{ width: 52 }} />
                        <col />
                      </colgroup>
                      <thead>
                        <tr className="text-[10.5px]" style={{ background: C.gutterBg, color: C.gutterFg }}>
                          <th colSpan={2} className="text-left font-semibold px-3 py-1" style={{ borderBottom: `1px solid ${C.line}` }}>改动前</th>
                          <th colSpan={2} className="text-left font-semibold px-3 py-1" style={{ borderBottom: `1px solid ${C.line}`, borderLeft: `1px solid ${C.line}` }}>改动后</th>
                        </tr>
                      </thead>
                      <tbody>
                        {buildSplitRows(c.lines).map((r, i) =>
                          r.kind === "hunk" ? (
                            <tr key={i}>
                              <td colSpan={4} className="px-3 py-0.5 text-[10.5px]" style={{ background: C.hunkBg, color: C.hunkFg, fontFamily: MONO }}>
                                {r.text}
                              </td>
                            </tr>
                          ) : (
                            <tr key={i}>
                              <td style={gutter}>{r.left.no ?? ""}</td>
                              <td className="px-3 align-top" style={{ ...textStyle, background: sideBg(r.left.kind) }}>
                                <Segs segs={r.left.segs} text={r.left.text} tone="del" />
                              </td>
                              <td style={{ ...gutter, borderLeft: `1px solid ${C.line}` }}>{r.right.no ?? ""}</td>
                              <td className="px-3 align-top" style={{ ...textStyle, background: sideBg(r.right.kind) }}>
                                <Segs segs={r.right.segs} text={r.right.text} tone="add" />
                              </td>
                            </tr>
                          ),
                        )}
                      </tbody>
                    </table>
                  ) : (
                    <table style={{ width: wrap ? "100%" : "max-content", minWidth: "100%", borderCollapse: "collapse", fontFamily: MONO, fontSize: 12, lineHeight: 1.65 }}>
                      <tbody>
                        {c.lines.map((l, i) => {
                          const bg = l.kind === "add" ? C.addRow : l.kind === "del" ? C.delRow : l.kind === "hunk" ? C.hunkBg : "#fff";
                          const fg = l.kind === "hunk" ? C.hunkFg : l.kind === "meta" ? C.gutterFg : C.text;
                          const mark = l.kind === "add" ? "+" : l.kind === "del" ? "−" : " ";
                          const isGutter = l.kind === "hunk" || l.kind === "meta";
                          return (
                            <tr key={i} style={{ background: bg }}>
                              <td style={gutter}>{isGutter ? "" : l.oldNo ?? ""}</td>
                              <td style={{ ...gutter, borderRight: `1px solid ${C.line}` }}>{isGutter ? "" : l.newNo ?? ""}</td>
                              <td className="text-center align-top select-none" style={{ width: 1, padding: "0 4px 0 8px", color: fg }}>{mark}</td>
                              <td className="align-top pr-4" style={{ ...textStyle, color: fg }}>{l.text || " "}</td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  )}
                </section>
              );
            })}
            {chunks.length === 0 && <div className="p-6 text-[12px]" style={{ color: T.sub }}>这份 diff 没有可显示的内容。</div>}
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}

/* ------------------------------------------------------------------ 面板内的摘要 */

/** 文件列表 + 「查看完整 diff」弹窗。任务卡旁的变更面板与审批页共用这一份渲染。 */
export function DiffBody({ diff, emptyText = "没有代码改动。" }: { diff: DiffPayload; emptyText?: string }) {
  const [open, setOpen] = useState<string | null | false>(false); // false = 关;null = 开但不定位;string = 开并定位到该文件
  if (diff.files_changed === 0) {
    return <div className="text-[10.5px]" style={{ color: T.faint }}>{emptyText}</div>;
  }
  return (
    <>
      <div className="space-y-1">
        {diff.files.map((f) => (
          <button
            key={f.path}
            onClick={() => setOpen(f.path)}
            title={`${f.path} · 点开在大窗口里看这个文件的改动`}
            className="w-full flex items-center gap-1.5 text-[10.5px] rounded-lg px-2 py-1.5 text-left hover:bg-[#ECEDF3]"
            style={{ background: T.panel }}
          >
            <FileCode size={11} style={{ color: T.sub, flex: "none" }} />
            <span className="truncate flex-1">{f.path}</span>
            <Tag tone={f.status === "A" ? "grn" : f.status === "D" ? "red" : undefined}>
              {STATUS_ZH[f.status] ?? f.status}
            </Tag>
            {f.added > 0 && (
              <span className="flex items-center" style={{ color: T.green }}><Plus size={9} />{f.added}</span>
            )}
            {f.removed > 0 && (
              <span className="flex items-center" style={{ color: T.red }}><Minus size={9} />{f.removed}</span>
            )}
          </button>
        ))}
      </div>

      <button
        onClick={() => setOpen(null)}
        className="w-full mt-2.5 py-1.5 rounded-lg text-[11px] font-semibold inline-flex items-center justify-center gap-1.5"
        style={{ background: T.indigoSoft, color: T.indigo }}
      >
        <Maximize2 size={11} /> 查看完整 diff
      </button>
      {open !== false && <DiffModal diff={diff} focus={open} onClose={() => setOpen(false)} />}
    </>
  );
}

export function DiffPanel({ diff }: { diff: DiffPayload | null }) {
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

      <DiffBody diff={diff} emptyText="本次运行没有产生代码改动。" />
      {!none && (
        <div className="mt-2 text-[9.5px] leading-relaxed" style={{ color: T.faint }}>
          改动在隔离分支上,主仓未被触碰。合入与推送需单独授权(P5),Runner 不代劳。
        </div>
      )}
    </Card>
  );
}
