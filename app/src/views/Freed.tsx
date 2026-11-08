import { Button, Empty } from "@cloudflare/kumo";
import { ArrowClockwise, ChartBar } from "@phosphor-icons/react";
import { useState } from "react";
import { Columns, WeekTable } from "../components/viz/Columns";
import { Figure } from "../components/viz/Figure";
import { Ranked } from "../components/viz/Ranked";
import { ErrorPanel, Loading, PageHeader } from "../components/ui";
import { bytes, plural, tilde } from "../lib/format";
import { COMMANDS, localDay, localTime } from "../lib/freed";
import type { CliError, FreedSum, StatsDocument } from "../lib/types";

/**
 * What residue has freed on this machine (`residue stats --json`), in the
 * words of `residue stats`: the spans, each week, each command, the trash
 * and restores.
 */
export function Freed({
  doc,
  error,
  loading,
  sinceOpen,
  home,
  onReload,
}: {
  doc: StatsDocument | undefined;
  error: CliError | undefined;
  loading: boolean;
  /** What residue freed since the app was last open; null on a first open. */
  sinceOpen: (FreedSum & { since: string }) | null;
  home: string | undefined;
  onReload: () => void;
}) {
  const freedAny = doc ? doc.freed.all_time.worktrees > 0 : false;
  return (
    <>
      <PageHeader
        title="Freed"
        description="What residue freed on this machine, by any of its commands."
        actions={
          <Button variant="secondary" icon={<ArrowClockwise />} loading={loading} disabled={loading} onClick={onReload}>
            Read again
          </Button>
        }
      />
      <div className="page-body" aria-busy={loading}>
        {error ? (
          <ErrorPanel
            title="residue could not read its stats"
            error={error}
            action={
              <Button variant="secondary" icon={<ArrowClockwise />} onClick={onReload}>
                Try again
              </Button>
            }
          />
        ) : null}
        {!doc && loading ? <Loading title="Reading residue's stats ledger" /> : null}
        {doc ? (
          <>
            {freedAny && doc.since ? (
              <section className="block" aria-labelledby="freed-spans">
                <h2 id="freed-spans">Freed since {localDay(doc.since)}</h2>
                <dl className="kpis">
                  <Kpi label="Last 7 days" sum={doc.freed.last_7_days} />
                  <Kpi label="Last 30 days" sum={doc.freed.last_30_days} />
                  <Kpi label="All time" sum={doc.freed.all_time} />
                  {sinceOpen ? (
                    <Kpi
                      label="Since last open"
                      sum={sinceOpen}
                      note={localTime(sinceOpen.since)}
                    />
                  ) : null}
                </dl>
                <p className="hint">{doc.estimate_note}</p>
              </section>
            ) : (
              <Empty
                icon={<ChartBar size={32} />}
                title="No freed space counted yet"
                description="Space counts here once residue 0.0.8 or newer deletes a worktree. A move into the trash frees nothing until a later removal purges it."
              />
            )}

            {freedAny ? (
              <Figure
                label="Freed each week"
                head={<h2 className="viz-title">Last {doc.weeks.length} weeks</h2>}
                busy={loading}
                caption="One column per week, the latest ending now. Point at a week for its commands."
              >
                {doc.weeks.some((w) => w.worktrees > 0) ? (
                  <>
                    <Columns weeks={doc.weeks} />
                    <details className="viz-details">
                      <summary>Each week as a table</summary>
                      <WeekTable weeks={doc.weeks} />
                    </details>
                  </>
                ) : (
                  <p className="quiet">Nothing freed in these weeks.</p>
                )}
              </Figure>
            ) : null}

            {freedAny ? (
              <section className="block" aria-labelledby="freed-by">
                <h2 id="freed-by">By command, all time</h2>
                <Ranked
                  label="Freed by command, all time"
                  items={COMMANDS.filter((c) => doc.freed.all_time.by[c.by].worktrees > 0).map((c) => {
                    const last = doc.last_freed_at[c.by];
                    return {
                      key: c.by,
                      label: c.label,
                      sum: doc.freed.all_time.by[c.by],
                      note: last
                        ? `last ${localTime(last)}`
                        : null,
                    };
                  })}
                />
              </section>
            ) : null}

            {freedAny && doc.repositories.length > 0 ? <Repositories repos={doc.repositories} home={home} /> : null}

            <Trash doc={doc} home={home} />
          </>
        ) : null}
      </div>
    </>
  );
}

