import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type MouseEvent } from "react";

import {
  asOpError,
  backend,
  type OpError,
  type ProgressEvent,
  type StatusFile,
  type StatusResult,
  type WorkspaceInfo,
} from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";
import { ProgressBar } from "./ProgressBar";

interface Props {
  info: WorkspaceInfo;
  onStatus: (s: StatusResult) => void;
  onError: (e: OpError) => void;
}

/** One group of the list: the default changes, a named changelist, or new files. */
export interface Group {
  key: string;
  title: string;
  files: StatusFile[];
}

const LETTER: Record<StatusFile["status"], string> = {
  modified: "M",
  added: "A",
  deleted: "D",
  missing: "!",
  conflicted: "C",
  untracked: "?",
  unchanged: " ",
};

const STATUS_TEXT: Record<StatusFile["status"], string> = {
  modified: "Modified",
  added: "Added",
  deleted: "Deleted",
  missing: "Missing (deleted outside lode)",
  conflicted: "Conflicted",
  untracked: "New, not added",
  unchanged: "Unchanged",
};

/** Files grouped as `lode status` lists them: changes outside changelists first, then each
 * changelist by name, then files that are not added yet. */
export function groupFiles(files: StatusFile[]): Group[] {
  const main: StatusFile[] = [];
  const lists = new Map<string, StatusFile[]>();
  const fresh: StatusFile[] = [];
  for (const f of files) {
    if (f.status === "unchanged") continue;
    if (f.status === "untracked") fresh.push(f);
    else if (f.changelist) lists.set(f.changelist, [...(lists.get(f.changelist) ?? []), f]);
    else main.push(f);
  }
  const groups: Group[] = [];
  if (main.length) groups.push({ key: "", title: "Changes", files: main });
  for (const name of [...lists.keys()].sort()) {
    groups.push({ key: `cl:${name}`, title: `Changelist ${name}`, files: lists.get(name) ?? [] });
  }
  if (fresh.length) groups.push({ key: "new", title: "Not added yet", files: fresh });
  return groups;
}

function splitPath(path: string): [string, string] {
  const at = path.lastIndexOf("/");
  return at < 0 ? ["", path] : [path.slice(0, at + 1), path.slice(at + 1)];
}

interface Menu {
  x: number;
  y: number;
  file: StatusFile;
}

