import { useState, type FormEvent } from "react";

import { asOpError, backend, type BranchInfo, type MergeResult, type OpError, type WorkspaceInfo } from "../api";
import { parseRev } from "./BranchPicker";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";
import { ProgressBar, useProgress } from "./ProgressBar";

interface Props {
  info: WorkspaceInfo;
  branches: BranchInfo[];
  /** The branch to merge from, as picked in the branch list. */
  source: string;
  onClose: () => void;
  onMerged: (result: MergeResult) => void;
}

/** Merge another branch into the workspace: everything since the merge base, or one revision. */
export function MergeDialog({ info, branches, source, onClose, onMerged }: Props) {
  const others = branches.filter((b) => b.state === "active" && b.name !== info.branch);
  const [from, setFrom] = useState(others.some((b) => b.name === source) ? source : (others[0]?.name ?? ""));
  const [one, setOne] = useState(false);
  const [rev, setRev] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const progress = useProgress();

  const b = branches.find((x) => x.name === from);
  const wanted = parseRev(rev);
  const badRev = one && (wanted === null || Number.isNaN(wanted) || (!!b && (wanted < 1 || wanted > b.head_rev)));

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!from || badRev) return;
    setBusy(true);
    setError(null);
    try {
      onMerged(await (await backend()).merge(info.root, from, one ? wanted : null));
    } catch (err) {
      setError(asOpError(err));
      setBusy(false);
    }
  };

  return (
    <Modal title={`Merge into ${info.branch}`} onClose={busy ? () => {} : onClose}>
      <form className="form" onSubmit={submit}>
        <label>
          Merge from
          <select value={from} onChange={(e) => setFrom(e.target.value)} disabled={busy}>
            {others.map((x) => (
              <option key={x.name} value={x.name}>
                {x.name} (r{x.head_rev})
              </option>
            ))}
          </select>
        </label>
        <fieldset className="choice" disabled={busy}>
          <legend>What to merge</legend>
          <label className="check-label">
            <input type="radio" name="what" checked={!one} onChange={() => setOne(false)} />
            Everything since the last merge or where it started
          </label>
          <label className="check-label">
            <input type="radio" name="what" checked={one} onChange={() => setOne(true)} />
            Only one revision
          </label>
          {one && (
            <span className="input-row indent">
              <input
                value={rev}
                onChange={(e) => setRev(e.target.value)}
                placeholder={b ? `r${b.head_rev}` : "r"}
                aria-label="Revision to merge"
                aria-invalid={badRev && rev !== ""}
                className="mono"
                size={12}
                autoFocus
              />
            </span>
          )}
        </fieldset>
        <p className="hint">
          Changes are applied to your workspace; files changed on both sides are merged or left in conflict. Your next
          commit records the merge.
        </p>
        {busy && progress && !progress.finished && <ProgressBar event={progress} />}
        {error && <ErrorBanner error={error} />}
        <div className="form-actions">
          <button type="button" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button className="primary" type="submit" disabled={busy || !from || badRev}>
            {busy ? "Merging…" : "Merge"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

const range = (r: MergeResult) => (r.from_rev === r.to_rev ? `r${r.to_rev}` : `r${r.from_rev}..r${r.to_rev}`);

/** What a merge did to each file. */
export function MergeResultDialog({
  result,
  onClose,
  onChanges,
}: {
  result: MergeResult;
  onClose: () => void;
  onChanges: () => void;
}) {
  const conflicted = result.files.filter((f) => f.outcome === "conflicted");
  if (result.nothing_to_do) {
    return (
      <Modal title="Nothing to merge" onClose={onClose}>
        <p>Everything on {result.source} is already merged here.</p>
        <div className="form-actions">
          <button className="primary" onClick={onClose} autoFocus>
            Done
          </button>
        </div>
      </Modal>
    );
  }
  return (
    <Modal title={`Merged ${result.source} ${range(result)}`} onClose={onClose} wide>
      <p>
        {Object.entries(result.counts)
          .map(([k, n]) => `${n} ${k}`)
          .join(", ")}
        . Your next commit records the merge.
      </p>
      <ul className="outcomes sync-outcomes" aria-label="Files">
        {result.files.map((f) => (
          <li key={f.path}>
            <span className={`tag outcome-${f.outcome}`}>{f.outcome}</span>
            <span className="mono">{f.path}</span>
          </li>
        ))}
      </ul>
      {conflicted.length > 0 && (
        <div className="notice warn" role="note" aria-label="Conflicts">
          {conflicted.length === 1 ? "1 file" : `${conflicted.length} files`} changed on both sides and did not merge.
          Resolve them in Changes before you commit.
        </div>
      )}
      {result.files.some((f) => f.outcome === "skipped") && (
        <p className="muted small">Skipped files are outside this workspace's view.</p>
      )}
      <div className="form-actions">
        <button className={conflicted.length ? "primary" : ""} onClick={onChanges} autoFocus={conflicted.length > 0}>
          {conflicted.length ? "Resolve conflicts" : "Show changes"}
        </button>
        {conflicted.length === 0 && (
          <button className="primary" onClick={onClose} autoFocus>
            Done
          </button>
        )}
      </div>
    </Modal>
  );
}
