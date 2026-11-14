import { useCallback, useState } from "react";

import {
  asOpError,
  backend,
  type BranchInfo,
  type MergeResult,
  type OpError,
  type StatusResult,
  type UpdateResult,
  type WorkspaceInfo,
} from "../api";
import { BlameView } from "./BlameView";
import { BranchPicker, CreateBranchDialog, SyncResultDialog } from "./BranchPicker";
import { ConflictView } from "./ConflictView";
import { MergeDialog, MergeResultDialog } from "./MergeDialog";
import { FinishUpdateContext } from "./ErrorBanner";
import { Modal } from "./Modal";
import { ProgressBar, useProgress } from "./ProgressBar";
import { ChangesView } from "./ChangesView";
import { DiffView } from "./DiffView";
import { HistoryView } from "./HistoryView";
import { LocksView } from "./LocksView";
import { ShelvesView } from "./ShelvesView";
import { ViewEditor } from "./ViewEditor";

interface Props {
  info: WorkspaceInfo;
  onError: (e: OpError) => void;
  /** The workspace moved (another branch or revision). */
  onInfo?: (info: WorkspaceInfo) => void;
}

type Dialog =
  | { kind: "branches"; select?: string }
  | { kind: "create"; from: string; branches: BranchInfo[] }
  | { kind: "finishing" }
  | { kind: "merge"; source: string; branches: BranchInfo[] }
  | { kind: "merged"; result: MergeResult }
  | { kind: "result"; result: UpdateResult }
  | null;

/** The progress of finishing a stopped update. */
function Finishing() {
  const progress = useProgress();
  return (
    <Modal title="Finishing the update" onClose={() => {}}>
      {progress && !progress.finished ? <ProgressBar event={progress} /> : <p className="muted small">Working…</p>}
    </Modal>
  );
}

function hostOf(server: string): string {
  try {
    return new URL(server).host;
  } catch {
    return server;
  }
}

const SECTIONS = ["Changes", "History", "Locks", "Shelves", "Workspace"] as const;
type Section = (typeof SECTIONS)[number];

/** What the main area shows. */
type View =
  | { kind: "changes" }
  | { kind: "locks" }
  | { kind: "shelves" }
  | { kind: "workspace" }
  | { kind: "history"; path: string | null; rev: number | null }
  | { kind: "blame"; path: string; rev: number | null }
  | { kind: "diff"; path: string; rev: number | null }
  | { kind: "conflict"; path: string };

function sectionOf(v: View): Section {
  if (v.kind === "locks") return "Locks";
  if (v.kind === "workspace") return "Workspace";
  if (v.kind === "shelves") return "Shelves";
  return v.kind === "changes" || v.kind === "conflict" || (v.kind === "diff" && v.rev === null) ? "Changes" : "History";
}