/** The pending changes of a workspace, with picking, committing, adding and reverting. */
export function ChangesView({ info, onStatus, onError }: Props) {
  const [status, setStatus] = useState<StatusResult | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [commitError, setCommitError] = useState<OpError | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [menu, setMenu] = useState<Menu | null>(null);
  const [confirmRevert, setConfirmRevert] = useState<string[] | null>(null);
  const root = info.root;
  const onStatusRef = useRef(onStatus);
  onStatusRef.current = onStatus;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  const refresh = useCallback(async () => {
    try {
      const b = await backend();
      const st = await b.status(root);
      setStatus(st);
      onStatusRef.current(st);
      const present = new Set(st.files.map((f) => f.path));
      setPicked((p) => new Set([...p].filter((x) => present.has(x))));
      setSelected((s) => (s && present.has(s) ? s : null));
    } catch (e) {
      onErrorRef.current(asOpError(e));
    }
  }, [root]);

  // Refresh when shown, when files change on disk, and when the window gets focus.
  useEffect(() => {
    let stopChanged: (() => void) | undefined;
    let stopProgress: (() => void) | undefined;
    void refresh();
    void backend().then((b) => {
      void b.watch(root).catch((e) => onErrorRef.current(asOpError(e)));
      stopChanged = b.onChanged((w) => {
        if (w === root) void refresh();
      });
      stopProgress = b.onProgress(setProgress);
    });
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("focus", onFocus);
      stopChanged?.();
      stopProgress?.();
      void backend().then((b) => b.unwatch(root));
    };
  }, [root, refresh]);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("blur", close);
    };
  }, [menu]);

  const groups = useMemo(() => groupFiles(status?.files ?? []), [status]);
  const all = groups.flatMap((g) => g.files.map((f) => f.path));
  const byPath = new Map((status?.files ?? []).map((f) => [f.path, f]));
  const pickedFiles = [...picked].filter((p) => byPath.has(p));

  const toggle = (paths: string[], on: boolean) =>
    setPicked((p) => {
      const n = new Set(p);
      for (const x of paths) {
        if (on) n.add(x);
        else n.delete(x);
      }
      return n;
    });

  const run = async (label: string, f: () => Promise<void>) => {
    setBusy(label);
    setDone(null);
    try {
      await f();
    } catch (e) {
      onErrorRef.current(asOpError(e));
    } finally {
      setBusy(null);
      await refresh();
    }
  };

  const add = (paths: string[]) => run("Adding", async () => (await backend()).add(root, paths));
  const revert = (paths: string[]) => run("Reverting", async () => (await backend()).revert(root, paths));

  const commit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy("Committing");
    setCommitError(null);
    setDone(null);
    setProgress(null);
    try {
      const b = await backend();
      const untracked = pickedFiles.filter((p) => byPath.get(p)?.status === "untracked");
      if (untracked.length) await b.add(root, untracked);
      const r = await b.commit(root, pickedFiles, message.trim());
      setDone(`Committed r${r.rev} (${r.files} file${r.files === 1 ? "" : "s"}).`);
      setMessage("");
      setPicked(new Set());
    } catch (err) {
      setCommitError(asOpError(err));
    } finally {
      setBusy(null);
      setProgress(null);
      await refresh();
    }
  };

  const openMenu = (e: MouseEvent, file: StatusFile) => {
    e.preventDefault();
    setSelected(file.path);
    setMenu({ x: e.clientX, y: e.clientY, file });
  };

  const sel = selected ? byPath.get(selected) : undefined;
  const errorPaths: string[] =
    commitError && typeof commitError.details === "object" && commitError.details !== null
      ? (((commitError.details as { paths?: unknown }).paths as string[] | undefined) ?? [])
      : [];

  return (
    <div className="changes">
      <section className="changes-main" aria-label="Pending changes">
        <header className="changes-head">
          <label className="check">
            <input
              type="checkbox"
              aria-label="Pick all files"
              checked={all.length > 0 && all.every((p) => picked.has(p))}
              onChange={(e) => toggle(all, e.target.checked)}
              disabled={all.length === 0}
            />
          </label>
          <h1>Changes</h1>
          <span className="muted">{all.length === 0 ? "" : `${pickedFiles.length} of ${all.length} picked`}</span>
          <button className="refresh" onClick={() => void refresh()} disabled={!!busy}>
            Refresh
          </button>
        </header>

        <div className="file-list">
          {status && groups.length === 0 && (
            <p className="empty">No changes. Edit files in the workspace folder and they show up here.</p>
          )}
          {groups.map((g) => {
            const paths = g.files.map((f) => f.path);
            const allOn = paths.every((p) => picked.has(p));
            return (
              <div key={g.key} className="group" role="group" aria-label={g.title}>
                <div className="group-head">
                  <input
                    type="checkbox"
                    aria-label={`Pick all of ${g.title}`}
                    checked={allOn}
                    onChange={(e) => toggle(paths, e.target.checked)}
                  />
                  <span className="group-title">{g.title}</span>
                  <span className="group-count">{g.files.length}</span>
                </div>
                <ul>
                  {g.files.map((f) => {
                    const [dir, name] = splitPath(f.path);
                    return (
                      <li
                        key={f.path}
                        className={`file ${selected === f.path ? "selected" : ""}`}
                        onClick={() => setSelected(f.path)}
                        onContextMenu={(e) => openMenu(e, f)}
                      >
                        <input
                          type="checkbox"
                          aria-label={`Pick ${f.path}`}
                          checked={picked.has(f.path)}
                          onClick={(e) => e.stopPropagation()}
                          onChange={(e) => toggle([f.path], e.target.checked)}
                        />
                        <span className={`st st-${f.status}`} title={STATUS_TEXT[f.status]}>
                          {LETTER[f.status]}
                        </span>
                        <span className="file-path" title={f.path}>
                          <span className="file-dir">{dir}</span>
                          <span className="file-name">{name}</span>
                        </span>
                        {f.copy_from && (
                          <span className="tag">
                            {f.moved ? "moved" : "copied"} from {f.copy_from.path}
                          </span>
                        )}
                        {f.locked && (
                          <span className="tag lock" title="You hold the lock">
                            locked
                          </span>
                        )}
                        {f.needs_lock && (
                          <span className="tag warn" title="The server refuses to commit this file until you lock it">
                            needs lock
                          </span>
                        )}
                      </li>
                    );
                  })}
                </ul>
              </div>
            );
          })}
        </div>

        <form className="commit-box" onSubmit={commit} aria-label="Commit">
          <textarea
            aria-label="Commit message"
            placeholder="Describe what changed"
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            rows={3}
            disabled={busy === "Committing"}
          />
          {busy === "Committing" && progress && <ProgressBar event={progress} />}
          {commitError && (
            <div className="commit-error">
              <ErrorBanner error={commitError} onDismiss={() => setCommitError(null)} />
              {errorPaths.length > 0 && (
                <ul className="error-paths" aria-label="Files the server refused">
                  {errorPaths.map((p) => (
                    <li key={p} className="mono">
                      {p}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}
          <div className="commit-actions">
            {done && (
              <span className="done" role="status">
                {done}
              </span>
            )}
            <button
              className="primary"
              type="submit"
              disabled={!!busy || pickedFiles.length === 0 || !message.trim()}
            >
              {busy === "Committing"
                ? "Committing…"
                : `Commit ${pickedFiles.length} file${pickedFiles.length === 1 ? "" : "s"}`}
            </button>
          </div>
        </form>
      </section>

      <aside className="details" aria-label="Details">
        {sel ? (
          <>
            <h2 className="details-title">{splitPath(sel.path)[1]}</h2>
            <dl className="facts compact">
              <dt>Path</dt>
              <dd className="mono">{sel.path}</dd>
              <dt>Status</dt>
              <dd>{STATUS_TEXT[sel.status]}</dd>
              {sel.copy_from && (
                <>
                  <dt>{sel.moved ? "Moved from" : "Copied from"}</dt>
                  <dd className="mono">
                    {sel.copy_from.path}@r{sel.copy_from.rev}
                  </dd>
                </>
              )}
              <dt>Changelist</dt>
              <dd>{sel.changelist ?? "none"}</dd>
              <dt>Lock</dt>
              <dd>
                {sel.locked
                  ? "You hold the lock"
                  : sel.needs_lock
                    ? "Needs a lock before it can be committed"
                    : sel.lock_required
                      ? "Lock required for changes"
                      : "Not needed"}
              </dd>
            </dl>
            <div className="details-actions">
              {sel.status === "untracked" ? (
                <button onClick={() => void add([sel.path])} disabled={!!busy}>
                  Add
                </button>
              ) : (
                <button onClick={() => setConfirmRevert([sel.path])} disabled={!!busy}>
                  Revert…
                </button>
              )}
            </div>
          </>
        ) : (
          <>
            <h2 className="details-title">{info.depot}</h2>
            <dl className="facts compact">
              <dt>Folder</dt>
              <dd className="mono">{info.root}</dd>
              <dt>Server</dt>
              <dd className="mono">{info.server}</dd>
              <dt>Branch</dt>
              <dd>{status?.branch ?? info.branch}</dd>
              <dt>Revision</dt>
              <dd>r{status?.rev ?? info.rev}</dd>
            </dl>
            <p className="muted small">Select a file to see more about it. Right-click a file to add or revert it.</p>
          </>
        )}
      </aside>

      {menu && (
        <div className="menu" role="menu" style={{ left: menu.x, top: menu.y }}>
          {menu.file.status === "untracked" ? (
            <button role="menuitem" onClick={() => void add([menu.file.path])}>
              Add
            </button>
          ) : (
            <button role="menuitem" onClick={() => setConfirmRevert([menu.file.path])}>
              Revert…
            </button>
          )}
          <button role="menuitem" onClick={() => toggle([menu.file.path], !picked.has(menu.file.path))}>
            {picked.has(menu.file.path) ? "Unpick" : "Pick"}
          </button>
        </div>
      )}

      {confirmRevert && (
        <Modal title="Revert changes?" onClose={() => setConfirmRevert(null)}>
          <p>
            The local changes to {confirmRevert.length === 1 ? <strong>{confirmRevert[0]}</strong> : `${confirmRevert.length} files`}{" "}
            will be lost. This cannot be undone.
          </p>
          <div className="form-actions">
            <button onClick={() => setConfirmRevert(null)} autoFocus>
              Cancel
            </button>
            <button
              className="danger"
              onClick={() => {
                const paths = confirmRevert;
                setConfirmRevert(null);
                void revert(paths);
              }}
            >
              Revert
            </button>
          </div>
        </Modal>
      )}
    </div>
  );
}
