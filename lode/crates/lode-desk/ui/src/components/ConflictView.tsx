import { useEffect, useMemo, useState } from "react";

import { asOpError, backend, type ConflictInfo, type ConflictSide, type OpError, type WorkspaceInfo } from "../api";
import { changedLines, lines, parseConflicts, takeRegion, type Take } from "../merge";
import { ErrorBanner } from "./ErrorBanner";
import { humanBytes } from "./ProgressBar";

interface Props {
  info: WorkspaceInfo;
  path: string;
  onBack: () => void;
  /** The conflict was resolved; `how` says how, for the message shown after. */
  onResolved: (how: string) => void;
}

/** One side of the three-way view, with the lines that differ from the base marked. */
function Pane({ label, side, base }: { label: string; side: ConflictSide | null; base: string[] | null }) {
  const own = useMemo(() => (side?.text != null ? lines(side.text) : null), [side]);
  const changed = useMemo(() => (own && base ? changedLines(base, own) : null), [own, base]);
  return (
    <figure className="pane" aria-label={label}>
      <figcaption>
        <strong>{label}</strong>
        {side ? (
          <span className="muted small">{humanBytes(side.size)}</span>
        ) : (
          <span className="muted small">no file</span>
        )}
        {changed && changed.size > 0 && (
          <span className="muted small">
            {changed.size} line{changed.size === 1 ? "" : "s"} changed
          </span>
        )}
      </figcaption>
      <div className="pane-body">
        {own ? (
          <pre>
            {own.map((l, i) => (
              <div
                key={i}
                className={changed?.has(i) ? "line changed" : "line"}
                data-changed={changed?.has(i) || undefined}
              >
                <span className="ln">{i + 1}</span>
                <span className="lt">{l || " "}</span>
              </div>
            ))}
          </pre>
        ) : (
          <p className="muted small pane-empty">
            {side ? "Too large to show." : "The file does not exist on this side."}
          </p>
        )}
      </div>
    </figure>
  );
}

function sideNote(side: ConflictSide | null, deleted: string): string {
  return side ? humanBytes(side.size) : deleted;
}

/** Resolves one conflicted file: a three-way view with an editable result for text, or a choice
 * of sides for a binary file or a symlink. */
