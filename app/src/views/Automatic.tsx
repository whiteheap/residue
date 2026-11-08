import { Button, Checkbox, Dialog } from "@cloudflare/kumo";
import { ArrowClockwise, Eye, FolderPlus, Info, Warning } from "@phosphor-icons/react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useMemo, useState } from "react";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { Command, ErrorPanel, Flags, Loading, PageHeader, SolidButton, Toggle } from "../components/ui";
import { Meter } from "../components/viz/Meter";
import { api, asCliError } from "../lib/api";
import { baseName, bytes, plural, shellWord, someOf, tilde } from "../lib/format";
import type {
  AppInfo,
  CliError,
  ScanDocument,
  ServiceChange,
  ServicePlan,
  ServicePreview,
  ServiceProgram,
  ServiceStatus,
  WatchFetch,
  WatchSettings,
} from "../lib/types";
import { useCli } from "../lib/useCli";
import { cleanupLevels, diskMeters, type CleanupLevels, type DiskMeter } from "../lib/viz";
import { diskLow, duration, fetchWords, settingFlags, settingLines } from "../lib/watch";

export function Automatic({
  info,
  roots,
  suggested,
  home,
  refreshKey,
  scanDoc,
  scanning,
}: {
  info: AppInfo | null;
  roots: string[];
  suggested: string[];
  home: string | undefined;
  refreshKey: number;
  /** The Worktrees scan, which reads each repository's free space. */
  scanDoc: ScanDocument | undefined;
  scanning: boolean;
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
        description="residue watch runs at login and removes worktrees whose work landed, or when the disk runs low."
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
              RESIDUE_APP_DEV_SERVICE=preview: the switch never installs or uninstalls the service. It
              shows the command and runs only residue watch --once --dry-run, which removes nothing.
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
                    <dd>log only (--dry-run): removes nothing</dd>
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

        <FreeSpace
          doc={scanDoc}
          scanning={scanning}
          settings={status ? (on && program ? program.settings : status.plan.settings) : null}
          on={on}
        />

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
          <p className="quiet">The service runs without the app. Turn it off from a terminal:</p>
          {/* The binary the service runs: the app's own copy is not on PATH. */}
          <Command
            text={`${program ? shellWord(program.binary) : "residue"} service uninstall`}
          />
          {info?.path_source === "fallback" ? (
            <p className="hint">
              The app could not read your login shell's PATH, so residue uses the standard folders
              only. The service may not find a Git installed elsewhere.
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
      {". Unknown to this app: "}
      <code>
        <Flags flags={each} />
      </code>
      ; a save keeps {each.length === 1 ? "it" : "them"} as written
    </>
  );
}

/** Whether the preview fetches, as the service would: before it runs (`done` false) or after. */
function previewFetch(fetch: WatchFetch, done: boolean): string {
  switch (fetch.mode) {
    case "every":
      return done
        ? `It fetched first, as the service does about every ${duration(fetch.value)}, so a branch deleted on the remote shows here. A fetch changes only remote-tracking refs.`
        : "It fetches first, as the service does, which can take a while. A fetch changes only remote-tracking refs.";
    case "off":
      return done
        ? "It did not fetch (--no-fetch), like the service: a branch deleted on the remote shows after your next git fetch --prune."
        : "It does not fetch (--no-fetch), like the service.";
    case "never":
      return `A residue older than ${fetch.since} never fetches, so neither ${done ? "did" : "does"} the preview.`;
    default:
      return `Whether it ${done ? "fetched" : "fetches"} depends on the residue version, which the app could not read.`;
  }
}

/**
 * One meter per disk the scanned repositories are on: how full it is, where
 * automatic cleanup starts removing (below `--min-free`) and the free space
 * it removes until. residue reads free space per repository; a newer residue
 * says which disk each is on, an older one leaves equal totals to match on.
 */
function FreeSpace({
  doc,
  scanning,
  settings,
  on,
}: {
  doc: ScanDocument | undefined;
  scanning: boolean;
  settings: WatchSettings | null;
  on: boolean;
}) {
  const { disks, unread } = useMemo(() => (doc ? diskMeters(doc) : { disks: [], unread: 0 }), [doc]);
  const levels = settings ? cleanupLevels(settings) : null;
  const byTotals = disks.some((d) => d.by === "totals");
  return (
    <section className="block" aria-labelledby="free-space-title" aria-busy={scanning}>
      <h2 id="free-space-title">Free space</h2>
      {!doc ? (
        <p className="quiet">
          {scanning ? "Free space appears when the scan finishes." : "Free space appears after a scan."}
        </p>
      ) : disks.length === 0 ? (
        <p className="quiet">residue could not read the free space of any scanned repository.</p>
      ) : (
        <>
          <div className="disks" data-busy={scanning}>
            {disks.map((d) => (
              <Disk key={d.key} disk={d} levels={levels} on={on} />
            ))}
          </div>
          <p className="viz-caption">
            Each disk the scanned repositories are on, from the last scan.
            {levels
              ? ` The marks show where automatic cleanup ${on ? "starts" : "would start"} removing idle worktrees, and where it stops.`
              : ""}
            {byTotals
              ? " This residue does not name each repository's disk, so disks of the same size show as one."
              : ""}
            {unread > 0 ? ` residue could not read the free space of ${plural(unread, "repository", "repositories")}.` : ""}
          </p>
        </>
      )}
    </section>
  );
}

function Disk({ disk, levels, on }: { disk: DiskMeter; levels: CleanupLevels | null; on: boolean }) {
  const name = someOf(disk.repos.map((r) => baseName(r.main_path)));
  const free = `${bytes(disk.free)} free of ${bytes(disk.total)}, ${disk.freePercent.toFixed(1)}%`;
  const low = levels !== null && disk.freePercent < levels.start;
  return (
    <div className="disk">
      <p className="disk-name">{name}</p>
      <Meter
        value={disk.total - disk.free}
        max={disk.total}
        label={`Disk used, with ${name}`}
        valueText={free}
        ticks={
          levels
            ? [
                { at: 1 - levels.start / 100, label: `starts below ${levels.start}% free`, side: "above" },
                { at: 1 - levels.until / 100, label: `removes until ${levels.until}% free`, side: "below" },
              ]
            : []
        }
      />
      <p className="disk-free num">{free}</p>
      {low ? (
        <p className="disk-low">
          <Warning aria-hidden weight="bold" /> Below {levels.start}% free:{" "}
          {on
            ? "automatic cleanup removes idle worktrees at its next free-space read."
            : "automatic cleanup would remove idle worktrees once turned on."}
        </p>
      ) : null}
    </div>
  );
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
          Work landed: its upstream branch is gone and it is unchanged for{" "}
          {duration(settings.grace.value)}. {fetchWords(settings.fetch, version)}
        </li>
        <li>
          Disk below {low.below} free: worktrees unchanged for {duration(settings.idle.value)},
          largest first, until {low.until} is free.
        </li>
      </ul>
      <ul className="plain-list bullets">
        <li>
          Every check runs first: changes, untracked files, a running process or a lock keep a
          worktree.
        </li>
        <li>It deletes instead of using the trash. Branches and commits stay.</li>
        <li>
          Restore recreates what it deleted for at least {duration(settings.keep_removed.value)} (
          {settings.keep_removed.default ? "residue's default " : "the service's "}
          <Flags flags={["--keep-removed"]} />
          ).
        </li>
        <li>
          It also deletes trash entries older than {duration(settings.keep_trash.value)} (residue's
          default <Flags flags={["--keep-trash"]} />) in each repository it removes from.
        </li>
      </ul>
      <details className="viz-details">
        <summary>When a removal record is forgotten</summary>
        <p className="hint">
          No run forgets a record before the <Flags flags={["--keep-removed"]} /> it was written
          with, and automatic cleanup forgets one only once it is also older than its own. A
          forgotten record cannot be restored.
        </p>
      </details>
      <p className="hint">
        {set.length === 0 ? (
          running ? (
            "The service sets no options."
          ) : null
        ) : (
          <>
            {running ? "The service sets " : "Turning it on keeps "}
            <Flags flags={set} />.
          </>
        )}
        {defaults.length > 0 ? (
          <>
            {set.length > 0
              ? " residue's defaults for the rest: "
              : running
                ? " It uses residue's defaults: "
                : "Turning it on uses residue's defaults: "}
            <Flags flags={defaults} />.
          </>
        ) : set.length === 0 && !running ? (
          "Turning it on uses residue's defaults."
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
              <Loading title="Reading the service" />
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
        Deletes a worktree when its upstream branch is gone and it is unchanged for{" "}
        {duration(settings.grace.value)}, or when the disk is below {low.below} free and it is
        unchanged for {duration(settings.idle.value)}. It starts now and at every login, runs every
        check first, and deletes instead of using the trash; branches and commits stay.
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
        label="Log only, remove nothing (--dry-run)"
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
                  <code>{plan.keep.join(" ")}</code>, kept as written
                  <OtherOptions options={current?.other_options ?? []} />
                </>
              ) : replacing ? (
                "none set, so residue's defaults"
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
            <strong className="warn">Cannot save. </strong>
            {plan.blocked}
          </p>
        ) : null}
      </section>

      <section className="plan-section">
        <div className="plan-section-head">
          <h3>What it would remove now</h3>
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
              {previewed.lines.length > 0 ? previewed.lines.join("\n") : "Nothing to remove now."}
            </pre>
            <p className="hint">{previewFetch(previewed.fetch, true)}</p>
          </>
        ) : (
          <p className="hint">
            One pass of the service as saved, with <Flags flags={["--dry-run"]} />, which removes
            nothing. {previewFetch(settings.fetch, false)}
          </p>
        )}
      </section>

      {changedSince ? (
        <div className="notice" role="alert">
          <p>
            <strong className="warn">Nothing saved: the service changed after this dialog opened. </strong>
            Check it as it is now, then save again.
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
          {replacing ? "Save folders" : "Turn on automatic cleanup"}
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
          Stops residue watch and removes its login item. Worktrees and the log stay.
        </Dialog.Description>
        {error ? <ErrorPanel title="residue could not turn it off" error={error} /> : null}
        <div className="dialog-buttons">
          <Button variant="secondary" onClick={onClose} disabled={working}>
            Cancel
          </Button>
          <SolidButton loading={working} disabled={working} onClick={off}>
            Turn off automatic cleanup
          </SolidButton>
        </div>
      </Dialog>
    </Dialog.Root>
  );
}
