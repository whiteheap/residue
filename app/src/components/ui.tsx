import { Button, Loader } from "@cloudflare/kumo";
import { Check, Copy, WarningCircle } from "@phosphor-icons/react";
import { Fragment, useEffect, useRef, useState, type ComponentProps, type ReactNode } from "react";
import type { CliError, Verdict } from "../lib/types";
import { verdictHelp, verdictLabel } from "../lib/worktree";

/**
 * The one filled button on a screen: white on black. The secondary variant
 * colors a disabled label `!text-kumo-default/70`, light on this light fill,
 * so the label keeps its own color and Kumo's `disabled:opacity-50` alone
 * shows that it is off.
 */
export const SOLID =
  "!bg-kumo-contrast !text-kumo-inverse disabled:!text-kumo-inverse !ring-kumo-contrast not-disabled:hover:!bg-kumo-contrast/85";

export function SolidButton(props: ComponentProps<typeof Button>) {
  return <Button variant="secondary" {...props} className={`${SOLID} ${props.className ?? ""}`} />;
}

export function VerdictTag({ verdict, id }: { verdict: Verdict; id?: string }) {
  return (
    <span className="verdict" id={id} data-verdict={verdict} title={verdictHelp(verdict)}>
      {verdictLabel(verdict)}
    </span>
  );
}

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="page-header" data-tauri-drag-region>
      <div className="page-header-text" data-tauri-drag-region>
        <h1 data-tauri-drag-region>{title}</h1>
        {description ? <p data-tauri-drag-region>{description}</p> : null}
      </div>
      {actions ? <div className="page-header-actions">{actions}</div> : null}
    </header>
  );
}

/** residue's own words, with its `backticked` commands set as code. */
export function Words({ text }: { text: string }) {
  const parts = text.split(/`([^`]+)`/);
  return (
    <>
      {parts.map((part, i) =>
        // The split alternates text and code; a part's index is its identity.
        i % 2 === 1 ? <code key={i}>{part}</code> : part,
      )}
    </>
  );
}

/** Options in a sentence, each kept on one line: a browser breaks a line after a hyphen. */
export function Flags({ flags }: { flags: string[] }) {
  return (
    <>
      {flags.map((flag, i) => (
        <Fragment key={flag}>
          {i > 0 ? ", " : null}
          <span className="flag">{flag}</span>
        </Fragment>
      ))}
    </>
  );
}

/** A sentence the app writes, each `--option` in it kept on one line, as `Flags` keeps one. */
export function FlagWords({ text }: { text: string }) {
  const parts = text.split(/(--[a-z][a-z-]*)/);
  return (
    <>
      {parts.map((part, i) =>
        // The split alternates text and options; a part's index is its identity.
        i % 2 === 1 ? (
          <span key={i} className="flag">
            {part}
          </span>
        ) : (
          part
        ),
      )}
    </>
  );
}

const PERMISSION =/operation not permitted|permission denied/i;

/** macOS privacy settings block a folder: the fix is in System Settings. */
export function isPermissionError(text: string | null | undefined): boolean {
  return !!text && PERMISSION.test(text);
}

export function PermissionHint() {
  return (
    <p className="hint">
      macOS kept residue from reading a folder. Open System Settings, Privacy & Security, Files
      and Folders (or Full Disk Access), allow residue, then try again.
    </p>
  );
}

/** A failed residue run, shown as it happened: message, command, stderr. */
export function ErrorPanel({
  title,
  error,
  action,
}: {
  title: string;
  error: CliError;
  action?: ReactNode;
}) {
  const permission = isPermissionError(error.stderr) || isPermissionError(error.message);
  return (
    <section className="error-panel" role="alert">
      <div className="error-panel-head">
        <WarningCircle size={18} weight="bold" aria-hidden />
        <h2>{title}</h2>
      </div>
      <p>{error.message}</p>
      {error.kind === "missing" ? (
        <p className="hint">
          The residue binary inside the app is missing or cannot start. Download the app again
          from github.com/whiteheap/residue/releases.
        </p>
      ) : null}
      {permission ? <PermissionHint /> : null}
      {error.command ? (
        <dl className="error-facts">
          <dt>Command</dt>
          <dd>
            <code>{error.command}</code>
          </dd>
          {error.exit_code != null ? (
            <>
              <dt>Exit status</dt>
              <dd>{error.exit_code}</dd>
            </>
          ) : null}
        </dl>
      ) : null}
      {error.stderr ? (
        <details>
          <summary>What residue printed</summary>
          <pre className="log">{error.stderr}</pre>
        </details>
      ) : null}
      {action ? <div className="error-panel-actions">{action}</div> : null}
    </section>
  );
}

export function Loading({ title, detail }: { title: string; detail?: ReactNode }) {
  return (
    <output className="loading" aria-live="polite">
      <Loader size="base" />
      <span className="loading-text">
        <span className="loading-title">{title}</span>
        {detail ? <span className="loading-detail">{detail}</span> : null}
      </span>
    </output>
  );
}

/** A command the user can copy and run in a terminal. */
export function Command({ text, label }: { text: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // The text stays selectable; the button says it did not copy.
      setCopied(false);
    }
  };
  return (
    <div className="command">
      <code>{text}</code>
      <Button
        variant="ghost"
        size="sm"
        shape="square"
        icon={copied ? <Check /> : <Copy />}
        aria-label={copied ? "Copied" : `Copy ${label ?? "command"}`}
        title={copied ? "Copied" : "Copy"}
        onClick={copy}
      />
    </div>
  );
}

/**
 * An on/off switch: a button with role="switch", black and white. `name` is
 * what it controls, read before the visible `label`.
 */
export function Toggle({
  checked,
  onChange,
  name,
  label,
  busy,
  disabled,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  name: string;
  label: string;
  busy?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-busy={busy || undefined}
      className="toggle"
      disabled={disabled || busy}
      onClick={() => onChange(!checked)}
    >
      <span className="toggle-track" aria-hidden>
        <span className="toggle-thumb" />
      </span>
      <span className="toggle-label">
        <span className="visually-hidden">{name} </span>
        {label}
      </span>
    </button>
  );
}
