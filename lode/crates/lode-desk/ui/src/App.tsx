import { useCallback, useEffect, useRef, useState } from "react";

import { asOpError, backend, type OpError, type RecentWorkspace, type WorkspaceInfo } from "./api";
import { CheckoutDialog } from "./components/CheckoutDialog";
import { ErrorBanner } from "./components/ErrorBanner";
import { type Candidate, ChooseWorkspaceDialog, NoWorkspaceDialog, RefusedLinkDialog } from "./components/LinkDialogs";
import { type Link, sameServer, tryParse } from "./links";
import { SignInDialog } from "./components/SignInDialog";
import { UpdateBanner, useUpdates } from "./components/Updates";
import { Welcome } from "./components/Welcome";
import { WorkspaceView } from "./components/WorkspaceView";

type Dialog =
  | { kind: "signin"; server?: string }
  | { kind: "checkout"; target?: string; branch?: string; then?: Link }
  | { kind: "refused"; text: string; why: string }
  | { kind: "choose"; link: Link; candidates: Candidate[] }
  | { kind: "none"; link: Link }
  | null;

/** Where a workspace should go once it shows, because a link asked for it. */
export interface Goto {
  root: string;
  link: Link;
}

/** The window: a tab strip (Home plus one tab per open workspace) over the active page. */
export function App() {
  const [recent, setRecent] = useState<RecentWorkspace[]>([]);
  const [tabs, setTabs] = useState<WorkspaceInfo[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog>(null);
  const [error, setError] = useState<OpError | null>(null);
  const [version, setVersion] = useState("");
  const [goto, setGoto] = useState<Goto | null>(null);
  const [linkNote, setLinkNote] = useState<{ root: string; text: string } | null>(null);
  const updates = useUpdates();
  const tabsRef = useRef(tabs);
  tabsRef.current = tabs;

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
      setTabs((t) =>
        t.some((w) => w.root === info.root) ? t.map((w) => (w.root === info.root ? info : w)) : [...t, info],
      );
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

  /** Shows what `link` points at in the workspace at `path`, opening it first if needed. */
  const follow = useCallback(
    async (link: Link, path: string) => {
      setDialog(null);
      let info = tabsRef.current.find((t) => t.root === path);
      if (info) setActive(info.root);
      else {
        try {
          info = await (await backend()).openWorkspace(path);
        } catch (e) {
          setError(asOpError(e));
          return;
        }
        show(info);
      }
      setGoto({ root: info.root, link });
      setLinkNote(
        link.kind === "file" && link.rev === null && link.branch !== null && link.branch !== info.branch
          ? {
              root: info.root,
              text: `The link is for ${link.path} on ${link.branch}, and this workspace is on ${info.branch}, so its history on ${info.branch} is shown.`,
            }
          : null,
      );
    },
    [show],
  );

  /** Acts on a `lode://` link handed to the app: refused, or shown in a workspace of its depot. */
  const openLinkText = useCallback(
    async (text: string) => {
      const parsed = tryParse(text);
      if ("error" in parsed) {
        setDialog({ kind: "refused", text, why: parsed.error });
        return;
      }
      const link = parsed.link;
      const b = await backend();
      const recents = await b.recentWorkspaces().catch(() => [] as RecentWorkspace[]);
      setRecent(recents);
      const seen = new Set<string>();
      const all: Candidate[] = [];
      const add = (path: string, server: string, depot: string, branch: string, open: boolean) => {
        if (seen.has(path) || depot !== link.depot || !sameServer(server, link.server)) return;
        seen.add(path);
        all.push({ path, branch, open });
      };
      for (const t of tabsRef.current) add(t.root, t.server, t.depot, t.branch, true);
      for (const r of recents) add(r.path, r.server, r.depot, r.branch, false);
      // A branch link is best shown in a workspace on that branch.
      const wanted = link.kind === "file" ? link.branch : null;
      all.sort((x, y) => Number(y.branch === wanted) - Number(x.branch === wanted));
      const [only] = all;
      if (all.length === 0) setDialog({ kind: "none", link });
      else if (all.length === 1 && only) await follow(link, only.path);
      else setDialog({ kind: "choose", link, candidates: all });
    },
    [follow],
  );

  // Links the app was started with, and links handed to it while it runs.
  useEffect(() => {
    let stop: (() => void) | undefined;
    let live = true;
    void backend().then((b) => {
      const take = async () => {
        const text = await b.pendingLink();
        if (text && live) await openLinkText(text);
      };
      if (!live) return;
      stop = b.onOpenLink(() => void take());
      void take();
    });
    return () => {
      live = false;
      stop?.();
    };
  }, [openLinkText]);

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

      {updates.offer && <UpdateBanner offer={updates.offer} onLater={updates.later} />}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {linkNote && linkNote.root === current?.root && (
        <p className="notice link-note" role="status">
          {linkNote.text}
          <button className="link" onClick={() => setLinkNote(null)}>
            Dismiss
          </button>
        </p>
      )}

      <main className="page">
        {current ? (
          <WorkspaceView
            key={current.root}
            info={current}
            onError={setError}
            onInfo={show}
            goto={goto?.root === current.root ? goto.link : null}
            onGone={() => setGoto(null)}
          />
        ) : (
          <Welcome
            recent={recent}
            version={version}
            onOpen={open}
            onOpenFolder={openFolder}
            onForget={forget}
            onCheckout={() => setDialog({ kind: "checkout" })}
            onSignIn={() => setDialog({ kind: "signin" })}
            onUpdateFound={updates.found}
          />
        )}
      </main>

      {dialog?.kind === "signin" && <SignInDialog initialServer={dialog.server} onClose={() => setDialog(null)} />}
      {dialog?.kind === "checkout" && (
        <CheckoutDialog
          initialTarget={dialog.target}
          initialBranch={dialog.branch}
          onClose={() => setDialog(null)}
          onDone={(info) => {
            const then = dialog.then;
            setDialog(null);
            show(info);
            if (then) void follow(then, info.root);
          }}
          onNeedSignIn={(server) => setDialog({ kind: "signin", server })}
        />
      )}
      {dialog?.kind === "refused" && (
        <RefusedLinkDialog text={dialog.text} why={dialog.why} onClose={() => setDialog(null)} />
      )}
      {dialog?.kind === "choose" && (
        <ChooseWorkspaceDialog
          link={dialog.link}
          candidates={dialog.candidates}
          onPick={(path) => void follow(dialog.link, path)}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "none" && (
        <NoWorkspaceDialog
          link={dialog.link}
          onClose={() => setDialog(null)}
          onCheckout={() =>
            setDialog({
              kind: "checkout",
              target: `${dialog.link.server}/${dialog.link.depot}`,
              branch: dialog.link.kind === "file" ? (dialog.link.branch ?? undefined) : undefined,
              then: dialog.link,
            })
          }
        />
      )}
    </div>
  );
}
