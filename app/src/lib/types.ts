// The residue CLI's JSON documents, as residue 0.0.6 prints them. These are
// the contract between the CLI and the window: the window shows them and
// never decides on its own what is safe to remove.

export type Verdict = "candidate" | "keep" | "unknown" | "stale";
export type CheckStatus = "pass" | "block" | "unknown";

export interface Check {
  id: string;
  status: CheckStatus;
  detail: string;
}

export interface ActivityScope {
  backend: string;
  available: boolean;
  inspected_processes: number | null;
  running_processes: number | null;
  reason: string | null;
  detail: string | null;
}

export interface Activity {
  state: string;
  processes: { pid: number; command: string }[];
  reason: string | null;
  detail: string | null;
}

export interface IgnoredEntry {
  path: string;
  regenerable: boolean;
  files: number | null;
  bytes: number | null;
}

export interface Size {
  apparent_bytes: number;
  allocated_bytes: number;
  reclaimable_bytes: number;
  entries: number;
  unreadable_entries: number;
  newest_mtime: number | null;
}

export interface LastChange {
  unix: number;
  at: string;
  source: "files" | "git_dir" | "reflog";
  what: string;
}

export interface Worktree {
  path: string;
  canonical_path: string | null;
  verdict: Verdict;
  stale_reason: string | null;
  head: string | null;
  branch: string | null;
  detached: boolean;
  locked: boolean;
  lock_reason: string | null;
  checks: Check[];
  activity: Activity | null;
  ignored: IgnoredEntry[];
  local_only_commits: number | null;
  local_only_commits_error: string | null;
  upstream: string | null;
  upstream_ref: string | null;
  upstream_gone: boolean | null;
  upstream_error: string | null;
  size: Size | null;
  last_change: LastChange | null;
  last_change_error: string | null;
}

export interface FreeSpace {
  free_bytes: number;
  total_bytes: number;
  free_inodes: number;
  total_inodes: number;
}

/** One entry in a repository's residue trash. */
export interface TrashItem {
  entry: string;
  entry_path: string;
  worktree_path: string | null;
  original_path: string | null;
  branch: string | null;
  head: string | null;
  trashed_at: string | null;
  allocated_bytes: number | null;
  reclaimable_bytes: number | null;
  registered: boolean;
  error: string | null;
}

export interface Repository {
  common_dir: string;
  main_path: string;
  bare: boolean;
  free_space: FreeSpace | null;
  worktrees: Worktree[];
  trash: TrashItem[];
  interrupted: unknown[];
}

export interface ScanDocument {
  schema: "residue.scan/v1";
  version: string;
  generated_at: string;
  activity: ActivityScope;
  repositories: Repository[];
  orphans: { path: string; missing_gitdir: string }[];
  errors: string[];
  summary: {
    linked_worktrees: number;
    candidates: number;
    candidate_allocated_bytes: number;
    candidate_reclaimable_bytes: number;
    trash_entries: number;
    interrupted_removals: number;
  };
}

export interface ExplainDocument {
  schema: "residue.explain/v1";
  version: string;
  generated_at: string;
  activity: ActivityScope;
  main_path: string;
  worktree: Worktree;
}

export type RemoveResult = "removed" | "trashed" | "refused" | "skipped" | "failed" | "invalid";
export type PurgeResult = "purged" | "kept" | "skipped" | "failed" | "invalid" | "not_attempted";

export interface RemoveDocument {
  schema: "residue.remove/v1";
  version: string;
  generated_at: string;
  activity: ActivityScope;
  trash: { enabled: boolean; keep: string; keep_seconds: number };
  results: {
    path: string;
    result: RemoveResult;
    reason: string | null;
    trash_entry: string | null;
    worktree: Worktree | null;
  }[];
  purged: {
    result: PurgeResult;
    reason: string | null;
    entry: TrashItem;
    worktree: Worktree | null;
  }[];
  errors: string[];
}

