import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";

import { asOpError, backend, type OpError, type Revision, type WorkspaceInfo } from "../api";
import { LANE, layoutGraph, type GraphRow } from "../graph";
import { ErrorBanner } from "./ErrorBanner";

interface Props {
  info: WorkspaceInfo;
  /** One file's history, or the branch's when null. */
  path: string | null;
  /** The revision to show first, when one was asked for (from blame). */
  initialRev: number | null;
  onPath: (path: string | null) => void;
  onBlame: (path: string, rev: number) => void;
  onDiff: (path: string, rev: number) => void;
}

/** Revisions loaded per page. */
export const PAGE = 100;
/** Height of one row of the list, in pixels. */
export const ROW = 40;
/** Rows rendered above and below the visible ones. */
const OVERSCAN = 8;
/** Load the next page when the last loaded row is this close to the visible ones. */
const PREFETCH = 20;
/** Lane centers of the graph. */
const LANE_X = [12, 28];

/** The first line of a message. */
export function summary(message: string): string {
  return message.split("\n")[0] ?? "";
}

export function formatDate(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? iso
    : d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

function fileName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** The badges of a revision: agent, imported, merge. */
function Badges({ r }: { r: Revision }) {
  const agent = r.provenance?.agent;
  const imported = r.provenance?.imported;
  return (
    <>
      {(agent || r.author.kind === "agent") && (
        <span className="tag agent" title={agent?.session ? `Agent session ${agent.session}` : "Made by an agent"}>
          agent
        </span>
      )}
      {imported && (
        <span className="tag" title={`Imported from ${imported.system} ${imported.id}`}>
          imported from {imported.system}
        </span>
      )}
      {r.merged_from.map((m) => (
        <span key={`${m.branch}:${m.from_rev}`} className="tag merge" title={`Merged ${m.branch} r${m.from_rev}..r${m.to_rev}`}>
          merge {m.branch}
        </span>
      ))}
    </>
  );
}

/** The graph cell of one row: the branch's lane, and an edge from lane 1 at each merge. */
function GraphCell({ row, lanes }: { row: GraphRow; lanes: number }) {
  const width = lanes === 1 ? 24 : 40;
  const mid = ROW / 2;
  const x = LANE_X[0];
  const own = row.inherited ? LANE.inherited : LANE.own;
  return (
    <svg className="graph" width={width} height={ROW} aria-hidden="true">
      {row.lineAbove && (
        <line x1={x} y1={0} x2={x} y2={mid} stroke={row.aboveInherited ? LANE.inherited : LANE.own} strokeWidth={2} />
      )}
      {row.lineBelow && <line x1={x} y1={mid} x2={x} y2={ROW} stroke={own} strokeWidth={2} />}
      {row.merges.length > 0 && (
        <>
          <path
            d={`M ${LANE_X[1]} ${ROW} C ${LANE_X[1]} ${mid + 4}, ${x + 6} ${mid}, ${x} ${mid}`}
            fill="none"
            stroke={LANE.merged}
            strokeWidth={2}
          />
          <circle cx={LANE_X[1]} cy={ROW - 3} r={2.5} fill={LANE.merged} />
        </>
      )}
      <circle
        cx={x}
        cy={mid}
        r={row.merges.length > 0 ? 5 : 4}
        fill={row.merges.length > 0 ? "var(--surface)" : own}
        stroke={own}
        strokeWidth={2}
      />
    </svg>
  );
}

/** A branch's or a file's history: a virtual list of revisions with a graph, loaded in pages,
 * and the selected revision's details. */
export function HistoryView({ info, path, initialRev, onPath, onBlame, onDiff }: Props) {
  const [revisions, setRevisions] = useState<Revision[]>([]);
  const [more, setMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const [selected, setSelected] = useState<number | null>(initialRev);
  const [detail, setDetail] = useState<Revision | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(600);
  const listRef = useRef<HTMLDivElement>(null);
  const loadingRef = useRef(false);
  // Requests made for an earlier path are ignored when they finish.
  const generation = useRef(0);

  const loadPage = useCallback(
    async (before: number | null, reset: boolean) => {
      if (loadingRef.current && !reset) return;
      const gen = reset ? ++generation.current : generation.current;
      loadingRef.current = true;
      setLoading(true);
      try {
        const page = await (await backend()).log(info.root, path, before, PAGE);
        if (gen !== generation.current) return;
        setRevisions((old) => (reset ? page.revisions : [...old, ...page.revisions]));
        setMore(page.more);
        setError(null);
        if (reset) setSelected((s) => s ?? page.revisions[0]?.rev ?? null);
      } catch (e) {
        if (gen === generation.current) setError(asOpError(e));
      } finally {
        if (gen === generation.current) {
          loadingRef.current = false;
          setLoading(false);
        }
      }
    },
    [info.root, path],
  );

  useEffect(() => {
    setRevisions([]);
    setMore(false);
    setScrollTop(0);
    if (listRef.current) listRef.current.scrollTop = 0;
    void loadPage(null, true);
  }, [loadPage]);

  useEffect(() => {
    const el = listRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight || 600));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // The selected revision with its changed files.
  useEffect(() => {
    if (selected === null) {
      setDetail(null);
      return;
    }
    let live = true;
    void (async () => {
      try {
        const r = await (await backend()).revision(info.root, selected);
        if (live) setDetail(r);
      } catch (e) {
        if (live) setError(asOpError(e));
      }
    })();
    return () => {
      live = false;
    };
  }, [info.root, selected]);

  const first = Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN);
  const last = Math.min(revisions.length, Math.ceil((scrollTop + height) / ROW) + OVERSCAN);

  // Load the next page as the end of the loaded rows comes near.
  useEffect(() => {
    if (more && !loading && revisions.length - last < PREFETCH) {
      const oldest = revisions[revisions.length - 1];
      if (oldest) void loadPage(oldest.rev, false);
    }
  }, [more, loading, last, revisions, loadPage]);

  const graph = layoutGraph(revisions, info.branch, more);

  const select = (rev: number, index: number) => {
    setSelected(rev);
    const el = listRef.current;
    if (!el) return;
    const top = index * ROW;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + ROW > el.scrollTop + height) el.scrollTop = top + ROW - height;
  };

  const onKey = (e: KeyboardEvent) => {
    const i = revisions.findIndex((r) => r.rev === selected);
    const next = e.key === "ArrowDown" ? i + 1 : e.key === "ArrowUp" ? i - 1 : -1;
    const r = revisions[next];
    if (r && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      select(r.rev, next);
    }
  };

  return (
    <div className="history">
      <section className="history-main" aria-label="History">
        <header className="changes-head">
          {path ? (
            <>
              <h1>
                History of <span className="mono">{fileName(path)}</span>
              </h1>
              <span className="muted small mono" title={path}>
                {path}
              </span>
              <button className="refresh" onClick={() => onPath(null)}>
                Branch history
              </button>
            </>
          ) : (
            <>
              <h1>History of {info.branch}</h1>
              <span className="muted small">{info.depot}</span>
              <button className="refresh" onClick={() => void loadPage(null, true)} disabled={loading}>
                Refresh
              </button>
            </>
          )}
        </header>
        {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
        <div
          className="rev-list"
          ref={listRef}
          role="listbox"
          aria-label="Revisions"
          tabIndex={0}
          onKeyDown={onKey}
          onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
        >
          {revisions.length === 0 && !loading && !error && (
            <p className="empty">{path ? "No revision changed this file." : "No revisions yet."}</p>
          )}
          <div style={{ height: revisions.length * ROW, position: "relative" }}>
            {revisions.slice(first, last).map((r, k) => {
              const i = first + k;
              const row = graph.rows[i];
              if (!row) return null;
              const label =
                (i === 0 && !row.inherited) || row.branchPoint ? r.branch : null;
              return (
                <div
                  key={r.rev}
                  role="option"
                  aria-selected={selected === r.rev}
                  aria-label={`r${r.rev} ${summary(r.message)}`}
                  className={`rev-row ${selected === r.rev ? "selected" : ""} ${row.inherited ? "inherited" : ""}`}
                  style={{ top: i * ROW, height: ROW }}
                  onClick={() => select(r.rev, i)}
                >
                  <GraphCell row={row} lanes={graph.lanes} />
                  <span className="rev-num mono">r{r.rev}</span>
                  <span className="rev-summary">
                    {label && <span className={`tag branch ${row.inherited ? "inherited" : ""}`}>{label}</span>}
                    {summary(r.message)}
                  </span>
                  <span className="rev-badges">
                    <Badges r={r} />
                  </span>
                  <span className="rev-author">{r.author.name}</span>
                  <span className="rev-date">{formatDate(r.created_at)}</span>
                </div>
              );
            })}
          </div>
          {loading && <p className="muted small rev-loading">Loading revisions…</p>}
        </div>
      </section>

      <aside className="details" aria-label="Revision">
        {detail ? (
          <RevisionDetails r={detail} branch={info.branch} onPath={onPath} onBlame={onBlame} onDiff={onDiff} />
        ) : (
          <p className="muted small">Select a revision to see what it changed.</p>
        )}
      </aside>
    </div>
  );
}

const ACTION: Record<string, string> = { add: "A", modify: "M", delete: "D" };

function RevisionDetails({
  r,
  branch,
  onPath,
  onBlame,
  onDiff,
}: {
  r: Revision;
  branch: string;
  onPath: (path: string) => void;
  onBlame: (path: string, rev: number) => void;
  onDiff: (path: string, rev: number) => void;
}) {
  const [, ...rest] = r.message.split("\n");
  const body = rest.join("\n").trim();
  const p = r.provenance;
  return (
    <>
      <h2 className="details-title">
        r{r.rev} <span className="muted">{r.branch !== branch ? `on ${r.branch}` : ""}</span>
      </h2>
      <p className="rev-message">{summary(r.message)}</p>
      {body && <p className="rev-body">{body}</p>}
      <div className="rev-badges block">
        <Badges r={r} />
      </div>
      <dl className="facts compact">
        <dt>Author</dt>
        <dd>
          {r.author.name}
          {r.author.kind === "agent" && <span className="muted"> (agent)</span>}
        </dd>
        {r.submitter.name !== r.author.name && (
          <>
            <dt>Submitted by</dt>
            <dd>{r.submitter.name}</dd>
          </>
        )}
        <dt>Date</dt>
        <dd>{formatDate(r.created_at)}</dd>
        <dt>Branch</dt>
        <dd className="mono">{r.branch}</dd>
        {r.merged_from.map((m) => (
          <div key={`${m.branch}:${m.from_rev}`} className="fact-pair">
            <dt>Merged</dt>
            <dd className="mono">
              {m.branch} r{m.from_rev}..r{m.to_rev}
            </dd>
          </div>
        ))}
        {p?.agent && (
          <>
            <dt>Agent</dt>
            <dd className="mono">
              {Object.entries(p.agent)
                .map(([k, v]) => `${k} ${v}`)
                .join(", ")}
            </dd>
          </>
        )}
        {p?.imported && (
          <>
            <dt>Imported</dt>
            <dd className="mono" title={p.imported.id}>
              {p.imported.system} {p.imported.id.slice(0, 12)}
            </dd>
          </>
        )}
        {p?.client && (
          <>
            <dt>Client</dt>
            <dd className="mono">{p.client}</dd>
          </>
        )}
      </dl>
      <h3 className="details-sub">
        Changed files <span className="group-count">{r.changes?.length ?? 0}</span>
      </h3>
      <ul className="rev-files" aria-label="Changed files">
        {(r.changes ?? []).map((c) => (
          <li key={c.path}>
            <span className={`st st-${c.action === "add" ? "added" : c.action === "delete" ? "deleted" : "modified"}`}>
              {ACTION[c.action] ?? "?"}
            </span>
            <button
              className="link file-path mono"
              title={c.path}
              onClick={() => onDiff(c.path, r.rev)}
              aria-label={`Diff of ${c.path}`}
            >
              {fileName(c.path)}
            </button>
            <span className="rev-file-actions">
              <button className="link" onClick={() => onPath(c.path)} aria-label={`History of ${c.path}`}>
                History
              </button>
              {c.action !== "delete" && (
                <button className="link" onClick={() => onBlame(c.path, r.rev)} aria-label={`Blame ${c.path}`}>
                  Blame
                </button>
              )}
            </span>
          </li>
        ))}
      </ul>
      {r.changes_truncated && <p className="muted small">More files changed than are listed.</p>}
    </>
  );
}
