// The app's view of Lode. In the desktop app every call goes to a Tauri command that runs the
// matching `lode-ops` operation; in a plain browser (the mocked preview, screenshots, tests)
// it goes to the mock backend instead.

/** An operation's error, as `lode --json` reports it. */
export interface OpError {
  code: string;
  message: string;
  details?: unknown;
}

/** A progress update from a running operation. */
export interface ProgressEvent {
  label: string;
  done: number;
  total: number;
  bytes: number;
  finished: boolean;
}

/** Who a token belongs to (`lode whoami --json`). */
export interface WhoAmI {
  server: string;
  name: string;
  kind: string;
  scopes: string[];
}

/** A workspace the app has opened before. */
export interface RecentWorkspace {
  path: string;
  depot: string;
  server: string;
  branch: string;
}

/** Where a workspace points. */
export interface WorkspaceInfo {
  root: string;
  server: string;
  depot: string;
  branch: string;
  rev: number;
}

/** One file of `lode status --json`. */
export interface StatusFile {
  path: string;
  status: "modified" | "added" | "deleted" | "missing" | "conflicted" | "untracked" | "unchanged";
  copy_from: { path: string; rev: number } | null;
  moved: boolean;
  changelist: string | null;
  locked: boolean;
  needs_lock: boolean;
  lock_required: boolean;
}

/** `lode status --json`. */
export interface StatusResult {
  branch: string;
  rev: number;
  files: StatusFile[];
}

/** `lode commit --json`. */
export interface CommitResult {
  rev: number;
  branch: string;
  files: number;
}

/** Who did something. */
export interface PrincipalRef {
  name: string;
  kind: "user" | "agent";
}

/** A merge recorded by a revision. */
export interface MergedFrom {
  branch: string;
  from_rev: number;
  to_rev: number;
}

/** One changed path of a revision (`lode log -v`). */
export interface ChangeView {
  path: string;
  action: "add" | "modify" | "delete";
  kind?: string;
  size?: number;
  copy_from?: { path: string; rev: number } | null;
}

/** A revision as the server describes it (`lode log --json`). */
export interface Revision {
  rev: number;
  branch: string;
  parent: number | null;
  author: PrincipalRef;
  submitter: PrincipalRef;
  created_at: string;
  message: string;
  provenance?: {
    client?: string;
    agent?: Record<string, string>;
    imported?: { system: string; id: string; author?: string; date?: string };
    submitted_shelf?: number;
  } | null;
  merged_from: MergedFrom[];
  changes?: ChangeView[];
  changes_truncated?: boolean;
}

/** One page of `lode log --json`. */
export interface LogPage {
  depot: string;
  branch: string;
  revisions: Revision[];
  more: boolean;
}

/** `lode blame --json`. */
export interface BlameResult {
  path: string;
  rev: number;
  lines: { rev: number; text: string }[];
  revisions: { rev: number; branch: string; author: PrincipalRef; created_at: string; summary: string }[];
  truncated?: boolean;
}

/** One file of `lode diff --json`. */
export interface FileDiff {
  path: string;
  status: string;
  old_path: string | null;
  binary: boolean;
  old_size: number | null;
  new_size: number | null;
  old_mode: string | null;
  new_mode: string | null;
  insertions: number;
  deletions: number;
  patch?: string;
}

/** One changelist of `lode changelist list --json`; `default` holds files in none. */
export interface ChangelistInfo {
  name: string;
  description: string;
  files: string[];
}

/** One lock of `lode locks --json`. */
export interface LockInfo {
  path: string;
  branch: string;
  owner: PrincipalRef;
  workspace: string;
  comment?: string;
  created_at: string;
  /** Held by this workspace. */
  mine: boolean;
}

/** One file's diff in a shelf against the shelf's base. */
export interface ShelfFileDiff {
  path: string;
  /** Unified diff, for text files. */
  patch?: string;
  /** Why there is no patch: `binary`, `symlink` or `too_large`. */
  skipped?: string;
}

/** A shelf as `lode shelves --json` and `lode shelf diff --json` give it. */
export interface ShelfInfo {
  id: number;
  /** Raised by every update; a submit names the version that was reviewed. */
  version: number;
  branch: string;
  base_rev: number;
  owner: PrincipalRef;
  /** The user who owns the agent, when the owner is an agent. */
  agent_owner?: string;
  description: string;
  state: "open" | "submitted" | "deleted";
  submitted_rev?: number;
  created_at: string;
  updated_at: string;
  /** Only when one shelf is asked for. */
  changes?: ChangeView[];
  merged_from?: MergedFrom[];
  diff?: ShelfFileDiff[];
}

