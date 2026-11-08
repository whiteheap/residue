import type { Check, Verdict, Worktree } from "./types";
import { age, plural, shortBranch } from "./format";

export const VERDICT_LABEL: Record<Verdict, string> = {
  candidate: "can go",
  keep: "kept",
  unknown: "unknown",
  stale: "stale",
};

export const VERDICT_HELP: Record<Verdict, string> = {
  candidate: "Every check passed. residue checks it again before it moves it.",
  keep: "A check blocked, so residue keeps it.",
  unknown: "residue could not finish a check, so it keeps it.",
  stale: "Git lists it, but its directory is missing or does not match.",
};

/** The check names residue uses, in words. */
export const CHECK_LABEL: Record<string, string> = {
  identity: "Identity",
  locked: "Lock",
  activity: "In use",
  tracked_changes: "Tracked changes",
  untracked: "Untracked files",
  ignored: "Ignored files",
  index_flags: "Index flags",
  operation: "Git operation",
  private_refs: "Private refs",
  head: "HEAD",
  layout: "Layout",
  contents: "Contents",
  idle: "Idle",
};

/**
 * `map[key]` for a key from residue's document, which a newer residue can
 * send without this app knowing it. Undefined for such a key, and for a name
 * every object inherits (`constructor`), so a lookup never yields a value the
 * window cannot render.
 */
export function known<T>(map: Readonly<Record<string, T>>, key: string): T | undefined {
  return Object.hasOwn(map, key) ? map[key] : undefined;
}

export function checkLabel(id: string): string {
  return known(CHECK_LABEL, id) ?? id;
}

/** The verdict in words; one this app does not know shows as residue wrote it. */
export function verdictLabel(verdict: string): string {
  return known(VERDICT_LABEL, verdict) ?? verdict;
}

export function verdictHelp(verdict: string): string | undefined {
  return known(VERDICT_HELP, verdict);
}

/** The checks that keep it: residue's own words for each. */
export function keptBy(w: Worktree): Check[] {
  return w.checks.filter((c) => c.status !== "pass");
}

/**
 * The facts that never block, as `residue scan` prints them, without its
 * "branch kept": every removal keeps the branch, and the confirmation says so.
 */
export function facts(w: Worktree, nowSeconds: number): string[] {
  const out: string[] = [];
  if (w.local_only_commits_error) out.push("local commits unknown");
  else if (w.local_only_commits == null) out.push("no remote-tracking refs");
  else if (w.local_only_commits === 0) out.push("all commits on a remote");
  else out.push(`${plural(w.local_only_commits, "local-only commit")}`);
  if (w.upstream_gone) out.push("upstream gone");
  if (w.last_change) out.push(`changed ${age(nowSeconds - w.last_change.unix)}`);
  return out;
}

/** One line on why a worktree has its verdict. */
export function summary(w: Worktree, nowSeconds: number): string {
  if (w.verdict === "stale") return w.stale_reason ?? VERDICT_HELP.stale;
  const kept = keptBy(w);
  if (kept.length > 0) {
    const first = kept[0]!;
    const more = kept.length > 1 ? ` (+${kept.length - 1} more)` : "";
    const label = checkLabel(first.id);
    // "In use: in use by node[12]" reads twice; residue's words carry it alone.
    if (first.detail.toLowerCase().startsWith(label.toLowerCase())) {
      return `${first.detail.charAt(0).toUpperCase()}${first.detail.slice(1)}${more}`;
    }
    return `${label}: ${first.detail}${more}`;
  }
  return facts(w, nowSeconds).join(" · ");
}

export function displayPath(w: Worktree): string {
  return w.canonical_path ?? w.path;
}

export function branchLabel(w: Worktree): string {
  return shortBranch(w.branch) ?? (w.head ? `detached at ${w.head.slice(0, 7)}` : "detached");
}

export function sizeOf(w: Worktree): number | null {
  return w.size ? w.size.reclaimable_bytes : null;
}
