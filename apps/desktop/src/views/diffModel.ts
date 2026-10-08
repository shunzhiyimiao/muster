/* 统一 diff 的解析与左右对比模型(纯逻辑,无 React),弹窗与测试共用。 */

export type LineKind = "add" | "del" | "ctx" | "hunk" | "meta";
export interface DiffLine {
  kind: LineKind;
  text: string;
  oldNo?: number;
  newNo?: number;
}
export interface FileChunk {
  path: string;
  lines: DiffLine[];
}

/** 按文件切开统一 diff,并给每行算出新旧行号(审代码没有行号等于没有坐标)。 */
export function parsePatch(patch: string): FileChunk[] {
  const chunks: FileChunk[] = [];
  let cur: FileChunk | null = null;
  let inHunk = false;
  let oldNo = 0;
  let newNo = 0;
  const rows = patch.split("\n");
  if (rows.length && rows[rows.length - 1] === "") rows.pop();
  for (const raw of rows) {
    if (raw.startsWith("diff --git ")) {
      const m = raw.match(/ b\/(.+)$/);
      cur = { path: m ? m[1] : raw.slice("diff --git ".length), lines: [] };
      chunks.push(cur);
      inHunk = false;
      continue;
    }
    if (!cur) continue;
    if (raw.startsWith("@@")) {
      const m = raw.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
      oldNo = m ? parseInt(m[1], 10) : 0;
      newNo = m ? parseInt(m[2], 10) : 0;
      inHunk = true;
      cur.lines.push({ kind: "hunk", text: raw });
      continue;
    }
    // 文件头部(index / --- / +++ / 模式与重命名说明)只在第一个 @@ 之前出现;
    // 之后以 --- 开头的只能是被删掉的、内容恰好以 -- 开头的代码行
    if (!inHunk || raw.startsWith("\\")) {
      cur.lines.push({ kind: "meta", text: raw });
      continue;
    }
    if (raw.startsWith("+")) {
      cur.lines.push({ kind: "add", text: raw.slice(1), newNo: newNo++ });
    } else if (raw.startsWith("-")) {
      cur.lines.push({ kind: "del", text: raw.slice(1), oldNo: oldNo++ });
    } else {
      cur.lines.push({ kind: "ctx", text: raw.slice(1), oldNo: oldNo++, newNo: newNo++ });
    }
  }
  return chunks;
}

/* ------------------------------------------------------------------ 词级比较 */

export interface Seg {
  text: string;
  changed: boolean;
}

/** 单词 / 空白 / 单个符号各算一个 token;中文没有 \w 边界,按字比较,正好够用。 */
function tokenize(s: string): string[] {
  return s.match(/\w+|\s+|[^\w\s]/g) ?? [];
}

/** 一对删除行 / 新增行的词级差异:两边各自标出"只属于自己"的片段(LCS)。 */
export function tokenDiff(a: string, b: string): [Seg[], Seg[]] {
  const ta = tokenize(a);
  const tb = tokenize(b);
  const n = ta.length;
  const m = tb.length;
  // 行太长就不做词级比较:整行标为改动,别为一行日志把界面卡死
  if (n * m > 250_000) {
    return [[{ text: a, changed: true }], [{ text: b, changed: true }]];
  }
  const dp: Uint16Array[] = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = ta[i] === tb[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const la: Seg[] = [];
  const lb: Seg[] = [];
  const push = (arr: Seg[], text: string, changed: boolean) => {
    const last = arr[arr.length - 1];
    if (last && last.changed === changed) last.text += text;
    else arr.push({ text, changed });
  };
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (ta[i] === tb[j]) {
      push(la, ta[i], false);
      push(lb, tb[j], false);
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      push(la, ta[i], true);
      i++;
    } else {
      push(lb, tb[j], true);
      j++;
    }
  }
  while (i < n) push(la, ta[i++], true);
  while (j < m) push(lb, tb[j++], true);
  return [la, lb];
}

/* ------------------------------------------------------------------ 左右对齐 */

export type SideKind = "ctx" | "add" | "del" | "empty";
export interface Side {
  kind: SideKind;
  no?: number;
  text: string;
  /** 词级片段;只有成对的删除 / 新增行才有 */
  segs?: Seg[];
}
export type SplitRow = { kind: "pair"; left: Side; right: Side } | { kind: "hunk"; text: string };

/**
 * 把一个文件的 diff 行排成左右两栏:上下文两边都有;连续的一段删除与一段新增
 * 按顺序配对(第 i 个删除对第 i 个新增),多出来的一边补空行,这样行号与视线
 * 都不会错位——Diffchecker 的看法。文件头部的 meta 行在对比视图里没有意义,不排。
 */
export function buildSplitRows(lines: DiffLine[]): SplitRow[] {
  const rows: SplitRow[] = [];
  let dels: DiffLine[] = [];
  let adds: DiffLine[] = [];
  const flush = () => {
    const n = Math.max(dels.length, adds.length);
    for (let i = 0; i < n; i++) {
      const d = dels[i];
      const a = adds[i];
      let ls: Seg[] | undefined;
      let rs: Seg[] | undefined;
      if (d && a) [ls, rs] = tokenDiff(d.text, a.text);
      rows.push({
        kind: "pair",
        left: d ? { kind: "del", no: d.oldNo, text: d.text, segs: ls } : { kind: "empty", text: "" },
        right: a ? { kind: "add", no: a.newNo, text: a.text, segs: rs } : { kind: "empty", text: "" },
      });
    }
    dels = [];
    adds = [];
  };
  for (const l of lines) {
    if (l.kind === "del") {
      dels.push(l);
      continue;
    }
    if (l.kind === "add") {
      adds.push(l);
      continue;
    }
    flush();
    if (l.kind === "ctx") {
      rows.push({
        kind: "pair",
        left: { kind: "ctx", no: l.oldNo, text: l.text },
        right: { kind: "ctx", no: l.newNo, text: l.text },
      });
    } else if (l.kind === "hunk") {
      rows.push({ kind: "hunk", text: l.text });
    }
  }
  flush();
  return rows;
}