/** What to shelve. */
export interface ShelveRequest {
  /** Depot paths; all changes when empty and no changelist is named. */
  paths: string[];
  changelist: string | null;
  /** Description; the changelist's, or the updated shelf's, when null. */
  message: string | null;
  /** Leave the changes in the workspace. */
  keep: boolean;
  /** Replace the content of this open shelf of mine. */
  update: number | null;
}

/** `lode shelve --json`. */
export interface ShelveResult {
  shelf: number;
  version: number;
  files: number;
  kept: boolean;
  updated: boolean;
}

/** `lode unshelve --json`: what happened to each file. */
export interface UnshelveResult {
  shelf: number;
  version: number;
  files: { path: string; outcome: "applied" | "merged" | "conflicted" | "unchanged" | "skipped" | string }[];
  counts: Record<string, number>;
}

/** `lode shelf submit --json`. */
export interface SubmitResult {
  rev: number;
  branch: string;
  shelf: number;
  version: number;
}

/** One unmet check of a `checks_required` refusal. */
export interface MissingCheck {
  name: string;
  /** Who must report it. */
  reporter: string;
  reason: "absent" | "not_success" | "wrong_reporter" | string;
  state?: string;
  reported_by?: string;
}

/** One check of `lode checks --json`: a result an outside system reported. */
export interface CheckInfo {
  name: string;
  state: "pending" | "success" | "failure" | "error" | "cancelled";
  summary?: string;
  url?: string;
  reporter: { name: string; kind: "user" | "agent"; owner?: string };
  updated_at: string;
}

/** The checks of several targets (`rN`, `ID@VERSION`); `available` is false when the server
 * has no checks module. */
export interface ChecksResult {
  available: boolean;
  checks: Record<string, CheckInfo[]>;
}

/** One side of an image diff. */
export interface ImageSide {
  size: number;
  /** Where the window can load it from; null when it is over 64 MB and not read. */
  url: string | null;
}

/** A branch of a depot (`lode branch list --json`). */
export interface BranchInfo {
  name: string;
  head_rev: number;
  created_rev: number;
  /** Where it started; null for the depot's first branch. */
  parent: { branch: string; rev: number } | null;
  state: "active" | "archived";
}

/** `lode branch list --json`: the depot's branches and the workspace's. */
export interface BranchList {
  current: string;
  branches: BranchInfo[];
}

/** `lode update --json` and `lode switch --json`: what happened to each file. */
export interface UpdateResult {
  rev: number;
  branch: string;
  /** The branch the workspace left, after a switch; null after an update. */
  switched_from: string | null;
  /** Locks held on the old branch, which stay there. */
  locks_left: string[];
  from: number;
  full: boolean;
  files: { path: string; outcome: "added" | "updated" | "merged" | "conflicted" | "deleted" | "kept" | string }[];
  counts: Record<string, number>;
  downloaded: number;
}

/** `lode branch create --json`, with the switch's result when it switched. */
export type CreatedBranch = BranchInfo & { switch?: UpdateResult };

/** `lode checkout --json`. */
export interface CheckoutResult {
  workspace: string;
  depot: string;
  branch: string;
  rev: number;
  files: number;
}

