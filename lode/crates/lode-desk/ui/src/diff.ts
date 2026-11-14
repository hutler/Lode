// Turns the patch of one file (`lode diff --json`, `"patch"`) into rows for the unified and the
// side-by-side layouts, with word-level highlights inside changed lines.

export interface DiffLine {
  kind: "context" | "add" | "delete";
  text: string;
  /** Line number in the old file (context and deleted lines). */
  old: number | null;
  /** Line number in the new file (context and added lines). */
  new: number | null;
  /** The line's words, marked where they differ from its counterpart. */
  words?: Segment[];
}

export interface Hunk {
  header: string;
  lines: DiffLine[];
}

export interface Segment {
  text: string;
  changed: boolean;
}

/** One row of the side-by-side layout. */
export interface SplitRow {
  left: DiffLine | null;
  right: DiffLine | null;
}

/** Patches longer than this are shown as plain text, without layouts or highlights. */
export const PLAIN_LIMIT = 512 * 1024;
/** Lines longer than this get no word highlights. */
const WORD_LIMIT = 400;

/** The hunks of a unified patch, with line numbers; the header lines are skipped. */
export function parsePatch(patch: string): Hunk[] {
  const hunks: Hunk[] = [];
  let hunk: Hunk | null = null;
  let oldNo = 0;
  let newNo = 0;
  const lines = patch.split("\n");
  if (lines[lines.length - 1] === "") lines.pop();
  for (const l of lines) {
    const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(l);
    if (m) {
      hunk = { header: l, lines: [] };
      hunks.push(hunk);
      oldNo = Number(m[1]);
      newNo = Number(m[2]);
      continue;
    }
    if (!hunk) continue;
    // Some tools write an empty context line as an empty line.
    const mark = l === "" ? " " : l[0];
    const text = l.slice(1);
    if (mark === " ") hunk.lines.push({ kind: "context", text, old: oldNo++, new: newNo++ });
    else if (mark === "-") hunk.lines.push({ kind: "delete", text, old: oldNo++, new: null });
    else if (mark === "+") hunk.lines.push({ kind: "add", text, old: null, new: newNo++ });
    // "\ No newline at end of file" and anything else is not a line of the file.
  }
  for (const h of hunks) highlight(h.lines);
  return hunks;
}

/** Splits a line into words, runs of spaces and single other characters. */
export function tokens(line: string): string[] {
  return line.match(/\w+|\s+|[^\w\s]/g) ?? [];
}

/** Word-level difference of two lines: each side's segments, with the changed ones marked. */
export function wordDiff(a: string, b: string): [Segment[], Segment[]] {
  const x = tokens(a);
  const y = tokens(b);
  // Longest common subsequence of the tokens.
  const n = x.length;
  const m = y.length;
  const table: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      table[i]![j] = x[i] === y[j] ? table[i + 1]![j + 1]! + 1 : Math.max(table[i + 1]![j]!, table[i]![j + 1]!);
    }
  }
  const left: Segment[] = [];
  const right: Segment[] = [];
  const push = (side: Segment[], text: string, changed: boolean) => {
    const last = side[side.length - 1];
    if (last && last.changed === changed) last.text += text;
    else side.push({ text, changed });
  };
  let i = 0;
  let j = 0;
  while (i < n || j < m) {
    if (i < n && j < m && x[i] === y[j]) {
      push(left, x[i]!, false);
      push(right, y[j]!, false);
      i++;
      j++;
    } else if (j < m && (i >= n || table[i]![j + 1]! >= table[i + 1]![j]!)) {
      push(right, y[j++]!, true);
    } else {
      push(left, x[i++]!, true);
    }
  }
  return [left, right];
}

/** How alike two lines are, from 0 to 1: the share of their tokens they have in common. */
function similarity(a: string, b: string): number {
  if (a.length > WORD_LIMIT || b.length > WORD_LIMIT) return a === b ? 1 : 0;
  const [l, r] = wordDiff(a, b);
  const same = (segs: Segment[]) =>
    segs.filter((s) => !s.changed).reduce((n, s) => n + s.text.replace(/\s+/g, "").length, 0);
  const total = a.replace(/\s+/g, "").length + b.replace(/\s+/g, "").length;
  return total === 0 ? 1 : (same(l) + same(r)) / total;
}

/** Lines this alike are shown as one changed line rather than a removal and an addition. */
const ALIKE = 0.5;
/** Runs longer than this are paired in order, without comparing lines. */
const ALIGN_LIMIT = 40;

/** Rows for a run of deleted lines followed by a run of added ones: each deleted line next to
 * the most alike added line after the last pair, the others alone, in file order. */
export function align<T extends { text: string }>(dels: T[], adds: T[]): [T | null, T | null][] {
  const rows: [T | null, T | null][] = [];
  if (dels.length > ALIGN_LIMIT || adds.length > ALIGN_LIMIT) {
    for (let k = 0; k < Math.max(dels.length, adds.length); k++) rows.push([dels[k] ?? null, adds[k] ?? null]);
    return rows;
  }
  let next = 0;
  for (const d of dels) {
    // The most alike added line still free; the earliest of equals.
    let match = -1;
    let best = ALIKE;
    for (let j = next; j < adds.length; j++) {
      const score = similarity(d.text, adds[j]!.text);
      if (score > best || (score === best && match < 0)) {
        match = j;
        best = score;
      }
    }
    if (match < 0) {
      rows.push([d, null]);
      continue;
    }
    for (; next < match; next++) rows.push([null, adds[next]!]);
    rows.push([d, adds[match]!]);
    next = match + 1;
  }
  for (; next < adds.length; next++) rows.push([null, adds[next]!]);
  return rows;
}

/** Splits a hunk's lines into context lines and runs of changes (deletions, then additions). */
function runs(lines: DiffLine[], each: (context: DiffLine | null, dels: DiffLine[], adds: DiffLine[]) => void) {
  let i = 0;
  while (i < lines.length) {
    const l = lines[i]!;
    if (l.kind === "context") {
      each(l, [], []);
      i++;
      continue;
    }
    const dels: DiffLine[] = [];
    const adds: DiffLine[] = [];
    while (i < lines.length && lines[i]!.kind === "delete") dels.push(lines[i++]!);
    while (i < lines.length && lines[i]!.kind === "add") adds.push(lines[i++]!);
    each(null, dels, adds);
  }
}

/** Marks the words that differ between each deleted line and the added line it pairs with. */
function highlight(lines: DiffLine[]) {
  runs(lines, (_, dels, adds) => {
    for (const [d, a] of align(dels, adds)) {
      if (!d || !a || d.text.length > WORD_LIMIT || a.text.length > WORD_LIMIT) continue;
      const [l, r] = wordDiff(d.text, a.text);
      d.words = l;
      a.words = r;
    }
  });
}

/** The side-by-side rows of a hunk: context on both sides, each changed line next to the line
 * it pairs with. */
export function splitRows(h: Hunk): SplitRow[] {
  const rows: SplitRow[] = [];
  runs(h.lines, (context, dels, adds) => {
    if (context) rows.push({ left: context, right: context });
    for (const [left, right] of align(dels, adds)) rows.push({ left, right });
  });
  return rows;
}
