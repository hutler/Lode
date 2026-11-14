// A backend with made-up data, for the browser preview, screenshots and tests. It behaves
// like a small server with one user (`alice`, password `correct horse`) and two depots.

import type {
  Backend,
  BranchInfo,
  ChangelistInfo,
  ConflictInfo,
  MergeResult,
  PendingMerge,
  LockInfo,
  LogPage,
  CheckoutResult,
  CommitResult,
  OpError,
  ProgressEvent,
  RecentWorkspace,
  StatusFile,
  StatusResult,
  UpdateResult,
  WhoAmI,
  WorkspaceInfo,
} from "./api";
import { demoRevisionDiff, demoWorkingDiff } from "./mockDiff";
import { demoBlame, demoHead, demoLog, demoRevision } from "./mockHistory";
import { imageType } from "./api";
import { demoChecks } from "./mockChecks";
import { demoImage } from "./mockImages";
import { demoConflict } from "./mockMerge";
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
  /** An update of the first workspace stopped half way, so commits are refused until it is finished. */
  unfinishedUpdate?: boolean;
}

const DEMO_SERVER = "https://lode.example.com";

const DEMO_RECENT: RecentWorkspace[] = [
  { path: "D:\\Work\\Skyfall", depot: "skyfall", server: DEMO_SERVER, branch: "main" },
  { path: "D:\\Work\\Skyfall-release", depot: "skyfall", server: DEMO_SERVER, branch: "release-1.2" },
  { path: "D:\\Tools\\pipeline", depot: "pipeline", server: DEMO_SERVER, branch: "main" },
];

/** The demo depots' branches. Only `main` and `release-1.2` of skyfall have their own history; the
 * others show their parent's up to where they started. */
function demoBranches(depot: string): BranchInfo[] {
  if (depot !== "skyfall") return [{ name: "main", head_rev: 311, created_rev: 1, parent: null, state: "active" }];
  return [
    { name: "main", head_rev: 1482, created_rev: 1, parent: null, state: "active" },
    { name: "release-1.2", head_rev: 1404, created_rev: 1391, parent: { branch: "main", rev: 1390 }, state: "active" },
    {
      name: "art/harbor-night",
      head_rev: 1470,
      created_rev: 1471,
      parent: { branch: "main", rev: 1470 },
      state: "active",
    },
    {
      name: "proto/boats",
      head_rev: 1150,
      created_rev: 1151,
      parent: { branch: "main", rev: 1150 },
      state: "archived",
    },
  ];
}

