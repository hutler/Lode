import { useEffect, useMemo, useState, type FormEvent } from "react";

import {
  asOpError,
  backend,
  type BranchInfo,
  type CreatedBranch,
  type OpError,
  type UpdateResult,
  type WorkspaceInfo,
} from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";
import { ProgressBar, useProgress } from "./ProgressBar";

/** `r1390` or `1390` as a revision; null when blank; NaN when it is neither. */
export function parseRev(text: string): number | null {
  const t = text.trim().replace(/^r/i, "");
  if (!t) return null;
  return /^\d+$/.test(t) ? Number(t) : NaN;
}

function origin(b: BranchInfo): string {
  return b.parent ? `from ${b.parent.branch}@r${b.parent.rev}` : "first branch";
}

interface PickerProps {
  info: WorkspaceInfo;
  onClose: () => void;
  /** The workspace moved to another branch. */
  onSwitched: (result: UpdateResult) => void;
  /** Opens the create dialog, starting from `from` (`BRANCH`). */
  onCreate: (from: string, branches: BranchInfo[]) => void;
  /** The branch to select first (default: the current one). */
  initial?: string;
  /** Opens the merge dialog for merging `source` into the workspace. */
  onMerge?: (source: string, branches: BranchInfo[]) => void;
}

/** The depot's branches: filter, switch to one (its head or a revision), create or archive. */
export function BranchPicker({ info, onClose, onSwitched, onCreate, initial, onMerge }: PickerProps) {
  const [list, setList] = useState<{
    current: string;
    branches: BranchInfo[];
  } | null>(null);
  const [all, setAll] = useState(false);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<string | null>(initial ?? null);
  const [rev, setRev] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<OpError | null>(null);
  const [archiving, setArchiving] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const progress = useProgress();

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const b = await backend();
        const l = await b.branches(info.root, all);
        if (!live) return;
        setList(l);
        setSelected((s) => (s && l.branches.some((x) => x.name === s) ? s : l.current));
      } catch (e) {
        if (live) setError(asOpError(e));
      }
    })();
    return () => {
      live = false;
    };
  }, [info.root, all, reload]);

  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const branches = list?.branches ?? [];
    // The current branch first, then active ones by name, archived ones last.
    return branches
      .filter((b) => !f || b.name.toLowerCase().includes(f))
      .sort(
        (a, b) =>
          Number(b.name === list?.current) - Number(a.name === list?.current) ||
          Number(a.state === "archived") - Number(b.state === "archived") ||
          a.name.localeCompare(b.name),
      );
  }, [list, filter]);

  const pick = list?.branches.find((b) => b.name === selected) ?? null;
  const isCurrent = !!pick && pick.name === list?.current;
  const wantedRev = parseRev(rev);
  const badRev =
    Number.isNaN(wantedRev) || (wantedRev !== null && !!pick && (wantedRev < 1 || wantedRev > pick.head_rev));

  const doSwitch = async (e: FormEvent) => {
    e.preventDefault();
    if (!pick || badRev) return;
    setBusy(`Switching to ${pick.name}…`);
    setError(null);
    try {
      const b = await backend();
      const result = await b.switchBranch(info.root, pick.name, wantedRev);
      onSwitched(result);
    } catch (err) {
      setError(asOpError(err));
      setBusy(null);
    }
  };

  /** Brings the workspace to the head of its own branch. */
  const doUpdate = async () => {
    setBusy("Updating…");
    setError(null);
    try {
      const result = await (await backend()).update(info.root, null);
      onSwitched(result);
    } catch (err) {
      setError(asOpError(err));
      setBusy(null);
    }
  };

  const doArchive = async (name: string) => {
    setArchiving(null);
    setBusy(`Archiving ${name}…`);
    setError(null);
    try {
      const b = await backend();
      await b.branchArchive(info.root, name);
      setReload((n) => n + 1);
    } catch (err) {
      setError(asOpError(err));
    } finally {
      setBusy(null);
    }
  };

  if (archiving) {
    return (
      <Modal title={`Archive ${archiving}?`} onClose={() => setArchiving(null)}>
        <p>
          The branch leaves the list and nobody can commit to it any more. Its history stays, and workspaces on it keep
          their files.
        </p>
        <div className="form-actions">
          <button onClick={() => setArchiving(null)} autoFocus>
            Cancel
          </button>
          <button className="danger" onClick={() => void doArchive(archiving)}>
            Archive branch
          </button>
        </div>
      </Modal>
    );
  }

  return (
    <Modal title="Branches" onClose={busy ? () => {} : onClose} wide>
      <div className="branch-picker">
        <div className="branch-filter">
          <input
            type="search"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter branches"
            aria-label="Filter branches"
            autoFocus
          />
          <label className="check-label">
            <input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} />
            Show archived
          </label>
        </div>
        {!list && !error && <p className="muted small">Loading branches…</p>}
        {list && (
          <ul className="branch-list" role="listbox" aria-label="Branches">
            {shown.map((b) => (
              <li
                key={b.name}
                role="option"
                aria-selected={b.name === selected}
                className={`branch-row ${b.name === selected ? "selected" : ""} ${b.state === "archived" ? "archived" : ""}`}
                onClick={() => {
                  setSelected(b.name);
                  setRev("");
                  setError(null);
                }}
              >
                <span className="branch-name">{b.name}</span>
                {b.name === list.current && <span className="tag branch">current</span>}
                {b.state === "archived" && <span className="tag">archived</span>}
                <span className="branch-meta muted small">
                  <span className="mono">r{b.head_rev}</span> · {origin(b)}
                </span>
              </li>
            ))}
            {shown.length === 0 && <li className="muted small branch-empty">No branch matches “{filter.trim()}”.</li>}
          </ul>
        )}
        {busy && progress && !progress.finished && <ProgressBar event={progress} />}
        {busy && <p className="muted small">{busy}</p>}
        {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
        <form className="branch-actions" onSubmit={doSwitch}>
          {pick && !isCurrent && (
            <label className="branch-rev">
              Revision
              <input
                value={rev}
                onChange={(e) => setRev(e.target.value)}
                placeholder={`head (r${pick.head_rev})`}
                aria-label="Revision"
                aria-invalid={badRev}
                className="mono"
                size={12}
                disabled={!!busy}
              />
            </label>
          )}
          <span className="spacer" />
          <button
            type="button"
            onClick={() => list && onCreate(pick?.name ?? list.current, list.branches)}
            disabled={!list || !!busy}
          >
            New branch…
          </button>
          {pick && !isCurrent && pick.state === "active" && onMerge && list && (
            <button type="button" onClick={() => onMerge(pick.name, list.branches)} disabled={!!busy}>
              Merge…
            </button>
          )}
          {pick && !isCurrent && pick.state === "active" && (
            <button type="button" onClick={() => setArchiving(pick.name)} disabled={!!busy}>
              Archive…
            </button>
          )}
          {pick && !isCurrent && (
            <button className="primary" type="submit" disabled={!!busy || badRev}>
              Switch
            </button>
          )}
          {(!pick || isCurrent) && (
            <button type="button" onClick={onClose} disabled={!!busy}>
              Close
            </button>
          )}
          {isCurrent && pick && (
            <button className="primary" type="button" onClick={() => void doUpdate()} disabled={!!busy}>
              Update to r{pick.head_rev}
            </button>
          )}
        </form>
        {pick && !isCurrent && (
          <p className="hint">
            Your local changes come along to <strong>{pick.name}</strong>. Files that do not merge are left in conflict
            in Changes.
          </p>
        )}
      </div>
    </Modal>
  );
}

