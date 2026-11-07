import { Button, Checkbox, Dialog } from "@cloudflare/kumo";
import { ArrowClockwise, Eye, FolderPlus, Info } from "@phosphor-icons/react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useState } from "react";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { Command, ErrorPanel, Flags, Loading, PageHeader, SolidButton, Toggle } from "../components/ui";
import { api, asCliError } from "../lib/api";
import { shellWord, tilde } from "../lib/format";
import type {
  AppInfo,
  CliError,
  ServiceChange,
  ServicePlan,
  ServicePreview,
  ServiceProgram,
  ServiceStatus,
  WatchFetch,
} from "../lib/types";
import { useCli } from "../lib/useCli";
import { diskLow, duration, fetchWords, settingFlags, settingLines } from "../lib/watch";

export function Automatic({
  info,
  roots,
  suggested,
  home,
  refreshKey,
}: {
  info: AppInfo | null;
  roots: string[];
  suggested: string[];
  home: string | undefined;
  refreshKey: number;
}) {
  const q = useCli(`service:${refreshKey}`, api.serviceStatus);
  const status: ServiceStatus | null = q.data ?? null;
  const error = q.error;
  const loading = q.loading;
  const load = q.reload;
  const [dialog, setDialog] = useState<"on" | "off" | null>(null);
  const [change, setChange] = useState<ServiceChange | null>(null);

  const changed = (c: ServiceChange) => {
    setChange(c);
    setDialog(null);
    load();
  };

  // The save dialog reads the service itself. When what it last read is not
  // what this screen shows, the screen reads it again.
  const closeSave = (seen: ServiceStatus | null) => {
    setDialog(null);
    if (seen && JSON.stringify(seen.basis) !== JSON.stringify(status?.basis)) load();
  };

  const on = status?.installed ?? false;
  const program = status?.program ?? null;
  const preview = info?.dev_service_preview ?? false;

  return (
    <>
      <PageHeader
        title="Automatic cleanup"
        description="residue watch runs at login and removes a worktree when its work has landed, or when the disk runs low."
        actions={
          <Button variant="secondary" icon={<ArrowClockwise />} loading={loading} disabled={loading} onClick={load}>
            Refresh
          </Button>
        }
      />
      <div className="page-body">
        {preview ? (
          <section className="notice dev">
            <h2>Development preview</h2>
            <p>
              RESIDUE_APP_DEV_SERVICE=preview is set in this debug build. The switch never runs
              residue service install or uninstall: it shows the command it would run and the
              output of residue watch --once --dry-run, which removes nothing.
            </p>
          </section>
        ) : null}
        {error ? <ErrorPanel title="residue could not read the service" error={error} /> : null}
        {!status && loading ? <Loading title="Reading the service" /> : null}

        {status ? (
          <section className="service">
            <div className="service-switch">
              <Toggle
                checked={on}
                name="Automatic cleanup"
                label={on ? "On" : "Off"}
                busy={loading}
                onChange={(next) => setDialog(next ? "on" : "off")}
              />
              <p className="service-summary">{status.summary}</p>
            </div>
            {on && program ? (
              <dl className="facts">
                <div>
                  <dt>Watching</dt>
                  <dd>
                    {program.roots.length > 0 ? program.roots.map((r) => tilde(r, home)).join(", ") : "no folders"}
                  </dd>
                </div>
                <div>
                  <dt>Runs</dt>
                  <dd>
                    <code>{tilde(program.binary, home)}</code>
                    {program.version ? ` (${program.version})` : ""}
                    {program.managed ? ", the app's own copy, kept outside the app" : ""}
                    {!program.binary_exists ? (
                      <strong className="warn"> missing: turn automatic cleanup off and on again</strong>
                    ) : null}
                  </dd>
                </div>
                {program.dry_run ? (
                  <div>
                    <dt>Mode</dt>
                    <dd>log only (--dry-run): it records what it would remove and removes nothing</dd>
                  </div>
                ) : null}
                {program.options.length > 0 ? (
                  <div>
                    <dt>Options</dt>
                    <dd>
                      <code>{program.options.join(" ")}</code>
                      <OtherOptions options={program.other_options} />
                    </dd>
                  </div>
                ) : null}
                {program.unknown.length > 0 ? (
                  <div>
                    <dt>Cannot read</dt>
                    <dd>
                      <code>{program.unknown.join(" ")}</code>: neither an option nor a folder
                    </dd>
                  </div>
                ) : null}
                {status.agent_file ? (
                  <div>
                    <dt>Agent file</dt>
                    <dd>
                      <code>{tilde(status.agent_file, home)}</code>
                    </dd>
                  </div>
                ) : null}
              </dl>
            ) : null}
            {on ? (
              <div className="service-actions">
                <Button variant="secondary" onClick={() => setDialog("on")}>
                  Change folders…
                </Button>
              </div>
            ) : null}
          </section>
        ) : null}

        {change ? (
          <section className="block">
            <h2>{change.dev_dry_run ? "Would run" : "Ran"}</h2>
            <Command text={change.command} />
            {change.output.length > 0 ? <pre className="log">{change.output.join("\n")}</pre> : null}
          </section>
        ) : null}

        {status ? <WhatItRemoves on={on} program={program} plan={status.plan} /> : null}

        {status ? (
          <section className="block">
            <h2>Log</h2>
            {status.log_path ? (
              <p className="quiet">
                <code>{tilde(status.log_path, home)}</code>
              </p>
            ) : null}
            {status.log_error ? <p className="hint">{status.log_error}</p> : null}
            {status.log_lines.length > 0 ? (
              <pre className="log full" aria-label="Recent log lines">
                {status.log_lines.join("\n")}
              </pre>
            ) : (
              <p className="quiet">No log lines yet.</p>
            )}
          </section>
        ) : null}

        <section className="block">
          <h2>Without the app</h2>
          <p className="quiet">The service does not need the app. To turn it off from a terminal:</p>
          {/* The binary the service runs: the app's own copy is not on PATH. */}
          <Command
            text={`${program ? shellWord(program.binary) : "residue"} service uninstall`}
          />
          {info?.path_source === "fallback" ? (
            <p className="hint">
              The app could not read your login shell's PATH, so residue runs with the standard
              folders only. If Git lives somewhere else, the service may not find it.
            </p>
          ) : null}
        </section>
      </div>

      {dialog !== null ? (
        // The dialogs' own boundary: a failure to draw one stays in a dialog,
        // and closing it reads the service again, since a change may have run.
        <ErrorBoundary
          onClose={() => {
            setDialog(null);
            load();
          }}
        >
          {dialog === "on" ? (
            <TurnOnDialog
              appRoots={roots}
              suggested={suggested}
              home={home}
              onClose={closeSave}
              onChanged={changed}
              onFailed={load}
            />
          ) : null}
          {dialog === "off" ? <TurnOffDialog onClose={() => setDialog(null)} onChanged={changed} /> : null}
        </ErrorBoundary>
      ) : null}
    </>
  );
}

