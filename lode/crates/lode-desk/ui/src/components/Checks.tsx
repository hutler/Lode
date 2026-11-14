import { backend, type CheckInfo } from "../api";
import { summarize } from "../checks";
import { formatDate } from "./HistoryView";

const MARK: Record<CheckInfo["state"], string> = {
  success: "✓",
  pending: "●",
  cancelled: "⊘",
  error: "!",
  failure: "✕",
};

const WORD: Record<CheckInfo["state"], string> = {
  success: "passed",
  pending: "pending",
  cancelled: "cancelled",
  error: "error",
  failure: "failed",
};

/** One badge for the checks of a revision or shelf: the worst state, with a count. */
export function CheckBadge({ checks }: { checks: CheckInfo[] | undefined }) {
  const s = summarize(checks ?? []);
  if (!s) return null;
  const text = s.state === "success" ? `${s.total}` : `${s.count}/${s.total}`;
  const label =
    s.state === "success"
      ? `${s.total} check${s.total === 1 ? "" : "s"} passed`
      : `${s.count} of ${s.total} check${s.total === 1 ? "" : "s"} ${WORD[s.state]}`;
  const title = (checks ?? []).map((c) => `${c.name}: ${WORD[c.state]}${c.summary ? ` (${c.summary})` : ""}`).join("\n");
  return (
    <span className={`check-badge check-${s.state}`} role="img" aria-label={label} title={title}>
      <span aria-hidden="true">{MARK[s.state]}</span> {text}
    </span>
  );
}

/** Each check of a revision or shelf version, with its link opened in the system browser. */
export function ChecksList({ checks }: { checks: CheckInfo[] | undefined }) {
  if (!checks || checks.length === 0) return null;
  const sorted = [...checks].sort((a, b) => a.name.localeCompare(b.name));
  return (
    <>
      <h3 className="details-sub">
        Checks <span className="group-count">{checks.length}</span>
      </h3>
      <ul className="check-list" aria-label="Checks">
        {sorted.map((c) => (
          <li key={c.name}>
            <span className={`check-state check-${c.state}`}>
              <span aria-hidden="true">{MARK[c.state]}</span> {WORD[c.state]}
            </span>
            <div className="check-main">
              {c.url ? (
                <button
                  className="link mono check-name"
                  title={c.url}
                  onClick={() => void backend().then((b) => b.openLink(c.url!))}
                >
                  {c.name}
                </button>
              ) : (
                <span className="mono check-name">{c.name}</span>
              )}
              {c.summary && <div className="check-summary">{c.summary}</div>}
              <div className="muted small">
                {c.reporter.name}
                {c.reporter.kind === "agent" && c.reporter.owner ? ` for ${c.reporter.owner}` : ""}, {formatDate(c.updated_at)}
              </div>
            </div>
          </li>
        ))}
      </ul>
    </>
  );
}