function Kpi({ label, sum, note }: { label: string; sum: FreedSum; note?: string }) {
  return (
    <div className="kpi">
      <dt>{label}</dt>
      <dd className="kpi-figure num">{sum.worktrees > 0 ? bytes(sum.bytes) : "nothing"}</dd>
      <dd className="kpi-note">
        {plural(sum.worktrees, "worktree")}
        {note ? `, since ${note}` : ""}
      </dd>
    </div>
  );
}

/** How many repositories the list shows before "Show the other N". */
const REPOS_SHOWN = 8;

/**
 * Each repository residue freed space in, most first, as `residue stats`
 * lists them (`repositories`). Past the first eight, the rest wait behind
 * one button, so a machine with many repositories keeps the trash in view.
 */
function Repositories({ repos, home }: { repos: StatsDocument["repositories"]; home: string | undefined }) {
  const [all, setAll] = useState(false);
  const shown = all ? repos : repos.slice(0, REPOS_SHOWN);
  const hidden = repos.length - REPOS_SHOWN;
  return (
    <section className="block" aria-labelledby="freed-repos">
      <h2 id="freed-repos">By repository, all time</h2>
      <Ranked
        label="Freed by repository, all time"
        items={shown.map((r) => ({
          key: r.common_dir,
          label: tilde(r.path, home),
          sum: r,
          note: `last ${localTime(r.last_freed_at)}`,
        }))}
      />
      {hidden > 0 ? (
        <button type="button" className="link-button more" aria-expanded={all} onClick={() => setAll((a) => !a)}>
          {all ? `Show the first ${REPOS_SHOWN} only` : `Show the other ${plural(hidden, "repository", "repositories")}`}
        </button>
      ) : null}
    </section>
  );
}

/** The trash, restores, and what residue could not read, as `residue stats` ends. */
function Trash({ doc, home }: { doc: StatsDocument; home: string | undefined }) {
  const moved = doc.trashed.worktrees > 0;
  const now = doc.trash_now.entries > 0;
  const r = doc.restored;
  return (
    <>
      {moved || now ? (
        <section className="block" aria-labelledby="freed-trash">
          <h2 id="freed-trash">Trash</h2>
          <dl className="facts">
            {moved ? (
              <div>
                <dt>Moved there, all time</dt>
                <dd>
                  <span className="num">{bytes(doc.trashed.bytes)}</span>, {plural(doc.trashed.worktrees, "worktree")}
                </dd>
              </div>
            ) : null}
            {now ? (
              <div>
                <dt>In it now</dt>
                <dd>
                  <span className="num">{bytes(doc.trash_now.bytes)}</span>,{" "}
                  {plural(doc.trash_now.entries, "worktree")}, in{" "}
                  {plural(doc.trash_now.repositories, "repository", "repositories")}
                </dd>
              </div>
            ) : null}
          </dl>
          <p className="hint">
            A move into the trash frees nothing until a remove run purges it, after{" "}
            <span className="flag">--keep-trash</span>.
          </p>
        </section>
      ) : null}
      {r.worktrees > 0 ? (
        <p className="quiet">
          Restored {plural(r.worktrees, "worktree")}: {r.from_trash} from the trash, {r.from_removal_records} from
          removal records.
        </p>
      ) : null}
      {doc.errors.length > 0 ? (
        <section className="notice">
          <h2>residue could not read the trash of</h2>
          <ul>
            {doc.errors.map((e) => (
              <li key={e}>{e}</li>
            ))}
          </ul>
        </section>
      ) : null}
      <p className="hint">
        Ledger: <code>{tilde(doc.ledger, home)}</code>
        {doc.skipped_lines > 0 ? `, skipped ${plural(doc.skipped_lines, "line")} residue could not read` : ""}
      </p>
    </>
  );
}
