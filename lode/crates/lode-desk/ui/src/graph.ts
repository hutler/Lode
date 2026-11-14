// Lays out the history graph of one branch. A Lode branch is a single line of revisions (no
// Git-style DAG): merges are recorded on the merging revision as ranges of another branch, and
// a branch's history continues into its parent's below the branch point. So the graph is the
// branch's own lane, with an incoming edge from a second lane at each merge, and the lane
// changing color where the history becomes the parent branch's.

import type { MergedFrom, Revision } from "./api";

export interface GraphRow {
  rev: number;
  /** The branch this revision was made on (the parent's below the branch point). */
  branch: string;
  /** True when the revision belongs to an ancestor branch. */
  inherited: boolean;
  /** Merges this revision records; each draws an edge from lane 1. */
  merges: MergedFrom[];
  /** Draw the line above the node (false for the newest loaded revision). */
  lineAbove: boolean;
  /** The line above belongs to an ancestor branch (the row above is inherited). */
  aboveInherited: boolean;
  /** Draw the line below the node (false for the oldest revision there is). */
  lineBelow: boolean;
  /** True on the first row of the parent's history: the branch point. */
  branchPoint: boolean;
}

export interface Graph {
  rows: GraphRow[];
  /** Lanes used: 1, or 2 when any merge is shown. */
  lanes: number;
}

/** The graph for `revisions` (newest first) of `branch`; `more` when older revisions exist
 * that are not loaded yet. */
export function layoutGraph(revisions: Revision[], branch: string, more = false): Graph {
  let lanes = 1;
  const rows = revisions.map((r, i) => {
    const inherited = r.branch !== branch;
    const prev = revisions[i - 1];
    if (r.merged_from.length > 0) lanes = 2;
    return {
      rev: r.rev,
      branch: r.branch,
      inherited,
      merges: r.merged_from,
      lineAbove: i > 0,
      aboveInherited: prev !== undefined && prev.branch !== branch,
      lineBelow: r.parent !== null && (i < revisions.length - 1 || more),
      branchPoint: inherited && (prev === undefined || prev.branch !== r.branch),
    };
  });
  return { rows, lanes };
}

/** Colors of the lanes: the branch's own, its ancestors', and merged branches'. */
export const LANE = { own: "var(--accent)", inherited: "var(--muted)", merged: "var(--gold)" };
