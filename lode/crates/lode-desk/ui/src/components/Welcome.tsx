import type { RecentWorkspace } from "../api";

interface Props {
  recent: RecentWorkspace[];
  version: string;
  onOpen: (path: string) => void;
  onOpenFolder: () => void;
  onForget: (path: string) => void;
  onCheckout: () => void;
  onSignIn: () => void;
}

function hostOf(server: string): string {
  try {
    return new URL(server).host;
  } catch {
    return server;
  }
}

/** The Home tab: recent workspaces and the ways to get a new one. */
export function Welcome({ recent, version, onOpen, onOpenFolder, onForget, onCheckout, onSignIn }: Props) {
  return (
    <div className="welcome">
      <section className="welcome-recent" aria-labelledby="recent-title">
        <h1 id="recent-title">Recent workspaces</h1>
        {recent.length === 0 ? (
          <p className="muted">
            Nothing here yet. Check out a depot or open a folder that already holds a workspace.
          </p>
        ) : (
          <ul className="recent-list">
            {recent.map((r) => (
              <li key={r.path}>
                <button className="recent-item" onClick={() => onOpen(r.path)}>
                  <span className="recent-name">
                    {r.depot}
                    <span className="badge">{r.branch}</span>
                  </span>
                  <span className="recent-path">{r.path}</span>
                  <span className="recent-server">{hostOf(r.server)}</span>
                </button>
                <button className="icon-button" aria-label={`Forget ${r.path}`} title="Remove from this list" onClick={() => onForget(r.path)}>
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>
      <aside className="welcome-actions" aria-label="Start">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true" />
          <div>
            <div className="brand-name">Lode Desk</div>
            {version && <div className="muted small">Version {version}</div>}
          </div>
        </div>
        <button className="primary" onClick={onCheckout}>
          Check out a depot…
        </button>
        <button onClick={onOpenFolder}>Open a workspace folder…</button>
        <button onClick={onSignIn}>Sign in to a server…</button>
      </aside>
    </div>
  );
}
