import { Button } from "@cloudflare/kumo";
import { ArrowClockwise, ArrowLeft, Check, FolderOpen, Question, Trash, X } from "@phosphor-icons/react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { useState } from "react";
import { Command, ErrorPanel, Loading, PageHeader, SolidButton, VerdictTag, Words } from "../components/ui";
import { api } from "../lib/api";
import { age, baseName, bytes, grouped, shortHead, tilde } from "../lib/format";
import type { CheckStatus, Check as ResidueCheck, Worktree } from "../lib/types";
import { documentTime, useCli } from "../lib/useCli";
import { VERDICT_HELP, branchLabel, checkLabel, displayPath, keptBy, known, verdictHelp } from "../lib/worktree";

// A status a newer residue adds gets the question mark and its own name.
const STATUS_ICON: Record<CheckStatus, typeof Check> = { pass: Check, block: X, unknown: Question };
const STATUS_LABEL: Record<CheckStatus, string> = { pass: "pass", block: "keeps it", unknown: "not finished" };

/** `residue explain --json PATH`: every check, in residue's own words. */
export function Detail({
  path,
  home,
  onBack,
  onRemove,
}: {
  path: string;
  home: string | undefined;
  onBack: () => void;
  onRemove: (paths: string[]) => void;
}) {
  const q = useCli(path, () => api.explain(path));
  // Another worktree's report is not shown as this one's.
  const doc = q.dataKey === path ? q.data : undefined;
  const error = q.error;
  const loading = q.loading;
  const load = q.reload;
  const [revealError, setRevealError] = useState<string | null>(null);

  const w = doc?.worktree;
  const shown = w ? displayPath(w) : path;
  const now = documentTime(doc?.generated_at) ?? 0;

  const reveal = async () => {
    setRevealError(null);
    try {
      await revealItemInDir(shown);
    } catch (e) {
      setRevealError(String(e));
    }
  };

  return (
    <>
      <PageHeader
        title={baseName(shown)}
        description={tilde(shown, home)}
        actions={
          <>
            <Button variant="ghost" icon={<ArrowLeft />} onClick={onBack} title="Back (Escape)">
              Back
            </Button>
            <Button
              variant="secondary"
              icon={<ArrowClockwise />}
              loading={loading}
              disabled={loading}
              onClick={load}
            >
              Check again
            </Button>
          </>
        }
      />
      <div className="page-body">
        {error ? <ErrorPanel title="residue could not explain this worktree" error={error} /> : null}
        {loading && !doc ? <Loading title="Checking this worktree" /> : null}
        {w ? (
          <>
            <section className="detail-head">
              <div className="detail-verdict">
                <VerdictTag verdict={w.verdict} />
                <p>{verdictText(w)}</p>
              </div>
              <div className="detail-actions">
                <Button variant="secondary" icon={<FolderOpen />} onClick={reveal}>
                  Show in Finder
                </Button>
                {w.verdict === "candidate" ? (
                  <SolidButton icon={<Trash />} onClick={() => onRemove([w.path])}>
                    Move to trash…
                  </SolidButton>
                ) : null}
              </div>
              {revealError ? <p className="hint">Could not open Finder: {revealError}</p> : null}
            </section>

            <dl className="facts">
              <div>
                <dt>Branch</dt>
                <dd>{branchLabel(w)}</dd>
              </div>
              <div>
                <dt>HEAD</dt>
                <dd>
                  <code>{shortHead(w.head) ?? "none"}</code>
                </dd>
              </div>
              <div>
                <dt>Size</dt>
                <dd className="num">
                  {w.size
                    ? `${bytes(w.size.allocated_bytes)} on disk, about ${bytes(w.size.reclaimable_bytes)} reclaimable, ${grouped(w.size.entries)} entries`
                    : "not measured"}
                  {w.size && w.size.unreadable_entries > 0
                    ? `, ${grouped(w.size.unreadable_entries)} unreadable`
                    : ""}
                </dd>
              </div>
              <div>
                <dt>Last change</dt>
                <dd>
                  {w.last_change
                    ? `${age(now - w.last_change.unix)}: ${w.last_change.what === "." ? "the worktree folder" : w.last_change.what} (from ${w.last_change.source.replace("_", " ")})`
                    : (w.last_change_error ?? "unknown")}
                </dd>
              </div>
              <div>
                <dt>Upstream</dt>
                <dd>
                  {w.upstream_error ??
                    (w.upstream ? `${w.upstream}${w.upstream_gone ? " (gone)" : ""}` : "none")}
                </dd>
              </div>
              <div>
                <dt>Local-only commits</dt>
                <dd>
                  {w.local_only_commits_error ??
                    (w.local_only_commits == null ? "no remote-tracking refs" : grouped(w.local_only_commits))}
                </dd>
              </div>
              {w.locked ? (
                <div>
                  <dt>Locked</dt>
                  <dd>{w.lock_reason ?? "yes"}</dd>
                </div>
              ) : null}
            </dl>

            <section className="block">
              <h2>Checks</h2>
              <ul className="checks">
                {checksFirstBlocking(w.checks).map((c) => {
                  const Icon = known(STATUS_ICON, c.status) ?? Question;
                  return (
                    <li key={c.id} className="check" data-status={c.status}>
                      <Icon className="check-icon" weight="bold" aria-hidden />
                      <span className="check-name">{checkLabel(c.id)}</span>
                      <span className="visually-hidden">{known(STATUS_LABEL, c.status) ?? c.status}</span>
                      <span className="check-detail">
                        <Words text={c.detail} />
                      </span>
                    </li>
                  );
                })}
              </ul>
            </section>

            {w.activity && w.activity.processes.length > 0 ? (
              <section className="block">
                <h2>Processes using it</h2>
                <ul className="plain-list">
                  {w.activity.processes.map((p) => (
                    <li key={p.pid}>
                      <code>{p.command}</code> <span className="quiet">pid {p.pid}</span>
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}

            {w.ignored.length > 0 ? (
              <section className="block">
                <h2>Ignored files</h2>
                <table className="table">
                  <thead>
                    <tr>
                      <th scope="col">Path</th>
                      <th scope="col">Regenerable</th>
                      <th scope="col" className="num-col">
                        Files
                      </th>
                      <th scope="col" className="num-col">
                        Size
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {w.ignored.map((i) => (
                      <tr key={i.path}>
                        <td>
                          <code>{i.path}</code>
                        </td>
                        <td>{i.regenerable ? "yes" : "no, keeps it"}</td>
                        <td className="num-col">{i.files == null ? "?" : grouped(i.files)}</td>
                        <td className="num-col">{bytes(i.bytes)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </section>
            ) : null}

            <section className="block">
              <h2>Keep it</h2>
              <p className="quiet">
                A locked worktree stays through every cleanup, by hand or automatic, until you
                unlock it.
              </p>
              <Command text={`git worktree lock --reason "keep" ${quote(w.path)}`} label="lock command" />
            </section>
          </>
        ) : null}
      </div>
    </>
  );
}

function verdictText(w: Worktree): string {
  if (w.verdict === "stale") return w.stale_reason ?? VERDICT_HELP.stale;
  const kept = keptBy(w).map((c) => checkLabel(c.id).toLowerCase());
  if (w.verdict === "keep" && kept.length > 0) {
    return `Kept by: ${kept.join(", ")}. It stays until every check passes.`;
  }
  if (w.verdict === "unknown" && kept.length > 0) {
    return `Could not finish: ${kept.join(", ")}. residue keeps it until every check can run.`;
  }
  return (
    verdictHelp(w.verdict) ??
    `residue's verdict is "${w.verdict}", which this version of the app does not know.`
  );
}

/** The checks that keep it come first; otherwise residue's order. */
function checksFirstBlocking(checks: ResidueCheck[]): ResidueCheck[] {
  return [...checks.filter((c) => c.status !== "pass"), ...checks.filter((c) => c.status === "pass")];
}

function quote(path: string): string {
  return /^[A-Za-z0-9/._-]+$/.test(path) ? path : `'${path.replaceAll("'", "'\\''")}'`;
}
