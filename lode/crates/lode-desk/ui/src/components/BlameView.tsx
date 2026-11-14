import { useEffect, useState } from "react";

import { asOpError, backend, type BlameResult, type OpError, type WorkspaceInfo } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { formatDate } from "./HistoryView";

interface Props {
  info: WorkspaceInfo;
  path: string;
  /** The revision to blame at; the workspace's when null. */
  rev: number | null;
  onRevision: (rev: number) => void;
  onHistory: (path: string) => void;
}

/** Who last changed each line of one file. The gutter names the revision on the first line of
 * each run of lines from the same revision. */
export function BlameView({ info, path, rev, onRevision, onHistory }: Props) {
  const [blame, setBlame] = useState<BlameResult | null>(null);
  const [error, setError] = useState<OpError | null>(null);

  useEffect(() => {
    let live = true;
    setBlame(null);
    void (async () => {
      try {
        const b = await (await backend()).blame(info.root, path, rev);
        if (live) {
          setBlame(b);
          setError(null);
        }
      } catch (e) {
        if (live) setError(asOpError(e));
      }
    })();
    return () => {
      live = false;
    };
  }, [info.root, path, rev]);

  const byRev = new Map(blame?.revisions.map((r) => [r.rev, r]) ?? []);
  // Alternate the shade of runs so neighbouring runs stand apart.
  let shade = false;

  return (
    <section className="blame" aria-label="Blame">
      <header className="changes-head">
        <h1>
          Blame <span className="mono">{path.slice(path.lastIndexOf("/") + 1)}</span>
        </h1>
        <span className="muted small">
          {path} {blame ? `at r${blame.rev}` : ""}
        </span>
        <button className="refresh" onClick={() => onHistory(path)}>
          File history
        </button>
      </header>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {blame && (
        <div className="blame-body">
          <table className="blame-table">
            <tbody>
              {blame.lines.map((l, i) => {
                const start = i === 0 || blame.lines[i - 1]?.rev !== l.rev;
                if (start) shade = !shade;
                const r = byRev.get(l.rev);
                return (
                  <tr key={i} className={`${shade ? "shade" : ""} ${start ? "run-start" : ""}`}>
                    <td className="blame-who">
                      {start && (
                        <button
                          className="link blame-rev"
                          onClick={() => onRevision(l.rev)}
                          title={r ? `${r.summary}\n${r.author.name}, ${formatDate(r.created_at)}` : undefined}
                        >
                          <span className="mono">r{l.rev}</span>
                          <span className="blame-author">{r?.author.name}</span>
                          <span className="blame-summary">{r?.summary}</span>
                        </button>
                      )}
                    </td>
                    <td className="blame-num mono">{i + 1}</td>
                    <td className="blame-text mono">{l.text || " "}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          {blame.truncated && <p className="muted small">The file is longer than blame shows.</p>}
        </div>
      )}
    </section>
  );
}
