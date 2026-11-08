import { Fragment, useMemo, useRef, useState, type CSSProperties, type FocusEvent, type KeyboardEvent, type PointerEvent } from "react";
import { baseName, bytes, grouped, plural, someOf } from "../../lib/format";
import type { ScanDocument, Worktree } from "../../lib/types";
import {
  largestIgnored,
  layoutTreemap,
  MORE_ID,
  repoValue,
  scanTree,
  verdictTone,
  type Box,
  type Filter,
  type Leaf,
  type PlacedMore,
  type PlacedRepo,
  type RepoBranch,
  type Tone,
} from "../../lib/viz";
import { branchLabel, displayPath, summary, verdictLabel } from "../../lib/worktree";
import { Figure, Hatch, Legend, Mark, useSvgId } from "./Figure";
import { fitLabel, labelWidth, useWidth } from "./measure";
import { TipLines, tipTranslate, Tooltip } from "./Tooltip";
import { useTreeKeys, type TreeItem } from "./useTreeKeys";

type ChartNode =
  | { kind: "repo"; box: Box; repo: PlacedRepo }
  | { kind: "tile"; box: Box; repo: PlacedRepo; leaf: Leaf }
  | { kind: "more"; box: Box; more: PlacedMore };

/**
 * One step of a zoom: the repositories it shows, and the box it grows from.
 * The Worktrees screen keeps the steps, so a zoom outlasts a visit to a
 * worktree's checks.
 */
export interface ZoomLevel {
  ids: string[];
  from: Box;
}

/** What a zoom level is called in the breadcrumb. */
function levelName(repos: RepoBranch[]): string {
  return repos.length === 1 ? repos[0]!.name : plural(repos.length, "smaller repository", "smaller repositories");
}

/** The node to focus after leaving `level`: the one it was zoomed from. */
function zoomedFrom(level: ZoomLevel): string {
  return level.ids.length === 1 ? level.ids[0]! : MORE_ID;
}

/** The worktrees a tile or a group stands for: a fold holds several. */
function countOf(branch: RepoBranch): number {
  return branch.children.reduce((n, c) => n + (c.kind === "fold" ? c.worktrees.length : 1), 0);
}

/** The node an event happened in: the nearest element with `data-node`. */
function nodeOf(target: EventTarget): string | null {
  return target instanceof Element ? (target.closest("[data-node]")?.getAttribute("data-node") ?? null) : null;
}

/**
 * The scanned worktrees as a squarified treemap: one group per repository,
 * one tile per worktree, its area the bytes removing it gives back (residue's
 * `reclaimable_bytes`). Tiles too small to point at fold into "+N smaller".
 * Clicking a tile opens its checks; clicking a group zooms into it. The list
 * below the chart holds every value, so the chart is a `role="tree"` to move
 * through, not the only copy.
 */
