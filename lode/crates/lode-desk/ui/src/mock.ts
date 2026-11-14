// A backend with made-up data, for the browser preview, screenshots and tests. It behaves
// like a small server with one user (`alice`, password `correct horse`) and two depots.

import type {
  Backend,
  ChangelistInfo,
  LockInfo,
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
import { demoRevisionDiff, demoWorkingDiff } from "./mockDiff";
import { demoBlame, demoHead, demoLog, demoRevision } from "./mockHistory";
import { imageType } from "./api";
import { demoChecks } from "./mockChecks";
import { demoImage } from "./mockImages";
import { demoShelves, type MockShelf } from "./mockShelves";

export interface MockOptions {
  /** Workspaces listed on the welcome page. */
  recent?: RecentWorkspace[];
  /** Servers already signed in to. */
  signedIn?: string[];
  /** Milliseconds each progress step takes (0 in tests). */
  stepMs?: number;
  /** Makes every commit fail with this error. */
  refuseCommit?: OpError;
  /** The user is a depot admin, so they may break other people's locks. */
  admin?: boolean;
  /** The server has no locks module. */
  noLocks?: boolean;
  /** The server has no shelves module. */
  noShelves?: boolean;
  /** The server has no checks module. */
  noChecks?: boolean;
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

const LOCK_TIME = "2026-11-14T07:12:00Z";

/** Locks other people hold on the demo depot's `main`. */
function demoLocks(): LockInfo[] {
  return [
    {
      path: "Content/Props/Crate_02.uasset",
      branch: "main",
      owner: { name: "carol", kind: "user" },
      workspace: "carol-studio",
      comment: "Repainting for the market",
      created_at: "2026-11-13T16:40:00Z",
      mine: false,
    },
    {
      path: "Content/Maps/Docks.umap",
      branch: "main",
      owner: { name: "bob", kind: "user" },
      workspace: "bob-laptop",
      comment: "Lighting pass",
      created_at: "2026-11-14T06:05:00Z",
      mine: false,
    },
    {
      path: "Source/Skyfall/Weather.cpp",
      branch: "main",
      owner: { name: "refactor-bot", kind: "agent" },
      workspace: "ci-agent-3",
      created_at: "2026-11-14T08:30:00Z",
      mine: false,
    },
  ];
}

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
  /** Updates a shelf as its owner would from elsewhere, raising its version. */
  bumpShelf(workspace: string, id: number): void;
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
  const commitFiles = async (workspace: string, picked: StatusFile[], message: string): Promise<CommitResult> => {
    const st = statusFor(workspace);
    if (opts.refuseCommit) throw opts.refuseCommit;
    if (!message.trim()) fail("aborted", "empty commit message; nothing was committed");
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
  };
  // Changelists per workspace (names and descriptions; files come from the status).
  const lists = new Map<string, ChangelistInfo[]>();
  const listsFor = (workspace: string): ChangelistInfo[] => {
    let l = lists.get(workspace);
    if (!l) {
      const named = [...new Set(statusFor(workspace).files.map((f) => f.changelist).filter((c): c is string => !!c))].sort();
      l = named.map((name) => ({
        name,
        description: name === "sails" ? "Sail rigging: flap harder when the wind turns" : "",
        files: [],
      }));
      lists.set(workspace, l);
    }
    return l;
  };
  // Locks per depot and branch; `workspace` names the holder's workspace root.
  const lockTables = new Map<string, LockInfo[]>();
  const locksOf = (workspace: string): LockInfo[] => {
    const info = workspaces.get(workspace);
    if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
    if (opts.noLocks) fail("module_disabled", "the locks module is not enabled on this server");
    const key = `${info.depot}/${info.branch}`;
    let table = lockTables.get(key);
    if (!table) {
      table = info.depot === "skyfall" && info.branch === "main" ? demoLocks() : [];
      // Locks the demo status shows as held here.
      for (const f of statusFor(workspace).files.filter((x) => x.locked)) {
        table.push({ path: f.path, branch: info.branch, owner: { name: "alice", kind: "user" }, workspace, created_at: LOCK_TIME, mine: true });
      }
      lockTables.set(key, table);
    }
    return table;
  };
  const pause = () => new Promise((r) => setTimeout(r, stepMs));
  // Shelves per depot.
  const shelfTables = new Map<string, MockShelf[]>();
  const shelvesOf = (workspace: string): MockShelf[] => {
    const info = workspaces.get(workspace);
    if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
    if (opts.noShelves) fail("module_disabled", "this server does not have the shelves module enabled");
    let table = shelfTables.get(info.depot);
    if (!table) {
      table = info.depot === "skyfall" ? demoShelves() : [];
      shelfTables.set(info.depot, table);
    }
    return table;
  };
  const shelfOf = (workspace: string, id: number): MockShelf => {
    const s = shelvesOf(workspace).find((x) => x.id === id);
    if (!s) fail("not_found", `no shelf ${id}`);
    return s;
  };
  const now = () => new Date(Date.UTC(2026, 10, 14, 9, 30)).toISOString();

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
      return commitFiles(
        workspace,
        st.files.filter((f) => paths.includes(f.path) && f.status !== "untracked"),
        message,
      );
    },
    commitChangelist: async (workspace, name, message) => {
      const st = statusFor(workspace);
      const list = listsFor(workspace).find((l) => l.name === name);
      if (name !== "default" && !list) fail("not_found", `no changelist \`${name}\``);
      const files = st.files.filter((f) => (f.changelist ?? "default") === name && f.status !== "untracked");
      const r = await commitFiles(workspace, files, message ?? list?.description ?? "");
      if (list && !st.files.some((f) => f.changelist === name)) {
        lists.set(workspace, listsFor(workspace).filter((l) => l !== list));
      }
      return r;
    },
    changelists: async (workspace) => {
      const st = statusFor(workspace);
      const filesOf = (name: string) =>
        st.files.filter((f) => f.status !== "untracked" && (f.changelist ?? "default") === name).map((f) => f.path);
      return {
        changelists: [
          { name: "default", description: "", files: filesOf("default") },
          ...listsFor(workspace).map((l) => ({ ...l, files: filesOf(l.name) })),
        ],
      };
    },
    changelistNew: async (workspace, name, description) => {
      if (!/^[A-Za-z0-9._-]{1,64}$/.test(name)) {
        fail("usage", `invalid changelist name \`${name}\`: use 1 to 64 letters, digits, \`.\`, \`_\` or \`-\``);
      }
      if (name === "default") fail("usage", "`default` always exists");
      if (listsFor(workspace).some((l) => l.name === name)) fail("conflict", `changelist \`${name}\` already exists`);
      listsFor(workspace).push({ name, description: description.trim(), files: [] });
    },
    changelistMove: async (workspace, name, paths) => {
      const st = statusFor(workspace);
      if (name !== "default" && !listsFor(workspace).some((l) => l.name === name)) {
        fail("not_found", `no changelist \`${name}\``);
      }
      const untracked = st.files.filter((f) => paths.includes(f.path) && f.status === "untracked");
      if (untracked.length) fail("usage", `${untracked[0]!.path} is not tracked: add it first`);
      for (const f of st.files) if (paths.includes(f.path)) f.changelist = name === "default" ? null : name;
    },
    changelistDelete: async (workspace, name) => {
      if (!listsFor(workspace).some((l) => l.name === name)) fail("not_found", `no changelist \`${name}\``);
      for (const f of statusFor(workspace).files) if (f.changelist === name) f.changelist = null;
      lists.set(workspace, listsFor(workspace).filter((l) => l.name !== name));
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
    diff: async (workspace, path) => {
      const f = statusFor(workspace).files.find((x) => x.path === path);
      return f ? demoWorkingDiff(path, f.status) : null;
    },
    diffRevision: async (workspace, rev, path) => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      const change = demoRevision(info.branch, rev)?.changes?.find((c) => c.path === path);
      if (!change) fail("not_found", `r${rev} did not change ${path}`);
      return demoRevisionDiff(rev, path, change.action);
    },
    lock: async (workspace, paths, comment) => {
      const table = locksOf(workspace);
      const st = statusFor(workspace);
      const held = table.filter((l) => paths.includes(l.path) && l.workspace !== workspace);
      if (held.length) {
        const e: OpError = {
          code: "locked",
          message: `locked by someone else: ${held.map((l) => `${l.path} (${l.owner.name})`).join(", ")}`,
          details: { paths: held.map((l) => l.path) },
        };
        throw e;
      }
      for (const p of paths) {
        if (table.some((l) => l.path === p)) continue;
        table.push({
          path: p,
          branch: st.branch,
          owner: { name: "alice", kind: "user" },
          workspace,
          ...(comment?.trim() ? { comment: comment.trim() } : {}),
          created_at: LOCK_TIME,
          mine: true,
        });
        const f = st.files.find((x) => x.path === p);
        if (f) {
          f.locked = true;
          f.needs_lock = false;
        }
      }
      return { locked: paths };
    },
    unlock: async (workspace, paths, force) => {
      const table = locksOf(workspace);
      const others = table.filter((l) => paths.includes(l.path) && l.workspace !== workspace);
      if (others.length && !force) {
        fail("not_locked", `you hold no lock on ${others.map((l) => l.path).join(", ")}`);
      }
      if (others.length && !opts.admin) fail("permission_denied", "breaking locks needs admin on the depot");
      const st = statusFor(workspace);
      for (const p of paths) {
        const i = table.findIndex((l) => l.path === p);
        if (i >= 0) table.splice(i, 1);
        const f = st.files.find((x) => x.path === p);
        if (f) {
          f.locked = false;
          f.needs_lock = f.lock_required;
        }
      }
      return { released: paths };
    },
    locks: async (workspace, mine) => {
      const table = locksOf(workspace).map((l) => ({ ...l, mine: l.workspace === workspace }));
      return { locks: mine ? table.filter((l) => l.mine) : table };
    },
    shelve: async (workspace, req) => {
      const st = statusFor(workspace);
      const table = shelvesOf(workspace);
      const list = req.changelist ? (req.changelist === "default" ? null : listsFor(workspace).find((l) => l.name === req.changelist)) : null;
      if (req.changelist && req.changelist !== "default" && !list) fail("not_found", `no changelist \`${req.changelist}\``);
      const existing = req.update === null ? null : shelfOf(workspace, req.update);
      if (existing && (existing.owner.name !== "alice" || existing.state !== "open")) {
        fail("permission_denied", `shelf ${existing.id} is not an open shelf of yours`);
      }
      const description = (req.message ?? list?.description ?? existing?.description ?? "").trim();
      if (!description) fail("usage", "a shelf needs a description: pass -m");
      const files = st.files.filter(
        (f) =>
          f.status !== "untracked" &&
          f.status !== "unchanged" &&
          (req.changelist ? (f.changelist ?? "default") === req.changelist : req.paths.length === 0 || req.paths.includes(f.path)),
      );
      if (files.length === 0) {
        fail("nothing_to_shelve", req.changelist ? `nothing to shelve in changelist ${req.changelist}` : "nothing to shelve");
      }
      for (let i = 1; i <= files.length; i++) {
        await pause();
        emit({ label: "Uploading", done: i, total: files.length, bytes: i * 800_000, finished: i === files.length });
      }
      const changes = files.map((f) => ({
        path: f.path,
        action: f.status === "added" ? ("add" as const) : f.status === "deleted" ? ("delete" as const) : ("modify" as const),
        kind: "file",
      }));
      const diff = files.map((f) => {
        const d = demoWorkingDiff(f.path, f.status);
        return d && !d.binary ? { path: f.path, patch: d.patch } : { path: f.path, skipped: "binary" };
      });
      let shelf: MockShelf;
      if (existing) {
        existing.version += 1;
        existing.description = description;
        existing.changes = changes;
        existing.diff = diff;
        existing.updated_at = now();
        existing.base_rev = st.rev;
        delete existing.unmet;
        shelf = existing;
      } else {
        shelf = {
          id: Math.max(0, ...table.map((x) => x.id)) + 1,
          version: 1,
          branch: st.branch,
          base_rev: st.rev,
          owner: { name: "alice", kind: "user" },
          description,
          state: "open",
          created_at: now(),
          updated_at: now(),
          changes,
          diff,
        };
        table.unshift(shelf);
      }
      if (!req.keep) st.files = st.files.filter((f) => !files.includes(f));
      return { shelf: shelf.id, version: shelf.version, files: files.length, kept: req.keep, updated: !!existing };
    },
    shelves: async (workspace, mine, state) => {
      const want = state ?? "open";
      const list = shelvesOf(workspace).filter((s) => s.state === want && (!mine || s.owner.name === "alice"));
      return { shelves: list.map(({ changes: _c, diff: _d, unmet: _u, ...s }) => ({ ...s })) };
    },
    shelf: async (workspace, id) => {
      const { unmet: _u, ...s } = shelfOf(workspace, id);
      return { ...s, changes: s.changes.map((c) => ({ ...c })), diff: s.diff.map((d) => ({ ...d })) };
    },
    shelfSubmit: async (workspace, id, version, message) => {
      const s = shelfOf(workspace, id);
      if (s.state === "deleted") fail("not_found", `shelf ${id} was deleted`);
      if (s.state === "submitted") fail("conflict", `shelf ${id} was already submitted as r${s.submitted_rev}`);
      if (s.version !== version) {
        const e: OpError = {
          code: "conflict",
          message: `the shelf was changed since that version; it is at version ${s.version} now: review it again`,
          details: { version: s.version },
        };
        throw e;
      }
      if (s.unmet?.length) {
        const lines = s.unmet.map((m) =>
          m.reason === "absent" ? `${m.name}: not reported yet (expected from ${m.reporter})` : `${m.name}: ${m.state ?? "?"}`,
        );
        const e: OpError = {
          code: "checks_required",
          message: `shelf ${id} version ${version} cannot be submitted until these checks pass:\n  ${lines.join("\n  ")}`,
          details: { shelf_id: id, shelf_version: version, missing: s.unmet },
        };
        throw e;
      }
      const info = workspaces.get(workspace);
      const head = Math.max(statusFor(workspace).rev, ...shelvesOf(workspace).map((x) => x.submitted_rev ?? 0));
      s.state = "submitted";
      s.submitted_rev = head + 1;
      s.updated_at = now();
      if (message?.trim()) s.description = message.trim();
      return { rev: head + 1, branch: info?.branch ?? s.branch, shelf: id, version };
    },
    shelfDelete: async (workspace, id) => {
      const s = shelfOf(workspace, id);
      if (s.owner.name !== "alice" && !opts.admin) {
        fail("permission_denied", `shelf ${id} belongs to ${s.owner.name}; only its owner or a depot admin may delete it`);
      }
      s.state = "deleted";
      s.updated_at = now();
    },
    unshelve: async (workspace, id) => {
      const s = shelfOf(workspace, id);
      if (s.state === "deleted") fail("not_found", `shelf ${id} was deleted`);
      const st = statusFor(workspace);
      const files = s.changes.map((c) => {
        const here = st.files.find((f) => f.path === c.path && f.status !== "untracked");
        if (here) {
          // A file changed here too: text merges, except Boat.cpp, which the demo leaves in conflict.
          const conflicted = c.path.endsWith("Boat.cpp") || s.diff.find((d) => d.path === c.path)?.skipped !== undefined;
          if (conflicted) here.status = "conflicted";
          return { path: c.path, outcome: conflicted ? "conflicted" : "merged" };
        }
        st.files.push({
          path: c.path,
          status: c.action === "add" ? "added" : c.action === "delete" ? "deleted" : "modified",
          copy_from: null,
          moved: false,
          changelist: null,
          locked: false,
          needs_lock: false,
          lock_required: false,
        });
        return { path: c.path, outcome: "applied" };
      });
      const counts: Record<string, number> = {};
      for (const f of files) counts[f.outcome] = (counts[f.outcome] ?? 0) + 1;
      changed.forEach((l) => l(workspace));
      return { shelf: id, version: s.version, files, counts };
    },
    checks: async (workspace, targets) => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      if (opts.noChecks) return { available: false, checks: {} };
      const checks: Record<string, ReturnType<typeof demoChecks>> = {};
      for (const t of targets) if (info.depot === "skyfall") checks[t] = demoChecks(t);
      return { available: true, checks };
    },
    imageSide: async (workspace, path, rev, side) => {
      if (!imageType(path)) fail("usage", `${path} is not an image`);
      // Files named Huge* stand for images over 64 MB.
      const size = path.includes("/Huge") ? 80_000_000 : side === "old" ? 1_802_340 : 1_874_211;
      const image = (night: boolean) => ({ size, url: size > 64 * 1024 * 1024 ? null : demoImage(night) });
      let action: string | undefined;
      if (rev === null) {
        const f = statusFor(workspace).files.find((x) => x.path === path);
        action = f?.status === "added" ? "add" : f?.status === "deleted" ? "delete" : f ? "modify" : undefined;
      } else {
        const info = workspaces.get(workspace);
        action = demoRevision(info?.branch ?? "main", rev)?.changes?.find((c) => c.path === path)?.action;
      }
      if (!action) fail("not_found", rev === null ? `${path} has no changes` : `r${rev} did not change ${path}`);
      if (side === "old") return action === "add" ? null : image(false);
      return action === "delete" ? null : image(true);
    },
    openLink: async () => {},
    bumpShelf: (workspace, id) => {
      const s = shelfOf(workspace, id);
      s.version += 1;
      s.updated_at = now();
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
