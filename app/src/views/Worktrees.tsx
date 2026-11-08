import { Button, Checkbox, Empty, Tabs } from "@cloudflare/kumo";
import { ArrowClockwise, CaretRight, FolderPlus, Trash } from "@phosphor-icons/react";
import { useId, useMemo, useState } from "react";
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
import type { CliError, Repository, ScanDocument, Worktree } from "../lib/types";
import { documentTime } from "../lib/useCli";
import { branchLabel, displayPath, sizeOf, summary } from "../lib/worktree";

type Filter = "all" | "candidate" | "kept";

const FILTERS = [
  { value: "all", label: "All" },
  { value: "candidate", label: "Can go" },
  { value: "kept", label: "Kept" },
];

function matches(w: Worktree, filter: Filter): boolean {
  if (filter === "all") return true;
  if (filter === "candidate") return w.verdict === "candidate";
  return w.verdict !== "candidate";
}

export function Worktrees({
  doc,
  error,
  scanning,
  roots,
  home,
  selected,
  onSelect,
  onRescan,
  onOpen,
  onRemove,
  onChooseFolders,
}: {
  doc: ScanDocument | undefined;
  error: CliError | undefined;
  scanning: boolean;
  roots: string[];
  home: string | undefined;
  selected: Set<string>;
  onSelect: (paths: string[], on: boolean) => void;
  onRescan: () => void;
  onOpen: (path: string) => void;
  onRemove: (paths: string[]) => void;
  onChooseFolders: () => void;
}) {
  const [filter, setFilter] = useState<Filter>("all");
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
          : "Choose the folders that hold your repositories."
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
          <Empty
            icon={<FolderPlus size={32} />}
            title="No folders to scan"
            description="residue looks for Git repositories in the folders you choose, and in every folder under them."
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
            detail="residue checks and measures every linked worktree. A large folder takes a minute."
          />
        ) : null}

        {doc ? (
          <>
            <Summary doc={doc} scanning={scanning} />
            {doc.errors.length > 0 ? <ScanErrors errors={doc.errors} /> : null}
            {doc.summary.linked_worktrees === 0 ? (
              <Empty
                title="No linked worktrees"
                description="These folders have no Git worktrees besides each repository's main checkout. Nothing takes extra space."
              />
            ) : (
              <>
                <div className="toolbar">
                  <Tabs
                    variant="segmented"
                    size="sm"
                    tabs={FILTERS}
                    value={filter}
                    onValueChange={(v) => setFilter(v as Filter)}
                  />
                </div>
                {repos.length === 0 ? (
                  <p className="quiet">
                    {filter === "candidate"
                      ? "No worktree can go right now: each one has a check that keeps it."
                      : "Every worktree can go."}
                  </p>
                ) : (
                  repos.map(({ repo, worktrees }) => (
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
                  ))
                )}
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
              Move to trash…
            </SolidButton>
          </div>
        </footer>
      ) : null}
    </>
  );
}

function Summary({ doc, scanning }: { doc: ScanDocument; scanning: boolean }) {
  const s = doc.summary;
  const a = doc.activity;
  return (
    <section className="summary" aria-label="Summary">
      <div className="summary-main">
        <p className="summary-figure num">{bytes(s.candidate_reclaimable_bytes)}</p>
        <p className="summary-caption">
          reclaimable: {s.candidates} of {plural(s.linked_worktrees, "linked worktree")} can go
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
              {s.interrupted_removals}; run <code>residue scan</code> in a terminal for details
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
              aria-label={`Select every worktree that can go in ${name}`}
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
