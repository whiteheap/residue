import { Button, Dialog } from "@cloudflare/kumo";
import { Trash } from "@phosphor-icons/react";
import { useEffect, useState } from "react";
import { ErrorPanel, Flags, Loading, SolidButton } from "../components/ui";
import { api, asCliError } from "../lib/api";
import { ageOf, baseName, bytes, plural, span, tilde } from "../lib/format";
import { removalLine } from "../lib/freed";
import { removedBy } from "../lib/records";
import type { CliError, RemoveDocument } from "../lib/types";
import { documentTime } from "../lib/useCli";
import { displayPath, keptBy, checkLabel } from "../lib/worktree";

type Phase =
  | { step: "planning" }
  | { step: "plan"; plan: RemoveDocument }
  | { step: "running"; plan: RemoveDocument }
  | { step: "done"; result: RemoveDocument }
  | { step: "error"; error: CliError; plan: RemoveDocument | null };

/**
 * Move worktrees to the trash. First residue's own plan (`residue remove
 * --trash --json PATH...` without --yes, which removes nothing): each worktree
 * checked again, the expired trash entries the run would purge, and the
 * removal records it would forget. The user confirms exactly that list; then
 * the same command with --yes.
 */
export function RemoveDialog({
  paths,
  home,
  estimateNote,
  onClose,
  onRemoved,
}: {
  paths: string[];
  home: string | undefined;
  /** residue's sentence on how it measures sizes, from its stats, shown with the freed line. */
  estimateNote: string | undefined;
  onClose: () => void;
  onRemoved: (doc: RemoveDocument) => void;
}) {
  const [phase, setPhase] = useState<Phase>({ step: "planning" });

  useEffect(() => {
    let live = true;
    api
      .planRemoval(paths)
      .then((r) => live && setPhase({ step: "plan", plan: r.document }))
      .catch((e) => live && setPhase({ step: "error", error: asCliError(e), plan: null }));
    return () => {
      live = false;
    };
  }, [paths]);

  const busy = phase.step === "planning" || phase.step === "running";

  const confirm = (plan: RemoveDocument, ready: string[]) => {
    setPhase({ step: "running", plan });
    api
      .removeToTrash(ready)
      .then((r) => {
        setPhase({ step: "done", result: r.document });
        onRemoved(r.document);
      })
      .catch((e) => setPhase({ step: "error", error: asCliError(e), plan }));
  };

  return (
    <Dialog.Root
      role="alertdialog"
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <Dialog size="xl" className="dialog">
        <Dialog.Title className="dialog-title">
          {phase.step === "done" ? "Done" : "Move to the trash"}
        </Dialog.Title>
        {phase.step === "planning" ? (
          <>
            <Dialog.Description className="dialog-text">
              residue is checking {plural(paths.length, "worktree")} again before it shows you
              the plan. Nothing moves yet.
            </Dialog.Description>
            <Loading title="Checking again" />
          </>
        ) : null}

        {phase.step === "plan" || phase.step === "running" ? (
          <Plan
            plan={phase.plan}
            home={home}
            running={phase.step === "running"}
            onCancel={onClose}
            onConfirm={(ready) => confirm(phase.plan, ready)}
          />
        ) : null}

        {phase.step === "done" ? <Result doc={phase.result} home={home} estimateNote={estimateNote} onClose={onClose} /> : null}

        {phase.step === "error" ? (
          <>
            <Dialog.Description className="dialog-text">
              {phase.plan
                ? "residue stopped before it reported. Scan again to see what moved."
                : "residue could not make a plan, so nothing moved."}
            </Dialog.Description>
            <ErrorPanel title="residue remove failed" error={phase.error} />
            <div className="dialog-buttons">
              <SolidButton onClick={onClose}>Close</SolidButton>
            </div>
          </>
        ) : null}
      </Dialog>
    </Dialog.Root>
  );
}

