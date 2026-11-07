import { Button, Empty } from "@cloudflare/kumo";
import { ArrowClockwise, ArrowCounterClockwise, ClockCounterClockwise } from "@phosphor-icons/react";
import { useState } from "react";
import { ErrorPanel, Loading, PageHeader } from "../components/ui";
import { api, asCliError } from "../lib/api";
import { ageOf, baseName, bytes, shortBranch, tilde } from "../lib/format";
import type { CliError, Listing, Restorable } from "../lib/types";
import { documentTime, useCli } from "../lib/useCli";
import { duration } from "../lib/watch";

/** Everything the listings return, once each. */
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
        bytes: t.reclaimable_bytes,
        registered: t.registered,
        error: t.error,
      });
    }
  }
  return out.toSorted((a, b) => (b.at ?? "").localeCompare(a.at ?? ""));
}

export function Restore({
  roots,
  home,
  keepTrash,
  refreshKey,
  onRestored,
}: {
  roots: string[];
  home: string | undefined;
  /** residue's default `--keep-trash` (`7d`), which the app's removals use. */
  keepTrash: string | undefined;
  refreshKey: number;
  onRestored: () => void;
}) {
  const key = roots.length > 0 ? `${refreshKey}:${JSON.stringify(roots)}` : null;
  const q = useCli(key, () => api.listRestorable(roots));
  const listings = q.data ?? null;
  const error = q.error;
  const loading = q.loading;
  const load = q.reload;
  const [working, setWorking] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<{ ok: boolean; text: string; error?: CliError } | null>(null);

  const restore = async (item: Restorable) => {
    setWorking(item.entryPath);
    setOutcome(null);
    try {
      const doc = await api.restore(item.entryPath, item.root);
      setOutcome({
        ok: true,
        text: `Restored to ${tilde(doc.restored ?? item.originalPath ?? item.name, home)}`,
      });
      onRestored();
      load();
    } catch (e) {
      setOutcome({ ok: false, text: `Could not restore ${item.name}`, error: asCliError(e) });
    } finally {
      setWorking(null);
    }
  };

  const items = listings ? restorables(listings) : [];
  // Ages count to when residue listed the trash.
  const now = documentTime(listings?.find((l) => l.document)?.document?.generated_at);
  const failed = (listings ?? []).filter((l) => l.error);
  const listErrors = (listings ?? []).flatMap((l) => l.document?.errors ?? []);

  return (
    <>
      <PageHeader
        title="Restore"
        description="Worktrees residue moved to the trash. Restore puts one back where it was, on its branch."
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
            <output className="notice">{outcome.text}</output>
          ) : outcome.error ? (
            <ErrorPanel title={outcome.text} error={outcome.error} />
          ) : null
        ) : null}
        {error ? <ErrorPanel title="residue could not list the trash" error={error} /> : null}
        {failed.map((l) => (
          <ErrorPanel key={l.root} title={`residue could not list the trash in ${tilde(l.root, home)}`} error={l.error!} />
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
          <Empty title="No folders to look in" description="Choose folders first; Restore lists the trash of the repositories in them." />
        ) : loading && !listings ? (
          <Loading title="Reading the trash" />
        ) : listings && items.length === 0 ? (
          <Empty
            icon={<ClockCounterClockwise size={32} />}
            title="Nothing to restore"
            description="When you move worktrees to the trash, they are listed here until residue deletes them from the trash."
          />
        ) : (
          <>
            <ul className="rows restore-rows">
              {items.map((item) => {
                const name = baseName(item.originalPath ?? item.name);
                const branch = shortBranch(item.branch) ?? "detached";
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
                        : `${item.kind === "trash" ? "moved to the trash" : "deleted"} ${ageOf(item.at, now) ?? ""}${item.registered ? "" : ", no longer registered with Git"}`}
                    </span>
                    <span className="row-path">{tilde(item.originalPath ?? item.entryPath, home)}</span>
                  </span>
                  <span className="row-size num">{bytes(item.bytes)}</span>
                  <span className="row-action">
                    <Button
                      variant="secondary"
                      size="sm"
                      icon={<ArrowCounterClockwise />}
                      loading={working === item.entryPath}
                      disabled={working !== null}
                      onClick={() => restore(item)}
                      aria-label={`Restore ${name}`}
                    >
                      Restore
                    </Button>
                  </span>
                </li>
                );
              })}
            </ul>
            {items.length > 0 ? (
              <p className="hint">
                A trashed worktree still takes its disk space. The next removal or automatic
                cleanup in its repository deletes the entries older than its --keep-trash
                {keepTrash
                  ? `: ${duration(keepTrash)}, residue's default, which the app and automatic cleanup use`
                  : ""}
                . Each one it deletes leaves a removal record, which this screen does not list yet:
                residue restore in a terminal recreates the worktree from it.
              </p>
            ) : null}
          </>
        )}
      </div>
    </>
  );
}