export interface Backend {
  appVersion(): Promise<string>;
  recentWorkspaces(): Promise<RecentWorkspace[]>;
  forgetWorkspace(path: string): Promise<void>;
  /** Who is signed in to `server`, or null. */
  signedIn(server: string): Promise<WhoAmI | null>;
  signIn(server: string, name: string, password: string): Promise<WhoAmI>;
  openWorkspace(path: string): Promise<WorkspaceInfo>;
  checkout(target: string, dir: string, branch: string): Promise<CheckoutResult>;
  status(workspace: string): Promise<StatusResult>;
  /** Starts tracking untracked files (depot paths). */
  add(workspace: string, paths: string[]): Promise<void>;
  /** Discards local changes to files (depot paths). */
  revert(workspace: string, paths: string[]): Promise<void>;
  /** Commits the changes to `paths`; untracked ones must be added first. */
  commit(workspace: string, paths: string[], message: string): Promise<CommitResult>;
  /** Up to `max` revisions older than `before` (newest first); with `path`, that file's
   * history followed through renames. */
  log(workspace: string, path: string | null, before: number | null, max: number): Promise<LogPage>;
  /** One revision with its changed files. */
  revision(workspace: string, rev: number): Promise<Revision>;
  /** Who last changed each line of `path`, as of `rev` (default: the workspace's). */
  blame(workspace: string, path: string, rev: number | null): Promise<BlameResult>;
  /** A working file (depot path) against its base; null when it has no change. */
  diff(workspace: string, path: string): Promise<FileDiff | null>;
  /** A file as revision `rev` changed it. */
  diffRevision(workspace: string, rev: number, path: string): Promise<FileDiff>;
  /** Commits exactly the files of changelist `name`; its description when `message` is null. */
  commitChangelist(workspace: string, name: string, message: string | null): Promise<CommitResult>;
  /** The changelists, `default` first, with their changed files. */
  changelists(workspace: string): Promise<{ changelists: ChangelistInfo[] }>;
  changelistNew(workspace: string, name: string, description: string): Promise<void>;
  /** Puts tracked files into `name`; `default` takes them out of any. */
  changelistMove(workspace: string, name: string, paths: string[]): Promise<void>;
  /** Deletes a changelist; its files return to `default`. */
  changelistDelete(workspace: string, name: string): Promise<void>;
  /** Locks `paths` (depot paths), all or none. */
  lock(workspace: string, paths: string[], comment: string | null): Promise<{ locked: string[] }>;
  /** Releases locks; `force` breaks other people's (depot admin). */
  unlock(workspace: string, paths: string[], force: boolean): Promise<{ released: string[] }>;
  /** The branch's locks; `mine` keeps this workspace's only. */
  locks(workspace: string, mine: boolean): Promise<{ locks: LockInfo[] }>;
  /** Puts changes on a shelf on the server (or replaces an open shelf's content). */
  shelve(workspace: string, request: ShelveRequest): Promise<ShelveResult>;
  /** The depot's shelves, newest first; `state` null means open ones. */
  shelves(workspace: string, mine: boolean, state: ShelfInfo["state"] | null): Promise<{ shelves: ShelfInfo[] }>;
  /** One shelf with its changes and each file's diff. */
  shelf(workspace: string, id: number): Promise<ShelfInfo>;
  /** Submits `version` of a shelf; refused with `conflict` when it changed since. */
  shelfSubmit(workspace: string, id: number, version: number, message: string | null): Promise<SubmitResult>;
  shelfDelete(workspace: string, id: number): Promise<void>;
  /** Applies a shelf's changes to the workspace. */
  unshelve(workspace: string, id: number): Promise<UnshelveResult>;
  /** The checks of each target: `rN` for a revision, `ID@VERSION` for a shelf. */
  checks(workspace: string, targets: string[]): Promise<ChecksResult>;
  /** One side of an image diff: a working file (`rev` null) against its base, or a file as
   * revision `rev` changed it; null when that side has no file. */
  imageSide(workspace: string, path: string, rev: number | null, side: "old" | "new"): Promise<ImageSide | null>;
  /** The depot's branches; `all` adds archived ones. */
  branches(workspace: string, all: boolean): Promise<BranchList>;
  /** Creates a branch from `from` (`BRANCH` or `BRANCH@REV`; the workspace's branch at its
   * revision when null), switching the workspace to it when `switchTo`. */
  branchCreate(workspace: string, name: string, from: string | null, switchTo: boolean): Promise<CreatedBranch>;
  /** Archives a branch (depot admin). */
  branchArchive(workspace: string, name: string): Promise<BranchInfo>;
  /** Moves the workspace to `branch` (its head, or `rev`), carrying local changes over. */
  switchBranch(workspace: string, branch: string, rev: number | null): Promise<UpdateResult>;
  /** Updates to the branch head (or `rev`); also finishes an update or switch that stopped. */
  update(workspace: string, rev: number | null): Promise<UpdateResult>;
  /** Opens a web link in the system browser. */
  openLink(url: string): Promise<void>;
  /** Reports changes to files in `workspace` through `onChanged` until `unwatch`. */
  watch(workspace: string): Promise<void>;
  unwatch(workspace: string): Promise<void>;
  /** Calls `listener` with a workspace root when its files changed. */
  onChanged(listener: (workspace: string) => void): () => void;
  /** Asks the user for a folder; null when they cancel. */
  pickFolder(title: string): Promise<string | null>;
  /** Calls `listener` for each progress update until the returned function is called. */
  onProgress(listener: (e: ProgressEvent) => void): () => void;
}

const IMAGE_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
};

/** The MIME type of an image the window can show, by the file's extension; null otherwise. */
export function imageType(path: string): string | null {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? (IMAGE_TYPES[name.slice(dot + 1).toLowerCase()] ?? null) : null;
}

/** Reads the `image_side` answer: a tag byte (0 no file, 1 too large, 2 content), the size as
 * eight little-endian bytes, then the content, which becomes an object URL. */
export function decodeImageSide(raw: ArrayBuffer, path: string): ImageSide | null {
  const view = new DataView(raw);
  const tag = view.getUint8(0);
  if (tag === 0) return null;
  const size = Number(view.getBigUint64(1, true));
  if (tag === 1) return { size, url: null };
  const blob = new Blob([raw.slice(9)], { type: imageType(path) ?? "application/octet-stream" });
  return { size, url: URL.createObjectURL(blob) };
}

/** Whether `e` looks like an operation error. */
export function isOpError(e: unknown): e is OpError {
  return typeof e === "object" && e !== null && "code" in e && "message" in e;
}

