import { useCallback, useEffect, useMemo, useState } from "react";

import {
  asOpError,
  backend,
  type OpError,
  type TreeEntry,
  type ViewApplied,
  type ViewPreview,
  type ViewSpec,
  type ViewSummary,
  type WorkspaceInfo,
} from "../api";
import { folderState, parsePatterns, tickFolder } from "../viewPatterns";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";
import { humanBytes, ProgressBar, useProgress } from "./ProgressBar";

interface Props {
  info: WorkspaceInfo;
  /** Whether a view change is running (the sidebar shows it). */
  onBusy?: (busy: boolean) => void;
  /** Files came or went; other views load again. */
  onApplied?: () => void;
}

const count = (n: number, one: string) => `${n.toLocaleString("en-US")} ${one}${n === 1 ? "" : "s"}`;

function same(a: ViewSpec, b: ViewSpec) {
  return a.include.join("\n") === b.include.join("\n") && a.exclude.join("\n") === b.exclude.join("\n");
}

/** One folder of the tree, with its children once opened. */
function Folder({
  path,
  name,
  depth,
  spec,
  busy,
  load,
  onTick,
}: {
  path: string;
  name: string;
  depth: number;
  spec: ViewSpec;
  busy: boolean;
  load: (path: string) => Promise<TreeEntry[]>;
  onTick: (path: string, on: boolean) => void;
}) {
  const [open, setOpen] = useState(false);
  const [children, setChildren] = useState<TreeEntry[] | null>(null);
  const state = folderState(spec, path);
  const toggle = async () => {
    if (!open && !children) setChildren(await load(path));
    setOpen(!open);
  };
  const dirs = children?.filter((c) => c.kind === "dir") ?? [];
  const files = (children?.length ?? 0) - dirs.length;
  return (
    <li role="treeitem" aria-expanded={open} aria-label={path} aria-level={depth + 1} aria-selected={false}>
      <div className="folder-row" style={{ paddingLeft: depth * 18 + 6 }}>
        <button
          className="folder-toggle"
          onClick={() => void toggle()}
          aria-label={`${open ? "Close" : "Open"} ${path}`}
        >
          {open ? "▾" : "▸"}
        </button>
        <input
          type="checkbox"
          aria-label={`Include ${path}`}
          checked={state.on}
          ref={(el) => {
            if (el) el.indeterminate = state.mixed;
          }}
          disabled={busy || !!state.lockedBy}
          title={state.lockedBy ? `Left out by ${state.lockedBy}; edit the patterns to bring it back` : undefined}
          onChange={(e) => onTick(path, e.target.checked)}
        />
        <span className={state.on ? "folder-name" : "folder-name out"}>{name}</span>
        {state.mixed && <span className="muted small">partly</span>}
        {state.lockedBy && <span className="muted small mono">{state.lockedBy}</span>}
      </div>
      {open && children && (
        <ul role="group">
          {dirs.map((c) => (
            <Folder
              key={c.name}
              path={`${path}/${c.name}`}
              name={c.name}
              depth={depth + 1}
              spec={spec}
              busy={busy}
              load={load}
              onTick={onTick}
            />
          ))}
          {files > 0 && (
            <li className="muted small folder-files" style={{ paddingLeft: (depth + 1) * 18 + 30 }}>
              {count(files, "file")}
            </li>
          )}
        </ul>
      )}
    </li>
  );
}

