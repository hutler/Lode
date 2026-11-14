import { useEffect, useState, type FormEvent } from "react";

import { asOpError, backend, type OpError, type ShelfInfo } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";

/** What to shelve: picked files, or every file of a changelist. */
export type ShelveWhat =
  | { kind: "files"; paths: string[]; untracked: string[] }
  | { kind: "changelist"; name: string; description: string; files: number };

interface Props {
  workspace: string;
  what: ShelveWhat;
  onClose: () => void;
  /** Called after shelving, with a line to show. */
  onDone: (message: string) => void;
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

/** Puts changes on a new shelf, or on an open shelf of mine as its next version. */
export function ShelveDialog({ workspace, what, onClose, onDone }: Props) {
  const count = what.kind === "files" ? what.paths.length + what.untracked.length : what.files;
  const [text, setText] = useState(what.kind === "changelist" ? what.description : "");
  const [keep, setKeep] = useState(false);
  const [mine, setMine] = useState<ShelfInfo[]>([]);
  const [target, setTarget] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const r = await (await backend()).shelves(workspace, true, "open");
        if (live) setMine(r.shelves);
      } catch {
        // Without the list, only a new shelf is offered; shelving itself reports any error.
      }
    })();
    return () => {
      live = false;
    };
  }, [workspace]);

  const choose = (id: number | null) => {
    setTarget(id);
    const shelf = mine.find((s) => s.id === id);
    if (shelf && !text.trim()) setText(shelf.description);
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const b = await backend();
      let paths: string[] = [];
      if (what.kind === "files") {
        if (what.untracked.length) await b.add(workspace, what.untracked);
        paths = [...what.paths, ...what.untracked];
      }
      const r = await b.shelve(workspace, {
        paths,
        changelist: what.kind === "changelist" ? what.name : null,
        message: text.trim() || null,
        keep,
        update: target,
      });
      const where = r.updated ? `Updated shelf ${r.shelf} to version ${r.version}` : `Shelved as shelf ${r.shelf}`;
      onDone(`${where} (${plural(r.files, "file")})${r.kept ? "" : "; the files were reverted here"}.`);
      onClose();
    } catch (err) {
      setError(asOpError(err));
    } finally {
      setBusy(false);
    }
  };

  const title = what.kind === "changelist" ? `Shelve changelist ${what.name}` : `Shelve ${plural(count, "file")}`;
  return (
    <Modal title={title} onClose={onClose}>
      <form className="form" onSubmit={(e) => void submit(e)} aria-label="Shelve">
        <p className="muted small lock-what">
          A shelf keeps changes on the server without committing them, so others can review or unshelve them.
        </p>
        {mine.length > 0 && (
          <label>
            Put on
            <select value={target ?? ""} onChange={(e) => choose(e.target.value ? Number(e.target.value) : null)}>
              <option value="">A new shelf</option>
              {mine.map((s) => (
                <option key={s.id} value={s.id}>
                  Shelf {s.id} (version {s.version}): {s.description.split("\n")[0]}
                </option>
              ))}
            </select>
            {target !== null && <span className="hint">Its content is replaced and its version goes up.</span>}
          </label>
        )}
        <label>
          Description
          <textarea
            rows={3}
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="What the changes are for"
            autoFocus
          />
        </label>
        <label className="check-label">
          <input type="checkbox" checked={keep} onChange={(e) => setKeep(e.target.checked)} />
          Keep my changes in the workspace
        </label>
        {!keep && <span className="hint">Otherwise the shelved files are reverted here.</span>}
        {error && <ErrorBanner error={error} />}
        <div className="form-actions">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button className="primary" type="submit" disabled={busy || (!text.trim() && target === null)}>
            {busy ? "Shelving…" : target === null ? "Shelve" : `Update shelf ${target}`}
          </button>
        </div>
      </form>
    </Modal>
  );
}
