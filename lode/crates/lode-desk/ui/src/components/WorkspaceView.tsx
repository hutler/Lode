import { useState } from "react";

import type { OpError, StatusResult, WorkspaceInfo } from "../api";
import { ChangesView } from "./ChangesView";

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

/** One open workspace: the sidebar and the section it shows. */
export function WorkspaceView({ info, onError }: Props) {
  const [status, setStatus] = useState<StatusResult | null>(null);

  const changed = status?.files.filter((f) => f.status !== "unchanged").length ?? 0;
  const locked = status?.files.filter((f) => f.locked).length ?? 0;

  return (
    <div className="workspace">
      <aside className="sidebar" aria-label="Workspace">
        <div className="sidebar-head">
          <div className="sidebar-depot">{info.depot}</div>
          <div className="sidebar-server">{hostOf(info.server)}</div>
        </div>
        <div className="sidebar-branch">
          <span className="branch-icon" aria-hidden="true">
            ⑂
          </span>
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
      <ChangesView info={info} onStatus={setStatus} onError={onError} />
    </div>
  );
}