/** One open workspace: the sidebar and the section it shows. */
export function WorkspaceView({ info, onError, onInfo }: Props) {
  const [status, setStatus] = useState<StatusResult | null>(null);
  const [view, setView] = useState<View>({ kind: "changes" });
  const [dialog, setDialog] = useState<Dialog>(null);
  // Bumped when the workspace moves, so every section loads again for the new branch.
  const [generation, setGeneration] = useState(0);
  // Said in Changes after a conflict is resolved.
  const [notice, setNotice] = useState<string | null>(null);
  // The view editor stays mounted once opened, so a change keeps running in the background.
  const [viewOpened, setViewOpened] = useState(false);
  const [viewBusy, setViewBusy] = useState(false);
  if (view.kind === "workspace" && !viewOpened) setViewOpened(true);

  const synced = useCallback(
    async (result: UpdateResult) => {
      setDialog({ kind: "result", result });
      setStatus(null);
      setGeneration((g) => g + 1);
      // History and blame of a revision on the old branch no longer apply.
      setView((v) =>
        v.kind === "history" || v.kind === "blame" || (v.kind === "diff" && v.rev !== null)
          ? { kind: "history", path: null, rev: null }
          : v,
      );
      try {
        onInfo?.(await (await backend()).openWorkspace(info.root));
      } catch (e) {
        onError(asOpError(e));
      }
    },
    [info.root, onInfo, onError],
  );

  const finishUpdate = useCallback(() => {
    setDialog({ kind: "finishing" });
    void (async () => {
      try {
        await synced(await (await backend()).update(info.root, null));
      } catch (e) {
        setDialog(null);
        onError(asOpError(e));
      }
    })();
  }, [info.root, synced, onError]);

  const changed = status?.files.filter((f) => f.status !== "unchanged").length ?? 0;
  const locked = status?.files.filter((f) => f.locked).length ?? 0;
  const active = sectionOf(view);
  const history = (path: string | null, rev: number | null = null) => setView({ kind: "history", path, rev });
  const blame = (path: string, rev: number | null) => setView({ kind: "blame", path, rev });
  const diff = (path: string, rev: number | null) => setView({ kind: "diff", path, rev });

  return (
    <FinishUpdateContext.Provider value={finishUpdate}>
      <div className="workspace">
        <aside className="sidebar" aria-label="Workspace">
          <div className="sidebar-head">
            <div className="sidebar-depot">{info.depot}</div>
            <div className="sidebar-server">{hostOf(info.server)}</div>
          </div>
          <button
            className="sidebar-branch"
            onClick={() => setDialog({ kind: "branches" })}
            aria-haspopup="dialog"
            title="Branches"
            aria-label={`Branch ${status?.branch ?? info.branch} at r${status?.rev ?? info.rev}: change branch`}
          >
            <span className="branch-icon" aria-hidden="true">
              ⑂
            </span>
            <span className="branch-label">{status?.branch ?? info.branch}</span>
            <span className="rev">r{status?.rev ?? info.rev}</span>
          </button>
          <nav className="sidebar-nav">
            {SECTIONS.map((s) => (
              <button
                key={s}
                className={s === active ? "active" : ""}
                aria-current={s === active ? "page" : undefined}
                onClick={() =>
                  setView(
                    s === "Changes"
                      ? { kind: "changes" }
                      : s === "Locks"
                        ? { kind: "locks" }
                        : s === "Shelves"
                          ? { kind: "shelves" }
                          : s === "Workspace"
                            ? { kind: "workspace" }
                            : { kind: "history", path: null, rev: null },
                  )
                }
              >
                {s}
                {s === "Changes" && changed > 0 && <span className="count">{changed}</span>}
                {s === "Locks" && locked > 0 && <span className="count">{locked}</span>}
                {s === "Workspace" && viewBusy && (
                  <span className="count busy" title="Updating the files on disk">
                    …
                  </span>
                )}
              </button>
            ))}
          </nav>
        </aside>
        {viewOpened && (
          <div className="section" hidden={view.kind !== "workspace"}>
            <ViewEditor key={generation} info={info} onBusy={setViewBusy} onApplied={() => setStatus(null)} />
          </div>
        )}
        {/* Kept mounted while hidden, so the picked files and the message stay. */}
        <div className="section" hidden={view.kind !== "changes"}>
          <ChangesView
            key={generation}
            info={info}
            onStatus={setStatus}
            onError={onError}
            onHistory={(path) => history(path)}
            onBlame={(path) => blame(path, null)}
            onDiff={(path) => diff(path, null)}
            onResolve={(path) => setView({ kind: "conflict", path })}
            notice={notice}
            active={view.kind === "changes"}
          />
        </div>
        {view.kind === "history" && (
          <div className="section">
            <HistoryView
              key={`${generation}:${view.path ?? ""}@${view.rev ?? ""}`}
              info={info}
              path={view.path}
              initialRev={view.rev}
              onPath={(path) => history(path)}
              onBlame={blame}
              onDiff={diff}
            />
          </div>
        )}
        {view.kind === "conflict" && (
          <div className="section">
            <ConflictView
              key={`${generation}:${view.path}`}
              info={info}
              path={view.path}
              onBack={() => setView({ kind: "changes" })}
              onResolved={(how) => {
                setNotice(`Resolved ${view.path}: ${how}.`);
                setView({ kind: "changes" });
              }}
            />
          </div>
        )}
        {view.kind === "locks" && (
          <div className="section">
            <LocksView key={generation} info={info} />
          </div>
        )}
        {view.kind === "shelves" && (
          <div className="section">
            <ShelvesView key={generation} info={info} />
          </div>
        )}
        {view.kind === "diff" && (
          <div className="section">
            <DiffView
              key={generation}
              info={info}
              source={
                view.rev === null
                  ? { kind: "working", path: view.path }
                  : { kind: "revision", rev: view.rev, path: view.path }
              }
              backLabel={view.rev === null ? "Changes" : `r${view.rev}`}
              onBack={() => (view.rev === null ? setView({ kind: "changes" }) : history(null, view.rev))}
            />
          </div>
        )}
        {view.kind === "blame" && (
          <div className="section">
            <BlameView
              key={generation}
              info={info}
              path={view.path}
              rev={view.rev}
              onRevision={(rev) => history(null, rev)}
              onHistory={(path) => history(path)}
            />
          </div>
        )}
        {dialog?.kind === "branches" && (
          <BranchPicker
            info={info}
            initial={dialog.select}
            onClose={() => setDialog(null)}
            onSwitched={(r) => void synced(r)}
            onCreate={(from, branches) => setDialog({ kind: "create", from, branches })}
            onMerge={(source, branches) => setDialog({ kind: "merge", source, branches })}
          />
        )}
        {dialog?.kind === "create" && (
          <CreateBranchDialog
            info={info}
            from={dialog.from}
            branches={dialog.branches}
            onClose={() => setDialog({ kind: "branches" })}
            onCreated={(made) =>
              made.switch ? void synced(made.switch) : setDialog({ kind: "branches", select: made.name })
            }
          />
        )}
        {dialog?.kind === "finishing" && <Finishing />}
        {dialog?.kind === "merge" && (
          <MergeDialog
            info={info}
            source={dialog.source}
            branches={dialog.branches}
            onClose={() => setDialog({ kind: "branches", select: dialog.source })}
            onMerged={(result) => {
              setDialog({ kind: "merged", result });
              setGeneration((g) => g + 1);
            }}
          />
        )}
        {dialog?.kind === "merged" && (
          <MergeResultDialog
            result={dialog.result}
            onClose={() => setDialog(null)}
            onChanges={() => {
              setDialog(null);
              setView({ kind: "changes" });
            }}
          />
        )}
        {dialog?.kind === "result" && (
          <SyncResultDialog
            result={dialog.result}
            onClose={() => setDialog(null)}
            onChanges={() => {
              setDialog(null);
              setView({ kind: "changes" });
            }}
          />
        )}
      </div>
    </FinishUpdateContext.Provider>
  );
}
