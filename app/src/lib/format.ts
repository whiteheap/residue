// Display only. Sizes and ages are formatted the way the residue CLI prints
// them, so the window and the terminal read the same.

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

/** `1.2 GB`, `512 MB`, `0 B`: decimal units, like `residue scan`. */
export function bytes(n: number | null | undefined): string {
  if (n == null) return "size unknown";
  let value = n;
  let unit = 0;
  while (value >= 1000 && unit + 1 < UNITS.length) {
    value /= 1000;
    unit += 1;
  }
  if (unit === 0) return `${n} B`;
  return value >= 100 ? `${value.toFixed(0)} ${UNITS[unit]}` : `${value.toFixed(1)} ${UNITS[unit]}`;
}

/** `just now`, `5 minutes ago`, `3 hours ago`, `2 days ago`. */
export function age(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 120) return "just now";
  if (s < 7200) return `${Math.floor(s / 60)} minutes ago`;
  if (s < 172800) return `${Math.floor(s / 3600)} hours ago`;
  return `${Math.floor(s / 86400)} days ago`;
}

/** How long before `nowSeconds` the time `iso` was. */
export function ageOf(iso: string | null | undefined, nowSeconds: number | null): string | null {
  if (!iso || nowSeconds == null) return null;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  return age(nowSeconds - t / 1000);
}

export function grouped(n: number): string {
  return n.toLocaleString("en-US");
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${grouped(n)} ${n === 1 ? one : many}`;
}

/** `~/Work/app` for a path under the home folder. */
export function tilde(path: string, home: string | undefined): string {
  if (!home || home === "/") return path;
  const h = home.replace(/\/+$/, "");
  if (path === h) return "~";
  return path.startsWith(`${h}/`) ? `~${path.slice(h.length)}` : path;
}

/** `s` as one shell word: bare when that is safe, otherwise in single quotes. */
export function shellWord(s: string): string {
  return /^[\w@%+=:,./-]+$/.test(s) ? s : `'${s.replaceAll("'", `'\\''`)}'`;
}

/** `path` relative to `base` when it is inside it, else null. */
export function inside(path: string, base: string): string | null {
  const b = base.replace(/\/+$/, "");
  return path.startsWith(`${b}/`) ? path.slice(b.length + 1) : null;
}

export function baseName(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  return trimmed.slice(trimmed.lastIndexOf("/") + 1) || trimmed;
}

/** `refs/heads/feat/x` -> `feat/x`. */
export function shortBranch(ref: string | null | undefined): string | null {
  if (!ref) return null;
  return ref.startsWith("refs/heads/") ? ref.slice("refs/heads/".length) : ref;
}

export function shortHead(head: string | null | undefined): string | null {
  return head ? head.slice(0, 7) : null;
}

/** `7 days`, `12 hours`, `30 minutes`: a span residue states in seconds. */
export function span(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s % 86400 === 0 && s > 0) return plural(s / 86400, "day");
  if (s % 3600 === 0 && s > 0) return plural(s / 3600, "hour");
  if (s % 60 === 0 && s > 0) return plural(s / 60, "minute");
  return plural(s, "second");
}
