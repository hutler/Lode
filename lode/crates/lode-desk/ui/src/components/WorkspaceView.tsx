import { useCallback, useEffect, useState } from "react";

import { asOpError, backend, type OpError, type StatusResult, type WorkspaceInfo } from "../api";

interface Props {
  info: WorkspaceInfo;
  onError: (e: OpError) => void;
}

function hostOf(server: string): string {
  try {
    return new URL(server).host;
  } catch {
    return server;
  }
}

const SECTIONS = ["Changes", "History", "Locks", "Shelves"] as const;

/** One open workspace: the sidebar and its overview. */
export function WorkspaceView({ info, onError }: Props) {
  const [status, setStatus] = useState<StatusResult | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const b = await backend();
      setStatus(await b.status(info.root));
    } catch (e) {
      onError(asOpError(e));
    } finally {
      setLoading(false);
    }
  }, [info.root, onError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const changed = status?.files.filter((f) => f.status !== "untracked" && f.status !== "unchanged").length ?? 0;
  const untracked = status?.files.filter((f) => f.status === "untracked").length ?? 0;
  const needLock = status?.files.filter((f) => f.needs_lock).length ?? 0;
  const locked = status?.files.filter((f) => f.locked).length ?? 0;

  return (
    <div className="workspace">
      <aside className="sidebar" aria-label="Workspace">
        <div className="sidebar-head">
          <div className="sidebar-depot">{info.depot}</div>
          <div className="sidebar-server">{hostOf(info.server)}</div>
        </div>
        <div className="sidebar-branch">
          <span className="branch-icon" aria-hidden="true">⑂</span>
          {status?.branch ?? info.branch}
          <span className="rev">r{status?.rev ?? info.rev}</span>
        </div>
        <nav className="sidebar-nav">
          {SECTIONS.map((s, i) => (
            <button key={s} className={i === 0 ? "active" : ""} disabled={i !== 0}>
              {s}
              {s === "Changes" && changed > 0 && <span className="count">{changed}</span>}
              {s === "Locks" && locked > 0 && <span className="count">{locked}</span>}
            </button>
          ))}
        </nav>
      </aside>
      <section className="overview" aria-label="Overview">
        <header className="overview-head">
          <h1>{info.depot}</h1>
          <button onClick={() => void refresh()} disabled={loading}>
            {loading ? "Refreshing…" : "Refresh"}
          </button>
        </header>
        <dl className="facts">
          <dt>Folder</dt>
          <dd className="mono">{info.root}</dd>
          <dt>Server</dt>
          <dd className="mono">{info.server}</dd>
          <dt>Branch</dt>
          <dd>{status?.branch ?? info.branch}</dd>
          <dt>Revision</dt>
          <dd>r{status?.rev ?? info.rev}</dd>
        </dl>
        <div className="tiles">
          <div className="tile">
            <div className="tile-value">{changed}</div>
            <div className="tile-label">changed files</div>
          </div>
          <div className="tile">
            <div className="tile-value">{untracked}</div>
            <div className="tile-label">new, not added</div>
          </div>
          <div className="tile">
            <div className="tile-value">{locked}</div>
            <div className="tile-label">locked by you</div>
          </div>
          <div className={`tile ${needLock > 0 ? "warn" : ""}`}>
            <div className="tile-value">{needLock}</div>
            <div className="tile-label">need a lock to commit</div>
          </div>
        </div>
      </section>
    </div>
  );
}