/** The service's options this app does not know, such as one a newer residue added. */
function OtherOptions({ options }: { options: string[] }) {
  // Each option, with the value after it: a value never starts with "-" (`parse_watch`).
  const each = options.reduce<string[]>((out, arg) => {
    if (arg.startsWith("-") || out.length === 0) out.push(arg);
    else out[out.length - 1] += ` ${arg}`;
    return out;
  }, []);
  if (each.length === 0) return null;
  return (
    <>
      {". The app does not know "}
      <code>
        <Flags flags={each} />
      </code>
      , and a save passes {each.length === 1 ? "it" : "them"} on as written
    </>
  );
}

/** Whether the preview fetches, as the service would: before it runs (`done` false) or after. */
function previewFetch(fetch: WatchFetch, done: boolean): string {
  switch (fetch.mode) {
    case "every":
      return done
        ? `It fetched each repository first, as the service does on its first pass and about every ${duration(fetch.value)} after, so a worktree whose branch was deleted on the remote shows here. A fetch changes only remote-tracking refs.`
        : "Like the service's first pass, it fetches each repository first, which can take a while. A fetch changes only remote-tracking refs.";
    case "off":
      return done
        ? "It did not fetch (--no-fetch), as the service does not: a branch deleted on the remote shows after your next git fetch --prune."
        : "It does not fetch (--no-fetch), as the service does not.";
    case "never":
      return `A residue older than ${fetch.since} never fetches, so neither ${done ? "did" : "does"} the preview.`;
    default:
      return `Whether it ${done ? "fetched" : "fetches"} depends on the residue version, which the app could not read.`;
  }
}