export function Treemap({
  doc,
  filter,
  now,
  busy,
  zoom,
  onZoom,
  onOpen,
}: {
  doc: ScanDocument;
  filter: Filter;
  now: number;
  busy: boolean;
  zoom: ZoomLevel[];
  onZoom: (zoom: ZoomLevel[]) => void;
  onOpen: (path: string) => void;
}) {
  const tree = useMemo(() => scanTree(doc, filter), [doc, filter]);
  // The level a zoom in just added, which grows from its box; zooming out and
  // coming back from a worktree's checks draw a level in place.
  const [grown, setGrown] = useState<ZoomLevel | null>(null);
  // The levels whose repositories the filter still shows; the rest wait for it to change back.
  const levels = useMemo(() => {
    const out: (ZoomLevel & { repos: RepoBranch[] })[] = [];
    for (const level of zoom) {
      const repos = tree.repos.filter((r) => level.ids.includes(r.id));
      if (repos.length === 0) break;
      out.push({ ...level, repos });
    }
    return out;
  }, [zoom, tree]);
  const current = levels.at(-1);
  const shown = current?.repos ?? tree.repos;
  // One repository fills the chart: nothing in it zooms further.
  const single = shown.length === 1;
  const [plot, width] = useWidth<HTMLDivElement>();
  const height = Math.round(Math.min(360, Math.max(220, width * 0.36)));
  const layout = useMemo(() => (width > 0 ? layoutTreemap(shown, width, height) : null), [shown, width, height]);
  const hatch = useSvgId("tree-hatch");
  // The node under the pointer, and where the pointer came onto it. A move
  // within the node moves its tooltip through `tip`, not by drawing the chart
  // again: a render draws every tile.
  const [hover, setHover] = useState<{ id: string; x: number; y: number } | null>(null);
  const tip = useRef<HTMLDivElement>(null);
  const [focused, setFocused] = useState<string | null>(null);

  // The tree in reading order: each repository, then its tiles, largest first;
  // the folded repositories last.
  const { nodes, items } = useMemo(() => {
    const byId = new Map<string, ChartNode>();
    const order: TreeItem[] = [];
    for (const r of layout?.repos ?? []) {
      byId.set(r.branch.id, { kind: "repo", box: r.box, repo: r });
      order.push({ id: r.branch.id, parent: null, expanded: r.open });
      for (const l of r.leaves) {
        byId.set(l.leaf.id, { kind: "tile", box: l.box, repo: r, leaf: l.leaf });
        order.push({ id: l.leaf.id, parent: r.branch.id, expanded: null });
      }
    }
    const more = layout?.more;
    if (more) {
      byId.set(MORE_ID, { kind: "more", box: more.box, more });
      order.push({ id: MORE_ID, parent: null, expanded: false });
    }
    return { nodes: byId, items: order };
  }, [layout]);

  const go = (next: ZoomLevel[], focus: string, grows: ZoomLevel | null) => {
    onZoom(next);
    setGrown(grows);
    setHover(null);
    keys.focusAfterRender(focus);
  };
  const zoomIn = (id: string) => {
    const node = nodes.get(id);
    const base = zoom.slice(0, levels.length);
    if (node?.kind === "more") {
      const ids = node.more.branch.repos.map((r) => r.id);
      // The zoomed level reads largest first.
      const first = node.more.branch.repos.toSorted((a, b) => repoValue(b) - repoValue(a))[0]!;
      const level = { ids, from: node.box };
      go([...base, level], first.id, level);
    } else if (node && !single) {
      const level = { ids: [node.repo.branch.id], from: node.repo.box };
      go([...base, level], node.repo.branch.id, level);
    }
  };
  /** Back to `depth` levels deep, focusing the node the left level grew from. */
  const zoomTo = (depth: number) => {
    const left = levels[depth];
    if (left) go(zoom.slice(0, depth), zoomedFrom(left), null);
  };
  const zoomOut = () => zoomTo(levels.length - 1);
  const activate = (id: string) => {
    const node = nodes.get(id);
    if (node?.kind === "tile" && node.leaf.kind === "worktree") onOpen(node.leaf.worktree.path);
    else zoomIn(id);
  };
  const keys = useTreeKeys(items, { activate, expand: zoomIn, collapse: zoomOut });

  if (tree.repos.length === 0) {
    return (
      <p className="quiet">
        {tree.unmeasured > 0
          ? "residue could not measure these worktrees. The list below has each one."
          : "Removing these worktrees would free no space."}
      </p>
    );
  }

  const onKeyDown = (e: KeyboardEvent<SVGSVGElement>) => {
    if (e.key === "Escape" && current) {
      e.preventDefault();
      e.stopPropagation();
      zoomOut();
      return;
    }
    keys.onKeyDown(e);
  };
  const onPointerMove = (e: PointerEvent<SVGSVGElement>) => {
    const id = nodeOf(e.target);
    const box = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - box.left;
    const y = e.clientY - box.top;
    if (id !== null && id === hover?.id) {
      // React sets the style again only when a render changes `hover`, which
      // starts from where the pointer is then.
      if (tip.current) tip.current.style.translate = tipTranslate(x, y, width, height);
      return;
    }
    setHover(id ? { id, x, y } : null);
  };
  const onFocus = (e: FocusEvent<SVGSVGElement>) => {
    // Only keyboard focus shows a tooltip: a pointer has its own.
    setFocused(e.target.matches(":focus-visible") ? nodeOf(e.target) : null);
  };
  const onBlur = (e: FocusEvent<SVGSVGElement>) => {
    if (!(e.relatedTarget instanceof Node && e.currentTarget.contains(e.relatedTarget))) setFocused(null);
  };

  const legend = legendFor(shown, layout);
  const missing = current ? shown.reduce((n, r) => n + r.unmeasured, 0) : tree.unmeasured;
  const empty = current ? shown.reduce((n, r) => n + r.empty, 0) : tree.empty;
  const tipNode = hover ? nodes.get(hover.id) : focused ? nodes.get(focused) : undefined;
  const tipAt = hover ?? (tipNode ? { x: (tipNode.box.x0 + tipNode.box.x1) / 2, y: tipNode.box.y1 } : null);
  const tops = layout ? layout.repos.length + (layout.more ? 1 : 0) : 0;
  const from = current && zoom[levels.length - 1] === grown ? current.from : undefined;
  const zoomStyle =
    from && width > 0
      ? ({
          "--zoom-from": `translate(${from.x0}px, ${from.y0}px) scale(${(from.x1 - from.x0) / width}, ${(from.y1 - from.y0) / height})`,
        } as CSSProperties)
      : undefined;

  return (
    <Figure
      label="Reclaimable space by worktree"
      busy={busy}
      head={
        <>
          <nav className="viz-crumbs" aria-label="Chart zoom">
            {current ? (
              <>
                <button type="button" className="viz-crumb" onClick={() => zoomTo(0)} title="Zoom out">
                  All repositories
                </button>
                {levels.map((level, i) => (
                  <Fragment key={level.ids.join("\n")}>
                    <span aria-hidden>/</span>
                    {level === current ? (
                      <span aria-current="location">{levelName(level.repos)}</span>
                    ) : (
                      <button type="button" className="viz-crumb" onClick={() => zoomTo(i + 1)} title="Zoom out">
                        {levelName(level.repos)}
                      </button>
                    )}
                  </Fragment>
                ))}
              </>
            ) : (
              <span>All repositories</span>
            )}
          </nav>
          {legend.length > 1 ? <Legend items={legend} /> : null}
        </>
      }
      caption={
        <>
          Each tile is a worktree, sized by the space removing it frees. Click a tile for
          its checks{single ? "" : ", a repository to zoom in"}.{current ? " Escape zooms out." : ""}
          {missing > 0 ? ` ${notDrawn(missing, "residue could not measure")}` : ""}
          {empty > 0 ? ` ${notDrawn(empty, "with nothing to reclaim")}` : ""}
        </>
      }
    >
      <div className="viz-plot" ref={plot} onPointerLeave={() => setHover(null)}>
        {layout ? (
          <svg
            className="viz-tree"
            data-single={single ? "" : undefined}
            width={width}
            height={height}
            role="tree"
            tabIndex={-1}
            aria-label="Worktrees by reclaimable space"
            onKeyDown={onKeyDown}
            onClick={(e) => {
              const id = nodeOf(e.target);
              if (id) activate(id);
            }}
            onPointerMove={onPointerMove}
            onFocus={onFocus}
            onBlur={onBlur}
          >
            <defs>
              <Hatch id={hatch} />
            </defs>
            <g className="viz-zoom" key={levels.length} style={zoomStyle}>
              {layout.repos.map((r, i) => (
                // A flat tree: each repository, then its tiles as its children by `aria-level`.
                <Fragment key={r.branch.id}>
                  <Group
                    repo={r}
                    index={i}
                    size={tops}
                    zooms={!single}
                    hatch={hatch}
                    props={keys.itemProps(r.branch.id)}
                  />
                  {r.leaves.map((l, j) => (
                    <Tile
                      key={l.leaf.id}
                      leaf={l.leaf}
                      group={r.branch.name}
                      box={l.box}
                      index={j}
                      size={r.leaves.length}
                      zooms={!single}
                      hatch={hatch}
                      props={keys.itemProps(l.leaf.id)}
                    />
                  ))}
                </Fragment>
              ))}
              {layout.more ? (
                <More more={layout.more} index={layout.repos.length} size={tops} hatch={hatch} props={keys.itemProps(MORE_ID)} />
              ) : null}
            </g>
          </svg>
        ) : (
          <div style={{ blockSize: height }} />
        )}
        {tipNode && tipAt ? (
          <Tooltip ref={tip} x={tipAt.x} y={tipAt.y} width={width} height={height}>
            <Tip node={tipNode} now={now} zooms={!single} />
          </Tooltip>
        ) : null}
      </div>
    </Figure>
  );
}

