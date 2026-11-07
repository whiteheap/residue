// What the charts draw, from residue's documents: residue's numbers
// unchanged, plus layout. No React and no DOM, so each adapter reads as data
// in, data out. The charts never derive a size residue did not report: a
// worktree's area is its reclaimable bytes, never allocated bytes summed
// across worktrees (hard links would count twice), and an ignored entry's
// size is apparent bytes, so it is never scaled into reclaimable ones.

import { hierarchy, treemap, treemapSquarify, type HierarchyRectangularNode } from "d3-hierarchy";
import { baseName } from "./format";
import type { IgnoredEntry, Repository, Restorable, ScanDocument, Size, WatchSettings, Worktree } from "./types";
import { percent } from "./watch";
import { displayPath } from "./worktree";

export type Filter = "all" | "candidate" | "kept";

/** The Worktrees filter, which scopes the chart and the list alike. */
export function matches(w: Worktree, filter: Filter): boolean {
  if (filter === "all") return true;
  if (filter === "candidate") return w.verdict === "candidate";
  return w.verdict !== "candidate";
}

// ------------------------------------------------------------- treemap

/** A worktree's tile: its area is residue's `reclaimable_bytes`. */
export interface WorktreeLeaf {
  kind: "worktree";
  id: string;
  name: string;
  worktree: Worktree;
  value: number;
}

/** Tiles too small to read, drawn as one; zooming into the repository shows them. */
export interface FoldLeaf {
  kind: "fold";
  id: string;
  worktrees: Worktree[];
  value: number;
}

export type Leaf = WorktreeLeaf | FoldLeaf;

export interface RepoBranch {
  kind: "repo";
  id: string;
  name: string;
  repo: Repository;
  children: Leaf[];
  /** Its worktrees the filter keeps that are not drawn: unmeasured, or with nothing to reclaim. */
  unmeasured: number;
  empty: number;
}

export interface ScanTree {
  repos: RepoBranch[];
  /** Worktrees residue did not measure (`size: null`): never drawn, counted in the caption. */
  unmeasured: number;
  /** Measured worktrees with 0 reclaimable bytes, which have no area. */
  empty: number;
}

/** Repository, then worktree, for the worktrees the filter keeps. */
export function scanTree(doc: ScanDocument, filter: Filter): ScanTree {
  let unmeasured = 0;
  let empty = 0;
  const repos: RepoBranch[] = [];
  for (const repo of doc.repositories) {
    const children: Leaf[] = [];
    let missing = 0;
    let none = 0;
    for (const w of repo.worktrees) {
      if (!matches(w, filter)) continue;
      if (!w.size) {
        missing += 1;
      } else if (w.size.reclaimable_bytes <= 0) {
        none += 1;
      } else {
        children.push({
          kind: "worktree",
          id: `w:${w.path}`,
          name: baseName(displayPath(w)),
          worktree: w,
          value: w.size.reclaimable_bytes,
        });
      }
    }
    unmeasured += missing;
    empty += none;
    if (children.length > 0) {
      repos.push({
        kind: "repo",
        id: `r:${repo.common_dir}`,
        name: baseName(repo.main_path),
        repo,
        children,
        unmeasured: missing,
        empty: none,
      });
    }
  }
  return { repos, unmeasured, empty };
}

