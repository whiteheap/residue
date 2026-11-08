import { Button, Checkbox, Empty, Tabs } from "@cloudflare/kumo";
import { ArrowClockwise, ArrowsClockwise, CaretRight, FolderPlus, Trash } from "@phosphor-icons/react";
import { useId, useMemo } from "react";
import { Treemap, type ZoomLevel } from "../components/viz/Treemap";
import {
  ErrorPanel,
  Loading,
  PageHeader,
  PermissionHint,
  SolidButton,
  VerdictTag,
  Words,
  isPermissionError,
} from "../components/ui";
import { baseName, bytes, inside, plural, tilde } from "../lib/format";
import { localTime } from "../lib/freed";
import type { CliError, FreedSum, Repository, ScanDocument, Worktree } from "../lib/types";
import { documentTime } from "../lib/useCli";
import { matches, type Filter } from "../lib/viz";
import { branchLabel, displayPath, sizeOf, summary } from "../lib/worktree";

/** One filter scopes the chart and the list alike. */
const FILTERS = [
  { value: "all", label: "All" },
  { value: "candidate", label: "Can go" },
  { value: "kept", label: "Kept" },
];

export function Worktrees({
  doc,
  error,
  scanning,
  roots,
  home,
  filter,
  onFilter,
  zoom,
  onZoom,
  selected,
  onSelect,
  onRescan,
  onOpen,
  onRemove,
  onChooseFolders,
  freed,
  cleaned,
  onFreed,
}: {
  doc: ScanDocument | undefined;
  error: CliError | undefined;
  scanning: boolean;
  roots: string[];
  home: string | undefined;
  /** The filter and the chart's zoom live in the app, so both outlast a visit to a worktree's checks. */
  filter: Filter;
  onFilter: (filter: Filter) => void;
  zoom: ZoomLevel[];
  onZoom: (zoom: ZoomLevel[]) => void;
  selected: Set<string>;
  onSelect: (paths: string[], on: boolean) => void;
  onRescan: () => void;
  onOpen: (path: string) => void;
  onRemove: (paths: string[]) => void;
  onChooseFolders: () => void;
  /** What residue freed in the last 7 days and its estimate note; "unavailable" when its stats could not be read, null while they are read. */
  freed: { week: FreedSum; note: string } | "unavailable" | null;
  /** What automatic cleanup freed since the app was last open (`cleanedSinceLastOpen`), with the estimate note; null when there is nothing to say. */
  cleaned: (FreedSum & { since: string; note: string }) | null;
  onFreed: () => void;
}) {
  const now = documentTime(doc?.generated_at) ?? 0;

  const repos = useMemo(() => {
    if (!doc) return [];
    return doc.repositories
      .map((repo) => ({ repo, worktrees: repo.worktrees.filter((w) => matches(w, filter)) }))
      .filter((r) => r.worktrees.length > 0);
  }, [doc, filter]);

  const candidates = useMemo(
    () =>
      new Map(
        (doc?.repositories ?? [])
          .flatMap((r) => r.worktrees)
          .filter((w) => w.verdict === "candidate")
          .map((w) => [w.path, w] as const),
      ),
    [doc],
  );
  const chosen = [...selected].filter((p) => candidates.has(p));
  const chosenBytes = chosen.reduce((sum, p) => sum + (sizeOf(candidates.get(p)!) ?? 0), 0);

  const header = (
    <PageHeader
      title="Worktrees"
      description={
        roots.length > 0
          ? `Linked Git worktrees under ${roots.map((r) => tilde(r, home)).join(", ")}`
          : "Choose the folders that hold your code."
      }
      actions={
        <Button
          variant="secondary"
          icon={<ArrowClockwise />}
          loading={scanning}
          disabled={scanning || roots.length === 0}
          onClick={onRescan}
          title="Scan again (Command-R)"
        >
          Scan again
        </Button>
      }
    />
  );

  if (roots.length === 0) {
    return (
      <>
        {header}
        <div className="page-body">
          <SinceOpen cleaned={cleaned} onFreed={onFreed} />
          <Empty
            icon={<FolderPlus size={32} />}
            title="No folders to scan"
            description="residue scans each folder you choose and every folder inside it."
            contents={
              <SolidButton icon={<FolderPlus />} onClick={onChooseFolders}>
                Choose folders
              </SolidButton>
            }
          />
        </div>
      </>
    );
  }

  return (
    <>
      {header}
      <div className="page-body" aria-busy={scanning}>
        <SinceOpen cleaned={cleaned} onFreed={onFreed} />
        {error ? (
          <ErrorPanel
            title="The scan did not finish"
            error={error}
            action={
              <>
                <Button variant="secondary" icon={<ArrowClockwise />} onClick={onRescan}>
                  Try again
                </Button>
                <Button variant="ghost" icon={<FolderPlus />} onClick={onChooseFolders}>
                  Change folders
                </Button>
              </>
            }
          />
        ) : null}

        {scanning && !doc ? (
          <Loading
            title={`Scanning ${roots.map((r) => tilde(r, home)).join(", ")}`}
            detail="A large folder can take a minute."
          />
        ) : null}

        {doc ? (
          <>
            <Summary doc={doc} scanning={scanning} freed={freed} onFreed={onFreed} />
            {doc.errors.length > 0 ? <ScanErrors errors={doc.errors} /> : null}
            {doc.summary.linked_worktrees === 0 ? (
              <Empty
                title="No linked worktrees"
                description="These folders hold only each repository's main checkout."
              />
            ) : (
              <>
                <div className="toolbar">
                  <Tabs
                    variant="segmented"
                    size="sm"
                    tabs={FILTERS}
                    value={filter}
                    onValueChange={(v) => onFilter(v as Filter)}
                  />
                </div>
                {repos.length === 0 ? (
                  <p className="quiet">
                    {filter === "candidate"
                      ? "None can go now: a check keeps each one."
                      : "Every worktree can go."}
                  </p>
                ) : (
                  <Treemap
                    doc={doc}
                    filter={filter}
                    now={now}
                    busy={scanning}
                    zoom={zoom}
                    onZoom={onZoom}
                    onOpen={onOpen}
                  />
                )}
                {repos.map(({ repo, worktrees }) => (
                  <RepoGroup
                    key={repo.common_dir}
                    repo={repo}
                    worktrees={worktrees}
                    home={home}
                    now={now}
                    selected={selected}
                    onSelect={onSelect}
                    onOpen={onOpen}
                  />
                ))}
              </>
            )}
          </>
        ) : null}
      </div>
      {chosen.length > 0 ? (
        <footer className="action-bar" aria-label="Selection">
          <p>
            <strong>{plural(chosen.length, "worktree")}</strong> selected,{" "}
            <span className="num">{bytes(chosenBytes)}</span>
          </p>
          <div className="action-bar-buttons">
            <Button variant="ghost" onClick={() => onSelect(chosen, false)}>
              Clear
            </Button>
            <SolidButton icon={<Trash />} onClick={() => onRemove(chosen)}>
              Move {chosen.length} to the trash…
            </SolidButton>
          </div>
        </footer>
      ) : null}
    </>
  );
}