/** What the service does, from its arguments; when it is off, what turning it on installs. */
function WhatItRemoves({
  on,
  program,
  plan,
}: {
  on: boolean;
  program: ServiceProgram | null;
  plan: ServicePlan;
}) {
  const running = on && program ? program : null;
  const settings = running ? running.settings : plan.settings;
  const version = running ? running.version : plan.version;
  const low = diskLow(settings);
  const { set, defaults } = settingFlags(settings);
  return (
    <section className="block what">
      <h2>
        <Info aria-hidden /> {running ? "What it removes" : "What it would remove"}
      </h2>
      <ul className="plain-list bullets">
        <li>
          A worktree whose work landed: its upstream branch was deleted, as merging a pull request
          with branch deletion does, and the worktree has not changed for{" "}
          {duration(settings.grace.value)}. {fetchWords(settings.fetch, version)}
        </li>
        <li>
          When a disk is below {low.below} free: worktrees unchanged for{" "}
          {duration(settings.idle.value)}, largest first, until {low.until} is free.
        </li>
      </ul>
      <p>
        Every check runs before each removal, so a worktree with changes, untracked files or a
        running process stays. Automatic cleanup deletes the worktree instead of using the trash,
        because freeing the disk is its job. The branch and its commits stay, and a locked worktree
        stays. In each repository it removes from, it also deletes trash entries older than{" "}
        {duration(settings.keep_trash.value)}, residue's default <Flags flags={["--keep-trash"]} />, and
        forgets the removal records older than {duration(settings.keep_removed.value)},{" "}
        {settings.keep_removed.default ? "residue's default " : "its "}
        <Flags flags={["--keep-removed"]} />. Until then, Restore recreates a worktree it deleted.
      </p>
      <p className="hint">
        {set.length === 0 ? (
          running ? (
            "The service sets no options."
          ) : (
            "Turning it on starts from residue's defaults."
          )
        ) : (
          <>
            {running ? "From the service's arguments: " : "Turning it on keeps "}
            <Flags flags={set} />.
          </>
        )}
        {defaults.length > 0 ? (
          <>
            {" residue's defaults, where it sets none: "}
            <Flags flags={defaults} />.
          </>
        ) : null}
        {running ? "" : ` It would run ${version ?? "a residue of unknown version"}.`}
      </p>
    </section>
  );
}

/** Where the dialog starts: the running service's folders and mode, or the app's folders, live. */
function startFrom(status: ServiceStatus, appRoots: string[], suggested: string[]) {
  const running = status.installed ? status.program : null;
  return {
    roots: running?.roots.length ? running.roots : appRoots.length ? appRoots : suggested,
    dryRun: running?.dry_run ?? false,
  };
}

function sameSet(set: Set<string>, list: string[]): boolean {
  const other = new Set(list);
  return set.size === other.size && list.every((x) => set.has(x));
}

