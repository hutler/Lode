import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type MouseEvent } from "react";

import {
  asOpError,
  backend,
  type ChangelistInfo,
  type OpError,
  type PendingMerge,
  type ProgressEvent,
  type StatusFile,
  type StatusResult,
  type WorkspaceInfo,
} from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { ShelveDialog, type ShelveWhat } from "./ShelveDialog";
import { ChangelistDialogs, type ChangelistAsk } from "./ChangelistDialogs";
import { LockDialog } from "./LockDialog";
import { Modal } from "./Modal";
import { ProgressBar } from "./ProgressBar";

interface Props {
  info: WorkspaceInfo;
  onStatus: (s: StatusResult) => void;
  onError: (e: OpError) => void;
  /** Opens a file's history. */
  onHistory?: (path: string) => void;
  /** Opens a file's blame. */
  onBlame?: (path: string) => void;
  /** Opens a file's diff against its base. */
  onDiff?: (path: string) => void;
  /** Opens a conflicted file to resolve it. */
  onResolve?: (path: string) => void;
  /** Said in the commit area when it changes (after a conflict is resolved elsewhere). */
  notice?: string | null;
  /** Whether the view is showing; it refreshes when it comes back. */
  active?: boolean;
}

/** One group of the list: the default changes, a named changelist, or new files. */
export interface Group {
  key: string;
  title: string;
  files: StatusFile[];
  /** The changelist files dropped here go to (`default` for plain changes); null for new files. */
  changelist: string | null;
  description?: string;
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
 * changelist by name, then files that are not added yet. With `lists`, empty changelists show
 * too, and so does an empty "Changes" group when there are named changelists, as places to
 * move files to. */
export function groupFiles(files: StatusFile[], lists: ChangelistInfo[] = []): Group[] {
  const main: StatusFile[] = [];
  const byList = new Map<string, StatusFile[]>();
  const fresh: StatusFile[] = [];
  for (const f of files) {
    if (f.status === "unchanged") continue;
    if (f.status === "untracked") fresh.push(f);
    else if (f.changelist) byList.set(f.changelist, [...(byList.get(f.changelist) ?? []), f]);
    else main.push(f);
  }
  const named = lists.filter((l) => l.name !== "default");
  for (const l of named) if (!byList.has(l.name)) byList.set(l.name, []);
  const describe = new Map(named.map((l) => [l.name, l.description]));
  const groups: Group[] = [];
  if (main.length || named.length) groups.push({ key: "", title: "Changes", files: main, changelist: "default" });
  for (const name of [...byList.keys()].sort()) {
    groups.push({
      key: `cl:${name}`,
      title: `Changelist ${name}`,
      files: byList.get(name) ?? [],
      changelist: name,
      description: describe.get(name) || undefined,
    });
  }
  if (fresh.length) groups.push({ key: "new", title: "Not added yet", files: fresh, changelist: null });
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
/** The commit message a merge suggests: `Merge BRANCH rA..rB` (or `rN` for one revision). */
export function mergeMessage(merges: PendingMerge[]): string {
  return merges
    .map((m) => `Merge ${m.branch} ${m.from_rev === m.to_rev ? `r${m.to_rev}` : `r${m.from_rev}..r${m.to_rev}`}`)
    .join("; ");
}

export function ChangesView({
  info,
  onStatus,
  onError,
  onHistory,
  onBlame,
  onDiff,
  onResolve,
  notice,
  active = true,
}: Props) {
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
  const [lockPaths, setLockPaths] = useState<string[] | null>(null);
  const [lists, setLists] = useState<ChangelistInfo[]>([]);
  const [listAsk, setListAsk] = useState<ChangelistAsk | null>(null);
  const [shelving, setShelving] = useState<ShelveWhat | null>(null);
  const [confirmAbandon, setConfirmAbandon] = useState(false);
  useEffect(() => {
    if (notice) setDone(notice);
  }, [notice]);
  // Files being dragged, and the group under the pointer.
  const dragged = useRef<string[]>([]);
  const [dropOn, setDropOn] = useState<string | null>(null);
  const root = info.root;
  const onStatusRef = useRef(onStatus);
  onStatusRef.current = onStatus;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  const refresh = useCallback(async () => {
    try {
      const b = await backend();
      const [st, cl] = await Promise.all([b.status(root), b.changelists(root)]);
      setStatus(st);
      setLists(cl.changelists);
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

  // Locks and commits made elsewhere in the app show when the view comes back.
  const wasActive = useRef(active);
  useEffect(() => {
    if (active && !wasActive.current) void refresh();
    wasActive.current = active;
  }, [active, refresh]);

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

  const groups = useMemo(() => groupFiles(status?.files ?? [], lists), [status, lists]);
  const all = groups.flatMap((g) => g.files.map((f) => f.path));
  const byPath = new Map((status?.files ?? []).map((f) => [f.path, f]));
  const pickedFiles = [...picked].filter((p) => byPath.has(p));
  const merges = status?.merges ?? [];
  const merging = merges.length > 0;
  const conflicted = (status?.files ?? []).filter((f) => f.status === "conflicted");
  const suggestion = mergeMessage(merges);
  // A merge suggests its commit message once; what the user types stays.
  const suggested = useRef("");
  useEffect(() => {
    if (suggestion && suggestion !== suggested.current) {
      suggested.current = suggestion;
      setMessage((m) => (m.trim() ? m : suggestion));
    }
  }, [suggestion]);

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
  const unlock = (paths: string[]) =>
    run("Unlocking", async () => {
      await (await backend()).unlock(root, paths, false);
    });
  // The right-clicked file, or every picked file when it is one of them.
  const targets = (f: StatusFile) => (picked.has(f.path) && pickedFiles.length > 1 ? pickedFiles : [f.path]);
  const lockable = (f: StatusFile | undefined) => !!f && f.status !== "untracked" && f.status !== "added";
  const moveTo = (name: string, paths: string[]) =>
    run("Moving", async () => (await backend()).changelistMove(root, name, paths));
  const listOf = (f: StatusFile) => f.changelist ?? "default";

  const commit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy("Committing");
    setCommitError(null);
    setDone(null);
    setProgress(null);
    try {
      const b = await backend();
      let r;
      if (merging) {
        // A merge is committed whole.
        r = await b.commitAll(root, message.trim());
      } else {
        const untracked = pickedFiles.filter((p) => byPath.get(p)?.status === "untracked");
        if (untracked.length) await b.add(root, untracked);
        r = await b.commit(root, pickedFiles, message.trim());
      }
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
          <button
            className="refresh"
            onClick={() =>
              setListAsk({ kind: "new", picked: pickedFiles.filter((p) => byPath.get(p)?.status !== "untracked") })
            }
            disabled={!!busy}
          >
            New changelist…
          </button>
          <button className="refresh" onClick={() => void refresh()} disabled={!!busy}>
            Refresh
          </button>
        </header>

        {merging && (
          <div className="merge-notice" role="note" aria-label="Pending merge">
            <span>
              Merging{" "}
              {merges.map((m, i) => (
                <span key={i}>
                  {i > 0 && ", "}
                  <strong>{m.branch}</strong>{" "}
                  {m.from_rev === m.to_rev ? `r${m.to_rev}` : `r${m.from_rev}..r${m.to_rev}`}
                </span>
              ))}
              . The next commit records the merge and takes every change.
            </span>
            <button onClick={() => setConfirmAbandon(true)} disabled={!!busy}>
              Abandon merge…
            </button>
          </div>
        )}
        <div className="file-list">
          {status && groups.length === 0 && (
            <p className="empty">No changes. Edit files in the workspace folder and they show up here.</p>
          )}
          {groups.map((g) => {
            const paths = g.files.map((f) => f.path);
            const allOn = paths.every((p) => picked.has(p));
            return (
              <div
                key={g.key}
                className={`group ${dropOn === g.key ? "drop" : ""}`}
                role="group"
                aria-label={g.title}
                onDragOver={(e) => {
                  if (g.changelist === null || dragged.current.length === 0) return;
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "move";
                  setDropOn(g.key);
                }}
                onDragLeave={(e) => {
                  if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropOn(null);
                }}
                onDrop={(e) => {
                  e.preventDefault();
                  setDropOn(null);
                  const target = g.changelist;
                  const paths = dragged.current.filter((p) => byPath.get(p) && listOf(byPath.get(p)!) !== target);
                  dragged.current = [];
                  if (target && paths.length) void moveTo(target, paths);
                }}
              >
                <div className="group-head">
                  <input
                    type="checkbox"
                    aria-label={`Pick all of ${g.title}`}
                    checked={allOn}
                    onChange={(e) => toggle(paths, e.target.checked)}
                  />
                  <span className="group-title">{g.title}</span>
                  <span className="group-count">{g.files.length}</span>
                  {g.description && (
                    <span className="group-desc" title={g.description}>
                      {g.description}
                    </span>
                  )}
                  {g.changelist && g.changelist !== "default" && (
                    <span className="group-actions">
                      <button
                        className="link"
                        disabled={!!busy || g.files.length === 0 || merging || conflicted.length > 0}
                        title={
                          merging
                            ? "A merge is committed whole"
                            : conflicted.length
                              ? "Resolve the conflicts first"
                              : undefined
                        }
                        onClick={() =>
                          setListAsk({
                            kind: "commit",
                            name: g.changelist!,
                            description: g.description ?? "",
                            files: g.files.length,
                          })
                        }
                        aria-label={`Commit changelist ${g.changelist}`}
                      >
                        Commit…
                      </button>
                      <button
                        className="link"
                        disabled={!!busy || g.files.length === 0}
                        onClick={() =>
                          setShelving({
                            kind: "changelist",
                            name: g.changelist!,
                            description: g.description ?? "",
                            files: g.files.length,
                          })
                        }
                        aria-label={`Shelve changelist ${g.changelist}`}
                      >
                        Shelve…
                      </button>
                      <button
                        className="link"
                        disabled={!!busy}
                        onClick={() => setListAsk({ kind: "delete", name: g.changelist!, files: g.files.length })}
                        aria-label={`Delete changelist ${g.changelist}`}
                      >
                        Delete…
                      </button>
                    </span>
                  )}
                </div>
                {g.files.length === 0 && (
                  <p className="group-empty muted small">Drag files here, or right-click a file and move it.</p>
                )}
                <ul>
                  {g.files.map((f) => {
                    const [dir, name] = splitPath(f.path);
                    return (
                      <li
                        key={f.path}
                        className={`file ${selected === f.path ? "selected" : ""}`}
                        onClick={() => setSelected(f.path)}
                        onDoubleClick={() =>
                          f.status === "conflicted" && onResolve
                            ? onResolve(f.path)
                            : f.status !== "untracked" && onDiff?.(f.path)
                        }
                        onContextMenu={(e) => openMenu(e, f)}
                        draggable={f.status !== "untracked"}
                        onDragStart={(e) => {
                          dragged.current = targets(f).filter((p) => byPath.get(p)?.status !== "untracked");
                          e.dataTransfer.effectAllowed = "move";
                          e.dataTransfer.setData("text/plain", dragged.current.join("\n"));
                        }}
                        onDragEnd={() => {
                          dragged.current = [];
                          setDropOn(null);
                        }}
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
            {conflicted.length > 0 && (
              <span className="commit-why" id="commit-why">
                Resolve {conflicted.length === 1 ? "the conflicted file" : `${conflicted.length} conflicted files`}{" "}
                before you commit.
              </span>
            )}
            {done && (
              <span className="done" role="status">
                {done}
              </span>
            )}
            <button
              type="button"
              disabled={!!busy || pickedFiles.length === 0}
              onClick={() =>
                setShelving({
                  kind: "files",
                  paths: pickedFiles.filter((p) => byPath.get(p)?.status !== "untracked"),
                  untracked: pickedFiles.filter((p) => byPath.get(p)?.status === "untracked"),
                })
              }
            >
              Shelve…
            </button>
            <button
              className="primary"
              type="submit"
              disabled={!!busy || (!merging && pickedFiles.length === 0) || !message.trim() || conflicted.length > 0}
              aria-describedby={conflicted.length ? "commit-why" : undefined}
            >
              {busy === "Committing"
                ? "Committing…"
                : merging
                  ? "Commit merge"
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
              {sel.status === "conflicted" && onResolve && (
                <button className="primary" onClick={() => onResolve(sel.path)}>
                  Resolve…
                </button>
              )}
              {sel.needs_lock && (
                <button className="primary" onClick={() => setLockPaths([sel.path])} disabled={!!busy}>
                  Lock…
                </button>
              )}
              {onDiff && sel.status !== "untracked" && (
                <button
                  className={sel.needs_lock || sel.status === "conflicted" ? "" : "primary"}
                  onClick={() => onDiff(sel.path)}
                >
                  Show diff
                </button>
              )}
              {sel.status === "untracked" ? (
                <button onClick={() => void add([sel.path])} disabled={!!busy}>
                  Add
                </button>
              ) : (
                <button onClick={() => setConfirmRevert([sel.path])} disabled={!!busy}>
                  Revert…
                </button>
              )}
              {sel.locked ? (
                <button onClick={() => void unlock([sel.path])} disabled={!!busy}>
                  Unlock
                </button>
              ) : (
                lockable(sel) &&
                !sel.needs_lock && (
                  <button onClick={() => setLockPaths([sel.path])} disabled={!!busy}>
                    Lock…
                  </button>
                )
              )}
              {sel.status !== "untracked" && sel.status !== "added" && (
                <>
                  {onHistory && <button onClick={() => onHistory(sel.path)}>History</button>}
                  {onBlame && sel.status !== "deleted" && <button onClick={() => onBlame(sel.path)}>Blame</button>}
                </>
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
            <p className="muted small">
              Select a file to see more about it. Right-click a file to add, revert or lock it.
            </p>
          </>
        )}
      </aside>

      {confirmAbandon && (
        <Modal title="Abandon the merge?" onClose={() => setConfirmAbandon(false)}>
          <p>
            The next commit will not record the merge of {merges.map((m) => m.branch).join(", ")}. The files stay as
            they are; revert them if you do not want the changes either.
          </p>
          <div className="form-actions">
            <button onClick={() => setConfirmAbandon(false)} autoFocus>
              Cancel
            </button>
            <button
              className="danger"
              onClick={() => {
                setConfirmAbandon(false);
                suggested.current = "";
                if (message.trim() === suggestion) setMessage("");
                void run("Abandoning", async () => (await backend()).abandonMerge(root));
              }}
            >
              Abandon merge
            </button>
          </div>
        </Modal>
      )}
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
          {lockable(menu.file) &&
            (() => {
              const t = targets(menu.file).filter((p) => lockable(byPath.get(p)));
              const held = t.filter((p) => byPath.get(p)?.locked);
              const n = (k: number) => (k > 1 ? ` ${k} files` : "");
              return (
                <>
                  {held.length < t.length && (
                    <button role="menuitem" onClick={() => setLockPaths(t.filter((p) => !byPath.get(p)?.locked))}>
                      Lock{n(t.length - held.length)}…
                    </button>
                  )}
                  {held.length > 0 && (
                    <button role="menuitem" onClick={() => void unlock(held)}>
                      Unlock{n(held.length)}
                    </button>
                  )}
                </>
              );
            })()}
          {menu.file.status !== "untracked" &&
            (() => {
              const t = targets(menu.file).filter((p) => byPath.get(p)?.status !== "untracked");
              const others = lists.map((l) => l.name).filter((n) => t.some((p) => listOf(byPath.get(p)!) !== n));
              if (others.length === 0) return null;
              return (
                <>
                  <div className="menu-label">Move{t.length > 1 ? ` ${t.length} files` : ""} to</div>
                  {others.map((n) => (
                    <button key={n} role="menuitem" onClick={() => void moveTo(n, t)}>
                      {n === "default" ? "Changes (no changelist)" : n}
                    </button>
                  ))}
                  <div className="menu-sep" />
                </>
              );
            })()}
          <button role="menuitem" onClick={() => toggle([menu.file.path], !picked.has(menu.file.path))}>
            {picked.has(menu.file.path) ? "Unpick" : "Pick"}
          </button>
        </div>
      )}

      {listAsk && (
        <ChangelistDialogs
          workspace={root}
          ask={listAsk}
          onClose={() => setListAsk(null)}
          onDone={(msg) => {
            if (msg) setDone(msg);
            void refresh();
          }}
        />
      )}

      {shelving && (
        <ShelveDialog
          workspace={root}
          what={shelving}
          onClose={() => setShelving(null)}
          onDone={(msg) => {
            setDone(msg);
            setPicked(new Set());
            void refresh();
          }}
        />
      )}

      {lockPaths && (
        <LockDialog
          workspace={root}
          paths={lockPaths}
          onClose={() => setLockPaths(null)}
          onLocked={() => void refresh()}
        />
      )}

      {confirmRevert && (
        <Modal title="Revert changes?" onClose={() => setConfirmRevert(null)}>
          <p>
            The local changes to{" "}
            {confirmRevert.length === 1 ? <strong>{confirmRevert[0]}</strong> : `${confirmRevert.length} files`} will be
            lost. This cannot be undone.
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