export interface Box {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

export interface PlacedLeaf {
  leaf: Leaf;
  box: Box;
}

export interface PlacedRepo {
  /** The repository with its small tiles folded. */
  branch: RepoBranch;
  box: Box;
  /** The reclaimable bytes it holds. */
  value: number;
  /** Whether its tiles are drawn: a group too small for them is drawn as one block. */
  open: boolean;
  /** In size order. */
  leaves: PlacedLeaf[];
}

/** Repositories too small to point at, drawn as one block; zooming into it shows them. */
export interface MoreBranch {
  kind: "more";
  id: typeof MORE_ID;
  repos: RepoBranch[];
  value: number;
}

export interface PlacedMore {
  branch: MoreBranch;
  box: Box;
}

export interface TreemapLayout {
  repos: PlacedRepo[];
  /** The repositories folded into "+N repositories", drawn after the others. */
  more: PlacedMore | null;
  total: number;
}

/** The band at the top of a repository's group, which holds its name. */
export const GROUP_HEADER = 20;
/** A tile smaller than this folds into "+N smaller": a pointer target is at least 24 px. */
export const MIN_TILE = { w: 32, h: 24 };
/**
 * A repository too small for its tiles shows only its name; smaller than this,
 * it cannot show that either (five characters of the 11 px mono label inside
 * 6 px insets), so it folds into "+N repositories".
 */
export const MIN_GROUP = { w: 48, h: 24 };
/** The one "+N repositories" block a level has. */
export const MORE_ID = "m:repositories";

type Top = RepoBranch | MoreBranch;
type Datum = { kind: "root"; children: Top[] } | Top | Leaf;

const largestFirst = (a: { value: number }, b: { value: number }) => b.value - a.value;

/** The reclaimable bytes a repository's tiles hold. */
export function repoValue(r: RepoBranch): number {
  return r.children.reduce((sum, c) => sum + c.value, 0);
}

/** Repositories and their tiles, largest first, the order the squarified layout wants. */
function bySize(tops: Top[]): Top[] {
  return tops
    .map((t) =>
      t.kind === "more"
        ? { t, value: t.value }
        : { t: { ...t, children: t.children.toSorted(largestFirst) }, value: repoValue(t) },
    )
    .toSorted(largestFirst)
    .map(({ t }) => t);
}

function place(tops: Top[], width: number, height: number): HierarchyRectangularNode<Datum> {
  const root = hierarchy<Datum>({ kind: "root", children: bySize(tops) }, (d) =>
    d.kind === "root" || d.kind === "repo" ? d.children : undefined,
  ).sum((d) => (d.kind === "worktree" || d.kind === "fold" || d.kind === "more" ? d.value : 0));
  return treemap<Datum>()
    .tile(treemapSquarify)
    .size([width, height])
    // A 2 px gap between tiles, inside a 2 px frame under the repository's
    // name; 4 px between repositories.
    .paddingInner((n) => (n.depth === 0 ? 4 : 2))
    .paddingTop((n) => (n.depth === 1 ? GROUP_HEADER : 0))
    .paddingRight((n) => (n.depth === 1 ? 2 : 0))
    .paddingBottom((n) => (n.depth === 1 ? 2 : 0))
    .paddingLeft((n) => (n.depth === 1 ? 2 : 0))
    .round(true)(root);
}

function boxOf(n: HierarchyRectangularNode<Datum>): Box {
  return { x0: n.x0, y0: n.y0, x1: n.x1, y1: n.y1 };
}

function fits(n: HierarchyRectangularNode<Datum>, w: number, h: number): boolean {
  return n.x1 - n.x0 >= w && n.y1 - n.y0 >= h;
}

/** Whether a repository's group has room for one tile inside its name band and frame. */
function opens(g: HierarchyRectangularNode<Datum>): boolean {
  return fits(g, MIN_TILE.w + 4, MIN_TILE.h + GROUP_HEADER + 2);
}

/**
 * The repository again, with its tiles smaller than `MIN_TILE` folded into
 * its one "+N smaller" tile; null when nothing folds. A lone small tile stays
 * itself unless a fold is there to join. A group too small to show tiles
 * keeps them: it is drawn as one block.
 */
function foldTiles(g: HierarchyRectangularNode<Datum>): RepoBranch | null {
  const branch = g.data as RepoBranch;
  if (!opens(g)) return null;
  const small: WorktreeLeaf[] = [];
  for (const c of g.children ?? []) {
    if (c.data.kind === "worktree" && !fits(c, MIN_TILE.w, MIN_TILE.h)) small.push(c.data);
  }
  const before = branch.children.find((c): c is FoldLeaf => c.kind === "fold");
  if (small.length === 0 || (small.length === 1 && !before)) return null;
  const out = new Set(small.map((s) => s.id));
  const fold: FoldLeaf = {
    kind: "fold",
    id: `f:${branch.repo.common_dir}`,
    worktrees: [...(before?.worktrees ?? []), ...small.map((s) => s.worktree)],
    value: (before?.value ?? 0) + small.reduce((sum, s) => sum + s.value, 0),
  };
  return { ...branch, children: [...branch.children.filter((c) => c.kind === "worktree" && !out.has(c.id)), fold] };
}

/**
 * The level again, small tiles folded in each repository and the
 * repositories too small to show tiles or a name (`MIN_GROUP`) folded into
 * one "+N repositories" block; null when nothing folds. As with tiles, a lone small repository
 * stays itself unless the block is there to join. A block too small for its
 * own name takes in the smallest repository that shows only a name, so every
 * block can say what it is.
 */
function refold(root: HierarchyRectangularNode<Datum>): Top[] | null {
  let changed = false;
  const out: Top[] = [];
  const tiny: RepoBranch[] = [];
  let more: MoreBranch | undefined;
  let moreNameless = false;
  for (const g of root.children ?? []) {
    const d = g.data as Top;
    if (d.kind === "more") {
      more = d;
      moreNameless = !fits(g, MIN_GROUP.w, MIN_GROUP.h);
    } else if (!opens(g) && !fits(g, MIN_GROUP.w, MIN_GROUP.h)) tiny.push(d);
    else {
      const folded = foldTiles(g);
      if (folded) changed = true;
      out.push(folded ?? d);
    }
  }
  if (more && moreNameless && tiny.length === 0) {
    // Children keep the largest-first order `place` gave them.
    const named = (root.children ?? []).findLast((g) => g.data.kind === "repo" && !opens(g));
    if (named) {
      tiny.push(named.data as RepoBranch);
      out.splice(out.indexOf(named.data as Top), 1);
    }
  }
  if (tiny.length > 1 || (tiny.length === 1 && more)) {
    changed = true;
    const repos = [...(more?.repos ?? []), ...tiny];
    out.push({ kind: "more", id: MORE_ID, repos, value: repos.reduce((sum, r) => sum + repoValue(r), 0) });
  } else {
    out.push(...tiny);
    if (more) out.push(more);
  }
  return changed ? out : null;
}

/** A squarified treemap of `repos` in `width` x `height`, small tiles and repositories folded. */
export function layoutTreemap(repos: RepoBranch[], width: number, height: number): TreemapLayout {
  let root = place(repos, width, height);
  // Folding reshapes the other tiles, so one can turn small on the next
  // layout. At most three passes: a tile still small after them stays small.
  for (let pass = 0; pass < 3; pass += 1) {
    const next = refold(root);
    if (!next) break;
    root = place(next, width, height);
  }
  const placed: PlacedRepo[] = [];
  let more: PlacedMore | null = null;
  for (const g of root.children ?? []) {
    const d = g.data as Top;
    if (d.kind === "more") {
      more = { branch: d, box: boxOf(g) };
      continue;
    }
    const open = opens(g);
    placed.push({
      branch: d,
      box: boxOf(g),
      value: g.value ?? 0,
      open,
      leaves: open ? (g.children ?? []).map((c) => ({ leaf: c.data as Leaf, box: boxOf(c) })) : [],
    });
  }
  return { total: root.value ?? 0, repos: placed, more };
}

/** The largest ignored entry residue measured, for a tile's tooltip. */
export function largestIgnored(w: Worktree): (IgnoredEntry & { bytes: number }) | null {
  let best: (IgnoredEntry & { bytes: number }) | null = null;
  for (const i of w.ignored) {
    if (i.bytes != null && (!best || i.bytes > best.bytes)) best = { ...i, bytes: i.bytes };
  }
  return best;
}

// --------------------------------------------------------- composition

/**
 * Emphasis, the charts' one visual variable: the app has no color accent.
 * `bright` is what the chart is about (a worktree that can go; an ignored
 * entry that keeps its worktree), `gray` the rest of the data, `hatched` a
 * value residue could not settle, `rest` an aggregate (folded tiles,
 * everything else).
 */
export type Tone = "bright" | "gray" | "hatched" | "rest";

/** A worktree's tile: bright when it can go, gray when kept, hatched when residue could not decide. */
export function verdictTone(verdict: string): Tone {
  if (verdict === "candidate") return "bright";
  if (verdict === "keep") return "gray";
  return "hatched";
}

export interface Part {
  key: string;
  label: string;
  bytes: number;
  tone: Tone;
  /** What the tone means here, for the tooltip. */
  note: string;
}

export interface Composition {
  /** The whole the bar shows: apparent bytes. */
  total: number;
  parts: Part[];
}

const TOP_PARTS = 4;
const MIN_SHARE = 0.03;

/**
 * A worktree's apparent bytes, split into its own ignored entries (up to 4:
 * first those that are not regenerable, however small, since one keeps the
 * worktree; then the largest at 3% or more), the other ignored entries, and
 * everything else:
 * apparent bytes minus the ignored entries residue sized. Only residue's
 * numbers: it does not split tracked from untracked files, so neither does
 * this. Null when residue sized no ignored entry, which leaves one part.
 */
export function composition(size: Size, ignored: IgnoredEntry[]): Composition | null {
  const apparent = size.apparent_bytes;
  const sized = ignored
    .filter((i): i is IgnoredEntry & { bytes: number } => i.bytes != null && i.bytes > 0)
    .toSorted((a, b) => b.bytes - a.bytes);
  if (apparent <= 0 || sized.length === 0) return null;
  const unsized = ignored.filter((i) => i.bytes == null).length;
  const top = sized
    .filter((i) => !i.regenerable || i.bytes / apparent >= MIN_SHARE)
    .toSorted((a, b) => Number(a.regenerable) - Number(b.regenerable) || b.bytes - a.bytes)
    .slice(0, TOP_PARTS)
    .toSorted((a, b) => b.bytes - a.bytes);
  const rest = sized.filter((i) => !top.includes(i));
  const parts: Part[] = top.map((i) => ({
    key: `i:${i.path}`,
    label: i.path,
    bytes: i.bytes,
    tone: i.regenerable ? "gray" : "bright",
    note: i.regenerable ? "ignored, regenerable" : "ignored, not regenerable: keeps the worktree",
  }));
  if (rest.length > 0) {
    const regenerable = rest.every((i) => i.regenerable);
    parts.push({
      key: "other",
      label: rest.length === 1 ? "1 other ignored entry" : `${rest.length} other ignored entries`,
      bytes: rest.reduce((sum, i) => sum + i.bytes, 0),
      tone: regenerable ? "gray" : "bright",
      note: regenerable ? "ignored, regenerable" : "ignored, at least one not regenerable",
    });
  }
  const ignoredBytes = sized.reduce((sum, i) => sum + i.bytes, 0);
  const remainder = apparent - ignoredBytes;
  if (remainder > 0) {
    parts.push({
      key: "else",
      label: "everything else",
      bytes: remainder,
      tone: "rest",
      note:
        unsized > 0
          ? `tracked and untracked files, and ${unsized === 1 ? "1 ignored entry" : `${unsized} ignored entries`} residue did not size`
          : "tracked and untracked files",
    });
  }
  return { total: Math.max(apparent, ignoredBytes), parts };
}

/**
 * `46%`, `<1%`, `>99%`: a share as a reader needs it. Rounding never shows
 * "0%" for a part that holds bytes, nor "100%" for one that is not the whole.
 */
export function shareText(part: number, whole: number): string {
  if (whole <= 0) return "?";
  const p = (part / whole) * 100;
  if (p > 0 && p < 1) return "<1%";
  if (p > 99 && p < 100) return ">99%";
  return `${Math.round(p)}%`;
}

// ------------------------------------------------------------ restore

export interface TrashAge {
  seconds: number;
  keep: number;
  /** Age over the keep window, at most 1. */
  fraction: number;
  /** Older than the keep window: the next cleanup in its repository deletes it, leaving a removal record. */
  eligible: boolean;
}

/** How far a trash entry is through the keep window; null when residue gave no time. */
export function trashAge(at: string | null, nowSeconds: number | null, keepSeconds: number | null): TrashAge | null {
  if (!at || nowSeconds == null || keepSeconds == null || keepSeconds <= 0) return null;
  const t = Date.parse(at);
  if (Number.isNaN(t)) return null;
  const seconds = Math.max(0, nowSeconds - t / 1000);
  return { seconds, keep: keepSeconds, fraction: Math.min(1, seconds / keepSeconds), eligible: seconds > keepSeconds };
}

/** The trash entries, and the reclaimable bytes residue measured for them. */
export function trashTotals(items: Restorable[]): { entries: number; bytes: number; unmeasured: number } {
  const trash = items.filter((i) => i.kind === "trash");
  return {
    entries: trash.length,
    bytes: trash.reduce((sum, i) => sum + (i.bytes ?? 0), 0),
    unmeasured: trash.filter((i) => i.bytes == null).length,
  };
}

// --------------------------------------------------------------- disks

export interface DiskMeter {
  key: string;
  /**
   * How repositories were matched to the disk: residue's device number, or,
   * from a residue that does not send one, equal totals, which can join two
   * disks of the same size.
   */
  by: "device" | "totals";
  repos: Repository[];
  free: number;
  total: number;
  freePercent: number;
}

export interface CleanupLevels {
  /** watch removes idle worktrees on a disk below this percent free. */
  start: number;
  /** ...until this percent is free: `--min-free` and 5 points. */
  until: number;
}

/** One meter per disk the scanned repositories are on, in scan order. */
export function diskMeters(doc: ScanDocument): { disks: DiskMeter[]; unread: number } {
  const byKey = new Map<string, DiskMeter>();
  let unread = 0;
  for (const repo of doc.repositories) {
    const s = repo.free_space;
    if (!s || s.total_bytes <= 0) {
      unread += 1;
      continue;
    }
    const key = s.device != null ? `device:${s.device}` : `totals:${s.total_bytes}:${s.total_inodes}`;
    const disk = byKey.get(key);
    if (disk) {
      disk.repos.push(repo);
    } else {
      byKey.set(key, {
        key,
        by: s.device != null ? "device" : "totals",
        repos: [repo],
        free: s.free_bytes,
        total: s.total_bytes,
        freePercent: (s.free_bytes * 100) / s.total_bytes,
      });
    }
  }
  return { disks: [...byKey.values()], unread };
}

/** Where watch starts and stops removing for free space; null when `--min-free` is not a number. */
export function cleanupLevels(settings: WatchSettings): CleanupLevels | null {
  const p = percent(settings.min_free.value);
  return p === null ? null : { start: p, until: p + 5 };
}