function notDrawn(n: number, why: string): string {
  return `${plural(n, "worktree")} ${why} ${n === 1 ? "is" : "are"} not drawn; the list has ${n === 1 ? "it" : "them"}.`;
}

/** The tones the chart draws, in legend order, named by what they mean here. */
function legendFor(shown: RepoBranch[], layout: ReturnType<typeof layoutTreemap> | null): { tone: Tone; label: string }[] {
  const verdicts = new Set<string>();
  for (const r of shown) {
    for (const c of r.children) {
      if (c.kind === "worktree") verdicts.add(c.worktree.verdict);
      else for (const w of c.worktrees) verdicts.add(w.verdict);
    }
  }
  const out: { tone: Tone; label: string }[] = [];
  if (verdicts.has("candidate")) out.push({ tone: "bright", label: verdictLabel("candidate") });
  if (verdicts.has("keep")) out.push({ tone: "gray", label: verdictLabel("keep") });
  const unsure = [...verdicts].filter((v) => verdictTone(v) === "hatched").map(verdictLabel);
  if (unsure.length > 0) out.push({ tone: "hatched", label: unsure.join(" or ") });
  const folded = layout?.more || layout?.repos.some((r) => !r.open || r.leaves.some((l) => l.leaf.kind === "fold"));
  if (folded) out.push({ tone: "rest", label: "smaller ones, together" });
  return out;
}