function Plan({
  plan,
  home,
  running,
  onCancel,
  onConfirm,
}: {
  plan: RemoveDocument;
  home: string | undefined;
  running: boolean;
  onCancel: () => void;
  onConfirm: (ready: string[]) => void;
}) {
  // Without --yes residue marks each worktree it would move as skipped
  // (nothing was confirmed) and each one it keeps as refused.
  const ready = plan.results.filter((r) => r.result === "skipped" && r.worktree?.verdict === "candidate");
  const kept = plan.results.filter((r) => !ready.includes(r));
  const purges = plan.purged.filter((p) => p.result === "skipped");
  // Without --yes, a record the run would forget is skipped too.
  const forgets = plan.forgotten.filter((f) => f.result === "skipped");
  const total = ready.reduce((sum, r) => sum + (r.worktree?.size?.reclaimable_bytes ?? 0), 0);
  const now = documentTime(plan.generated_at);
  const recordsKept = span(plan.removal_records.keep_seconds);

  return (
    <>
      <Dialog.Description className="dialog-text">
        {ready.length > 0
          ? `residue moves ${plural(ready.length, "worktree")} into its repository's trash. Each branch and its commits stay. Restore puts a worktree back from the trash until residue deletes it from there after ${span(plan.trash.keep_seconds)}, then from the removal record that leaves, for at least ${recordsKept}, without its ignored files.`
          : "residue would keep every one of these worktrees, so nothing moves."}
      </Dialog.Description>

      {ready.length > 0 ? (
        <section className="plan-section">
          <h3>
            To the trash <span className="num">{bytes(total)}</span>
          </h3>
          <ul className="plan-list">
            {ready.map((r) => {
              const p = r.worktree ? displayPath(r.worktree) : r.path;
              return (
                <li key={r.path}>
                  <span className="plan-name">{baseName(p)}</span>
                  <span className="plan-path">{tilde(p, home)}</span>
                  <span className="num">{bytes(r.worktree?.size?.reclaimable_bytes)}</span>
                </li>
              );
            })}
          </ul>
          <p className="hint">
            The trash frees no space yet: a trashed worktree keeps its {bytes(total)} until
            residue deletes it, {span(plan.trash.keep_seconds)} from now, during a later removal or automatic
            cleanup in that repository.
          </p>
        </section>
      ) : null}

      {kept.length > 0 ? (
        <section className="plan-section">
          <h3>Stays</h3>
          <ul className="plan-list">
            {kept.map((r) => {
              const p = r.worktree ? displayPath(r.worktree) : r.path;
              const why = r.worktree ? keptBy(r.worktree)[0] : undefined;
              return (
                <li key={r.path}>
                  <span className="plan-name">{baseName(p)}</span>
                  <span className="plan-path">
                    {why ? `${checkLabel(why.id)}: ${why.detail}` : (r.reason ?? r.result)}
                  </span>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}

      {purges.length > 0 ? (
        <section className="plan-section">
          <h3>Deleted from the trash</h3>
          <p className="hint">
            The same run deletes these trash entries, older than {span(plan.trash.keep_seconds)}. Each
            leaves a removal record, listed in Restore for at least {recordsKept}: it recreates the
            worktree on its branch and HEAD, without its ignored files.
          </p>
          <ul className="plan-list">
            {purges.map((p) => (
              <li key={p.entry.entry_path}>
                <span className="plan-name">{p.entry.entry}</span>
                <span className="plan-path">{tilde(p.entry.original_path ?? p.entry.entry_path, home)}</span>
                <span className="num">{bytes(p.entry.reclaimable_bytes)}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {forgets.length > 0 ? (
        <section className="plan-section">
          <h3>Removal records forgotten</h3>
          <p className="hint">
            The same run forgets these removal records, each older than both {recordsKept} (
            <Flags flags={["--keep-removed"]} />) and the <Flags flags={["--keep-removed"]} /> it
            was written with. Each worktree was deleted already. Forgetting deletes its record and
            the ref that keeps its HEAD, so Restore can no longer bring it back. Branches stay. A commit only the record
            held, such as one on a branch deleted since, is left to the next git gc, which can
            delete it.
          </p>
          <ul className="plan-list">
            {forgets.map((f) => (
              <li key={f.entry.entry_path}>
                <span className="plan-name">{baseName(f.entry.original_path ?? f.entry.entry)}</span>
                <span className="plan-path">
                  {/* An unfinished record residue cannot read is forgotten an hour on; its error says why. */}
                  {f.entry.error ??
                    ["removed", ageOf(f.entry.removed_at, now), removedBy(f.entry.reason)].filter(Boolean).join(" ")}
                </span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {plan.errors.length > 0 ? (
        <ul className="plan-errors">
          {plan.errors.map((e) => (
            <li key={e}>{e}</li>
          ))}
        </ul>
      ) : null}

      {plan.warnings.length > 0 ? (
        <section className="plan-section">
          <h3>Warnings</h3>
          <ul className="plan-errors">
            {plan.warnings.map((w) => (
              <li key={w}>{w}</li>
            ))}
          </ul>
        </section>
      ) : null}

      <div className="dialog-buttons">
        <Button variant="secondary" onClick={onCancel} disabled={running}>
          Cancel
        </Button>
        {ready.length > 0 ? (
          <SolidButton
            icon={<Trash />}
            loading={running}
            disabled={running}
            onClick={() => onConfirm(ready.map((r) => r.path))}
          >
            {running ? "Moving…" : `Move ${plural(ready.length, "worktree")} to trash`}
          </SolidButton>
        ) : null}
      </div>
    </>
  );
}

const RESULT_TEXT: Record<string, string> = {
  trashed: "in the trash",
  removed: "deleted",
  refused: "kept",
  skipped: "not moved",
  failed: "failed",
  invalid: "not a worktree",
};

function Result({
  doc,
  home,
  estimateNote,
  onClose,
}: {
  doc: RemoveDocument;
  home: string | undefined;
  estimateNote: string | undefined;
  onClose: () => void;
}) {
  const freed = removalLine(doc.stats, doc.trash.keep);
  const moved = doc.results.filter((r) => r.result === "trashed");
  const purged = doc.purged.filter((p) => p.result === "purged");
  const forgotten = doc.forgotten.filter((f) => f.result === "forgotten");
  const other = doc.results.filter((r) => r.result !== "trashed");
  return (
    <>
      <Dialog.Description className="dialog-text">
        {moved.length > 0
          ? `${plural(moved.length, "worktree")} moved to the trash. Restore puts each one back.`
          : "Nothing moved."}
      </Dialog.Description>
      <ul className="plan-list">
        {doc.results.map((r) => {
          const p = r.worktree ? displayPath(r.worktree) : r.path;
          return (
            <li key={r.path} data-result={r.result}>
              <span className="plan-name">{baseName(p)}</span>
              <span className="plan-path" title={r.reason ?? undefined}>
                {r.reason && r.result !== "trashed" ? r.reason : tilde(p, home)}
              </span>
              <span className="plan-result">{RESULT_TEXT[r.result] ?? r.result}</span>
            </li>
          );
        })}
      </ul>
      {freed ? (
        // residue's own line, as `residue remove` ends, with its note on how sizes are measured.
        <p className="hint">
          {freed.charAt(0).toUpperCase()}
          {freed.slice(1)}.{estimateNote ? ` ${estimateNote}` : ""}
        </p>
      ) : null}
      {purged.length > 0 ? (
        <p className="hint">
          Deleted {plural(purged.length, "old trash entry", "old trash entries")}, freeing{" "}
          {bytes(purged.reduce((s, p) => s + (p.entry.reclaimable_bytes ?? 0), 0))}. Each left a
          removal record, listed in Restore.
        </p>
      ) : null}
      {forgotten.length > 0 ? (
        <p className="hint">
          Forgot {plural(forgotten.length, "removal record")}, each older than both{" "}
          {span(doc.removal_records.keep_seconds)} and the <Flags flags={["--keep-removed"]} /> it
          was written with.
        </p>
      ) : null}
      {other.length > 0 || doc.errors.length > 0 ? (
        <ul className="plan-errors">
          {doc.errors.map((e) => (
            <li key={e}>{e}</li>
          ))}
        </ul>
      ) : null}
      {doc.warnings.length > 0 ? (
        <section className="plan-section">
          <h3>Warnings</h3>
          <ul className="plan-errors">
            {doc.warnings.map((w) => (
              <li key={w}>{w}</li>
            ))}
          </ul>
        </section>
      ) : null}
      <div className="dialog-buttons">
        <SolidButton onClick={onClose}>
          Close
        </SolidButton>
      </div>
    </>
  );
}
