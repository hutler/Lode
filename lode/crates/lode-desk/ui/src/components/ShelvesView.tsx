import { useCallback, useEffect, useState, type FormEvent } from "react";

import {
  asOpError,
  backend,
  type ChangeView,
  type CheckInfo,
  type FileDiff,
  type MissingCheck,
  type OpError,
  type ShelfFileDiff,
  type ShelfInfo,
  type UnshelveResult,
  type WorkspaceInfo,
} from "../api";
import { useChecks } from "../checks";
import { CheckBadge, ChecksList } from "./Checks";
import { FileDiffBody, savedLayout, saveLayout, type Layout } from "./DiffView";
import { ErrorBanner } from "./ErrorBanner";
import { formatDate } from "./HistoryView";
import { Modal } from "./Modal";
import { humanBytes } from "./ProgressBar";

interface Props {
  info: WorkspaceInfo;
  /** Called after an unshelve changed the workspace. */
  onUnshelved?: () => void;
}

type Whose = "mine" | "all";
type State = ShelfInfo["state"];

const ACTION: Record<string, string> = { add: "A", modify: "M", delete: "D" };
const STATE_TEXT: Record<State, string> = { open: "Open", submitted: "Submitted", deleted: "Deleted" };

function firstLine(text: string): string {
  return text.split("\n")[0] ?? "";
}

function ownerText(s: ShelfInfo): string {
  return s.owner.kind === "agent" && s.agent_owner ? `${s.owner.name} for ${s.agent_owner}` : s.owner.name;
}

/** A shelf file's patch as the diff view takes it. */
export function asFileDiff(d: ShelfFileDiff, change: ChangeView | undefined): FileDiff {
  const lines = (d.patch ?? "").split("\n");
  const count = (sign: string, header: string) => lines.filter((l) => l.startsWith(sign) && !l.startsWith(header)).length;
  const action = change?.action ?? "modify";
  return {
    path: d.path,
    status: action === "add" ? "added" : action === "delete" ? "deleted" : "modified",
    old_path: action === "add" ? null : d.path,
    binary: d.patch === undefined,
    old_size: null,
    new_size: change?.size ?? null,
    old_mode: null,
    new_mode: null,
    insertions: count("+", "+++"),
    deletions: count("-", "---"),
    patch: d.patch,
  };
}

/** Why a missing check holds the submit back, in words. */
export function checkReason(m: MissingCheck): string {
  if (m.reason === "absent") return `not reported yet (expected from ${m.reporter})`;
  if (m.reason === "wrong_reporter") return `${m.state ?? "reported"}, but by ${m.reported_by ?? "someone else"}, not ${m.reporter}`;
  return m.state ?? "not passed";
}

function missingChecks(e: OpError | null): MissingCheck[] {
  if (!e || e.code !== "checks_required" || typeof e.details !== "object" || e.details === null) return [];
  const m = (e.details as { missing?: unknown }).missing;
  return Array.isArray(m) ? (m as MissingCheck[]) : [];
}