/**
 * Turn the service on, or save other folders. The dialog plans from its own
 * read of the service, made when it opens, never from the screen's, which can
 * be older: a `brew upgrade` or a terminal can change the service while the
 * window is open. A save sends that read's basis back, and installs nothing
 * when a fresh read differs.
 */
function TurnOnDialog({
  appRoots,
  suggested,
  home,
  onClose,
  onChanged,
  onFailed,
}: {
  /** The folders the app scans: where a service that is off starts. */
  appRoots: string[];
  suggested: string[];
  home: string | undefined;
  /** Closed without a save: the service as the dialog last read it. */
  onClose: (seen: ServiceStatus | null) => void;
  onChanged: (c: ServiceChange) => void;
  /** A save failed: read the service again, since it may have changed. */
  onFailed: () => void;
}) {
  // This dialog's own read, made when it opens. A save that finds the service
  // changed replaces it with the fresh read it returns (`adopted`).
  const q = useCli("save-dialog", api.serviceStatus);
  const [adopted, setAdopted] = useState<ServiceStatus | null>(null);
  const status = adopted ?? q.data ?? null;
  const [working, setWorking] = useState<"preview" | "install" | null>(null);

  const close = () => onClose(status);

  return (
    <Dialog.Root
      role="alertdialog"
      open
      onOpenChange={(open) => {
        if (!open && !working) close();
      }}
    >
      <Dialog size="xl" className="dialog">
        {status ? (
          <SaveForm
            status={status}
            appRoots={appRoots}
            suggested={suggested}
            home={home}
            working={working}
            setWorking={setWorking}
            onAdopt={setAdopted}
            onCancel={close}
            onChanged={onChanged}
            onFailed={onFailed}
          />
        ) : (
          <>
            <Dialog.Title className="dialog-title">Automatic cleanup</Dialog.Title>
            {q.error ? (
              <ErrorPanel
                title="residue could not read the service"
                error={q.error}
                action={
                  <Button variant="secondary" onClick={q.reload}>
                    Try again
                  </Button>
                }
              />
            ) : (
              <Loading title="Reading the service" detail="A save plans from what residue reports now." />
            )}
            <div className="dialog-buttons">
              <Button variant="secondary" onClick={close}>
                Cancel
              </Button>
            </div>
          </>
        )}
      </Dialog>
    </Dialog.Root>
  );
}

