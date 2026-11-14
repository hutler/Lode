// Check results for the rows on screen: loaded a page at a time, kept for a minute.

import { useEffect, useRef, useState } from "react";

import { backend, type CheckInfo } from "./api";

/** Targets asked for in one call. */
export const CHECK_PAGE = 50;
/** A row that comes back into view after this long loads its checks again. */
export const FRESH_MS = 60_000;

/** How bad each state is; the badge shows the worst. */
const RANK: Record<CheckInfo["state"], number> = { success: 0, pending: 1, cancelled: 2, error: 3, failure: 4 };

/** One badge for many checks: the worst state, how many are in it, and how many there are. */
export interface CheckSummary {
  state: CheckInfo["state"];
  count: number;
  total: number;
}

export function summarize(checks: CheckInfo[]): CheckSummary | null {
  if (checks.length === 0) return null;
  let state: CheckInfo["state"] = "success";
  for (const c of checks) if (RANK[c.state] > RANK[state]) state = c.state;
  return { state, count: checks.filter((c) => c.state === state).length, total: checks.length };
}

interface Entry {
  checks: CheckInfo[];
  at: number;
}

/** The checks of `targets` (`rN`, `ID@VERSION`), loaded for those not loaded in the last
 * minute. `available` turns false, without an error, on a server without the checks module. */
export function useChecks(workspace: string, targets: string[]) {
  const cache = useRef(new Map<string, Entry>());
  const inflight = useRef(new Set<string>());
  const [available, setAvailable] = useState(true);
  const [, setVersion] = useState(0);
  const key = targets.join(",");
  // Answers that arrive after the view closed, or for another workspace, are dropped.
  const current = useRef<string | null>(workspace);

  useEffect(() => {
    current.current = workspace;
    cache.current.clear();
    setAvailable(true);
    return () => {
      current.current = null;
    };
  }, [workspace]);

  useEffect(() => {
    if (!available || !key) return;
    const now = Date.now();
    const want = key.split(",").filter((t) => {
      const e = cache.current.get(t);
      return !inflight.current.has(t) && (!e || now - e.at > FRESH_MS);
    });
    if (want.length === 0) return;
    const live = () => current.current === workspace;
    // Marked before the first await, so a scroll in the meantime does not ask twice.
    want.forEach((t) => inflight.current.add(t));
    void (async () => {
      try {
        const b = await backend();
        for (let i = 0; i < want.length; i += CHECK_PAGE) {
          const page = want.slice(i, i + CHECK_PAGE);
          try {
            const r = await b.checks(workspace, page);
            if (!live()) return;
            if (!r.available) {
              setAvailable(false);
              return;
            }
            const at = Date.now();
            for (const t of page) cache.current.set(t, { checks: r.checks[t] ?? [], at });
            setVersion((v) => v + 1);
          } catch {
            // Badges are extra: a failed load shows none and is tried again when the row returns.
          } finally {
            page.forEach((t) => inflight.current.delete(t));
          }
        }
      } finally {
        want.forEach((t) => inflight.current.delete(t));
      }
    })();
  }, [workspace, key, available]);

  return {
    available,
    get: (target: string): CheckInfo[] | undefined => cache.current.get(target)?.checks,
  };
}