type ItemProps = ReturnType<ReturnType<typeof useTreeKeys>["itemProps"]>;

/**
 * A worktree's name as its tile shows it: whole when it fits, else without
 * its repository's name in front ("fix-07" in tools), which is the part that
 * tells the tiles apart, cut to fit.
 */
function tileName(name: string, group: string, width: number): string | null {
  if (labelWidth(name) <= width) return name;
  const own = name.startsWith(`${group}-`) ? name.slice(group.length + 1) : "";
  return fitLabel(own || name, width);
}

/** The name of a block that stands for several, whole words only: "+3 repositories", else "+3". */
function countLabel(n: number, what: string, width: number): string | null {
  return [`+${n} ${what}`, `+${n}`].find((label) => labelWidth(label) <= width) ?? null;
}

function Group({
  repo,
  index,
  size,
  zooms,
  hatch,
  props,
}: {
  repo: PlacedRepo;
  index: number;
  size: number;
  zooms: boolean;
  hatch: string;
  props: ItemProps;
}) {
  const { x0, y0, x1, y1 } = repo.box;
  const w = x1 - x0;
  const h = y1 - y0;
  const id = repo.branch.id;
  const count = countOf(repo.branch);
  const total = bytes(repo.value);
  const name = repo.branch.name;
  // The name and its total on one line when both fit; else the name alone, cut to fit.
  const both = labelWidth(`${name}  ${total}`) <= w - 12;
  const title = both ? name : fitLabel(name, w - 12);
  return (
    <g
      role="treeitem"
      className="viz-node viz-repo"
      data-tone={repo.open ? undefined : "rest"}
      data-node={id}
      aria-level={1}
      aria-setsize={size}
      aria-posinset={index + 1}
      aria-expanded={repo.open}
      aria-label={`${name}: ${plural(count, "worktree")}, ${total} reclaimable${repo.open ? "" : ", too small to show them"}${zooms && !repo.open ? "; Enter zooms in" : ""}`}
      {...props}
    >
      {repo.open ? (
        <rect className="viz-frame" x={x0} y={y0} width={w} height={h} rx={4} />
      ) : (
        <Mark tone="rest" hatch={hatch} x={x0} y={y0} width={w} height={h} radius={4} />
      )}
      {title && h >= 18 ? (
        <text className="viz-label viz-repo-label" x={x0 + 6} y={y0 + 14}>
          {title}
        </text>
      ) : null}
      {both && h >= 18 ? (
        <text className="viz-label viz-repo-total num" x={x1 - 6} y={y0 + 14} textAnchor="end">
          {total}
        </text>
      ) : null}
      <rect className="viz-ring" x={x0 + 1} y={y0 + 1} width={Math.max(0, w - 2)} height={Math.max(0, h - 2)} rx={4} />
    </g>
  );
}