function SaveForm({
  status,
  appRoots,
  suggested,
  home,
  working,
  setWorking,
  onAdopt,
  onCancel,
  onChanged,
  onFailed,
}: {
  /** The dialog's read: everything below plans from it. */
  status: ServiceStatus;
  appRoots: string[];
  suggested: string[];
  home: string | undefined;
  working: "preview" | "install" | null;
  setWorking: (w: "preview" | "install" | null) => void;
  /** A save found the service changed: plan from this read from now on. */
  onAdopt: (status: ServiceStatus) => void;
  onCancel: () => void;
  onChanged: (c: ServiceChange) => void;
  onFailed: () => void;
}) {
  const [options, setOptions] = useState<string[]>(() => [
    ...new Set([...startFrom(status, appRoots, suggested).roots, ...suggested]),
  ]);
  const [chosen, setChosen] = useState<Set<string>>(
    () => new Set(startFrom(status, appRoots, suggested).roots),
  );
  const [dryRun, setDryRun] = useState(() => startFrom(status, appRoots, suggested).dryRun);
  const [previewed, setPreviewed] = useState<ServicePreview | null>(null);
  const [error, setError] = useState<CliError | null>(null);
  const [changedSince, setChangedSince] = useState<string[] | null>(null);

  const plan = status.plan;
  const current = status.installed ? status.program : null;
  const replacing = status.installed;
  const roots = options.filter((o) => chosen.has(o));
  const settings = plan.settings;
  const low = diskLow(settings);
  const changes = current ? changedSettings(current, plan) : [];

  const add = async () => {
    const picked = await openDialog({ directory: true, multiple: true, title: "Folders to watch" });
    const list = picked == null ? [] : Array.isArray(picked) ? picked : [picked];
    if (list.length === 0) return;
    setOptions((o) => [...new Set([...o, ...list])]);
    setChosen((c) => new Set([...c, ...list]));
    setPreviewed(null);
  };

  const toggle = (root: string, on: boolean) => {
    setChosen((c) => {
      const next = new Set(c);
      if (on) next.add(root);
      else next.delete(root);
      return next;
    });
    setPreviewed(null);
  };

  const preview = async () => {
    setWorking("preview");
    setError(null);
    try {
      setPreviewed(await api.servicePreview(roots));
    } catch (e) {
      setError(asCliError(e));
    } finally {
      setWorking(null);
    }
  };

  const install = async () => {
    setWorking("install");
    setError(null);
    setChangedSince(null);
    try {
      const result = await api.serviceInstall(roots, dryRun, status.basis);
      if (result.outcome === "saved") {
        onChanged(result.change);
        return;
      }
      // Nothing was installed. The folders and the mode follow the fresh
      // read where the user had left them as the dialog started.
      const before = startFrom(status, appRoots, suggested);
      const after = startFrom(result.status, appRoots, suggested);
      setOptions((o) => [...new Set([...o, ...after.roots])]);
      setChosen((c) => (sameSet(c, before.roots) ? new Set(after.roots) : c));
      setDryRun((d) => (d === before.dryRun ? after.dryRun : d));
      setPreviewed(null);
      setChangedSince(result.changes);
      onAdopt(result.status);
      setWorking(null);
    } catch (e) {
      setError(asCliError(e));
      setWorking(null);
      onFailed();
    }
  };

  return (
    <>
      <Dialog.Title className="dialog-title">
        {replacing ? "Change the watched folders" : "Turn on automatic cleanup"}
      </Dialog.Title>
      <Dialog.Description className="dialog-text">
        residue watch starts now and at every login. It deletes a worktree in the folders below
        when its upstream branch is gone and it has not changed for{" "}
        {duration(settings.grace.value)}, or when the disk is below {low.below} free and it has
        not changed for {duration(settings.idle.value)}. Every check runs first. It deletes
        instead of using the trash; branches and commits stay.
      </Dialog.Description>

      <fieldset className="folder-choice">
        <legend>Folders to watch</legend>
        {options.map((root) => (
          <Checkbox
            key={root}
            label={tilde(root, home)}
            checked={chosen.has(root)}
            onCheckedChange={(on) => toggle(root, on === true)}
          />
        ))}
        <Button variant="ghost" size="sm" icon={<FolderPlus />} onClick={add}>
          Add a folder…
        </Button>
      </fieldset>

      <Checkbox
        label="Log only: record what it would remove, and remove nothing (--dry-run)"
        checked={dryRun}
        onCheckedChange={(on) => setDryRun(on === true)}
      />

      <section className="plan-section" aria-labelledby="save-settings">
        <h3 id="save-settings">{replacing ? "What a save keeps" : "Settings"}</h3>
        <dl className="facts">
          <div>
            <dt>Runs</dt>
            <dd>
              <code>{tilde(plan.binary, home)}</code>
              {plan.version ? ` (${plan.version})` : " (version unknown)"}
              {plan.managed ? ", the app's own copy, kept outside the app" : ""}
            </dd>
          </div>
          <div>
            <dt>Options</dt>
            <dd>
              {plan.keep.length > 0 ? (
                <>
                  <code>{plan.keep.join(" ")}</code>, kept from the service as written
                  <OtherOptions options={current?.other_options ?? []} />
                </>
              ) : replacing ? (
                "the service sets none, so residue's defaults apply"
              ) : (
                "residue's defaults"
              )}
            </dd>
          </div>
          <div>
            <dt>Fetching</dt>
            <dd>{fetchWords(settings.fetch, plan.version)}</dd>
          </div>
          {changes.length > 0 ? (
            <div>
              <dt>Changes</dt>
              <dd>{changes.join("; ")}</dd>
            </div>
          ) : null}
        </dl>
        {plan.blocked ? (
          <p className="notice" role="alert">
            <strong className="warn">Saving is off. </strong>
            {plan.blocked}
          </p>
        ) : null}
      </section>

      <section className="plan-section">
        <div className="plan-section-head">
          <h3>What it would remove right now</h3>
          <Button
            variant="secondary"
            size="sm"
            icon={<Eye />}
            loading={working === "preview"}
            disabled={working !== null || roots.length === 0}
            onClick={preview}
          >
            Preview
          </Button>
        </div>
        {previewed ? (
          <>
            <Command text={previewed.command} />
            <pre className="log full" aria-label="Preview">
              {previewed.lines.length > 0 ? previewed.lines.join("\n") : "Nothing to remove right now."}
            </pre>
            <p className="hint">{previewFetch(previewed.fetch, true)}</p>
          </>
        ) : (
          <p className="hint">
            Runs one pass of the service as a save sets it up:{" "}
            <span className="flag">residue watch --once --dry-run</span> on these folders, with every
            option above, by the residue under Runs
            {plan.managed ? " (for now the one inside the app, which a save copies there)" : ""}.{" "}
            <Flags flags={["--dry-run"]} /> removes nothing. {previewFetch(settings.fetch, false)}
          </p>
        )}
      </section>

      {changedSince ? (
        <div className="notice" role="alert">
          <p>
            <strong className="warn">Nothing was saved: the service changed after this dialog read it. </strong>
            The dialog now shows it as it is. Check it, then save again.
          </p>
          <ul>
            {changedSince.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
        </div>
      ) : null}

      {error ? <ErrorPanel title="residue did not finish" error={error} /> : null}

      <div className="dialog-buttons">
        <Button variant="secondary" onClick={onCancel} disabled={working !== null}>
          Cancel
        </Button>
        <SolidButton
          loading={working === "install"}
          disabled={working !== null || roots.length === 0 || plan.blocked !== null}
          onClick={install}
        >
          {replacing ? "Save" : "Turn on"}
        </SolidButton>
      </div>
    </>
  );
}

/** The settings a save changes, as `Fetching: never -> about every 30 minutes`. */
function changedSettings(current: ServiceProgram, plan: ServicePlan): string[] {
  const before = settingLines(current.settings, current.version);
  const after = settingLines(plan.settings, plan.version);
  return Object.keys(after)
    .filter((k) => before[k] !== after[k])
    .map((k) => `${k}: ${before[k]} → ${after[k]}`);
}

function TurnOffDialog({ onClose, onChanged }: { onClose: () => void; onChanged: (c: ServiceChange) => void }) {
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<CliError | null>(null);
  const off = async () => {
    setWorking(true);
    setError(null);
    try {
      onChanged(await api.serviceUninstall());
    } catch (e) {
      setError(asCliError(e));
      setWorking(false);
    }
  };
  return (
    <Dialog.Root
      role="alertdialog"
      open
      onOpenChange={(open) => {
        if (!open && !working) onClose();
      }}
    >
      <Dialog size="lg" className="dialog">
        <Dialog.Title className="dialog-title">Turn off automatic cleanup?</Dialog.Title>
        <Dialog.Description className="dialog-text">
          residue service uninstall stops residue watch and removes its login item. Worktrees stay
          until you remove them. The log stays.
        </Dialog.Description>
        {error ? <ErrorPanel title="residue could not turn it off" error={error} /> : null}
        <div className="dialog-buttons">
          <Button variant="secondary" onClick={onClose} disabled={working}>
            Cancel
          </Button>
          <SolidButton loading={working} disabled={working} onClick={off}>
            Turn off
          </SolidButton>
        </div>
      </Dialog>
    </Dialog.Root>
  );
}
