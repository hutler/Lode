import { useState, type FormEvent } from "react";

import { asOpError, backend, type OpError } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { errorPaths } from "./LockDialog";
import { Modal } from "./Modal";

/** What the Changes view asks about a changelist. */
export type ChangelistAsk =
  | { kind: "new"; picked: string[] }
  | { kind: "commit"; name: string; description: string; files: number }
  | { kind: "delete"; name: string; files: number };

interface Props {
  workspace: string;
  ask: ChangelistAsk;
  onClose: () => void;
  /** Called after a change, with a line to show when there is one. */
  onDone: (message: string | null) => void;
}

const NAME = /^[A-Za-z0-9._-]{1,64}$/;

/** Creating, committing and deleting a changelist. */
export function ChangelistDialogs({ workspace, ask, onClose, onDone }: Props) {
  const [name, setName] = useState("");
  const [text, setText] = useState(ask.kind === "commit" ? ask.description : "");
  const [movePicked, setMovePicked] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);

  const act = async (f: () => Promise<string | null>) => {
    setBusy(true);
    setError(null);
    try {
      const msg = await f();
      onDone(msg);
      onClose();
    } catch (e) {
      setError(asOpError(e));
    } finally {
      setBusy(false);
    }
  };

  if (ask.kind === "new") {
    const valid = NAME.test(name) && name !== "default";
    const submit = (e: FormEvent) => {
      e.preventDefault();
      void act(async () => {
        const b = await backend();
        await b.changelistNew(workspace, name, text);
        if (movePicked && ask.picked.length) await b.changelistMove(workspace, name, ask.picked);
        return null;
      });
    };
    return (
      <Modal title="New changelist" onClose={onClose}>
        <form className="form" onSubmit={submit} aria-label="New changelist">
          <label>
            Name
            <input value={name} onChange={(e) => setName(e.target.value.trim())} placeholder="harbor-lights" autoFocus />
            <span className="hint">Letters, digits, dots, dashes and underscores.</span>
          </label>
          <label>
            Description
            <textarea
              rows={3}
              value={text}
              onChange={(e) => setText(e.target.value)}
              placeholder="Used as the commit message when you commit the changelist"
            />
          </label>
          {ask.picked.length > 0 && (
            <label className="check-label">
              <input type="checkbox" checked={movePicked} onChange={(e) => setMovePicked(e.target.checked)} />
              Move the {ask.picked.length} picked file{ask.picked.length === 1 ? "" : "s"} into it
            </label>
          )}
          {error && <ErrorBanner error={error} />}
          <div className="form-actions">
            <button type="button" onClick={onClose}>
              Cancel
            </button>
            <button className="primary" type="submit" disabled={busy || !valid}>
              Create
            </button>
          </div>
        </form>
      </Modal>
    );
  }

  if (ask.kind === "commit") {
    const refused = errorPaths(error);
    const submit = (e: FormEvent) => {
      e.preventDefault();
      void act(async () => {
        const r = await (await backend()).commitChangelist(workspace, ask.name, text.trim() || null);
        return `Committed r${r.rev} (${r.files} file${r.files === 1 ? "" : "s"}) from ${ask.name}.`;
      });
    };
    return (
      <Modal title={`Commit changelist ${ask.name}`} onClose={onClose}>
        <form className="form" onSubmit={submit} aria-label="Commit changelist">
          <p className="muted small lock-what">
            Commits exactly the {ask.files} file{ask.files === 1 ? "" : "s"} in {ask.name}, whatever else is picked.
          </p>
          <label>
            Message
            <textarea rows={4} value={text} onChange={(e) => setText(e.target.value)} autoFocus />
          </label>
          {error && <ErrorBanner error={error} />}
          {refused.length > 0 && (
            <ul className="error-paths" aria-label="Files the server refused">
              {refused.map((p) => (
                <li key={p} className="mono">
                  {p}
                </li>
              ))}
            </ul>
          )}
          <div className="form-actions">
            <button type="button" onClick={onClose}>
              Cancel
            </button>
            <button className="primary" type="submit" disabled={busy || !text.trim()}>
              {busy ? "Committing…" : `Commit ${ask.files} file${ask.files === 1 ? "" : "s"}`}
            </button>
          </div>
        </form>
      </Modal>
    );
  }

  return (
    <Modal title={`Delete changelist ${ask.name}?`} onClose={onClose}>
      <p>
        {ask.files > 0
          ? `Its ${ask.files} file${ask.files === 1 ? "" : "s"} go back to Changes. No change is lost.`
          : "It holds no files. No change is lost."}
      </p>
      {error && <ErrorBanner error={error} />}
      <div className="form-actions">
        <button onClick={onClose} autoFocus>
          Cancel
        </button>
        <button
          className="danger"
          disabled={busy}
          onClick={() =>
            void act(async () => {
              await (await backend()).changelistDelete(workspace, ask.name);
              return null;
            })
          }
        >
          Delete
        </button>
      </div>
    </Modal>
  );
}