/** An error of any shape as an operation error. */
export function asOpError(e: unknown): OpError {
  if (isOpError(e)) return e;
  return { code: "internal", message: e instanceof Error ? e.message : String(e) };
}

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function tauriBackend(): Promise<Backend> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const { open } = await import("@tauri-apps/plugin-dialog");
  return {
    appVersion: () => invoke("app_version"),
    recentWorkspaces: () => invoke("recent_workspaces"),
    forgetWorkspace: (path) => invoke("forget_workspace", { path }),
    signedIn: (server) => invoke("signed_in", { server }),
    signIn: (server, name, password) => invoke("sign_in", { server, name, password }),
    openWorkspace: (path) => invoke("open_workspace", { path }),
    checkout: (target, dir, branch) => invoke("checkout", { target, dir, branch }),
    status: (workspace) => invoke("status", { workspace }),
    add: async (workspace, paths) => {
      await invoke("add", { workspace, paths });
    },
    revert: async (workspace, paths) => {
      await invoke("revert", { workspace, paths });
    },
    commit: (workspace, paths, message) => invoke("commit", { workspace, paths, message }),
    log: (workspace, path, before, max) => invoke("log", { workspace, path, before, max }),
    revision: (workspace, rev) => invoke("revision", { workspace, rev }),
    blame: (workspace, path, rev) => invoke("blame", { workspace, path, rev }),
    diff: (workspace, path) => invoke("diff", { workspace, path }),
    diffRevision: (workspace, rev, path) => invoke("diff_revision", { workspace, rev, path }),
    commitChangelist: (workspace, name, message) => invoke("commit_changelist", { workspace, name, message }),
    changelists: (workspace) => invoke("changelists", { workspace }),
    changelistNew: async (workspace, name, description) => {
      await invoke("changelist_new", { workspace, name, description });
    },
    changelistMove: async (workspace, name, paths) => {
      await invoke("changelist_move", { workspace, name, paths });
    },
    changelistDelete: async (workspace, name) => {
      await invoke("changelist_delete", { workspace, name });
    },
    lock: (workspace, paths, comment) => invoke("lock", { workspace, paths, comment }),
    unlock: (workspace, paths, force) => invoke("unlock", { workspace, paths, force }),
    locks: (workspace, mine) => invoke("locks", { workspace, mine }),
    shelve: (workspace, request) => invoke("shelve", { workspace, request }),
    shelves: (workspace, mine, state) => invoke("shelves", { workspace, mine, state }),
    shelf: (workspace, id) => invoke("shelf", { workspace, id }),
    shelfSubmit: (workspace, id, version, message) => invoke("shelf_submit", { workspace, id, version, message }),
    shelfDelete: async (workspace, id) => {
      await invoke("shelf_delete", { workspace, id });
    },
    unshelve: (workspace, id) => invoke("unshelve", { workspace, id }),
    checks: (workspace, targets) => invoke("checks", { workspace, targets }),
    branches: (workspace, all) => invoke("branches", { workspace, all }),
    branchCreate: (workspace, name, from, switchTo) => invoke("branch_create", { workspace, name, from, switch: switchTo }),
    branchArchive: (workspace, name) => invoke("branch_archive", { workspace, name }),
    switchBranch: (workspace, branch, rev) => invoke("switch", { workspace, branch, rev }),
    update: (workspace, rev) => invoke("update", { workspace, rev }),
    imageSide: async (workspace, path, rev, side) => {
      const raw = await invoke<ArrayBuffer>("image_side", { workspace, path, rev, side });
      return decodeImageSide(raw, path);
    },
    openLink: async (url) => {
      await invoke("open_link", { url });
    },
    watch: (workspace) => invoke("watch", { workspace }),
    unwatch: (workspace) => invoke("unwatch", { workspace }),
    onChanged: (listener) => subscribe<string>("lode://changed", listener),
    pickFolder: async (title) => {
      const picked = await open({ directory: true, multiple: false, title });
      return typeof picked === "string" ? picked : null;
    },
    onProgress: (listener) => subscribe<ProgressEvent>("lode://progress", listener),
  };

  function subscribe<T>(event: string, listener: (payload: T) => void): () => void {
    let stop: (() => void) | null = null;
    let stopped = false;
    void listen<T>(event, (e) => listener(e.payload)).then((un) => {
      if (stopped) un();
      else stop = un;
    });
    return () => {
      stopped = true;
      stop?.();
    };
  }
}

let current: Promise<Backend> | null = null;

/** The backend: Tauri in the app, the mock elsewhere. */
export function backend(): Promise<Backend> {
  if (!current) {
    current = inTauri()
      ? tauriBackend()
      : import("./mock").then((m) => m.createMockBackend());
  }
  return current;
}

/** Replaces the backend (tests). */
export function setBackend(b: Backend): void {
  current = Promise.resolve(b);
}