export function ConflictView({ info, path, onBack, onResolved }: Props) {
  const [conflict, setConflict] = useState<ConflictInfo | null>(null);
  const [result, setResult] = useState("");
  const [error, setError] = useState<OpError | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    setConflict(null);
    void (async () => {
      try {
        const c = await (await backend()).conflict(info.root, path);
        if (!live) return;
        setConflict(c);
        setResult(c.result?.text ?? "");
      } catch (e) {
        if (live) setError(asOpError(e));
      }
    })();
    return () => {
      live = false;
    };
  }, [info.root, path]);

  const regions = useMemo(() => parseConflicts(result).flatMap((p) => (p.kind === "conflict" ? [p] : [])), [result]);
  const baseLines = useMemo(() => (conflict?.base?.text != null ? lines(conflict.base.text) : []), [conflict]);

  const act = async (how: string, f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      onResolved(how);
    } catch (e) {
      setError(asOpError(e));
      setBusy(false);
    }
  };
  const using = (u: "mine" | "theirs") =>
    act(u === "mine" ? "kept your version" : "took their version", async () =>
      (await backend()).resolve(info.root, [path], u),
    );
  const save = () => act("saved the edited result", async () => (await backend()).resolveWith(info.root, path, result));
  const take = (i: number, t: Take) => setResult((r) => takeRegion(r, i, t));

  const name = path.slice(path.lastIndexOf("/") + 1);
  const from = conflict ? (conflict.theirs_rev === null ? "a shelf" : `r${conflict.theirs_rev}`) : "";
  const editable = !!conflict?.text && conflict.result?.text != null;

  return (
    <section className="conflict" aria-label="Resolve conflict">
      <header className="conflict-head">
        <button className="link" onClick={onBack}>
          ← Changes
        </button>
        <h1>Resolve {name}</h1>
        <span className="mono muted small">{path}</span>
        {conflict && <span className="muted small">Theirs came from {from}</span>}
      </header>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {!conflict && !error && <p className="muted small">Loading the conflict…</p>}
      {conflict && !editable && (
        <div className="conflict-binary">
          <p>
            {conflict.text
              ? "This file is too large to edit here. Keep one side, or edit it in another program and mark it resolved."
              : "This file cannot be merged line by line. Keep one side."}
          </p>
          <div className="binary-sides">
            <div className="binary-side" role="group" aria-label="Mine">
              <strong>Mine</strong>
              <span>{sideNote(conflict.mine, "Deleted here")}</span>
              <span className="muted small mono">{path}</span>
              <button className="primary" onClick={() => void using("mine")} disabled={busy}>
                Keep mine
              </button>
            </div>
            <div className="binary-side" role="group" aria-label="Theirs">
              <strong>Theirs ({from})</strong>
              <span>{sideNote(conflict.theirs, "Deleted there")}</span>
              {conflict.theirs_file && <span className="muted small mono">{conflict.theirs_file}</span>}
              <button className="primary" onClick={() => void using("theirs")} disabled={busy}>
                Take theirs
              </button>
            </div>
          </div>
          {conflict.text && (
            <button
              onClick={() =>
                void act("marked it resolved", async () => (await backend()).resolve(info.root, [path], "merged"))
              }
              disabled={busy}
            >
              Mark resolved as it is now
            </button>
          )}
        </div>
      )}
      {conflict && editable && (
        <>
          <div className="three-way">
            <Pane label="Base" side={conflict.base} base={null} />
            <Pane label="Mine" side={conflict.mine} base={baseLines} />
            <Pane label={`Theirs (${from})`} side={conflict.theirs} base={baseLines} />
          </div>
          {regions.length > 0 && (
            <div className="regions" aria-label="Conflict regions">
              {regions.map((r, i) => (
                <div key={i} className="region" role="group" aria-label={`Conflict ${i + 1} of ${regions.length}`}>
                  <div className="region-head">
                    <strong>
                      Conflict {i + 1} of {regions.length}
                    </strong>
                    <span className="region-buttons">
                      <button onClick={() => take(i, "mine")}>Use mine</button>
                      <button onClick={() => take(i, "theirs")}>Use theirs</button>
                      <button onClick={() => take(i, "both")}>Use both</button>
                    </span>
                  </div>
                  <div className="region-sides">
                    <pre aria-label="Mine" className="region-mine">
                      {r.mine.join("\n") || "(nothing)"}
                    </pre>
                    <pre aria-label="Theirs" className="region-theirs">
                      {r.theirs.join("\n") || "(nothing)"}
                    </pre>
                  </div>
                </div>
              ))}
            </div>
          )}
          <label className="result">
            <span className="result-head">
              <strong>Result</strong>
              <span className="muted small">
                {regions.length
                  ? `${regions.length} conflict${regions.length === 1 ? "" : "s"} left. Pick a side above, or edit the text.`
                  : "No conflicts left."}
              </span>
            </span>
            <textarea
              aria-label="Result"
              className="mono"
              value={result}
              onChange={(e) => setResult(e.target.value)}
              spellCheck={false}
              rows={14}
              disabled={busy}
            />
          </label>
          <div className="conflict-actions">
            <button onClick={() => void using("mine")} disabled={busy}>
              Take mine
            </button>
            <button onClick={() => void using("theirs")} disabled={busy}>
              Take theirs
            </button>
            <span className="spacer" />
            {regions.length > 0 && (
              <span className="muted small" id="save-why">
                Resolve every conflict first.
              </span>
            )}
            <button
              className="primary"
              onClick={() => void save()}
              disabled={busy || regions.length > 0}
              aria-describedby={regions.length ? "save-why" : undefined}
            >
              Save and mark resolved
            </button>
          </div>
        </>
      )}
    </section>
  );
}