function Tile({
  leaf,
  group,
  box,
  index,
  size,
  zooms,
  hatch,
  props,
}: {
  leaf: Leaf;
  /** Its repository's name, which a cramped tile drops from its own. */
  group: string;
  box: Box;
  index: number;
  size: number;
  zooms: boolean;
  hatch: string;
  props: ItemProps;
}) {
  const { x0, y0, x1, y1 } = box;
  const w = x1 - x0;
  const h = y1 - y0;
  const tone: Tone = leaf.kind === "worktree" ? verdictTone(leaf.worktree.verdict) : "rest";
  const value = bytes(leaf.value);
  const name =
    leaf.kind === "worktree" ? tileName(leaf.name, group, w - 12) : countLabel(leaf.worktrees.length, "smaller", w - 12);
  const sized = name !== null && h >= 36 && labelWidth(value) <= w - 12;
  const label =
    leaf.kind === "worktree"
      ? `${leaf.name}: ${value} reclaimable, ${verdictLabel(leaf.worktree.verdict)}`
      : `${plural(leaf.worktrees.length, "smaller worktree")}: ${value} reclaimable${zooms ? "; Enter zooms in" : "; the list has each one"}`;
  return (
    <g
      role="treeitem"
      className="viz-node viz-tile"
      data-node={leaf.id}
      // Leaving the worktree's checks focuses this tile again (App).
      data-opener={leaf.kind === "worktree" ? `tile:${leaf.worktree.path}` : undefined}
      data-tone={tone}
      aria-level={2}
      aria-setsize={size}
      aria-posinset={index + 1}
      aria-label={label}
      {...props}
    >
      <Mark tone={tone} hatch={hatch} x={x0} y={y0} width={w} height={h} radius={3} />
      <Labels name={name} value={sized ? value : null} x={x0} y={y0} h={h} chip={tone === "hatched"} />
      <rect className="viz-ring" x={x0 + 1} y={y0 + 1} width={Math.max(0, w - 2)} height={Math.max(0, h - 2)} rx={3} />
    </g>
  );
}

