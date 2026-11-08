// The residue CLI's JSON documents, as the CLI in this repository prints
// them. These are the contract between the CLI and the window: the window
// shows them and never decides on its own what is safe to remove.

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
  /**
   * The filesystem's device number as a decimal string (#45). Repositories
   * with the same one share a disk, within one scan only. An older residue
   * does not send it.
   */
  device?: string;
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

/** An ignored entry a deletion took, which restore does not bring back. */
export interface NotKept {
  path: string;
  files: number | null;
  bytes: number | null;
}

/**
 * A removal record: what residue keeps of a worktree it deleted, so
 * `residue restore` can recreate it on its branch and HEAD.
 */
export interface RemovalRecord {
  kind: "removed";
  entry: string;
  entry_path: string;
  ref: string;
  /**
   * `removed`: the worktree is gone, and restore recreates it. `interrupted`:
   * the removal did not finish, and Git still lists the worktree. `back`: Git
   * lists the worktree at `original_path` again, at the recorded HEAD or
   * ahead of it; restore puts back what it lacks of the record (its mark,
   * config.worktree and HEAD reflog). `unknown`: a worktree is there on its
   * branch, but residue could not compare its HEAD (`back_error`), so restore
   * offers only `--to`. `incomplete` and `unreadable`: residue cannot use the
   * record (`incomplete`, `error`). A newer residue may add a state.
   */
  state: "removed" | "interrupted" | "back" | "unknown" | "incomplete" | "unreadable";
  original_path: string | null;
  aside_path: string | null;
  branch: string | null;
  head: string | null;
  upstream: string | null;
  /** `remove`, `sweep`, `watch_landed`, `watch_low_disk`, `trash_purge`; a newer residue may add one. */
  reason: string | null;
  /** The command that removed it: `remove`, `sweep`, `watch`. */
  by: string | null;
  removed_at: string | null;
  /**
   * The `--keep-removed` the record was written with (`30d`), and in
   * seconds: no run forgets it before `removed_at` plus that. Null for a
   * record written before residue kept it.
   */
  keep: string | null;
  keep_seconds: number | null;
  allocated_bytes: number | null;
  reclaimable_bytes: number | null;
  not_kept: NotKept[];
  /** No `info` file: a run stopped while it wrote the record. */
  incomplete: boolean;
  /** Why residue cannot tell whether the worktree is back (`state` `unknown`); null otherwise. */
  back_error: string | null;
  residue_version: string | null;
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

/** In a plan (no `--yes`), a record the run would forget is `skipped`. */
export type ForgetResult = "forgotten" | "skipped" | "failed" | "not_attempted";

export interface RemoveDocument {
  schema: "residue.remove/v1";
  version: string;
  generated_at: string;
  activity: ActivityScope;
  trash: { enabled: boolean; keep: string; keep_seconds: number };
  /** The records this run writes, and `--keep-removed`: how long records are kept. */
  removal_records: { reason: string; by: string; keep: string; keep_seconds: number };
  results: {
    path: string;
    result: RemoveResult;
    reason: string | null;
    trash_entry: string | null;
    removal_record: string | null;
    worktree: Worktree | null;
  }[];
  purged: {
    result: PurgeResult;
    reason: string | null;
    entry: TrashItem;
    removal_record: string | null;
    worktree: Worktree | null;
  }[];
  /** Records older than `--keep-removed` in the repositories the run covers. */
  forgotten: {
    result: ForgetResult;
    reason: string | null;
    entry: RemovalRecord;
  }[];
  errors: string[];
  /** Records residue could not read or forget, or a ledger it could not write; never a reason the run failed. */
  warnings: string[];
  /** What this run freed and moved, and residue's total from its stats ledger (#38). */
  stats: RemoveStats;
}

/** `remove --json` `stats`. The totals are null when residue could not update its ledger (`error`). */
export interface RemoveStats {
  freed_bytes: number;
  freed_worktrees: number;
  trashed_bytes: number;
  trashed_worktrees: number;
  total_freed_bytes: number | null;
  total_freed_worktrees: number | null;
  since: string | null;
  ledger: string | null;
  error: string | null;
}

/** Known sizes only: a ledger line without one counts as a worktree and 0 bytes. */
export interface FreedSum {
  bytes: number;
  worktrees: number;
}

/** The commands residue splits what it freed by; `other` is a record that names none. */
export type FreedBy = "remove" | "sweep" | "watch" | "other";

export interface Freed extends FreedSum {
  by: Record<FreedBy, FreedSum>;
}

/** `residue stats --json`: what residue has freed on this machine, from its ledger (#38). */
export interface StatsDocument {
  schema: "residue.stats/v1";
  version: string;
  generated_at: string;
  ledger: string;
  /** The ledger's first line; null when it is empty. */
  since: string | null;
  /** residue's sentence on how sizes are measured, to show where totals appear. */
  estimate_note: string;
  freed: { all_time: Freed; last_30_days: Freed; last_7_days: Freed };
  /** Twelve weeks, oldest first; the last one ends at `generated_at`. */
  weeks: (Freed & { start: string; end: string })[];
  repositories: (Freed & { path: string; common_dir: string; last_freed_at: string })[];
  /** Moved to the trash: not freed until a purge deletes it. */
  trashed: FreedSum;
  trash_now: { entries: number; bytes: number; repositories: number };
  restored: { worktrees: number; from_trash: number; from_removal_records: number };
  last_freed_at: Record<"all" | FreedBy, string | null>;
  skipped_lines: number;
  errors: string[];
}

export interface Removal {
  document: RemoveDocument;
  exit_code: number | null;
}

/** How a restored worktree came back. */
export type Checkout =
  | "moved_back"
  | "checked_out_branch"
  | "recreated_branch"
  | "new_branch"
  | "detached"
  | "already_back";

export interface RestoreDocument {
  schema: "residue.restore/v1";
  version: string;
  generated_at: string;
  restored: string | null;
  /** After a restore: how it came back. */
  restore: {
    kind: "trash" | "removed";
    entry: string;
    path: string;
    branch: string | null;
    head: string | null;
    checkout: Checkout;
    moved_from: string | null;
    new_branch: string | null;
    branch_tip: string | null;
    not_kept: NotKept[];
    notes: string[];
  } | null;
  trash: TrashItem[];
  removed: RemovalRecord[];
  errors: string[];
}

/** `residue restore --json`, run in one scan folder. */
export interface Listing {
  root: string;
  document: RestoreDocument | null;
  error: CliError | null;
}

/**
 * Something the Restore screen can bring back: a trash entry, whose files
 * are still on disk, or a removal record of a worktree residue deleted.
 */
export type Restorable = {
  /** The scan folder whose listing returned it; restore runs there. */
  root: string;
  name: string;
  entryPath: string;
  originalPath: string | null;
  branch: string | null;
  at: string | null;
  error: string | null;
} & (
  | { kind: "trash"; bytes: number | null; registered: boolean }
  | {
      kind: "removed";
      state: RemovalRecord["state"];
      reason: string | null;
      /** What the deletion freed (`allocated_bytes`). */
      freed: number | null;
      notKept: NotKept[];
      asidePath: string | null;
      incomplete: boolean;
      backError: string | null;
      /** The `--keep-removed` it was written with, as `RemovalRecord` has it. */
      keep: string | null;
      keepSeconds: number | null;
    }
);

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
  /** `residue remove --keep-removed` when none is given: `30d`. */
  default_keep_removed: string;
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
  /** How long the removal records of its repositories are kept: `--keep-removed`, or residue's `30d`. */
  keep_removed: WatchSetting;
}

