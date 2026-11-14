// A backend with made-up data, for the browser preview, screenshots and tests. It behaves
// like a small server with one user (`alice`, password `correct horse`) and two depots.

import type {
  Backend,
  CheckoutResult,
  OpError,
  ProgressEvent,
  RecentWorkspace,
  StatusResult,
  WhoAmI,
  WorkspaceInfo,
} from "./api";

export interface MockOptions {
  /** Workspaces listed on the welcome page. */
  recent?: RecentWorkspace[];
  /** Servers already signed in to. */
  signedIn?: string[];
  /** Milliseconds each progress step takes (0 in tests). */
  stepMs?: number;
}

const DEMO_SERVER = "https://lode.example.com";

const DEMO_RECENT: RecentWorkspace[] = [
  { path: "D:\\Work\\Skyfall", depot: "skyfall", server: DEMO_SERVER, branch: "main" },
  { path: "D:\\Work\\Skyfall-release", depot: "skyfall", server: DEMO_SERVER, branch: "release-1.2" },
  { path: "D:\\Tools\\pipeline", depot: "pipeline", server: DEMO_SERVER, branch: "main" },
];

const DEPOTS: Record<string, { rev: number; files: number; branches: string[] }> = {
  skyfall: { rev: 1482, files: 18234, branches: ["main", "release-1.2"] },
  pipeline: { rev: 311, files: 412, branches: ["main"] },
};

const DEMO_STATUS: StatusResult = {
  branch: "main",
  rev: 1482,
  files: [
    {
      path: "Content/Maps/Harbor.umap",
      status: "modified",
      copy_from: null,
      moved: false,
      changelist: null,
      locked: true,
      needs_lock: false,
      lock_required: true,
    },
    {
      path: "Source/Skyfall/Boat.cpp",
      status: "modified",
      copy_from: null,
      moved: false,
      changelist: null,
      locked: false,
      needs_lock: false,
      lock_required: false,
    },
    {
      path: "Content/Props/Crate_02.uasset",
      status: "modified",
      copy_from: null,
      moved: false,
      changelist: null,
      locked: false,
      needs_lock: true,
      lock_required: true,
    },
    {
      path: "Source/Skyfall/Sail.cpp",
      status: "added",
      copy_from: null,
      moved: false,
      changelist: "sails",
      locked: false,
      needs_lock: false,
      lock_required: false,
    },
    {
      path: "notes.txt",
      status: "untracked",
      copy_from: null,
      moved: false,
      changelist: null,
      locked: false,
      needs_lock: false,
      lock_required: false,
    },
  ],
};

function fail(code: string, message: string): never {
  const e: OpError = { code, message };
  throw e;
}

function serverOf(url: string): string {
  return url.replace(/\/+$/, "");
}

function parseTarget(target: string): { server: string; depot: string } {
  const t = target.trim().replace(/\/+$/, "");
  const at = t.lastIndexOf("/");
  if (!t.includes("://") || at < 0 || t.slice(0, at).endsWith("/")) {
    fail("usage", `\`${target}\` has no depot: use <server-url>/<depot>`);
  }
  return { server: t.slice(0, at), depot: t.slice(at + 1) };
}

export function createMockBackend(opts: MockOptions = {}): Backend {
  const recent = [...(opts.recent ?? DEMO_RECENT)];
  const signed = new Set((opts.signedIn ?? [DEMO_SERVER]).map(serverOf));
  const listeners = new Set<(e: ProgressEvent) => void>();
  const stepMs = opts.stepMs ?? 120;
  const workspaces = new Map<string, WorkspaceInfo>();
  for (const r of recent) {
    workspaces.set(r.path, {
      root: r.path,
      server: r.server,
      depot: r.depot,
      branch: r.branch,
      rev: DEPOTS[r.depot]?.rev ?? 1,
    });
  }

  const who = (server: string): WhoAmI => ({
    server,
    name: "alice",
    kind: "user",
    scopes: ["read", "write", "lock", "shelve"],
  });

  const remember = (info: WorkspaceInfo) => {
    const i = recent.findIndex((r) => r.path === info.root);
    if (i >= 0) recent.splice(i, 1);
    recent.unshift({ path: info.root, depot: info.depot, server: info.server, branch: info.branch });
  };

  const emit = (e: ProgressEvent) => listeners.forEach((l) => l(e));
  const pause = () => new Promise((r) => setTimeout(r, stepMs));

  return {
    appVersion: async () => "0.1.0",
    recentWorkspaces: async () => [...recent],
    forgetWorkspace: async (path) => {
      const i = recent.findIndex((r) => r.path === path);
      if (i >= 0) recent.splice(i, 1);
    },
    signedIn: async (server) => (signed.has(serverOf(server)) ? who(serverOf(server)) : null),
    signIn: async (server, name, password) => {
      const s = serverOf(server);
      if (!/^https?:\/\//.test(s)) fail("usage", `${server} is not a server URL: use https://host`);
      if (name !== "alice" || password !== "correct horse") {
        fail("unauthenticated", "wrong name or password");
      }
      signed.add(s);
      return who(s);
    },
    openWorkspace: async (path) => {
      const info = workspaces.get(path);
      if (!info) {
        fail("not_a_workspace", `${path} is not in a lode workspace (no .lode directory found)`);
      }
      remember(info);
      return info;
    },
    checkout: async (target, dir, branch): Promise<CheckoutResult> => {
      const { server, depot } = parseTarget(target);
      if (!signed.has(server)) {
        fail("unauthenticated", `not signed in to ${server}/: run \`lode login ${server}/\` or set LODE_TOKEN_FILE`);
      }
      const d = DEPOTS[depot];
      if (!d) fail("not_found", `depot ${depot} not found`);
      if (!d.branches.includes(branch)) fail("not_found", `branch ${branch} not found`);
      if (workspaces.has(dir)) fail("already_a_workspace", `${dir} is already a workspace`);
      const steps = 8;
      for (let i = 1; i <= steps; i++) {
        await pause();
        emit({
          label: "Fetching",
          done: Math.round((d.files * i) / steps),
          total: d.files,
          bytes: i * 41_000_000,
          finished: false,
        });
      }
      emit({ label: "Fetching", done: d.files, total: d.files, bytes: steps * 41_000_000, finished: true });
      const info: WorkspaceInfo = { root: dir, server, depot, branch, rev: d.rev };
      workspaces.set(dir, info);
      remember(info);
      return { workspace: dir, depot, branch, rev: d.rev, files: d.files };
    },
    status: async (workspace) => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      return { ...DEMO_STATUS, branch: info.branch, rev: info.rev };
    },
    pickFolder: async () => "D:\\Work\\Skyfall-new",
    onProgress: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
