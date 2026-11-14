import { useEffect, useState, type FormEvent } from "react";

import { asOpError, backend, type OpError, type ProgressEvent, type WorkspaceInfo } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";
import { ProgressBar } from "./ProgressBar";

interface Props {
  /** Filled in when a link asked for the checkout. */
  initialTarget?: string;
  initialBranch?: string;
  onClose: () => void;
  onDone: (info: WorkspaceInfo) => void;
  onNeedSignIn: (server: string) => void;
}

/** The server part of `https://host/depot`, or null when there is no depot in it. */
export function serverOfTarget(target: string): string | null {
  const t = target.trim().replace(/\/+$/, "");
  const at = t.lastIndexOf("/");
  if (!t.includes("://") || at < 0 || t.slice(0, at).endsWith("/")) return null;
  return t.slice(0, at);
}

/** Creates a workspace from a depot: `lode checkout <server>/<depot> <folder> -b <branch>`. */
export function CheckoutDialog({ initialTarget, initialBranch, onClose, onDone, onNeedSignIn }: Props) {
  const [target, setTarget] = useState(initialTarget ?? "https://");
  const [folder, setFolder] = useState("");
  const [branch, setBranch] = useState(initialBranch ?? "main");
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [error, setError] = useState<OpError | null>(null);

  useEffect(() => {
    let stop: (() => void) | undefined;
    void backend().then((b) => {
      stop = b.onProgress(setProgress);
    });
    return () => stop?.();
  }, []);

  const pick = async () => {
    const b = await backend();
    const path = await b.pickFolder("Folder for the new workspace");
    if (path) setFolder(path);
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setProgress(null);
    try {
      const b = await backend();
      const done = await b.checkout(target.trim(), folder.trim(), branch.trim() || "main");
      onDone(await b.openWorkspace(done.workspace));
    } catch (err) {
      setError(asOpError(err));
    } finally {
      setBusy(false);
    }
  };

  const server = serverOfTarget(target);

  return (
    <Modal title="Check out a depot" onClose={busy ? () => {} : onClose}>
      <form className="form" onSubmit={submit}>
        <label>
          Depot
          <input
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            placeholder="https://lode.example.com/game"
            required
            autoFocus
            disabled={busy}
          />
          <span className="hint">The server address followed by the depot name.</span>
        </label>
        <label>
          Branch
          <input value={branch} onChange={(e) => setBranch(e.target.value)} disabled={busy} />
        </label>
        <label>
          Folder
          <span className="input-row">
            <input
              value={folder}
              onChange={(e) => setFolder(e.target.value)}
              placeholder="Where the files go"
              required
              disabled={busy}
            />
            <button type="button" onClick={pick} disabled={busy}>
              Browse…
            </button>
          </span>
        </label>
        {busy && progress && <ProgressBar event={progress} />}
        {error && (
          <>
            <ErrorBanner error={error} />
            {error.code === "unauthenticated" && server && (
              <button type="button" className="link" onClick={() => onNeedSignIn(server)}>
                Sign in to {server}
              </button>
            )}
          </>
        )}
        <div className="form-actions">
          <button type="button" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button className="primary" type="submit" disabled={busy}>
            {busy ? "Checking out…" : "Check out"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
