// Matching depot paths against view patterns the way the server does (globset with
// `literal_separator`), and turning folder ticks into patterns.

import type { ViewSpec } from "./api";

function toRegex(glob: string): RegExp {
  let out = "";
  let i = 0;
  let inClass = false;
  let braces = 0;
  while (i < glob.length) {
    const c = glob[i]!;
    if (inClass) {
      out += c === "\\" ? "\\\\" : c;
      if (c === "]") inClass = false;
      i += 1;
      continue;
    }
    if (c === "*" && glob[i + 1] === "*") {
      const atStart = i === 0 || glob[i - 1] === "/";
      const slashAfter = glob[i + 2] === "/";
      if (atStart && slashAfter) {
        out += "(?:.*/)?";
        i += 3;
      } else {
        out += ".*";
        i += 2;
      }
      continue;
    }
    if (c === "*") out += "[^/]*";
    else if (c === "?") out += "[^/]";
    else if (c === "[") {
      inClass = true;
      out += glob[i + 1] === "!" ? "[^" : "[";
      if (glob[i + 1] === "!") i += 1;
    } else if (c === "{") {
      braces += 1;
      out += "(?:";
    } else if (c === "}" && braces > 0) {
      braces -= 1;
      out += ")";
    } else if (c === "," && braces > 0) out += "|";
    else out += c.replace(/[.+^$()|\\/]/g, "\\$&");
    i += 1;
  }
  return new RegExp(`^${out}$`);
}

const cache = new Map<string, RegExp | null>();

/** Whether `path` matches the glob `pattern`; a pattern that does not compile matches nothing. */
export function globMatch(pattern: string, path: string): boolean {
  let re = cache.get(pattern);
  if (re === undefined) {
    try {
      re = toRegex(pattern);
    } catch {
      re = null;
    }
    cache.set(pattern, re);
  }
  return !!re && re.test(path);
}

/** Whether the view takes `path` (include empty means everything; exclude wins). */
export function inView(spec: ViewSpec, path: string): boolean {
  const included = spec.include.length === 0 || spec.include.some((p) => globMatch(p, path));
  return included && !spec.exclude.some((p) => globMatch(p, path));
}

/** A name no real pattern targets, standing for "anything in this folder". */
const PROBE = "⁣any";

export type FolderState = {
  /** Files in the folder are in the view. */
  on: boolean;
  /** Patterns go deeper into it, so some of it may differ. */
  mixed: boolean;
  /** An exclude pattern above the folder keeps it out; ticking cannot bring it back. */
  lockedBy: string | null;
};

/** How the view treats the folder `dir`. */
export function folderState(spec: ViewSpec, dir: string): FolderState {
  const probe = `${dir}/${PROBE}`;
  const own = `${dir}/**`;
  const on = inView(spec, probe);
  const mixed = [...spec.include, ...spec.exclude].some((p) => p.startsWith(`${dir}/`) && p !== own);
  const lockedBy = on ? null : (spec.exclude.find((p) => p !== own && globMatch(p, probe)) ?? null);
  return { on, mixed, lockedBy };
}

/** The view with folder `dir` ticked in or out. */
export function tickFolder(spec: ViewSpec, dir: string, on: boolean): ViewSpec {
  const own = `${dir}/**`;
  const probe = `${dir}/${PROBE}`;
  let include = [...spec.include];
  let exclude = [...spec.exclude];
  // Patterns inside the folder no longer mean anything once the whole folder is decided.
  const inside = (p: string) => p.startsWith(`${dir}/`) && p !== own;
  include = include.filter((p) => !inside(p));
  exclude = exclude.filter((p) => !inside(p));
  if (on) {
    exclude = exclude.filter((p) => p !== own);
    if (!inView({ include, exclude }, probe) && include.length > 0) include.push(own);
  } else {
    const others = include.filter((p) => p !== own);
    if (include.includes(own) && others.length > 0 && !inView({ include: others, exclude }, probe)) {
      include = others;
    } else if (!exclude.includes(own)) {
      exclude.push(own);
    }
  }
  return { include, exclude };
}

/** Patterns as typed, one per line, without blanks. */
export function parsePatterns(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
}