/** Which part of the depot is on disk: the patterns, a folder tree to tick, a preview, and the change. */
export function ViewEditor({ info, onBusy, onApplied }: Props) {
  const [summary, setSummary] = useState<ViewSummary | null>(null);
  const [roots, setRoots] = useState<TreeEntry[] | null>(null);
  const [draft, setDraft] = useState<ViewSpec>({ include: [], exclude: [] });
  const [includeText, setIncludeText] = useState("");
  const [excludeText, setExcludeText] = useState("");
  const [preview, setPreview] = useState<ViewPreview | null>(null);
  const [applying, setApplying] = useState(false);
  const [checking, setChecking] = useState(false);
  const [confirmForce, setConfirmForce] = useState(false);
  const [done, setDone] = useState<ViewApplied | null>(null);
  const [error, setError] = useState<OpError | null>(null);
  const progress = useProgress();

  const setSpec = useCallback((spec: ViewSpec) => {
    setDraft(spec);
    setIncludeText(spec.include.join("\n"));
    setExcludeText(spec.exclude.join("\n"));
    setPreview(null);
  }, []);

  const load = useCallback(async () => {
    try {
      const b = await backend();
      const [s, t] = await Promise.all([b.view(info.root), b.viewTree(info.root, null)]);
      setSummary(s);
      setRoots(t.entries);
      setSpec(s.view);
    } catch (e) {
      setError(asOpError(e));
    }
  }, [info.root, setSpec]);

  useEffect(() => {
    void load();
  }, [load]);

  const loadDir = useCallback(
    async (path: string) => {
      try {
        return (await (await backend()).viewTree(info.root, path)).entries;
      } catch (e) {
        setError(asOpError(e));
        return [];
      }
    },
    [info.root],
  );

  const dirty = !!summary && !same(draft, summary.view);
  const tick = (path: string, on: boolean) => {
    setDone(null);
    setSpec(tickFolder(draft, path, on));
  };
  const editText = (which: "include" | "exclude", text: string) => {
    setDone(null);
    if (which === "include") setIncludeText(text);
    else setExcludeText(text);
    setDraft((d) => ({ ...d, [which]: parsePatterns(text) }));
    setPreview(null);
  };

  const check = async () => {
    setChecking(true);
    setError(null);
    try {
      setPreview(await (await backend()).viewApply(info.root, draft.include, draft.exclude, false, true));
    } catch (e) {
      setError(asOpError(e));
    } finally {
      setChecking(false);
    }
  };

  const apply = async (force: boolean) => {
    setConfirmForce(false);
    setApplying(true);
    onBusy?.(true);
    setError(null);
    try {
      const r = await (await backend()).viewApply(info.root, draft.include, draft.exclude, force, false);
      setDone(r);
      setPreview(null);
      onApplied?.();
      await load();
    } catch (e) {
      setError(asOpError(e));
    } finally {
      setApplying(false);
      onBusy?.(false);
    }
  };

  const busy = applying || checking;
  const stopped = !!preview && (preview.blocked.length > 0 || preview.untracked.length > 0);
  const tree = useMemo(() => (roots ?? []).filter((e) => e.kind === "dir"), [roots]);

  return (
    <section className="view-editor" aria-label="Workspace view">
      <header className="changes-head">
        <h1>Workspace view</h1>
        <span className="muted">Which part of {info.depot} is on disk</span>
      </header>
      <div className="view-body">
        {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
        {summary && (
          <dl className="facts view-facts" aria-label="What the view holds">
            <dt>In the view at r{summary.rev}</dt>
            <dd>
              {count(summary.server.files, "file")}, {humanBytes(summary.server.bytes)}
            </dd>
            <dt>On disk</dt>
            <dd>
              {count(summary.disk.files, "file")}, {humanBytes(summary.disk.bytes)}
            </dd>
          </dl>
        )}
        <div className="view-columns">
          <div className="view-tree">
            <h2>Folders</h2>
            <p className="hint">Tick a folder to bring it to disk, untick it to leave it out.</p>
            {!roots && !error && <p className="muted small">Loading folders…</p>}
            {roots && (
              <ul role="tree" aria-label="Depot folders" className="folder-tree">
                {tree.map((e) => (
                  <Folder
                    key={e.name}
                    path={e.name}
                    name={e.name}
                    depth={0}
                    spec={draft}
                    busy={busy}
                    load={loadDir}
                    onTick={tick}
                  />
                ))}
              </ul>
            )}
          </div>
          <div className="view-patterns">
            <h2>Patterns</h2>
            <label>
              Include
              <textarea
                className="mono"
                aria-label="Include patterns"
                value={includeText}
                onChange={(e) => editText("include", e.target.value)}
                placeholder="Everything (no patterns)"
                rows={5}
                disabled={busy}
                spellCheck={false}
              />
            </label>
            <label>
              Exclude
              <textarea
                className="mono"
                aria-label="Exclude patterns"
                value={excludeText}
                onChange={(e) => editText("exclude", e.target.value)}
                placeholder="Nothing left out"
                rows={5}
                disabled={busy}
                spellCheck={false}
              />
            </label>
            <p className="hint">
              One pattern per line, such as <span className="mono">Content/Cinematics/**</span>.{" "}
              <span className="mono">*</span> stays within a folder, <span className="mono">**</span> goes into
              subfolders. Excludes win.
            </p>
          </div>
        </div>

        {preview && (
          <div className="view-preview" role="region" aria-label="Preview">
            <h2>What changes</h2>
            <p>
              Fetch {count(preview.fetch.files, "file")} ({humanBytes(preview.fetch.bytes)}); remove{" "}
              {count(preview.remove.files, "file")} from disk.
            </p>
            {(preview.fetch.paths.length > 0 || preview.remove.paths.length > 0) && (
              <details>
                <summary>Show files</summary>
                <ul className="preview-paths mono small">
                  {preview.fetch.paths.map((p) => (
                    <li key={`+${p}`}>+ {p}</li>
                  ))}
                  {preview.remove.paths.map((p) => (
                    <li key={`-${p}`}>− {p}</li>
                  ))}
                </ul>
              </details>
            )}
            {preview.blocked.length > 0 && (
              <div className="notice warn" role="note" aria-label="Files with local changes">
                These files have local changes and would leave the view, so they are kept and nothing changes. Commit,
                shelve or revert them first, or drop them anyway.
                <ul className="locks-left">
                  {preview.blocked.map((p) => (
                    <li key={p} className="mono">
                      {p}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {preview.untracked.length > 0 && (
              <div className="notice warn" role="note" aria-label="Untracked files in the way">
                Untracked files are where incoming files would go:
                <ul className="locks-left">
                  {preview.untracked.map((p) => (
                    <li key={p} className="mono">
                      {p}
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>
        )}

        {applying && progress && !progress.finished && <ProgressBar event={progress} />}
        {done && (
          <p className="done" role="status">
            View updated: {count(done.added, "file")} fetched, {done.removed.toLocaleString("en-US")} removed.
          </p>
        )}
        <div className="view-actions">
          {applying && <span className="muted small">Updating the files on disk. You can keep working meanwhile.</span>}
          <span className="spacer" />
          <button onClick={() => summary && setSpec(summary.view)} disabled={busy || !dirty}>
            Reset
          </button>
          {!preview ? (
            <button className="primary" onClick={() => void check()} disabled={busy || !dirty}>
              {checking ? "Checking…" : "Preview change"}
            </button>
          ) : stopped ? (
            <button className="danger" onClick={() => setConfirmForce(true)} disabled={busy}>
              Drop them anyway…
            </button>
          ) : (
            <button className="primary" onClick={() => void apply(false)} disabled={busy}>
              {applying ? "Applying…" : "Apply"}
            </button>
          )}
        </div>
      </div>
      {confirmForce && preview && (
        <Modal title="Drop local changes?" onClose={() => setConfirmForce(false)}>
          <p>
            {preview.blocked.length > 0 && (
              <>The changes to {count(preview.blocked.length, "file")} are lost when they leave the view. </>
            )}
            {preview.untracked.length > 0 && <>Untracked files in the way are overwritten. </>}
            This cannot be undone.
          </p>
          <div className="form-actions">
            <button onClick={() => setConfirmForce(false)} autoFocus>
              Cancel
            </button>
            <button className="danger" onClick={() => void apply(true)}>
              Drop and apply
            </button>
          </div>
        </Modal>
      )}
    </section>
  );
}
