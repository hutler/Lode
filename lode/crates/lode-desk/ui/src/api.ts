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
