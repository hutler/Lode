import { useCallback, useEffect, useState } from "react";

import { asOpError, backend, type OpError, type RecentWorkspace, type WorkspaceInfo } from "./api";
import { CheckoutDialog } from "./components/CheckoutDialog";
import { ErrorBanner } from "./components/ErrorBanner";
import { SignInDialog } from "./components/SignInDialog";
import { Welcome } from "./components/Welcome";
import { WorkspaceView } from "./components/WorkspaceView";

type Dialog = { kind: "signin"; server?: string } | { kind: "checkout" } | null;

/** The window: a tab strip (Home plus one tab per open workspace) over the active page. */
export function App() {
  const [recent, setRecent] = useState<RecentWorkspace[]>([]);
  const [tabs, setTabs] = useState<WorkspaceInfo[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog>(null);
  const [error, setError] = useState<OpError | null>(null);
  const [version, setVersion] = useState("");

  const refreshRecent = useCallback(async () => {
    const b = await backend();
    setRecent(await b.recentWorkspaces());
  }, []);

  useEffect(() => {
    void refreshRecent();
    void backend().then((b) => b.appVersion().then(setVersion));
  }, [refreshRecent]);

  const show = useCallback(
    (info: WorkspaceInfo) => {
      setTabs((t) => (t.some((w) => w.root === info.root) ? t.map((w) => (w.root === info.root ? info : w)) : [...t, info]));
      setActive(info.root);
      setError(null);
      void refreshRecent();
    },
    [refreshRecent],
  );

  const open = useCallback(
    async (path: string) => {
      try {
        const b = await backend();
        show(await b.openWorkspace(path));
      } catch (e) {
        setError(asOpError(e));
      }
    },
    [show],
  );

  const openFolder = useCallback(async () => {
    const b = await backend();
    const path = await b.pickFolder("Open a Lode workspace");
    if (path) await open(path);
  }, [open]);

  const forget = useCallback(
    async (path: string) => {
      const b = await backend();
      await b.forgetWorkspace(path);
      await refreshRecent();
    },
    [refreshRecent],
  );

  const close = (root: string) => {
    setTabs((t) => t.filter((w) => w.root !== root));
    if (active === root) setActive(null);
  };

  const current = tabs.find((t) => t.root === active) ?? null;

  return (
    <div className="app">
      <nav className="tabs" aria-label="Open workspaces">
        <button
          className={`tab home ${current ? "" : "active"}`}
          onClick={() => setActive(null)}
          aria-current={current ? undefined : "page"}
        >
          Home
        </button>
        {tabs.map((t) => (
          <div key={t.root} className={`tab ${t.root === active ? "active" : ""}`}>
            <button
              className="tab-label"
              onClick={() => setActive(t.root)}
              aria-current={t.root === active ? "page" : undefined}
              title={t.root}
            >
              {t.depot}
              <span className="tab-branch">{t.branch}</span>
            </button>
            <button className="tab-close" aria-label={`Close ${t.depot}`} onClick={() => close(t.root)}>
              ×
            </button>
          </div>
        ))}
      </nav>

      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      <main className="page">
        {current ? (
          <WorkspaceView key={current.root} info={current} onError={setError} onInfo={show} />
        ) : (
          <Welcome
            recent={recent}
            version={version}
            onOpen={open}
            onOpenFolder={openFolder}
            onForget={forget}
            onCheckout={() => setDialog({ kind: "checkout" })}
            onSignIn={() => setDialog({ kind: "signin" })}
          />
        )}
      </main>

      {dialog?.kind === "signin" && (
        <SignInDialog initialServer={dialog.server} onClose={() => setDialog(null)} />
      )}
      {dialog?.kind === "checkout" && (
        <CheckoutDialog
          onClose={() => setDialog(null)}
          onDone={(info) => {
            setDialog(null);
            show(info);
          }}
          onNeedSignIn={(server) => setDialog({ kind: "signin", server })}
        />
      )}
    </div>
  );
}