/**
 * What automatic cleanup freed while the app was closed, said once the
 * stats arrive, so nobody has to open Freed to learn it. The status region
 * (`<output>`: role status, polite) is always there, empty until then, so
 * a screen reader hears the line when it fills; an empty one takes no room
 * (`.since-open:empty`).
 */
function SinceOpen({
  cleaned,
  onFreed,
}: {
  cleaned: (FreedSum & { since: string; note: string }) | null;
  onFreed: () => void;
}) {
  return (
    <output className="since-open" aria-live="polite">
      {cleaned ? (
        <>
          <ArrowsClockwise size={16} aria-hidden className="since-open-icon" />
          <span>
            Automatic cleanup freed{" "}
            <span className="since-open-figure num">about {bytes(cleaned.bytes)}</span>,{" "}
            {plural(cleaned.worktrees, "worktree")}, since the app was last open.
          </span>
          <button
            type="button"
            className="link-button"
            title={`Since ${localTime(cleaned.since)}. ${cleaned.note}`}
            onClick={onFreed}
          >
            Open Freed
          </button>
        </>
      ) : null}
    </output>
  );
}

function Summary({
  doc,
  scanning,
  freed,
  onFreed,
}: {
  doc: ScanDocument;
  scanning: boolean;
  freed: { week: FreedSum; note: string } | "unavailable" | null;
  onFreed: () => void;
}) {
  const s = doc.summary;
  const a = doc.activity;
  return (
    <section className="summary" aria-label="Summary">
      <div className="summary-main">
        <p className="summary-figure num">{bytes(s.candidate_reclaimable_bytes)}</p>
        <p className="summary-caption">
          reclaimable: {s.candidates} of {plural(s.linked_worktrees, "worktree")} can go
        </p>
      </div>
      <dl className="summary-facts">
        <div>
          <dt>Scanned</dt>
          <dd>{scanning ? "scanning again…" : new Date(doc.generated_at).toLocaleTimeString()}</dd>
        </div>
        <div>
          <dt>In the trash</dt>
          <dd>{plural(s.trash_entries, "entry", "entries")}</dd>
        </div>
        <div>
          <dt>Freed, last 7 days</dt>
          <dd>
            {freed === null ? (
              "reading…"
            ) : freed === "unavailable" ? (
              "unavailable"
            ) : (
              // The note says how residue measures sizes; the Freed screen shows it in full.
              <button type="button" className="link-button" title={freed.note} onClick={onFreed}>
                {freed.week.worktrees > 0
                  ? `about ${bytes(freed.week.bytes)}, ${plural(freed.week.worktrees, "worktree")}`
                  : "nothing"}
              </button>
            )}
          </dd>
        </div>
        <div>
          <dt>Activity check</dt>
          <dd>
            {a.available
              ? `${a.backend} saw ${a.inspected_processes ?? "?"} of ${a.running_processes ?? "?"} processes`
              : `unavailable: ${a.detail ?? a.reason ?? "unknown"}`}
          </dd>
        </div>
        {s.interrupted_removals > 0 ? (
          <div>
            <dt>Interrupted removals</dt>
            <dd>
              {s.interrupted_removals}: <code>residue scan</code> in a terminal shows them
            </dd>
          </div>
        ) : null}
      </dl>
    </section>
  );
}

