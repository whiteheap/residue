import { Button, Empty } from "@cloudflare/kumo";
import { ArrowClockwise, ArrowCounterClockwise, ClockCounterClockwise } from "@phosphor-icons/react";
import { useState } from "react";
import { ErrorPanel, Loading, PageHeader } from "../components/ui";
import { Meter } from "../components/viz/Meter";
import { api, asCliError } from "../lib/api";
import { ageOf, baseName, bytes, plural, shortBranch, shortHead, tilde } from "../lib/format";
import { removedBy, sized } from "../lib/records";
import type { CliError, Listing, Restorable, RestoreDocument } from "../lib/types";
import { documentTime, useCli } from "../lib/useCli";
import { trashAge, trashTotals } from "../lib/viz";
import { duration, durationSeconds } from "../lib/watch";

/** Everything the listings return, once each, newest first. */
function restorables(listings: Listing[]): Restorable[] {
  const seen = new Set<string>();
  const out: Restorable[] = [];
  for (const l of listings) {
    for (const t of l.document?.trash ?? []) {
      if (seen.has(t.entry_path)) continue;
      seen.add(t.entry_path);
      out.push({
        kind: "trash",
        root: l.root,
        name: t.entry,
        entryPath: t.entry_path,
        originalPath: t.original_path,
        branch: t.branch,
        at: t.trashed_at,
        error: t.error,
        bytes: t.reclaimable_bytes,
        registered: t.registered,
      });
    }
    for (const r of l.document?.removed ?? []) {
      if (seen.has(r.entry_path)) continue;
      seen.add(r.entry_path);
      out.push({
        kind: "removed",
        root: l.root,
        name: r.entry,
        entryPath: r.entry_path,
        originalPath: r.original_path,
        branch: r.branch,
        at: r.removed_at,
        error: r.error,
        state: r.state,
        reason: r.reason,
        freed: r.allocated_bytes,
        notKept: r.not_kept,
        asidePath: r.aside_path,
        incomplete: r.incomplete,
      });
    }
  }
  return out.toSorted((a, b) => (b.at ?? "").localeCompare(a.at ?? ""));
}

/** The line under a removal record, before the click: what restore does and does not bring back. */
function recordNote(item: Extract<Restorable, { kind: "removed" }>, home: string | undefined): string | null {
  if (item.state === "interrupted") {
    return `Its removal did not finish, and Git still lists it${
      item.asidePath ? ` at ${tilde(item.asidePath, home)}` : ""
    }: restore moves it back.`;
  }
  if (item.notKept.length === 0) return null;
  return `Restore recreates its tracked files from its commits, not the ignored files the deletion took: ${item.notKept
    .map(sized)
    .join(", ")}. Your project's install or build makes them again.`;
}

/** How a restored worktree came back, in words. */
function restoredText(doc: RestoreDocument, item: Restorable, home: string | undefined): string[] {
  const done = doc.restore;
  const where = tilde(doc.restored ?? done?.path ?? item.originalPath ?? item.name, home);
  if (!done) return [`Restored to ${where}.`];
  const branch = shortBranch(done.branch);
  const how = (() => {
    switch (done.checkout) {
      case "moved_back":
        return done.kind === "trash" ? "moved back from the trash" : "moved back from where its removal stopped";
      case "checked_out_branch":
        return `on its branch ${branch ?? ""}`;
      case "recreated_branch":
        return `${branch ?? "its branch"} had been deleted, so restore created it again at the recorded HEAD`;
      case "new_branch":
        return `${branch ?? "its branch"} no longer contains the recorded HEAD, so restore created ${done.new_branch ?? "a new branch"} there; ${branch ?? "the branch"} stays as it is`;
      case "detached":
        return `detached at ${shortHead(done.head) ?? "the recorded HEAD"}, as it was`;
      default:
        return null;
    }
  })();
  const lines = [`Restored to ${where}${how ? `, ${how}` : ""}.`];
  if (done.not_kept.length > 0) {
    lines.push(
      `Not brought back: ${done.not_kept.map(sized).join(", ")}. Your project's install or build makes them again.`,
    );
  }
  return [...lines, ...done.notes];
}