interface CreateProps {
  info: WorkspaceInfo;
  /** The branch picked in the list, offered as the starting point. */
  from: string;
  branches: BranchInfo[];
  onClose: () => void;
  onCreated: (created: CreatedBranch) => void;
}

/** A new branch from the workspace's branch at its revision, or from any branch at any revision. */
export function CreateBranchDialog({ info, from, branches, onClose, onCreated }: CreateProps) {
  const [name, setName] = useState("");
  const [here, setHere] = useState(from === info.branch);
  const [base, setBase] = useState(from);
  const [rev, setRev] = useState("");
  const [switchTo, setSwitchTo] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const progress = useProgress();

  const active = branches.filter((b) => b.state === "active");
  const baseInfo = branches.find((b) => b.name === base);
  const wantedRev = parseRev(rev);
  const badRev =
    !here &&
    (Number.isNaN(wantedRev) || (wantedRev !== null && !!baseInfo && (wantedRev < 1 || wantedRev > baseInfo.head_rev)));

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (badRev || !name.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const b = await backend();
      const spec = here ? null : wantedRev === null ? base : `${base}@r${wantedRev}`;
      onCreated(await b.branchCreate(info.root, name.trim(), spec, switchTo));
    } catch (err) {
      setError(asOpError(err));
      setBusy(false);
    }
  };

  return (
    <Modal title="New branch" onClose={busy ? () => {} : onClose}>
      <form className="form" onSubmit={submit}>
        <label>
          Name
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="feature/harbor-lights"
            required
            autoFocus
            disabled={busy}
          />
          <span className="hint">Letters, digits, dots, dashes and underscores; slashes group branches.</span>
        </label>
        <fieldset className="choice" disabled={busy}>
          <legend>Start from</legend>
          <label className="check-label">
            <input type="radio" name="from" checked={here} onChange={() => setHere(true)} />
            This workspace: {info.branch} at r{info.rev}
          </label>
          <label className="check-label">
            <input type="radio" name="from" checked={!here} onChange={() => setHere(false)} />
            Another branch or revision
          </label>
          {!here && (
            <span className="input-row indent">
              <select value={base} onChange={(e) => setBase(e.target.value)} aria-label="Branch to start from">
                {active.map((b) => (
                  <option key={b.name} value={b.name}>
                    {b.name}
                  </option>
                ))}
              </select>
              <input
                value={rev}
                onChange={(e) => setRev(e.target.value)}
                placeholder={baseInfo ? `head (r${baseInfo.head_rev})` : "head"}
                aria-label="Revision to start from"
                aria-invalid={badRev}
                className="mono"
                size={12}
              />
            </span>
          )}
        </fieldset>
        <label className="check-label">
          <input type="checkbox" checked={switchTo} onChange={(e) => setSwitchTo(e.target.checked)} disabled={busy} />
          Switch this workspace to it
        </label>
        {busy && switchTo && progress && !progress.finished && <ProgressBar event={progress} />}
        {error && <ErrorBanner error={error} />}
        <div className="form-actions">
          <button type="button" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button className="primary" type="submit" disabled={busy || badRev || !name.trim()}>
            {busy ? "Creating…" : switchTo ? "Create and switch" : "Create branch"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

/** What an update or switch did: the counts, each file, conflicts and the locks left behind. */
export function SyncResultDialog({
  result,
  onClose,
  onChanges,
}: {
  result: UpdateResult;
  onClose: () => void;
  onChanges: () => void;
}) {
  const conflicted = result.files.filter((f) => f.outcome === "conflicted");
  const LIMIT = 40;
  const title = result.switched_from ? `Switched to ${result.branch}` : `Updated to r${result.rev}`;
  return (
    <Modal title={title} onClose={onClose} wide>
      <p>
        {result.switched_from ? (
          <>
            The workspace is on <strong>{result.branch}</strong> at r{result.rev}, from {result.switched_from}.{" "}
          </>
        ) : (
          <>
            The workspace is at r{result.rev}, from r{result.from}.{" "}
          </>
        )}
        {Object.entries(result.counts)
          .map(([k, n]) => `${n} ${k}`)
          .join(", ") || "No files changed."}
      </p>
      {result.files.length > 0 && (
        <ul className="outcomes sync-outcomes" aria-label="Files">
          {result.files.slice(0, LIMIT).map((f) => (
            <li key={f.path}>
              <span className={`tag outcome-${f.outcome}`}>{f.outcome}</span>
              <span className="mono">{f.path}</span>
            </li>
          ))}
          {result.files.length > LIMIT && <li className="muted small">and {result.files.length - LIMIT} more</li>}
        </ul>
      )}
      {conflicted.length > 0 && (
        <div className="notice warn" role="note" aria-label="Conflicts">
          {conflicted.length === 1 ? "1 file" : `${conflicted.length} files`} did not merge with your local changes.
          They are marked in Changes; resolve them before you commit.
        </div>
      )}
      {result.locks_left.length > 0 && (
        <div className="notice" role="note" aria-label="Locks left behind">
          <strong>Your locks stay on {result.switched_from}.</strong> Locks belong to a branch, so these are still held
          there. Unlock them from that branch when you are done:
          <ul className="locks-left">
            {result.locks_left.map((p) => (
              <li key={p} className="mono">
                {p}
              </li>
            ))}
          </ul>
        </div>
      )}
      <div className="form-actions">
        {conflicted.length > 0 && <button onClick={onChanges}>Show changes</button>}
        <button className="primary" onClick={onClose} autoFocus>
          Done
        </button>
      </div>
    </Modal>
  );
}