export interface Removal {
  document: RemoveDocument;
  exit_code: number | null;
}

export interface RestoreDocument {
  schema: "residue.restore/v1";
  version: string;
  generated_at: string;
  restored: string | null;
  trash: TrashItem[];
  errors: string[];
}

/** `residue restore --json`, run in one scan folder. */
export interface Listing {
  root: string;
  document: RestoreDocument | null;
  error: CliError | null;
}

/**
 * Something the Restore screen can bring back. residue 0.0.6 lists its
 * trash. A later release also lists worktrees it deleted, with a hidden ref
 * that keeps their commit (`kind: "removed"`); the screen shows both kinds.
 */
export interface Restorable {
  kind: "trash" | "removed";
  /** The scan folder whose listing returned it; restore runs there. */
  root: string;
  name: string;
  entryPath: string;
  originalPath: string | null;
  branch: string | null;
  at: string | null;
  bytes: number | null;
  registered: boolean;
  error: string | null;
}

export type ErrorKind = "missing" | "failed" | "timeout" | "bad_output" | "invalid";

/** What the Rust side returns when a residue run fails. */
export interface CliError {
  kind: ErrorKind;
  message: string;
  command: string | null;
  stderr: string | null;
  exit_code: number | null;
}

export interface AppInfo {
  app_version: string;
  residue_version: string;
  sidecar: string;
  home: string;
  path_source: "login_shell" | "fallback";
  dev_service_preview: boolean;
  /** `residue remove --keep-trash` when none is given: `7d`. */
  default_keep_trash: string;
}

export interface Settings {
  roots: string[];
  saved: boolean;
  suggested: string[];
}

/** One `residue watch` setting, as residue takes it: `1h`, `2d`, `10`. */
export interface WatchSetting {
  value: string;
  /** The service does not set it: residue's default. */
  default: boolean;
}

/** Whether `residue watch` fetches, so it sees a branch deleted on the remote. */
export type WatchFetch =
  | { mode: "every"; value: string; default: boolean }
  | { mode: "off" }
  /** The binary is older than `since`, so it never fetches. */
  | { mode: "never"; since: string; every: string }
  /** The binary's version, and so whether it fetches, is unknown. */
  | { mode: "unknown"; since: string; every: string };

export interface WatchSettings {
  grace: WatchSetting;
  idle: WatchSetting;
  min_free: WatchSetting;
  fetch: WatchFetch;
  keep_trash: WatchSetting;
}

export interface ServiceProgram {
  binary: string;
  binary_exists: boolean;
  version: string | null;
  managed: boolean;
  roots: string[];
  /** The options a save passes on, as written. */
  options: string[];
  dry_run: boolean;
  /** Arguments the app does not know; while there are any, it does not save. */
  unknown: string[];
  settings: WatchSettings;
}

/** What turning the service on, or saving other folders, installs. */
export interface ServicePlan {
  binary: string;
  version: string | null;
  managed: boolean;
  /** The running service's options, passed on as written. Empty when it is off. */
  keep: string[];
  /** What the service does after the save. */
  settings: WatchSettings;
  /** Why the app will not save, when it will not. */
  blocked: string | null;
}

export interface ServiceStatus {
  installed: boolean;
  state: string | null;
  pid: number | null;
  summary: string;
  agent_file: string | null;
  log_path: string | null;
  log_lines: string[];
  log_error: string | null;
  program: ServiceProgram | null;
  plan: ServicePlan;
  dev_dry_run: boolean;
}

export interface ServiceChange {
  command: string;
  output: string[];
  dev_dry_run: boolean;
  status: ServiceStatus;
}

/** `residue watch --once --dry-run`, which removes nothing. */
export interface ServicePreview {
  command: string;
  lines: string[];
  /** Options of the service the preview did not pass. */
  left_out: string[];
  left_out_reason: string | null;
}