export function Restore({
  roots,
  home,
  keepTrash,
  keepRemoved,
  refreshKey,
  onRestored,
}: {
  roots: string[];
  home: string | undefined;
  /** residue's default `--keep-trash` (`7d`), which the app's removals use. */
  keepTrash: string | undefined;
  /** residue's default `--keep-removed` (`30d`), which the app's removals use. */
  keepRemoved: string | undefined;
  refreshKey: number;
  onRestored: () => void;
}) {
  const key = roots.length > 0 ? `${refreshKey}:${JSON.stringify(roots)}` : null;
  const q = useCli(key, () => api.listRestorable(roots));
  const serviceQ = useCli(`restore-service:${refreshKey}`, api.serviceStatus);
  const listings = q.data ?? null;
  const error = q.error;
  const loading = q.loading;
  const load = q.reload;
  const [working, setWorking] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<{ ok: boolean; lines: string[]; error?: CliError } | null>(null);

  const restore = async (item: Restorable) => {
    setWorking(item.entryPath);
    setOutcome(null);
    try {
      const doc = await api.restore(item.entryPath, item.root);
      setOutcome({ ok: true, lines: restoredText(doc, item, home) });
      onRestored();
      load();
    } catch (e) {
      setOutcome({ ok: false, lines: [`Could not restore ${item.name}`], error: asCliError(e) });
    } finally {
      setWorking(null);
    }
  };

  const items = listings ? restorables(listings) : [];
  const totals = trashTotals(items);
  const trashed = totals.entries > 0;
  // Ages count to when residue listed them.
  const now = documentTime(listings?.find((l) => l.document)?.document?.generated_at);
  // The keep window in force: the app's removals and the service both use residue's default.
  const keepSeconds = keepTrash ? durationSeconds(keepTrash) : null;
  const failed = (listings ?? []).filter((l) => l.error);
  const listErrors = (listings ?? []).flatMap((l) => l.document?.errors ?? []);
  const appKeep = keepRemoved
    ? `${duration(keepRemoved)}, residue's default --keep-removed`
    : "residue's default --keep-removed";
  // Automatic cleanup forgets records after its own --keep-removed, which
  // the service's arguments set or residue's default supplies.
  const service = serviceQ.data?.installed ? (serviceQ.data.program?.settings.keep_removed ?? null) : null;
  const forgets = service
    ? `The app's removals forget the records older than ${appKeep}, and automatic cleanup those older than ${duration(service.value)}, ${service.default ? "residue's default, since the service sets none" : "the service's --keep-removed"}, each in the repositories it removes from.`
    : `A removal in its repository forgets the records older than ${appKeep}, which the app's removals use.`;

  return (
    <>
      <PageHeader
        title="Restore"
        description="Worktrees residue moved to the trash or deleted. Restore puts one back where it was, on its branch."
        actions={
          <Button
            variant="secondary"
            icon={<ArrowClockwise />}
            loading={loading}
            disabled={loading || roots.length === 0}
            onClick={load}
          >
            Refresh
          </Button>
        }
      />
      <div className="page-body">
        {outcome ? (
          outcome.ok ? (
            <output className="notice">
              {outcome.lines.map((line) => (
                <p key={line}>{line}</p>
              ))}
            </output>
          ) : outcome.error ? (
            <ErrorPanel title={outcome.lines[0] ?? "Could not restore it"} error={outcome.error} />
          ) : null
        ) : null}
        {error ? <ErrorPanel title="residue could not list what it can restore" error={error} /> : null}
        {failed.map((l) => (
          <ErrorPanel
            key={l.root}
            title={`residue could not list what it can restore in ${tilde(l.root, home)}`}
            error={l.error!}
          />
        ))}
        {listErrors.length > 0 ? (
          <section className="notice">
            <h2>Some entries could not be read</h2>
            <ul>
              {listErrors.map((e) => (
                <li key={e}>{e}</li>
              ))}
            </ul>
          </section>
        ) : null}

        {roots.length === 0 ? (
          <Empty
            title="No folders to look in"
            description="Choose folders first; Restore lists the trash and the removal records of the repositories in them."
          />
        ) : loading && !listings ? (
          <Loading title="Reading the trash and the removal records" />
        ) : listings && items.length === 0 ? (
          <Empty
            icon={<ClockCounterClockwise size={32} />}
            title="Nothing to restore"
            description={`A worktree residue moves to the trash is listed here until residue deletes it from the trash. One it deletes leaves a removal record, listed here until a later removal forgets it. ${forgets}`}
          />
        ) : (
          <>
            {trashed ? (
              <p className="stat-line">
                <strong className="num">{bytes(totals.bytes)}</strong> reclaimable in{" "}
                {plural(totals.entries, "trash entry", "trash entries")}
                {totals.unmeasured > 0 ? `, ${totals.unmeasured} not measured` : ""}
              </p>
            ) : null}
            <ul className="rows restore-rows">
              {items.map((item) => {
                const name = baseName(item.originalPath ?? item.name);
                // A record residue cannot read has no branch to show, detached or not.
                const branch = item.error ? name : (shortBranch(item.branch) ?? "detached");
                const note = item.kind === "removed" && !item.error ? recordNote(item, home) : null;
                // residue cannot restore from a record it cannot read.
                const restorable = item.kind === "trash" || (!item.error && !item.incomplete);
                const kept = item.kind === "trash" && !item.error ? trashAge(item.at, now, keepSeconds) : null;
                return (
                  <li key={item.entryPath} className="row" data-kind={item.kind}>
                    <span className="row-main static">
                      <span className="row-title">
                        <span className="row-name">{name}</span>
                        {branch === name ? null : <span className="row-branch">{branch}</span>}
                      </span>
                      <span className="row-reason">
                        {item.error
                          ? item.error
                          : item.kind === "trash"
                            ? `moved to the trash ${ageOf(item.at, now) ?? ""}${item.registered ? "" : ", no longer registered with Git"}`
                            : [
                                "removed",
                                ageOf(item.at, now),
                                removedBy(item.reason),
                                item.freed != null ? `· ${bytes(item.freed)} freed` : null,
                              ]
                                .filter(Boolean)
                                .join(" ")}
                      </span>
                      {kept && keepTrash ? (
                        <span className="row-age">
                          <Meter
                            size="thin"
                            value={kept.seconds}
                            max={kept.keep}
                            label={`Time in the trash, against --keep-trash ${keepTrash}`}
                            valueText={
                              kept.eligible
                                ? `past ${duration(keepTrash)}`
                                : `${Math.round(kept.fraction * 100)}% of ${duration(keepTrash)}`
                            }
                          />
                          <span>
                            {kept.eligible
                              ? `past ${duration(keepTrash)}: the next cleanup in its repository deletes it and leaves a removal record`
                              : `the first cleanup after ${duration(keepTrash)} deletes it and leaves a removal record`}
                          </span>
                        </span>
                      ) : null}
                      <span className="row-path">{tilde(item.originalPath ?? item.entryPath, home)}</span>
                      {note ? <span className="row-note">{note}</span> : null}
                    </span>
                    <span className="row-size num">{item.kind === "trash" ? bytes(item.bytes) : null}</span>
                    <span className="row-action">
                      {restorable ? (
                        <Button
                          variant="secondary"
                          size="sm"
                          icon={<ArrowCounterClockwise />}
                          loading={working === item.entryPath}
                          disabled={working !== null}
                          onClick={() => restore(item)}
                          aria-label={
                            item.kind === "trash" ? `Restore ${name} from the trash` : `Restore ${name} from its removal record`
                          }
                        >
                          Restore
                        </Button>
                      ) : null}
                    </span>
                  </li>
                );
              })}
            </ul>
            {items.length > 0 ? (
              <>
                {trashed ? (
                  <p className="hint">
                    A trashed worktree still takes its disk space. The next removal or automatic
                    cleanup in its repository deletes the entries older than its --keep-trash
                    {keepTrash
                      ? `: ${duration(keepTrash)}, residue's default, which the app and automatic cleanup use`
                      : ""}
                    . Each one it deletes leaves a removal record, listed here.
                  </p>
                ) : null}
                <p className="hint">
                  A removal record takes almost no space. Restore recreates the worktree from it with
                  git worktree add, on its branch and HEAD, and refuses when something is at its
                  path. {forgets} A forgotten record cannot be restored.
                </p>
              </>
            ) : null}
          </>
        )}
      </div>
    </>
  );
}