/** The depot's shelves: mine or everyone's, open, submitted or deleted, and one shelf's details. */
export function ShelvesView({ info, onUnshelved }: Props) {
  const [whose, setWhose] = useState<Whose>("all");
  const [state, setState] = useState<State>("open");
  const [shelves, setShelves] = useState<ShelfInfo[] | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [detail, setDetail] = useState<ShelfInfo | null>(null);
  const [me, setMe] = useState<string | null>(null);
  const [error, setError] = useState<OpError | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const r = await (await backend()).shelves(info.root, whose === "mine", state);
      setShelves(r.shelves);
      setUnavailable(false);
      setSelected((s) => (s !== null && r.shelves.some((x) => x.id === s) ? s : (r.shelves[0]?.id ?? null)));
    } catch (e) {
      const op = asOpError(e);
      if (op.code === "module_disabled") setUnavailable(true);
      else setError(op);
    }
  }, [info.root, whose, state]);

  const loadDetail = useCallback(
    async (id: number | null) => {
      if (id === null) {
        setDetail(null);
        return;
      }
      try {
        const d = await (await backend()).shelf(info.root, id);
        setDetail(d);
      } catch (e) {
        setError(asOpError(e));
      }
    },
    [info.root],
  );

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    setDetail((d) => (d?.id === selected ? d : null));
    void loadDetail(selected);
  }, [selected, loadDetail]);

  useEffect(() => {
    let live = true;
    void (async () => {
      const who = await (await backend()).signedIn(info.server).catch(() => null);
      if (live) setMe(who?.name ?? null);
    })();
    return () => {
      live = false;
    };
  }, [info.server]);

  // Checks belong to one version of a shelf.
  const targets = (shelves ?? []).map((s) => `${s.id}@${s.version}`);
  if (detail && !targets.includes(`${detail.id}@${detail.version}`)) targets.push(`${detail.id}@${detail.version}`);
  const checks = useChecks(info.root, targets);

  const reload = async () => {
    await load();
    await loadDetail(selected);
  };

  return (
    <section className="shelves" aria-label="Shelves">
      <header className="changes-head">
        <h1>Shelves</h1>
        <div className="segmented" role="group" aria-label="Whose shelves">
          <button aria-pressed={whose === "all"} onClick={() => setWhose("all")}>
            Everyone's
          </button>
          <button aria-pressed={whose === "mine"} onClick={() => setWhose("mine")}>
            Mine
          </button>
        </div>
        <select aria-label="Shelf state" value={state} onChange={(e) => setState(e.target.value as State)}>
          {(Object.keys(STATE_TEXT) as State[]).map((s) => (
            <option key={s} value={s}>
              {STATE_TEXT[s]}
            </option>
          ))}
        </select>
        <button className="refresh" onClick={() => void reload()}>
          Refresh
        </button>
      </header>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {notice && (
        <p className="shelf-notice done" role="status">
          {notice}
        </p>
      )}
      {unavailable ? (
        <p className="empty">This server does not have the shelves module, so changes cannot be shelved here.</p>
      ) : (
        <div className="shelves-body">
          <ul className="shelf-list" role="listbox" aria-label="Shelf list">
            {shelves && shelves.length === 0 && (
              <li className="empty">
                {state === "open" ? "No open shelves." : `No ${STATE_TEXT[state].toLowerCase()} shelves.`} Shelve changes from
                the Changes view.
              </li>
            )}
            {(shelves ?? []).map((s) => (
              <li
                key={s.id}
                role="option"
                aria-selected={selected === s.id}
                aria-label={`Shelf ${s.id}: ${firstLine(s.description)}`}
                className={`shelf-item ${selected === s.id ? "selected" : ""}`}
                onClick={() => setSelected(s.id)}
              >
                <div className="shelf-item-top">
                  <span className="mono muted">#{s.id}</span>
                  <span className="shelf-item-title">{firstLine(s.description)}</span>
                </div>
                <div className="shelf-item-meta muted small">
                  {s.owner.name === me ? <strong>you</strong> : ownerText(s)}
                  {s.owner.kind === "agent" && <span className="tag agent">agent</span>}
                  <span>v{s.version}</span>
                  <span>{formatDate(s.updated_at)}</span>
                  <CheckBadge checks={checks.get(`${s.id}@${s.version}`)} />
                </div>
              </li>
            ))}
          </ul>
          <div className="shelf-detail">
            {detail ? (
              <ShelfDetail
                key={`${detail.id}@${detail.version}`}
                info={info}
                shelf={detail}
                mine={detail.owner.name === me}
                checks={checks.get(`${detail.id}@${detail.version}`)}
                onChanged={async (msg) => {
                  setNotice(msg);
                  await reload();
                }}
                onReload={() => void reload()}
                onUnshelved={onUnshelved}
              />
            ) : (
              <p className="muted small">{selected === null ? "Select a shelf to review it." : "Loading…"}</p>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

function ShelfDetail({
  info,
  shelf,
  mine,
  checks,
  onChanged,
  onReload,
  onUnshelved,
}: {
  info: WorkspaceInfo;
  shelf: ShelfInfo;
  mine: boolean;
  checks: CheckInfo[] | undefined;
  onChanged: (message: string) => Promise<void>;
  onReload: () => void;
  onUnshelved?: () => void;
}) {
  const [layout, setLayout] = useState<Layout>(savedLayout);
  const [asking, setAsking] = useState<"submit" | "delete" | null>(null);
  const [unshelved, setUnshelved] = useState<UnshelveResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const [, ...rest] = shelf.description.split("\n");
  const body = rest.join("\n").trim();
  const changes = shelf.changes ?? [];
  const byPath = new Map(changes.map((c) => [c.path, c]));
  const open = shelf.state === "open";

  const unshelve = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await (await backend()).unshelve(info.root, shelf.id);
      setUnshelved(r);
      onUnshelved?.();
    } catch (e) {
      setError(asOpError(e));
    } finally {
      setBusy(false);
    }
  };

  const choose = (l: Layout) => {
    setLayout(l);
    saveLayout(l);
  };

  return (
    <article className="shelf" aria-label={`Shelf ${shelf.id}`}>
      <div className="shelf-head">
        <div>
          <h2 className="details-title">{firstLine(shelf.description)}</h2>
          <p className="muted small">
            Shelf {shelf.id} · version {shelf.version} · {STATE_TEXT[shelf.state].toLowerCase()}
          </p>
        </div>
        <div className="shelf-actions">
          {shelf.state !== "deleted" && (
            <button onClick={() => void unshelve()} disabled={busy}>
              {busy ? "Unshelving…" : "Unshelve"}
            </button>
          )}
          {open && (
            <button className="primary" onClick={() => setAsking("submit")} disabled={busy}>
              Submit…
            </button>
          )}
          {open && mine && (
            <button onClick={() => setAsking("delete")} disabled={busy}>
              Delete…
            </button>
          )}
        </div>
      </div>
      {body && <p className="rev-body">{body}</p>}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <dl className="facts compact shelf-facts">
        <dt>Owner</dt>
        <dd>
          {ownerText(shelf)}
          {shelf.owner.kind === "agent" && <span className="muted"> (agent)</span>}
        </dd>
        <dt>Branch</dt>
        <dd className="mono">
          {shelf.branch} at r{shelf.base_rev}
        </dd>
        <dt>Updated</dt>
        <dd>{formatDate(shelf.updated_at)}</dd>
        {shelf.submitted_rev !== undefined && (
          <>
            <dt>Submitted</dt>
            <dd>as r{shelf.submitted_rev}</dd>
          </>
        )}
        {(shelf.merged_from ?? []).map((m) => (
          <div key={`${m.branch}:${m.from_rev}`} className="fact-pair">
            <dt>Merges</dt>
            <dd className="mono">
              {m.branch} r{m.from_rev}..r{m.to_rev}
            </dd>
          </div>
        ))}
      </dl>
      <ChecksList checks={checks} />
      <div className="shelf-files-head">
        <h3 className="details-sub">
          Changed files <span className="group-count">{changes.length}</span>
        </h3>
        <div className="segmented" role="group" aria-label="Layout">
          <button aria-pressed={layout === "unified"} onClick={() => choose("unified")}>
            Unified
          </button>
          <button aria-pressed={layout === "split"} onClick={() => choose("split")}>
            Side by side
          </button>
        </div>
      </div>
      <ul className="rev-files shelf-files" aria-label="Changed files">
        {changes.map((c) => (
          <li key={c.path}>
            <span className={`st st-${c.action === "add" ? "added" : c.action === "delete" ? "deleted" : "modified"}`}>
              {ACTION[c.action] ?? "?"}
            </span>
            <a className="link file-path mono" href={`#shelf-file-${encodeURIComponent(c.path)}`} title={c.path}>
              {c.path}
            </a>
          </li>
        ))}
      </ul>
      {(shelf.diff ?? []).map((d) => (
        <section key={d.path} className="shelf-file" id={`shelf-file-${encodeURIComponent(d.path)}`} aria-label={`Diff of ${d.path}`}>
          <h4 className="shelf-file-name mono">{d.path}</h4>
          {d.patch !== undefined ? (
            <FileDiffBody diff={asFileDiff(d, byPath.get(d.path))} layout={layout} />
          ) : (
            <Skipped why={d.skipped ?? "binary"} change={byPath.get(d.path)} />
          )}
        </section>
      ))}

      {asking === "submit" && (
        <SubmitDialog
          info={info}
          shelf={shelf}
          onClose={() => setAsking(null)}
          onReload={() => {
            setAsking(null);
            onReload();
          }}
          onDone={(msg) => void onChanged(msg)}
        />
      )}
      {asking === "delete" && (
        <Modal title={`Delete shelf ${shelf.id}?`} onClose={() => setAsking(null)}>
          <p>
            Nobody will be able to unshelve or submit it any more. Changes still in your workspace are not touched.
          </p>
          <div className="form-actions">
            <button onClick={() => setAsking(null)} autoFocus>
              Cancel
            </button>
            <button
              className="danger"
              onClick={async () => {
                setAsking(null);
                try {
                  await (await backend()).shelfDelete(info.root, shelf.id);
                  await onChanged(`Deleted shelf ${shelf.id}.`);
                } catch (e) {
                  setError(asOpError(e));
                }
              }}
            >
              Delete shelf
            </button>
          </div>
        </Modal>
      )}
      {unshelved && (
        <Modal title={`Unshelved shelf ${unshelved.shelf}`} onClose={() => setUnshelved(null)}>
          <p>
            {Object.entries(unshelved.counts)
              .map(([k, n]) => `${n} ${k}`)
              .join(", ") || "Nothing to apply."}
          </p>
          <ul className="outcomes" aria-label="Unshelved files">
            {unshelved.files.map((f) => (
              <li key={f.path}>
                <span className={`tag outcome-${f.outcome}`}>{f.outcome}</span>
                <span className="mono">{f.path}</span>
              </li>
            ))}
          </ul>
          {unshelved.files.some((f) => f.outcome === "conflicted") && (
            <p className="muted small">
              Files in conflict show in Changes. Until the app can resolve them, run{" "}
              <span className="mono">lode resolve &lt;path&gt; --mine|--theirs|--merged</span>.
            </p>
          )}
          <div className="form-actions">
            <button className="primary" onClick={() => setUnshelved(null)} autoFocus>
              Done
            </button>
          </div>
        </Modal>
      )}
    </article>
  );
}

const SKIPPED: Record<string, [string, string]> = {
  binary: ["Binary file", "Binary content is not shown as a diff."],
  too_large: ["Too large to compare as text", "Lode compares files over 16 MB by size only."],
  symlink: ["Symbolic link", "Links are not shown as a diff."],
};

function Skipped({ why, change }: { why: string; change: ChangeView | undefined }) {
  const [title, text] = SKIPPED[why] ?? SKIPPED.binary!;
  return (
    <div className="diff-note" role="note">
      <div className="diff-note-title">{title}</div>
      {change?.size !== undefined && (
        <dl className="facts compact">
          <dt>Size</dt>
          <dd>{humanBytes(change.size)}</dd>
        </dl>
      )}
      <p className="muted small">{text}</p>
    </div>
  );
}

function SubmitDialog({
  info,
  shelf,
  onClose,
  onReload,
  onDone,
}: {
  info: WorkspaceInfo;
  shelf: ShelfInfo;
  onClose: () => void;
  onReload: () => void;
  onDone: (message: string) => void;
}) {
  const [text, setText] = useState(shelf.description);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const missing = missingChecks(error);
  const stale = error?.code === "conflict";

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const msg = text.trim() === shelf.description.trim() ? null : text.trim();
      const r = await (await backend()).shelfSubmit(info.root, shelf.id, shelf.version, msg);
      onClose();
      onDone(`Submitted shelf ${shelf.id} as r${r.rev} on ${r.branch}.`);
    } catch (err) {
      setError(asOpError(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={`Submit shelf ${shelf.id}, version ${shelf.version}`} onClose={onClose}>
      <form className="form" onSubmit={(e) => void submit(e)} aria-label="Submit shelf">
        <p className="muted small lock-what">
          Commits the shelf on {shelf.branch} exactly as shown. You are the submitter; {ownerText(shelf)} stays the author.
        </p>
        <label>
          Message
          <textarea rows={4} value={text} onChange={(e) => setText(e.target.value)} autoFocus />
        </label>
        {missing.length > 0 ? (
          <div className="error-banner" role="alert">
            <div>
              <div className="error-message">Required checks have not passed for version {shelf.version}:</div>
              <ul className="missing-checks" aria-label="Missing checks">
                {missing.map((m) => (
                  <li key={m.name}>
                    <span className="mono">{m.name}</span>: {checkReason(m)}
                  </li>
                ))}
              </ul>
              <div className="error-next">Submit again once they pass.</div>
              <div className="error-code">checks_required</div>
            </div>
          </div>
        ) : (
          error && <ErrorBanner error={error} />
        )}
        <div className="form-actions">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          {stale ? (
            <button className="primary" type="button" onClick={onReload}>
              Review the new version
            </button>
          ) : (
            <button className="primary" type="submit" disabled={busy || !text.trim()}>
              {busy ? "Submitting…" : "Submit"}
            </button>
          )}
        </div>
      </form>
    </Modal>
  );
}
