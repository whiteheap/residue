// What residue has freed, in residue's own words: `residue stats` and the
// line a `remove` run ends with (src/stats.rs). Display only; every number
// comes from residue's documents.

import { bytes, plural } from "./format";
import type { FreedBy, FreedSum, RemoveStats, StatsDocument } from "./types";

/** `residue stats`' name for each command, in its order. */
export const COMMANDS: { by: FreedBy; label: string }[] = [
  { by: "remove", label: "residue remove (by hand)" },
  { by: "sweep", label: "residue remove --all (sweeps)" },
  { by: "watch", label: "residue watch (automatic cleanup)" },
  { by: "other", label: "command not recorded" },
];

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** `2026-11-08`: the local date of `iso`, as residue prints a day. */
export function localDay(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso.slice(0, 10);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** `65.0 MB, 2 worktrees`. */
export function sumText(sum: FreedSum): string {
  return `${bytes(sum.bytes)}, ${plural(sum.worktrees, "worktree")}`;
}

/**
 * The line `residue remove` ends with (`Run::text`): what this run freed or
 * moved, and what residue has freed on this machine. `keep` is the run's
 * `--keep-trash`. Null when the run freed and moved nothing, as residue
 * prints none then.
 */
export function removalLine(stats: RemoveStats, keep: string): string | null {
  const parts: string[] = [];
  if (stats.freed_worktrees > 0) parts.push(`freed about ${bytes(stats.freed_bytes)}`);
  if (stats.trashed_worktrees > 0) {
    parts.push(
      `moved about ${bytes(stats.trashed_bytes)} to the trash, freed once a remove run purges it after ${keep}`,
    );
  }
  if (parts.length === 0) return null;
  const total =
    stats.total_freed_worktrees === null || stats.total_freed_bytes === null || stats.since === null
      ? "residue could not update its stats ledger; see the warning"
      : stats.total_freed_worktrees === 0
        ? "residue has freed nothing on this machine yet"
        : `residue has freed about ${bytes(stats.total_freed_bytes)} on this machine since ${localDay(stats.since)}`;
  return `${parts.join(" and ")} (${total})`;
}

/** What residue had freed when the app last read its stats. */
interface Mark {
  bytes: number;
  worktrees: number;
  /** When residue wrote that document. */
  at: string;
}

const MARK_KEY = "residue.freed.seen";

function readMark(): Mark | null {
  try {
    const raw = localStorage.getItem(MARK_KEY);
    if (raw === null) return null;
    const m: unknown = JSON.parse(raw);
    if (
      typeof m === "object" &&
      m !== null &&
      "bytes" in m &&
      "worktrees" in m &&
      "at" in m &&
      typeof m.bytes === "number" &&
      typeof m.worktrees === "number" &&
      typeof m.at === "string"
    ) {
      return { bytes: m.bytes, worktrees: m.worktrees, at: m.at };
    }
    return null;
  } catch {
    // Storage the web view refuses, or a value another version wrote: no
    // mark, so the window shows no "since the app was last open" figure.
    return null;
  }
}

/** The mark the last session left, read once, before this session writes its own. */
const lastOpen = readMark();

/** Keep what residue has freed now as the mark the next session compares with. */
export function rememberFreed(doc: StatsDocument): void {
  const now = doc.freed.all_time;
  try {
    localStorage.setItem(MARK_KEY, JSON.stringify({ bytes: now.bytes, worktrees: now.worktrees, at: doc.generated_at }));
  } catch {
    // Storage the web view refuses: the next session has no mark, and shows no figure.
  }
}

/**
 * What residue has freed since the last session's mark: null on a first
 * open, or when the total went down (another ledger, or one that was reset),
 * since then there is no difference to show.
 */
export function sinceLastOpen(doc: StatsDocument): (FreedSum & { since: string }) | null {
  const now = doc.freed.all_time;
  if (!lastOpen || now.worktrees < lastOpen.worktrees || now.bytes < lastOpen.bytes) return null;
  return { bytes: now.bytes - lastOpen.bytes, worktrees: now.worktrees - lastOpen.worktrees, since: lastOpen.at };
}
