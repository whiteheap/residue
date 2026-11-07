import { Button, Dialog } from "@cloudflare/kumo";
import { Trash } from "@phosphor-icons/react";
import { useEffect, useState } from "react";
import { ErrorPanel, Loading, SolidButton } from "../components/ui";
import { api, asCliError } from "../lib/api";
import { baseName, bytes, plural, span, tilde } from "../lib/format";
import type { CliError, RemoveDocument } from "../lib/types";
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
 * checked again, and the expired trash entries the run would purge. The user
 * confirms exactly that list; then the same command with --yes.
 */
export function RemoveDialog({
  paths,
  home,
  onClose,
  onRemoved,
}: {
  paths: string[];
  home: string | undefined;
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

        {phase.step === "done" ? <Result doc={phase.result} home={home} onClose={onClose} /> : null}

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
  const total = ready.reduce((sum, r) => sum + (r.worktree?.size?.reclaimable_bytes ?? 0), 0);

  return (
    <>
      <Dialog.Description className="dialog-text">
        {ready.length > 0
          ? `residue moves ${plural(ready.length, "worktree")} into its repository's trash. Each branch and its commits stay. Restore puts a worktree back until residue deletes it from the trash after ${span(plan.trash.keep_seconds)}.`
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
          <h3>Deleted from the trash for good</h3>
          <p className="hint">
            The same run deletes these trash entries, older than {span(plan.trash.keep_seconds)}. They cannot
            be restored afterwards.
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

      {plan.errors.length > 0 ? (
        <ul className="plan-errors">
          {plan.errors.map((e) => (
            <li key={e}>{e}</li>
          ))}
        </ul>
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

function Result({ doc, home, onClose }: { doc: RemoveDocument; home: string | undefined; onClose: () => void }) {
  const moved = doc.results.filter((r) => r.result === "trashed");
  const purged = doc.purged.filter((p) => p.result === "purged");
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
      {purged.length > 0 ? (
        <p className="hint">
          Deleted {plural(purged.length, "old trash entry", "old trash entries")} for good, freeing{" "}
          {bytes(purged.reduce((s, p) => s + (p.entry.reclaimable_bytes ?? 0), 0))}.
        </p>
      ) : null}
      {other.length > 0 || doc.errors.length > 0 ? (
        <ul className="plan-errors">
          {doc.errors.map((e) => (
            <li key={e}>{e}</li>
          ))}
        </ul>
      ) : null}
      <div className="dialog-buttons">
        <SolidButton onClick={onClose}>
          Close
        </SolidButton>
      </div>
    </>
  );
}
