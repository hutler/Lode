// Reading and editing a file with conflict markers, and marking what each side changed.

/** A run of lines outside any conflict, or one conflict region. */
export type Part =
  | { kind: "text"; lines: string[] }
  | {
      kind: "conflict";
      mine: string[];
      base: string[] | null;
      theirs: string[];
      mineLabel: string;
      theirsLabel: string;
    };

const START = /^<{7}(?: (.*))?$/;
const BASE = /^\|{7}(?: .*)?$/;
const MID = /^={7}$/;
const END = /^>{7}(?: (.*))?$/;

/** Lines without their line ends (a final newline does not add an empty line). */
export function lines(text: string): string[] {
  if (text === "") return [];
  const all = text.split(/\r?\n/);
  if (all[all.length - 1] === "") all.pop();
  return all;
}

/** Splits a file into plain runs and conflict regions. Markers that do not close are plain text. */
export function parseConflicts(text: string): Part[] {
  const src = lines(text);
  const parts: Part[] = [];
  let plain: string[] = [];
  let i = 0;
  while (i < src.length) {
    const start = START.exec(src[i]!);
    if (start) {
      // Find this region's middle and end before taking it as a conflict.
      let j = i + 1;
      let baseAt = -1;
      let midAt = -1;
      let endAt = -1;
      for (; j < src.length; j++) {
        const l = src[j]!;
        if (START.test(l)) break;
        if (midAt < 0 && baseAt < 0 && BASE.test(l)) baseAt = j;
        else if (midAt < 0 && MID.test(l)) midAt = j;
        else if (midAt >= 0 && END.test(l)) {
          endAt = j;
          break;
        }
      }
      if (midAt >= 0 && endAt >= 0) {
        if (plain.length) parts.push({ kind: "text", lines: plain });
        plain = [];
        parts.push({
          kind: "conflict",
          mine: src.slice(i + 1, baseAt >= 0 ? baseAt : midAt),
          base: baseAt >= 0 ? src.slice(baseAt + 1, midAt) : null,
          theirs: src.slice(midAt + 1, endAt),
          mineLabel: start[1] ?? "mine",
          theirsLabel: END.exec(src[endAt]!)?.[1] ?? "theirs",
        });
        i = endAt + 1;
        continue;
      }
    }
    plain.push(src[i]!);
    i += 1;
  }
  if (plain.length) parts.push({ kind: "text", lines: plain });
  return parts;
}

/** How many conflict regions `text` still has. */
export function countConflicts(text: string): number {
  return parseConflicts(text).filter((p) => p.kind === "conflict").length;
}

export type Take = "mine" | "theirs" | "both";

function render(parts: Part[], eol: string, finalNewline: boolean): string {
  const out: string[] = [];
  for (const p of parts) {
    if (p.kind === "text") out.push(...p.lines);
    else {
      out.push(`<<<<<<< ${p.mineLabel}`, ...p.mine);
      if (p.base) out.push("|||||||", ...p.base);
      out.push("=======", ...p.theirs, `>>>>>>> ${p.theirsLabel}`);
    }
  }
  return out.join(eol) + (finalNewline && out.length ? eol : "");
}

/** `text` with its `index`-th conflict region replaced by one side, or mine then theirs. */
export function takeRegion(text: string, index: number, take: Take): string {
  const eol = text.includes("\r\n") ? "\r\n" : "\n";
  const finalNewline = text.endsWith("\n");
  const parts = parseConflicts(text);
  let n = -1;
  const next = parts.map((p): Part => {
    if (p.kind !== "conflict") return p;
    n += 1;
    if (n !== index) return p;
    const chosen = take === "mine" ? p.mine : take === "theirs" ? p.theirs : [...p.mine, ...p.theirs];
    return { kind: "text", lines: chosen };
  });
  return render(next, eol, finalNewline);
}

/** Which lines of `side` are not in `base`, by a longest common subsequence of lines. Null when
 * the files are too long to compare here. */
export function changedLines(base: string[], side: string[], limit = 4_000_000): Set<number> | null {
  const n = base.length;
  const m = side.length;
  if (n * m > limit) return null;
  // Skip the common start and end, then compare what is left.
  let lo = 0;
  while (lo < n && lo < m && base[lo] === side[lo]) lo++;
  let hiB = n;
  let hiS = m;
  while (hiB > lo && hiS > lo && base[hiB - 1] === side[hiS - 1]) {
    hiB--;
    hiS--;
  }
  const a = base.slice(lo, hiB);
  const b = side.slice(lo, hiS);
  const w = b.length + 1;
  const dp = new Uint32Array((a.length + 1) * w);
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      dp[i * w + j] = a[i] === b[j] ? dp[(i + 1) * w + j + 1]! + 1 : Math.max(dp[(i + 1) * w + j]!, dp[i * w + j + 1]!);
    }
  }
  const changed = new Set<number>();
  let i = 0;
  let j = 0;
  while (j < b.length) {
    if (i < a.length && a[i] === b[j]) {
      i++;
      j++;
    } else if (i < a.length && dp[(i + 1) * w + j]! >= dp[i * w + j + 1]!) {
      i++;
    } else {
      changed.add(lo + j);
      j++;
    }
  }
  return changed;
}