/** Files an update or switch of the demo depot brings in from the server. */
const INCOMING = ["Content/Maps/Harbor_BuiltData.uasset", "Source/Skyfall/Skyfall.Build.cs", "Config/DefaultGame.ini"];

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
    refuseUnfinished(workspace);
    refuseConflicts(workspace);
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
    emit({
      label: "Uploading",
      done: picked.length,
      total: picked.length,
      bytes: picked.length * 1_200_000,
      finished: true,
    });
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
      const named = [
        ...new Set(
          statusFor(workspace)
            .files.map((f) => f.changelist)
            .filter((c): c is string => !!c),
        ),
      ].sort();
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
        table.push({
          path: f.path,
          branch: info.branch,
          owner: { name: "alice", kind: "user" },
          workspace,
          created_at: LOCK_TIME,
          mine: true,
        });
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
  // Branches per depot.
  const branchTables = new Map<string, BranchInfo[]>();
  const branchesOf = (depot: string): BranchInfo[] => {
    let t = branchTables.get(depot);
    if (!t) {
      t = demoBranches(depot);
      branchTables.set(depot, t);
    }
    return t;
  };
  const infoOf = (workspace: string): WorkspaceInfo => {
    const info = workspaces.get(workspace);
    if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
    return info;
  };
  /** The demo branch whose history `branch` shows, and the newest revision of it that does. */
  const historyOf = (depot: string, branch: string): { branch: string; cap: number } => {
    let name = branch;
    let cap = Number.MAX_SAFE_INTEGER;
    for (let i = 0; i < 8 && demoHead(name) === null; i++) {
      const b = branchesOf(depot).find((x) => x.name === name);
      if (!b?.parent) break;
      cap = Math.min(cap, b.parent.rev);
      name = b.parent.branch;
    }
    return { branch: name, cap };
  };
  // Pending merges and conflicts per workspace.
  const mergesOf = new Map<string, PendingMerge[]>();
  const conflictsOf = new Map<string, Map<string, ConflictInfo>>();
  const conflictTable = (workspace: string) => {
    let t = conflictsOf.get(workspace);
    if (!t) {
      t = new Map();
      conflictsOf.set(workspace, t);
    }
    return t;
  };
  const refuseConflicts = (workspace: string) => {
    const paths = [...conflictTable(workspace).keys()];
    if (paths.length) {
      const e: OpError = {
        code: "conflicted",
        message: `resolve the conflicts first (\`lode resolve\`): ${paths.join(", ")}`,
        details: { paths },
      };
      throw e;
    }
  };
  /** Leaves `path` in conflict with theirs from `rev`. */
  const conflictOn = (workspace: string, path: string, rev: number) => {
    const st = statusFor(workspace);
    const f = st.files.find((x) => x.path === path);
    if (f) f.status = "conflicted";
    conflictTable(workspace).set(path, demoConflict(path, rev));
  };
  let unfinished = opts.unfinishedUpdate ? (recent[0]?.path ?? null) : null;
  const refuseUnfinished = (workspace: string) => {
    if (unfinished === workspace) fail("update_incomplete", "an update did not finish; run `lode update` first");
  };
  /** Moves a workspace to `branch` at `rev` (its head when null), as `lode update` and `lode switch` do. */
  const sync = async (workspace: string, branch: string, rev: number | null): Promise<UpdateResult> => {
    const info = infoOf(workspace);
    const st = statusFor(workspace);
    const b = branchesOf(info.depot).find((x) => x.name === branch);
    if (!b) fail("not_found", `branch \`${branch}\` not found`);
    const target = rev ?? b.head_rev;
    if (target > b.head_rev || target < 1) fail("not_found", `revision ${target} is not on branch \`${branch}\``);
    const switching = branch !== info.branch;
    // Locks stay on the old branch: keep them in its table before the files forget them.
    const locksLeft = switching ? st.files.filter((f) => f.locked).map((f) => f.path) : [];
    if (switching && locksLeft.length && !opts.noLocks) locksOf(workspace);
    const incoming =
      info.depot === "skyfall" && (target !== info.rev || switching || unfinished === workspace) ? INCOMING : [];
    for (let i = 1; i <= 4; i++) {
      await pause();
      emit({
        label: "Fetching",
        done: Math.round((incoming.length * i) / 4),
        total: incoming.length,
        bytes: i * 3_100_000,
        finished: false,
      });
    }
    emit({ label: "Fetching", done: incoming.length, total: incoming.length, bytes: 12_400_000, finished: true });
    const files: UpdateResult["files"] = incoming.map((path) => ({ path, outcome: "updated" }));
    if (switching) {
      for (const f of st.files) {
        if (f.status === "untracked") continue;
        f.locked = false;
        // The release branch has its own fix to the boat, so the local edit does not merge.
        if (branch === "release-1.2" && f.path === "Source/Skyfall/Boat.cpp") {
          conflictOn(workspace, f.path, target);
          files.push({ path: f.path, outcome: "conflicted" });
        } else if (f.status === "modified") {
          files.push({ path: f.path, outcome: "merged" });
        }
      }
    }
    const counts: Record<string, number> = {};
    for (const f of files) counts[f.outcome] = (counts[f.outcome] ?? 0) + 1;
    const from = info.rev;
    const switchedFrom = switching ? info.branch : null;
    info.branch = branch;
    info.rev = target;
    st.branch = branch;
    st.rev = target;
    if (unfinished === workspace) unfinished = null;
    remember(info);
    changed.forEach((l) => l(workspace));
    return {
      rev: target,
      branch,
      switched_from: switchedFrom,
      locks_left: locksLeft,
      from,
      full: false,
      files,
      counts,
      downloaded: incoming.length * 3_100_000,
    };
  };

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
      return { ...st, files: st.files.map((f) => ({ ...f })), merges: [...(mergesOf.get(workspace) ?? [])] };
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
      const m = mergesOf.get(workspace)?.[0];
      if (m)
        fail(
          "usage",
          `the merge of \`${m.branch}\` r${m.from_rev}..r${m.to_rev} must be committed whole: commit without paths or -c`,
        );
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
        lists.set(
          workspace,
          listsFor(workspace).filter((l) => l !== list),
        );
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
      lists.set(
        workspace,
        listsFor(workspace).filter((l) => l.name !== name),
      );
    },
    log: async (workspace, path, before, max): Promise<LogPage> => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      await pause();
      const h = historyOf(info.depot, info.branch);
      const upTo = Math.min(before ?? Number.MAX_SAFE_INTEGER, h.cap + 1);
      return { depot: info.depot, branch: info.branch, ...demoLog(h.branch, path, upTo, max) };
    },
    revision: async (workspace, rev) => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      const h = historyOf(info.depot, info.branch);
      const r = rev <= h.cap ? demoRevision(h.branch, rev) : null;
      if (!r) fail("not_found", `revision ${rev} not found on ${info.branch}`);
      return r;
    },
    blame: async (workspace, path, rev) => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      const h = historyOf(info.depot, info.branch);
      return demoBlame(h.branch, path, rev ?? (h.cap < Number.MAX_SAFE_INTEGER ? h.cap : null));
    },
    diff: async (workspace, path) => {
      const f = statusFor(workspace).files.find((x) => x.path === path);
      return f ? demoWorkingDiff(path, f.status) : null;
    },
    diffRevision: async (workspace, rev, path) => {
      const info = workspaces.get(workspace);
      if (!info) fail("not_a_workspace", `${workspace} is not in a lode workspace`);
      const change = demoRevision(historyOf(info.depot, info.branch).branch, rev)?.changes?.find(
        (c) => c.path === path,
      );
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
      const list = req.changelist
        ? req.changelist === "default"
          ? null
          : listsFor(workspace).find((l) => l.name === req.changelist)
        : null;
      if (req.changelist && req.changelist !== "default" && !list)
        fail("not_found", `no changelist \`${req.changelist}\``);
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
          (req.changelist
            ? (f.changelist ?? "default") === req.changelist
            : req.paths.length === 0 || req.paths.includes(f.path)),
      );
      if (files.length === 0) {
        fail(
          "nothing_to_shelve",
          req.changelist ? `nothing to shelve in changelist ${req.changelist}` : "nothing to shelve",
        );
      }
      for (let i = 1; i <= files.length; i++) {
        await pause();
        emit({ label: "Uploading", done: i, total: files.length, bytes: i * 800_000, finished: i === files.length });
      }
      const changes = files.map((f) => ({
        path: f.path,
        action:
          f.status === "added" ? ("add" as const) : f.status === "deleted" ? ("delete" as const) : ("modify" as const),
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
          m.reason === "absent"
            ? `${m.name}: not reported yet (expected from ${m.reporter})`
            : `${m.name}: ${m.state ?? "?"}`,
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
        fail(
          "permission_denied",
          `shelf ${id} belongs to ${s.owner.name}; only its owner or a depot admin may delete it`,
        );
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
          const conflicted =
            c.path.endsWith("Boat.cpp") || s.diff.find((d) => d.path === c.path)?.skipped !== undefined;
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
        action = demoRevision(info ? historyOf(info.depot, info.branch).branch : "main", rev)?.changes?.find(
          (c) => c.path === path,
        )?.action;
      }
      if (!action) fail("not_found", rev === null ? `${path} has no changes` : `r${rev} did not change ${path}`);
      if (side === "old") return action === "add" ? null : image(false);
      return action === "delete" ? null : image(true);
    },
    branches: async (workspace, all) => {
      const info = infoOf(workspace);
      await pause();
      const list = branchesOf(info.depot).filter((b) => all || b.state === "active" || b.name === info.branch);
      return { current: info.branch, branches: list.map((b) => ({ ...b, parent: b.parent && { ...b.parent } })) };
    },
    branchCreate: async (workspace, name, from, switchTo) => {
      const info = infoOf(workspace);
      const table = branchesOf(info.depot);
      if (!/^[A-Za-z0-9][A-Za-z0-9._/-]{0,99}$/.test(name) || name.includes("//") || name.endsWith("/")) {
        fail("invalid_request", `invalid branch name \`${name}\`: use letters, digits, \`.\`, \`_\`, \`-\` and \`/\``);
      }
      if (table.some((b) => b.name === name)) fail("conflict", `branch \`${name}\` already exists`);
      let fromBranch = info.branch;
      let fromRev: number | null = info.rev;
      const spec = from?.trim();
      if (spec) {
        const at = spec.lastIndexOf("@");
        if (at >= 0) {
          const digits = spec.slice(at + 1).replace(/^r/, "");
          if (!/^\d+$/.test(digits)) fail("usage", `\`${spec}\` is not BRANCH or BRANCH@REV`);
          fromBranch = spec.slice(0, at);
          fromRev = Number(digits);
        } else {
          fromBranch = spec;
          fromRev = null;
        }
      }
      const parent = table.find((b) => b.name === fromBranch);
      if (!parent) fail("not_found", `branch \`${fromBranch}\` not found`);
      const rev = fromRev ?? parent.head_rev;
      if (rev > parent.head_rev || rev < 1) fail("not_found", `revision ${rev} is not on branch \`${fromBranch}\``);
      const made: BranchInfo = {
        name,
        head_rev: rev,
        created_rev: rev,
        parent: { branch: fromBranch, rev },
        state: "active",
      };
      table.push(made);
      return switchTo ? { ...made, switch: await sync(workspace, name, null) } : { ...made };
    },
    branchArchive: async (workspace, name) => {
      const info = infoOf(workspace);
      if (!opts.admin) fail("permission_denied", "archiving a branch needs admin on the depot");
      const b = branchesOf(info.depot).find((x) => x.name === name);
      if (!b) fail("not_found", `branch \`${name}\` not found`);
      b.state = "archived";
      return { ...b };
    },
    switchBranch: async (workspace, branch, rev) => {
      const info = infoOf(workspace);
      if (unfinished === workspace) {
        fail("update_incomplete", `an update to r${info.rev} did not finish; run \`lode update\` to complete it first`);
      }
      return sync(workspace, branch, rev);
    },
    update: async (workspace, rev) => sync(workspace, infoOf(workspace).branch, rev),
    commitAll: async (workspace, message) => {
      const st = statusFor(workspace);
      const r = await commitFiles(
        workspace,
        st.files.filter((f) => f.status !== "untracked"),
        message,
      );
      mergesOf.delete(workspace);
      return r;
    },
    merge: async (workspace, source, rev): Promise<MergeResult> => {
      const info = infoOf(workspace);
      refuseUnfinished(workspace);
      refuseConflicts(workspace);
      if (source === info.branch) fail("usage", `\`${source}\` is this workspace's branch`);
      const b = branchesOf(info.depot).find((x) => x.name === source);
      if (!b) fail("not_found", `branch \`${source}\` not found`);
      const first = b.created_rev;
      if (rev !== null && (rev < first || rev > b.head_rev))
        fail("not_found", `r${rev} is not on branch \`${source}\``);
      const from = rev ?? first;
      const to = rev ?? b.head_rev;
      if (b.head_rev <= b.created_rev && rev === null) {
        return { source, from_rev: to, to_rev: to, base: to, nothing_to_do: true, files: [], counts: {} };
      }
      for (let i = 1; i <= 3; i++) {
        await pause();
        emit({ label: "Merging", done: i, total: 3, bytes: i * 2_000_000, finished: i === 3 });
      }
      const st = statusFor(workspace);
      const files: MergeResult["files"] = [];
      const touch = (path: string, status: StatusFile["status"]) => {
        if (!st.files.some((f) => f.path === path)) {
          st.files.push({
            path,
            status,
            copy_from: null,
            moved: false,
            changelist: null,
            locked: false,
            needs_lock: false,
            lock_required: false,
          });
        }
      };
      files.push({ path: "Source/Skyfall/Weather.cpp", outcome: "applied" });
      touch("Source/Skyfall/Weather.cpp", "modified");
      if (rev === null) {
        files.push({ path: "Source/Skyfall/Wake.cpp", outcome: "applied" });
        touch("Source/Skyfall/Wake.cpp", "added");
        files.push({ path: "Config/DefaultGame.ini", outcome: "merged" });
        touch("Config/DefaultGame.ini", "modified");
        for (const path of ["Source/Skyfall/Boat.cpp", "Content/Maps/Harbor.umap"]) {
          if (st.files.some((f) => f.path === path && f.status === "modified")) {
            conflictOn(workspace, path, to);
            files.push({ path, outcome: "conflicted" });
          }
        }
        files.push({ path: "Docs/Release-1.2.md", outcome: "unchanged" });
      }
      const merges = mergesOf.get(workspace) ?? [];
      merges.push({ branch: source, from_rev: from, to_rev: to });
      mergesOf.set(workspace, merges);
      changed.forEach((l) => l(workspace));
      const counts: Record<string, number> = {};
      for (const f of files) counts[f.outcome] = (counts[f.outcome] ?? 0) + 1;
      return {
        source,
        from_rev: from,
        to_rev: to,
        base: rev === null ? (b.parent?.rev ?? null) : null,
        nothing_to_do: false,
        files,
        counts,
      };
    },
    abandonMerge: async (workspace) => {
      infoOf(workspace);
      mergesOf.delete(workspace);
    },
    conflict: async (workspace, path) => {
      const c = conflictTable(workspace).get(path);
      if (!c) fail("not_found", `not in conflict: ${path}`);
      return structuredClone(c);
    },
    resolve: async (workspace, paths, using) => {
      const t = conflictTable(workspace);
      const missing = paths.filter((p) => !t.has(p));
      if (missing.length) fail("not_found", `not in conflict: ${missing.join(", ")}`);
      const st = statusFor(workspace);
      for (const p of paths) {
        t.delete(p);
        const f = st.files.find((x) => x.path === p);
        if (f) f.status = "modified";
      }
      void using;
      changed.forEach((l) => l(workspace));
      return { resolved: paths };
    },
    resolveWith: async (workspace, path, content) => {
      const t = conflictTable(workspace);
      if (!t.has(path)) fail("not_found", `not in conflict: ${path}`);
      t.delete(path);
      const f = statusFor(workspace).files.find((x) => x.path === path);
      if (f) f.status = "modified";
      void content;
      changed.forEach((l) => l(workspace));
      return { resolved: [path] };
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
