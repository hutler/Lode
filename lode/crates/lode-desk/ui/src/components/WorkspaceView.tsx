import { useState } from "react";

import type { OpError, StatusResult, WorkspaceInfo } from "../api";
import { BlameView } from "./BlameView";
import { ChangesView } from "./ChangesView";
import { DiffView } from "./DiffView";
import { HistoryView } from "./HistoryView";

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
type Section = (typeof SECTIONS)[number];

/** What the main area shows. */
type View =
  | { kind: "changes" }
  | { kind: "history"; path: string | null; rev: number | null }
  | { kind: "blame"; path: string; rev: number | null }
  | { kind: "diff"; path: string; rev: number | null };

function sectionOf(v: View): Section {
  return v.kind === "changes" || (v.kind === "diff" && v.rev === null) ? "Changes" : "History";
}

/** One open workspace: the sidebar and the section it shows. */
export function WorkspaceView({ info, onError }: Props) {
  const [status, setStatus] = useState<StatusResult | null>(null);
  const [view, setView] = useState<View>({ kind: "changes" });

  const changed = status?.files.filter((f) => f.status !== "unchanged").length ?? 0;
  const locked = status?.files.filter((f) => f.locked).length ?? 0;
  const active = sectionOf(view);
  const history = (path: string | null, rev: number | null = null) => setView({ kind: "history", path, rev });
  const blame = (path: string, rev: number | null) => setView({ kind: "blame", path, rev });
  const diff = (path: string, rev: number | null) => setView({ kind: "diff", path, rev });

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
          {SECTIONS.map((s) => (
            <button
              key={s}
              className={s === active ? "active" : ""}
              aria-current={s === active ? "page" : undefined}
              disabled={s !== "Changes" && s !== "History"}
              onClick={() => setView(s === "Changes" ? { kind: "changes" } : { kind: "history", path: null, rev: null })}
            >
              {s}
              {s === "Changes" && changed > 0 && <span className="count">{changed}</span>}
              {s === "Locks" && locked > 0 && <span className="count">{locked}</span>}
            </button>
          ))}
        </nav>
      </aside>
      {/* Kept mounted while hidden, so the picked files and the message stay. */}
      <div className="section" hidden={view.kind !== "changes"}>
        <ChangesView
          info={info}
          onStatus={setStatus}
          onError={onError}
          onHistory={(path) => history(path)}
          onBlame={(path) => blame(path, null)}
          onDiff={(path) => diff(path, null)}
        />
      </div>
      {view.kind === "history" && (
        <div className="section">
          <HistoryView
            key={`${view.path ?? ""}@${view.rev ?? ""}`}
            info={info}
            path={view.path}
            initialRev={view.rev}
            onPath={(path) => history(path)}
            onBlame={blame}
            onDiff={diff}
          />
        </div>
      )}
      {view.kind === "diff" && (
        <div className="section">
          <DiffView
            info={info}
            source={view.rev === null ? { kind: "working", path: view.path } : { kind: "revision", rev: view.rev, path: view.path }}
            backLabel={view.rev === null ? "Changes" : `r${view.rev}`}
            onBack={() => (view.rev === null ? setView({ kind: "changes" }) : history(null, view.rev))}
          />
        </div>
      )}
      {view.kind === "blame" && (
        <div className="section">
          <BlameView
            info={info}
            path={view.path}
            rev={view.rev}
            onRevision={(rev) => history(null, rev)}
            onHistory={(path) => history(path)}
          />
        </div>
      )}
    </div>
  );
}