/** The repositories too small to point at, as one block at the end of the level. */
function More({
  more,
  index,
  size,
  hatch,
  props,
}: {
  more: PlacedMore;
  index: number;
  size: number;
  hatch: string;
  props: ItemProps;
}) {
  const { x0, y0, x1, y1 } = more.box;
  const w = x1 - x0;
  const h = y1 - y0;
  const n = more.branch.repos.length;
  const value = bytes(more.branch.value);
  const name = countLabel(n, "repositories", w - 12);
  return (
    <g
      role="treeitem"
      className="viz-node viz-repo"
      data-tone="rest"
      data-node={MORE_ID}
      aria-level={1}
      aria-setsize={size}
      aria-posinset={index + 1}
      aria-expanded={false}
      aria-label={`${plural(n, "smaller repository", "smaller repositories")}: ${value} reclaimable; Enter zooms in`}
      {...props}
    >
      <Mark tone="rest" hatch={hatch} x={x0} y={y0} width={w} height={h} radius={4} />
      <Labels name={name} value={name !== null && h >= 36 && labelWidth(value) <= w - 12 ? value : null} x={x0} y={y0} h={h} />
      <rect className="viz-ring" x={x0 + 1} y={y0 + 1} width={Math.max(0, w - 2)} height={Math.max(0, h - 2)} rx={4} />
    </g>
  );
}

/**
 * A block's name and, under it when there is room, its value. On a hatched
 * block they sit on a solid chip of the gray fill, so no line of the hatch
 * crosses the text.
 */
function Labels({
  name,
  value,
  x,
  y,
  h,
  chip = false,
}: {
  name: string | null;
  value: string | null;
  x: number;
  y: number;
  h: number;
  chip?: boolean;
}) {
  const shown = name && h >= 20 ? name : null;
  const lines = [shown, value].filter((t): t is string => t !== null);
  return (
    <>
      {chip && lines.length > 0 ? (
        <rect
          className="viz-chip"
          x={x + 3}
          y={y + 4}
          width={Math.max(...lines.map(labelWidth)) + 6}
          height={value ? 30 : 15}
          rx={2}
        />
      ) : null}
      {shown ? (
        <text className="viz-label" x={x + 6} y={y + 15}>
          {shown}
        </text>
      ) : null}
      {value ? (
        <text className="viz-label viz-label-value num" x={x + 6} y={y + 29}>
          {value}
        </text>
      ) : null}
    </>
  );
}

/** What a node's tooltip says: the value first. */
function Tip({ node, now, zooms }: { node: ChartNode; now: number; zooms: boolean }) {
  if (node.kind === "more") {
    const repos = node.more.branch.repos;
    return (
      <TipLines
        value={`${bytes(node.more.branch.value)} reclaimable`}
        lines={[
          `${plural(repos.length, "smaller repository", "smaller repositories")}: ${someOf(repos.map((r) => r.name))}`,
          "Click to zoom in.",
        ]}
      />
    );
  }
  if (node.kind === "repo") {
    const r = node.repo;
    return (
      <TipLines
        value={`${bytes(r.value)} reclaimable`}
        lines={[
          <strong key="name">{r.branch.name}</strong>,
          plural(countOf(r.branch), "worktree"),
          zooms ? "Click to zoom in." : null,
        ].filter((l) => l !== null)}
      />
    );
  }
  const leaf = node.leaf;
  if (leaf.kind === "fold") {
    return (
      <TipLines
        value={`${bytes(leaf.value)} reclaimable`}
        lines={[
          `${plural(leaf.worktrees.length, "smaller worktree")}: ${someOf(leaf.worktrees.map((w) => baseName(displayPath(w))))}`,
          zooms ? "Click to zoom in." : "Too small to draw apart; the list has each one.",
        ]}
      />
    );
  }
  const w: Worktree = leaf.worktree;
  const largest = largestIgnored(w);
  return (
    <TipLines
      value={`${bytes(leaf.value)} reclaimable`}
      lines={[
        <>
          <strong>{leaf.name}</strong> {branchLabel(w) === leaf.name ? "" : branchLabel(w)}
        </>,
        `${verdictLabel(w.verdict)}: ${summary(w, now)}`,
        w.size ? `${bytes(w.size.allocated_bytes)} on disk, ${grouped(w.size.entries)} entries` : null,
        largest ? `largest ignored: ${largest.path}, ${bytes(largest.bytes)} apparent` : null,
      ].filter((l) => l !== null)}
    />
  );
}
