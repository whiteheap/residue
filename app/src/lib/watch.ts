// What `residue watch` does, in words, from the settings the backend reads
// out of the service's arguments (`service.rs`, `watch_settings`). Nothing
// here assumes a value: an option the service does not set arrives as
// residue's default, marked as one.

import { plural } from "./format";
import type { WatchFetch, WatchSettings } from "./types";

const UNITS: Record<string, string> = { m: "minute", h: "hour", d: "day", w: "week" };

/** A residue DURATION in words: `30m` -> `30 minutes`, `1h` -> `1 hour`. */
export function duration(text: string): string {
  const m = /^(\d+)([mhdw])$/.exec(text);
  const unit = m ? UNITS[m[2] ?? ""] : undefined;
  return m && unit ? plural(Number(m[1]), unit) : text;
}

const UNIT_SECONDS: Record<string, number> = { m: 60, h: 3_600, d: 86_400, w: 604_800 };

/** A residue DURATION in seconds (`7d` -> 604800), or null when residue would refuse it. */
export function durationSeconds(text: string): number | null {
  const m = /^(\d+)([mhdw])$/.exec(text);
  const scale = m ? UNIT_SECONDS[m[2] ?? ""] : undefined;
  const n = m ? Number(m[1]) : 0;
  return scale && n > 0 ? n * scale : null;
}

/** `--min-free` as a number of percent, or null when residue would refuse it. */
export function percent(text: string): number | null {
  const n = Number.parseFloat(text.replace(/%$/, ""));
  return Number.isFinite(n) ? n : null;
}

/** When the disk is low, and the free space watch removes until: `--min-free` and 5 points more. */
export function diskLow(settings: WatchSettings): { below: string; until: string } {
  const p = percent(settings.min_free.value);
  if (p === null) return { below: settings.min_free.value, until: "5 points more" };
  return { below: `${p}%`, until: `${p + 5}%` };
}

/** How watch learns that a branch was deleted on the remote. */
export function fetchWords(fetch: WatchFetch, version: string | null): string {
  switch (fetch.mode) {
    case "every":
      return `It fetches about every ${duration(fetch.value)}, with --prune, so it sees a branch deleted on the remote without your own fetch.`;
    case "off":
      return "It does not fetch (--no-fetch): it sees a branch deleted on the remote after your next git fetch --prune.";
    case "never":
      return `${version ?? "This residue"} never fetches: it sees a branch deleted on the remote after your next git fetch --prune. residue ${fetch.since} and newer fetch about every ${duration(fetch.every)}, unless --no-fetch.`;
    default:
      return `Unknown residue version: before ${fetch.since}, residue never fetches, and sees a branch deleted on the remote after your next git fetch --prune. residue ${fetch.since} and newer fetch about every ${duration(fetch.every)}, unless --no-fetch.`;
  }
}

/** The settings as residue options, split into the ones the service sets and residue's defaults. */
export function settingFlags(settings: WatchSettings): { set: string[]; defaults: string[] } {
  const set: string[] = [];
  const defaults: string[] = [];
  const add = (flag: string, s: { value: string; default: boolean }) =>
    (s.default ? defaults : set).push(`${flag} ${s.value}`);
  add("--grace", settings.grace);
  add("--idle", settings.idle);
  add("--min-free", settings.min_free);
  add("--keep-removed", settings.keep_removed);
  if (settings.fetch.mode === "every") add("--fetch-every", settings.fetch);
  else if (settings.fetch.mode === "off") set.push("--no-fetch");
  return { set, defaults };
}

/** One line per setting, to compare the service before and after a save. */
export function settingLines(settings: WatchSettings, version: string | null): Record<string, string> {
  const fetch = settings.fetch;
  return {
    "Keeps a landed worktree for": duration(settings.grace.value),
    "Disk low below": diskLow(settings).below,
    "When low, keeps a worktree changed within": duration(settings.idle.value),
    "Keeps its removal records for at least": duration(settings.keep_removed.value),
    Fetching:
      fetch.mode === "every"
        ? `about every ${duration(fetch.value)}`
        : fetch.mode === "off"
          ? "off (--no-fetch)"
          : fetch.mode === "never"
            ? `never (${version ?? "this residue"})`
            : "unknown",
  };
}
