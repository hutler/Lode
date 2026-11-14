import { useCallback, useEffect, useState } from "react";

import { asOpError, backend, type LockInfo, type OpError, type WorkspaceInfo } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { formatDate } from "./HistoryView";
import { Modal } from "./Modal";

interface Props {
  info: WorkspaceInfo;
}

/** The branch's locks: who holds what, with unlock for mine and break for others'. */
export function LocksView({ info }: Props) {
  const [locks, setLocks] = useState<LockInfo[] | null>(null);
  const [mineOnly, setMineOnly] = useState(false);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<OpError | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [breaking, setBreaking] = useState<LockInfo | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const r = await (await backend()).locks(info.root, false);
      setLocks(r.locks);
      setUnavailable(false);
    } catch (e) {
      const op = asOpError(e);
      if (op.code === "module_disabled") setUnavailable(true);
      else setError(op);
    }
  }, [info.root]);

  useEffect(() => {
    void load();
  }, [load]);

  const release = async (paths: string[], force: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await (await backend()).unlock(info.root, paths, force);
    } catch (e) {
      setError(asOpError(e));
    } finally {
      setBusy(false);
      await load();
    }
  };

  const needle = filter.trim().toLowerCase();
  const shown = (locks ?? [])
    .filter((l) => !mineOnly || l.mine)
    .filter((l) => !needle || l.path.toLowerCase().includes(needle))
    .sort((a, b) => Number(b.mine) - Number(a.mine) || a.path.localeCompare(b.path));
  const mine = (locks ?? []).filter((l) => l.mine);

  return (
    <section className="locks" aria-label="Locks">
      <header className="changes-head">
        <h1>Locks on {info.branch}</h1>
        <span className="muted small">
          {locks ? `${locks.length} held, ${mine.length} by you` : ""}
        </span>
        <input
          className="lock-filter"
          type="search"
          aria-label="Filter by path"
          placeholder="Filter by path"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <label className="check-label">
          <input type="checkbox" checked={mineOnly} onChange={(e) => setMineOnly(e.target.checked)} />
          Only mine
        </label>
        {mine.length > 1 && (
          <button onClick={() => void release(mine.map((l) => l.path), false)} disabled={busy}>
            Unlock all mine
          </button>
        )}
        <button className="refresh" onClick={() => void load()} disabled={busy}>
          Refresh
        </button>
      </header>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <div className="lock-list">
        {unavailable ? (
          <p className="empty">This server does not have the locks module, so files cannot be locked here.</p>
        ) : locks && shown.length === 0 ? (
          <p className="empty">{locks.length === 0 ? "Nobody holds a lock on this branch." : "No lock matches."}</p>
        ) : (
          <table className="lock-table">
            <thead>
              <tr>
                <th>File</th>
                <th>Held by</th>
                <th>Since</th>
                <th>Comment</th>
                <th aria-label="Actions" />
              </tr>
            </thead>
            <tbody>
              {shown.map((l) => (
                <tr key={l.path} className={l.mine ? "mine" : ""}>
                  <td className="mono lock-path" title={l.path}>
                    {l.path}
                  </td>
                  <td>
                    {l.mine ? <strong>you</strong> : l.owner.name}
                    {l.owner.kind === "agent" && <span className="tag agent">agent</span>}
                    <div className="muted small mono">{l.workspace}</div>
                  </td>
                  <td className="muted">{formatDate(l.created_at)}</td>
                  <td>{l.comment ?? ""}</td>
                  <td className="lock-actions">
                    {l.mine ? (
                      <button onClick={() => void release([l.path], false)} disabled={busy} aria-label={`Unlock ${l.path}`}>
                        Unlock
                      </button>
                    ) : (
                      <button onClick={() => setBreaking(l)} disabled={busy} aria-label={`Break lock on ${l.path}`}>
                        Break…
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
      {breaking && (
        <Modal title="Break this lock?" onClose={() => setBreaking(null)}>
          <p>
            <span className="mono">{breaking.path}</span> is locked by <strong>{breaking.owner.name}</strong>
            {breaking.comment ? ` (${breaking.comment})` : ""}. Breaking it lets anyone commit the file, and{" "}
            {breaking.owner.name} may lose work if they commit after someone else. Only depot admins can break locks.
          </p>
          <div className="form-actions">
            <button onClick={() => setBreaking(null)} autoFocus>
              Cancel
            </button>
            <button
              className="danger"
              onClick={() => {
                const l = breaking;
                setBreaking(null);
                void release([l.path], true);
              }}
            >
              Break lock
            </button>
          </div>
        </Modal>
      )}
    </section>
  );
}
