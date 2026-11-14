import { useState, type FormEvent } from "react";

import { asOpError, backend, type LockInfo, type OpError } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { formatDate } from "./HistoryView";
import { Modal } from "./Modal";

/** The paths an error names in `details.paths`. */
export function errorPaths(e: OpError | null): string[] {
  if (!e || typeof e.details !== "object" || e.details === null) return [];
  const paths = (e.details as { paths?: unknown }).paths;
  return Array.isArray(paths) ? paths.filter((p): p is string => typeof p === "string") : [];
}

interface Props {
  workspace: string;
  paths: string[];
  onClose: () => void;
  /** Called after the files were locked. */
  onLocked: () => void;
}

/** Locks files with an optional comment. A refusal names who holds each file. */
export function LockDialog({ workspace, paths, onClose, onLocked }: Props) {
  const [comment, setComment] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const [holders, setHolders] = useState<LockInfo[]>([]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setHolders([]);
    const b = await backend();
    try {
      await b.lock(workspace, paths, comment.trim() || null);
      onLocked();
      onClose();
    } catch (err) {
      const op = asOpError(err);
      setError(op);
      if (op.code === "locked") {
        const named = new Set(errorPaths(op));
        try {
          const { locks } = await b.locks(workspace, false);
          setHolders(locks.filter((l) => named.has(l.path)));
        } catch {
          // The message already names the files; the holders are extra.
        }
      }
    } finally {
      setBusy(false);
    }
  };

  const one = paths.length === 1;
  return (
    <Modal title={one ? "Lock file" : `Lock ${paths.length} files`} onClose={onClose}>
      <form className="form" onSubmit={submit} aria-label="Lock">
        <p className="muted small lock-what">
          {one ? <span className="mono">{paths[0]}</span> : `${paths.length} files`}. Others can read the file but cannot commit
          it until you unlock it or commit.
        </p>
        <label>
          Comment (optional)
          <input
            value={comment}
            onChange={(e) => setComment(e.target.value)}
            placeholder="Why you need it, for the others"
            autoFocus
          />
        </label>
        {error && <ErrorBanner error={error} />}
        {holders.length > 0 && (
          <ul className="holders" aria-label="Held by others">
            {holders.map((h) => (
              <li key={h.path}>
                <span className="mono">{h.path}</span>
                <span>
                  {h.owner.name}
                  {h.owner.kind === "agent" && <span className="tag agent">agent</span>}
                </span>
                <span className="muted small">
                  since {formatDate(h.created_at)}
                  {h.comment ? `: ${h.comment}` : ""}
                </span>
              </li>
            ))}
          </ul>
        )}
        {error?.code === "out_of_date" && (
          <p className="muted small">Get the newest version first (Update), then lock again.</p>
        )}
        <div className="form-actions">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button className="primary" type="submit" disabled={busy}>
            {busy ? "Locking…" : "Lock"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