function ScanErrors({ errors }: { errors: string[] }) {
  const permission = errors.some((e) => isPermissionError(e));
  return (
    <section className="notice">
      <h2>residue could not read everything</h2>
      <ul>
        {errors.slice(0, 8).map((e) => (
          <li key={e}>{e}</li>
        ))}
        {errors.length > 8 ? <li>and {errors.length - 8} more</li> : null}
      </ul>
      {permission ? <PermissionHint /> : null}
    </section>
  );
}

function RepoGroup({
  repo,
  worktrees,
  home,
  now,
  selected,
  onSelect,
  onOpen,
}: {
  repo: Repository;
  worktrees: Worktree[];
  home: string | undefined;
  now: number;
  selected: Set<string>;
  onSelect: (paths: string[], on: boolean) => void;
  onOpen: (path: string) => void;
}) {
  const candidates = worktrees.filter((w) => w.verdict === "candidate").map((w) => w.path);
  const chosen = candidates.filter((p) => selected.has(p)).length;
  const total = worktrees.reduce((sum, w) => sum + (sizeOf(w) ?? 0), 0);
  const name = baseName(repo.main_path);
  return (
    <section className="repo" aria-label={name}>
      <header className="repo-head">
        <span className="repo-check">
          {candidates.length > 0 ? (
            <Checkbox
              aria-label={`Select all that can go in ${name}`}
              checked={chosen > 0 && chosen === candidates.length}
              indeterminate={chosen > 0 && chosen < candidates.length}
              onCheckedChange={(on) => onSelect(candidates, on === true)}
            />
          ) : null}
        </span>
        <h2>
          {name} <span className="repo-path">{tilde(repo.main_path, home)}</span>
        </h2>
        <span className="repo-meta num">
          {plural(worktrees.length, "worktree")} · {bytes(total)}
        </span>
      </header>
      <ul className="rows">
        {worktrees.map((w) => (
          <Row
            key={w.path}
            w={w}
            repoPath={repo.main_path}
            home={home}
            now={now}
            checked={selected.has(w.path)}
            onSelect={onSelect}
            onOpen={onOpen}
          />
        ))}
      </ul>
    </section>
  );
}

function Row({
  w,
  repoPath,
  home,
  now,
  checked,
  onSelect,
  onOpen,
}: {
  w: Worktree;
  repoPath: string;
  home: string | undefined;
  now: number;
  checked: boolean;
  onSelect: (paths: string[], on: boolean) => void;
  onOpen: (path: string) => void;
}) {
  const path = displayPath(w);
  const name = baseName(path);
  const id = useId();
  const branch = branchLabel(w);
  // The repository's own path heads the group; a row inside it needs only the rest.
  const where = inside(path, repoPath) ?? tilde(path, home);
  return (
    <li className="row" data-verdict={w.verdict}>
      <span className="row-check">
        {w.verdict === "candidate" ? (
          <Checkbox
            aria-label={`Select ${name}`}
            checked={checked}
            onCheckedChange={(on) => onSelect([w.path], on === true)}
          />
        ) : null}
      </span>
      <button
        type="button"
        className="row-main"
        // Back from the detail returns focus here (App.tsx).
        data-opener={`row:${w.path}`}
        aria-labelledby={`${id}-name`}
        aria-describedby={`${id}-verdict ${id}-reason`}
        onClick={() => onOpen(w.path)}
      >
        <span className="row-title">
          <span className="row-name" id={`${id}-name`}>
            {name}
          </span>
          {branch === name ? null : <span className="row-branch">{branch}</span>}
        </span>
        <span className="row-reason" id={`${id}-reason`}>
          <Words text={summary(w, now)} />
        </span>
        <span className="row-path" title={path}>
          {where}
        </span>
      </button>
      <span className="row-size num">{bytes(sizeOf(w))}</span>
      <span className="row-verdict">
        <VerdictTag verdict={w.verdict} id={`${id}-verdict`} />
      </span>
      <CaretRight className="row-caret" aria-hidden />
    </li>
  );
}
