// A backend with made-up data, for the browser preview, screenshots and tests. It behaves
// like a small server with one user (`alice`, password `correct horse`) and two depots.

import type {
  Backend,
  LogPage,
  CheckoutResult,
  CommitResult,
  OpError,
  ProgressEvent,
  RecentWorkspace,
  StatusFile,
  StatusResult,
  WhoAmI,
  WorkspaceInfo,
} from "./api";
import { demoBlame, demoHead, demoLog, demoRevision } from "./mockHistory";

export interface MockOptions {
  /** Workspaces listed on the welcome page. */
  recent?: RecentWorkspace[];
  /** Servers already signed in to. */
  signedIn?: string[];
  /** Milliseconds each progress step takes (0 in tests). */
  stepMs?: number;
  /** Makes every commit fail with this error. */
  refuseCommit?: OpError;
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

/** The mock backend, plus a way to pretend files changed on disk. */
export type MockBackend = Backend & {
  /** Changes a file's status as an editor would, and reports the change. */
  touch(workspace: string, file: StatusFile): void;
};

export function createMockBackend(opts: MockOptions = {}): MockBackend {
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
      rev: (r.depot === "skyfall" ? demoHead(r.branch) : null) ?? DEPOTS[r.depot]?.rev ?? 1,
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

  const changed = new Set<(workspace: string) => void>();
  const statusOf = new Map<string, StatusResult>();
  const statusFor = (workspace: string): StatusResult => {
    let st = statusOf.get(workspace);
    if (!st) {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      st = {
        branch: info.branch,
        rev: info.rev,
        files: info.depot === "skyfall" ? DEMO_STATUS.files.map((f) => ({ ...f })) : [],
      };
      statusOf.set(workspace, st);
    }
    return st;
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
      const st = statusFor(workspace);
      return { ...st, files: st.files.map((f) => ({ ...f })) };
    },
    add: async (workspace, paths) => {
      const st = statusFor(workspace);
      for (const f of st.files) if (paths.includes(f.path) && f.status === "untracked") f.status = "added";
    },
    revert: async (workspace, paths) => {
      const st = statusFor(workspace);
      st.files = st.files.filter((f) => !paths.includes(f.path) || f.status === "untracked");
    },
    commit: async (workspace, paths, message): Promise<CommitResult> => {
      const st = statusFor(workspace);
      if (opts.refuseCommit) throw opts.refuseCommit;
      if (!message.trim()) fail("aborted", "empty commit message; nothing was committed");
      const picked = st.files.filter((f) => paths.includes(f.path) && f.status !== "untracked");
      if (picked.length === 0) fail("nothing_to_commit", "nothing to commit in the given paths");
      const needLock = picked.filter((f) => f.needs_lock).map((f) => f.path);
      if (needLock.length > 0) {
        const e: OpError = {
          code: "lock_required",
          message: `these files need a lock: ${needLock.join(", ")}; run \`lode lock <path>\` first`,
          details: { paths: needLock },
        };
        throw e;
      }
      for (let i = 1; i <= picked.length; i++) {
        await pause();
        emit({ label: "Uploading", done: i, total: picked.length, bytes: i * 1_200_000, finished: false });
      }
      emit({ label: "Uploading", done: picked.length, total: picked.length, bytes: picked.length * 1_200_000, finished: true });
      st.rev += 1;
      st.files = st.files.filter((f) => !picked.includes(f));
      const info = workspaces.get(workspace);
      if (info) info.rev = st.rev;
      return { rev: st.rev, branch: st.branch, files: picked.length };
    },
    log: async (workspace, path, before, max): Promise<LogPage> => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      await pause();
      return { depot: info.depot, branch: info.branch, ...demoLog(info.branch, path, before, max) };
    },
    revision: async (workspace, rev) => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      const r = demoRevision(info.branch, rev);
      if (!r) fail("not_found", `revision ${rev} not found on ${info.branch}`);
      return r;
    },
    blame: async (workspace, path, rev) => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      return demoBlame(info.branch, path, rev);
    },
    watch: async () => {},
    unwatch: async () => {},
    onChanged: (listener) => {
      changed.add(listener);
      return () => changed.delete(listener);
    },
    touch: (workspace, file) => {
      const st = statusFor(workspace);
      st.files = [...st.files.filter((f) => f.path !== file.path), file];
      changed.forEach((l) => l(workspace));
    },
    pickFolder: async () => "D:\\Work\\Skyfall-new",
    onProgress: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