export interface ServiceProgram {
  binary: string;
  binary_exists: boolean;
  version: string | null;
  managed: boolean;
  roots: string[];
  /** The options a save passes on, as written, including those the app does not know. */
  options: string[];
  /** The options among `options` the app does not know, with their values: a save passes them on. */
  other_options: string[];
  dry_run: boolean;
  /** Arguments that are neither options nor folders; while there are any, the app does not save. */
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

/**
 * What a save is planned from: everything the save dialog shows about what a
 * save installs and the service it replaces. The dialog sends it back with
 * the save, and a fresh read that differs in any of it installs nothing.
 */
export interface ServiceBasis {
  installed: boolean;
  /** The binary a save installs with, and its `residue --version`. */
  binary: string;
  version: string | null;
  /** The options a save passes on. */
  keep: string[];
  /** The service a save replaces, when launchd runs one and its agent file can be read. */
  running: {
    binary: string;
    version: string | null;
    roots: string[];
    dry_run: boolean;
  } | null;
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
  basis: ServiceBasis;
  dev_dry_run: boolean;
}

export interface ServiceChange {
  command: string;
  output: string[];
  dev_dry_run: boolean;
  status: ServiceStatus;
}

/**
 * A save: installed (in the development preview, only shown), or refused
 * because a fresh read differs from the basis the dialog showed, with one line
 * per change (`what: before → after`) and the service as it is now.
 */
export type ServiceSave =
  | { outcome: "saved"; change: ServiceChange }
  | { outcome: "changed"; changes: string[]; status: ServiceStatus };

/**
 * `residue watch --once --dry-run` with every option a save passes on, run by
 * the binary a save installs with. It removes nothing.
 */
export interface ServicePreview {
  command: string;
  lines: string[];
  /** Whether it fetched first, as the service does. */
  fetch: WatchFetch;
}
