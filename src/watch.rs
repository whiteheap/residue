//! `residue watch`: stay running and reclaim storage from linked worktrees
//! when it is safe and worth it, reacting to events instead of a schedule.
//!
//! Two things lead to a removal. Work landed: a worktree's upstream branch
//! was deleted (a merged pull request, or `git push --delete`) and the
//! worktree has been unchanged for `--grace`. Disk pressure: free space on a
//! filesystem holding worktrees is below `--min-free`; then candidates
//! unchanged for `--idle` go, largest first, until free space is back above
//! the target.
//!
//! watch decides with `residue scan --json` and removes with `residue remove
//! --yes --idle --no-trash`, both run as child processes of this same
//! binary. Every check, and the re-check right before a deletion, is exactly
//! `remove`'s: watch only chooses when to ask. It never fetches. It deletes
//! rather than using the trash, which `--idle` would turn on: freeing the
//! disk is its job, a trashed worktree keeps every byte until a later remove
//! run purges it, and nothing would purge the trash of a repository where
//! watch removes nothing more.
//!
//! A removal because work landed also passes `--only-if-upstream-gone`, so
//! the child checks the reason itself, from fresh local refs, at its plan
//! and again right before the move: an upstream that is back by then keeps
//! the worktree. A disk-pressure removal does not need the upstream gone,
//! and does not pass it.
//!
//! It wakes on: a change under a repository's `refs/remotes`, `packed-refs`
//! or `reftable` (a fetch or prune); the moment a waiting worktree passes
//! `--grace` or `--idle`; a filesystem dropping below `--min-free` (free
//! space is read every five minutes, as there is no event for it); and every
//! six hours, to find repositories created under the roots since.

use crate::events::{self, Events};
use crate::jsonread::{self, Value};
use crate::output;
use crate::run::{self, RunError};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const HELP: &str = "\
residue watch: stay running and reclaim storage from Git worktrees as soon
as it is safe and worth it. No schedule: it reacts to Git and the disk.

Usage:
  residue watch [OPTIONS] ROOT...

It removes a linked worktree of a repository at or under ROOT when:
  its work landed  its upstream branch was deleted (a merged pull request,
                   or `git push --delete`) and it has not changed for --grace
  the disk is low  free space on its filesystem is below --min-free; then
                   worktrees unchanged for --idle go, largest first, until
                   free space is 5 points above --min-free

Each removal is `residue remove --yes --idle --no-trash`, with
--only-if-upstream-gone when its branch landed: every check runs,
and runs again right before the deletion. It deletes rather than using the
trash, and purges that repository's trash entries older than 7 days.
Branches and commits are always kept.
residue never fetches, so a deleted upstream is noticed after your next
`git fetch --prune` (`git config --global fetch.prune true` makes that the
default).

Options:
  --grace DURATION     Once its branch landed, keep a worktree that changed
                       within DURATION (default 1h)
  --idle DURATION      When the disk is low, keep a worktree that changed
                       within DURATION (default 2d)
  --min-free PERCENT   The disk is low below PERCENT free (default 10)
  --dry-run            Log what it would remove, and remove nothing
  --once               Decide and act once, then exit
  -h, --help           Show this help

DURATION is a whole number and a unit: m, h, d or w (30m, 1h, 2d, 1w).
Install it as a login service: residue service install ROOT...
";

#[derive(Clone)]
pub struct Span {
    pub seconds: i64,
    pub text: String,
}

impl Span {
    pub fn parse(flag: &str, text: &str) -> Result<Span, String> {
        let invalid = || {
            format!(
                "{flag} {text:?}: use a whole number and one unit, m, h, d or w, such as 30m, 1h, 2d or 1w"
            )
        };
        let unit = text.chars().last().ok_or_else(invalid)?;
        let seconds = match unit {
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            'w' => 604_800,
            _ => return Err(invalid()),
        };
        let digits = &text[..text.len() - 1];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let n: i64 = digits.parse().map_err(|_| invalid())?;
        if n == 0 {
            return Err(invalid());
        }
        let seconds = n.checked_mul(seconds).ok_or_else(invalid)?;
        Ok(Span {
            seconds,
            text: text.to_string(),
        })
    }
}

#[derive(Clone)]
pub struct Options {
    pub grace: Span,
    pub idle: Span,
    pub min_free: f64,
    pub dry_run: bool,
    pub once: bool,
    pub roots: Vec<PathBuf>,
}

impl Options {
    /// `None` when the arguments ask for help.
    pub fn parse(args: &[OsString]) -> Result<Option<Options>, String> {
        let mut grace = Span::parse("--grace", "1h")?;
        let mut idle = Span::parse("--idle", "2d")?;
        let mut min_free = 10.0;
        let mut dry_run = false;
        let mut once = false;
        let mut roots = Vec::new();
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let text = arg.to_string_lossy();
            let mut value = |flag: &str| -> Result<String, String> {
                it.next()
                    .map(|v| v.to_string_lossy().into_owned())
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match text.as_ref() {
                "-h" | "--help" => return Ok(None),
                "--grace" => grace = Span::parse("--grace", &value("--grace")?)?,
                "--idle" => idle = Span::parse("--idle", &value("--idle")?)?,
                "--min-free" => {
                    let v = value("--min-free")?;
                    min_free = v
                        .trim_end_matches('%')
                        .parse::<f64>()
                        .ok()
                        .filter(|p| *p > 0.0 && *p < 100.0)
                        .ok_or_else(|| {
                            format!(
                                "--min-free {v:?}: use a percentage between 0 and 100, such as 10"
                            )
                        })?;
                }
                "--dry-run" => dry_run = true,
                "--once" => once = true,
                flag if flag.starts_with('-') && flag != "-" => {
                    return Err(format!("unknown option {flag}"));
                }
                _ => {
                    let path = PathBuf::from(arg);
                    let root = path
                        .canonicalize()
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    if !root.is_dir() {
                        return Err(format!("{}: not a directory", path.display()));
                    }
                    roots.push(root);
                }
            }
        }
        if roots.is_empty() {
            return Err("name at least one ROOT, the folder that holds your repositories".into());
        }
        Ok(Some(Options {
            grace,
            idle,
            min_free,
            dry_run,
            once,
            roots,
        }))
    }

    /// The arguments that reproduce these options, for a service definition.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "watch".to_string(),
            "--grace".into(),
            self.grace.text.clone(),
            "--idle".into(),
            self.idle.text.clone(),
            "--min-free".into(),
            format!("{}", self.min_free),
        ];
        if self.dry_run {
            args.push("--dry-run".into());
        }
        args.extend(self.roots.iter().map(|r| r.to_string_lossy().into_owned()));
        args
    }
}

const FREE_CHECK: Duration = Duration::from_secs(5 * 60);
const REDISCOVER: Duration = Duration::from_secs(6 * 60 * 60);
const QUIET: Duration = Duration::from_secs(20);
const SETTLE_MAX: Duration = Duration::from_secs(2 * 60);
const MAX_WATCHES: usize = 4_000;

pub fn main(args: &[OsString]) -> u8 {
    let opts = match Options::parse(args) {
        Ok(Some(opts)) => opts,
        Ok(None) => {
            print!("{HELP}");
            return 0;
        }
        Err(message) => {
            eprintln!("residue watch: {}", output::clean(&message));
            eprintln!("Run `residue watch --help` for usage.");
            return 2;
        }
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log(&format!("cannot find the residue binary: {e}"));
            return 1;
        }
    };
    let _lock = match lock(&opts) {
        Ok(lock) => lock,
        Err(message) => {
            log(&message);
            return 1;
        }
    };
    let roots: Vec<String> = opts
        .roots
        .iter()
        .map(|r| output::home(&r.to_string_lossy()))
        .collect();
    log(&format!(
        "watching {} ({}). Removes a worktree when its upstream branch is deleted and it is unchanged for {}, and, below {}% free space, worktrees unchanged for {}, largest first.{}",
        roots.join(", "),
        events::BACKEND,
        opts.grace.text,
        opts.min_free,
        opts.idle.text,
        if opts.dry_run {
            " Dry run: nothing is removed."
        } else {
            ""
        }
    ));
    let mut events = match Events::new() {
        Ok(events) => Some(events),
        Err(e) => {
            log(&format!(
                "cannot watch for changes ({e}); using timers only"
            ));
            None
        }
    };
    let mut state = State::default();
    let mut rescans_in_a_row = 0;
    loop {
        let snap = match scan(&exe, &opts) {
            Ok(snap) => snap,
            Err(ScanError::Gone) => {
                log(
                    "the residue binary is gone, most likely upgraded; exiting so the service manager starts the new one",
                );
                return 1;
            }
            Err(ScanError::Failed(message)) => {
                log(&format!("scan failed: {message}"));
                if opts.once {
                    return 1;
                }
                std::thread::sleep(FREE_CHECK);
                continue;
            }
        };
        let removed = act(&exe, &opts, &snap, &mut state);
        if opts.once {
            return 0;
        }
        // Removals change free space and verdicts: look again at once, but
        // not forever.
        if removed > 0 && rescans_in_a_row < 3 {
            rescans_in_a_row += 1;
            continue;
        }
        rescans_in_a_row = 0;
        if let Some(events) = events.as_mut() {
            arm(events, &snap, &mut state);
        }
        let deadline = next_deadline(&opts, &snap, output::now());
        wait(events.as_mut(), &opts, &snap, deadline);
    }
}

fn log(message: &str) {
    eprintln!(
        "{} residue watch: {}",
        output::rfc3339(output::now()),
        output::clean(message)
    );
}

#[derive(Default)]
struct State {
    /// The last note about a filesystem that is low on space, so it is
    /// logged when it changes and not on every wake.
    pressure_notes: HashMap<u64, String>,
    watch_limit_logged: bool,
}

struct Worktree {
    path: String,
    verdict: String,
    upstream: Option<String>,
    upstream_gone: bool,
    last_change: Option<i64>,
    bytes: u64,
    /// The only checks that fail are `idle`: it becomes a candidate once it
    /// has been unchanged long enough.
    waits_for_idle: bool,
    /// The filesystem it is on.
    dev: Option<u64>,
}

struct Repo {
    common_dir: PathBuf,
}

struct Snapshot {
    repos: Vec<Repo>,
    worktrees: Vec<Worktree>,
}

enum ScanError {
    Gone,
    Failed(String),
}

fn scan(exe: &Path, opts: &Options) -> Result<Snapshot, ScanError> {
    let mut cmd = Command::new(exe);
    cmd.args(["scan", "--json", "--idle", &opts.grace.text])
        .args(&opts.roots);
    let out = match run::run(&mut cmd, Duration::from_secs(15 * 60), 256 << 20) {
        Ok(out) => out,
        Err(RunError::NotFound) => return Err(ScanError::Gone),
        Err(e) => return Err(ScanError::Failed(format!("residue scan: {e}"))),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let doc = jsonread::parse(&text).map_err(|e| {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().next().unwrap_or("").to_string();
        ScanError::Failed(format!(
            "residue scan exited with {}: {e} {first}",
            out.status
        ))
    })?;
    if doc.get("schema").and_then(Value::str) != Some("residue.scan/v1") {
        return Err(ScanError::Failed(
            "residue scan printed an unexpected document".into(),
        ));
    }
    let mut snap = Snapshot {
        repos: Vec::new(),
        worktrees: Vec::new(),
    };
    for repo in doc.get("repositories").map(Value::items).unwrap_or(&[]) {
        if let Some(dir) = repo.get("common_dir").and_then(Value::str) {
            snap.repos.push(Repo {
                common_dir: PathBuf::from(dir),
            });
        }
        for w in repo.get("worktrees").map(Value::items).unwrap_or(&[]) {
            let Some(path) = w.get("path").and_then(Value::str) else {
                continue;
            };
            let failing: Vec<&str> = w
                .get("checks")
                .map(Value::items)
                .unwrap_or(&[])
                .iter()
                .filter(|c| c.get("status").and_then(Value::str) != Some("pass"))
                .filter_map(|c| c.get("id").and_then(Value::str))
                .collect();
            snap.worktrees.push(Worktree {
                path: path.to_string(),
                verdict: w
                    .get("verdict")
                    .and_then(Value::str)
                    .unwrap_or("")
                    .to_string(),
                upstream: w.get("upstream").and_then(Value::str).map(str::to_string),
                upstream_gone: w.get("upstream_gone").and_then(Value::bool) == Some(true),
                last_change: w
                    .get("last_change")
                    .and_then(|c| c.get("unix"))
                    .and_then(Value::i64),
                bytes: w
                    .get("size")
                    .and_then(|s| s.get("reclaimable_bytes"))
                    .and_then(Value::i64)
                    .unwrap_or(0)
                    .max(0) as u64,
                waits_for_idle: !failing.is_empty() && failing.iter().all(|id| *id == "idle"),
                dev: std::fs::metadata(path).ok().map(|m| m.dev()),
            });
        }
    }
    Ok(snap)
}

/// Free and total bytes of the filesystem holding `path`.
fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs writes into the zeroed struct we own; `c` outlives it.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let unit = stat.f_frsize as u64;
    let total = stat.f_blocks as u64 * unit;
    (total > 0).then(|| (stat.f_bavail as u64 * unit, total))
}

/// One filesystem holding worktrees: its device, a path on it, free and
/// total bytes.
fn filesystems(snap: &Snapshot) -> Vec<(u64, String, u64, u64)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for w in &snap.worktrees {
        let Some(dev) = w.dev else { continue };
        if !seen.insert(dev) {
            continue;
        }
        if let Some((free, total)) = space(Path::new(&w.path)) {
            out.push((dev, w.path.clone(), free, total));
        }
    }
    out
}

fn percent(free: u64, total: u64) -> f64 {
    free as f64 * 100.0 / total as f64
}

fn act(exe: &Path, opts: &Options, snap: &Snapshot, state: &mut State) -> usize {
    let now = output::now();
    let mut removed = 0;
    // Worktrees already handed to a landed removal. If the child keeps one
    // (its upstream came back, say), this pass does not retry it as a
    // disk-pressure removal under another reason: the next scan judges
    // pressure on its own terms.
    let mut tried: HashSet<&str> = HashSet::new();

    let landed: Vec<&Worktree> = snap
        .worktrees
        .iter()
        .filter(|w| w.verdict == "candidate" && w.upstream_gone)
        .collect();
    for w in &landed {
        tried.insert(&w.path);
    }
    if !landed.is_empty() {
        removed += remove(exe, opts, &landed, &opts.grace, true, |w| {
            format!(
                "its upstream branch {} was deleted and it was unchanged for {}",
                w.upstream.as_deref().unwrap_or("(unknown)"),
                opts.grace.text
            )
        });
    }

    for (dev, sample, free, total) in filesystems(snap) {
        let pct = percent(free, total);
        if pct >= opts.min_free {
            state.pressure_notes.remove(&dev);
            continue;
        }
        let target = (total as f64 * (opts.min_free + 5.0) / 100.0) as u64;
        let mut need = target.saturating_sub(free);
        let mut eligible: Vec<&Worktree> = snap
            .worktrees
            .iter()
            .filter(|w| {
                w.dev == Some(dev)
                    && w.verdict == "candidate"
                    && !tried.contains(w.path.as_str())
                    && w.last_change.is_some_and(|t| now - t >= opts.idle.seconds)
            })
            .collect();
        eligible.sort_by_key(|w| std::cmp::Reverse(w.bytes));
        let mut chosen = Vec::new();
        for w in eligible {
            if need == 0 {
                break;
            }
            need = need.saturating_sub(w.bytes);
            chosen.push(w);
        }
        if chosen.is_empty() {
            let note = format!(
                "free space on the filesystem of {} is {pct:.1}%, below {}%, but no worktree on it is a candidate unchanged for {}",
                output::home(&sample),
                opts.min_free,
                opts.idle.text
            );
            if state.pressure_notes.get(&dev) != Some(&note) {
                log(&note);
                state.pressure_notes.insert(dev, note);
            }
            continue;
        }
        state.pressure_notes.remove(&dev);
        removed += remove(exe, opts, &chosen, &opts.idle, false, |_| {
            format!(
                "free space on its filesystem is {pct:.1}%, below {}%, and it was unchanged for {}",
                opts.min_free, opts.idle.text
            )
        });
    }
    removed
}

/// Remove `worktrees` with `residue remove --yes --idle SPAN --no-trash`,
/// plus `--only-if-upstream-gone` when they were chosen because their work
/// landed, and log each result and each trash entry the run purged.
/// Returns how many were removed.
fn remove(
    exe: &Path,
    opts: &Options,
    worktrees: &[&Worktree],
    span: &Span,
    landed: bool,
    why: impl Fn(&Worktree) -> String,
) -> usize {
    if opts.dry_run {
        for w in worktrees {
            log(&format!(
                "would remove {} ({}): {}",
                output::home(&w.path),
                output::bytes(w.bytes),
                why(w)
            ));
        }
        return 0;
    }
    let mut cmd = Command::new(exe);
    cmd.args([
        "remove",
        "--yes",
        "--json",
        "--no-trash",
        "--idle",
        &span.text,
    ]);
    if landed {
        cmd.arg("--only-if-upstream-gone");
    }
    for w in worktrees {
        cmd.arg(&w.path);
    }
    let out = match run::run(&mut cmd, Duration::from_secs(30 * 60), 64 << 20) {
        Ok(out) => out,
        Err(e) => {
            log(&format!("residue remove: {e}"));
            return 0;
        }
    };
    let doc = match jsonread::parse(&String::from_utf8_lossy(&out.stdout)) {
        Ok(doc) => doc,
        Err(e) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            log(&format!(
                "residue remove exited with {}: {e} {}",
                out.status,
                stderr.lines().next().unwrap_or("")
            ));
            return 0;
        }
    };
    let mut removed = 0;
    for result in doc.get("results").map(Value::items).unwrap_or(&[]) {
        let path = result.get("path").and_then(Value::str).unwrap_or("");
        let w = worktrees.iter().find(|w| w.path == path);
        let size = w.map(|w| output::bytes(w.bytes)).unwrap_or_default();
        let reason = w.map(|w| why(w)).unwrap_or_default();
        match result.get("result").and_then(Value::str) {
            Some("removed") => {
                removed += 1;
                log(&format!(
                    "removed {} ({size}): {reason}",
                    output::home(path)
                ));
            }
            // --no-trash makes this unreachable today; if a remove run ever
            // trashes instead, the worktree is gone from its path all the
            // same, and the log says where it went.
            Some("trashed") => {
                removed += 1;
                let entry = result.get("trash_entry").and_then(Value::str).unwrap_or("");
                log(&format!(
                    "moved {} to the trash ({size}): {reason}; put it back with: residue restore {}",
                    output::home(path),
                    output::shell(Path::new(entry))
                ));
            }
            other => {
                let reason = result
                    .get("reason")
                    .and_then(Value::str)
                    .unwrap_or("no reason given");
                log(&format!(
                    "kept {} ({}): {reason}",
                    output::home(path),
                    other.unwrap_or("unknown result")
                ));
            }
        }
    }
    // The same run purges the trash entries older than --keep-trash in the
    // repositories of these worktrees; say which, so the log accounts for
    // every byte it freed.
    for purge in doc.get("purged").map(Value::items).unwrap_or(&[]) {
        let entry = purge.get("entry");
        let name = entry
            .and_then(|e| e.get("entry"))
            .and_then(Value::str)
            .unwrap_or("(unnamed entry)");
        let was = entry
            .and_then(|e| e.get("original_path"))
            .and_then(Value::str)
            .map(|p| format!(" (was {})", output::home(p)))
            .unwrap_or_default();
        match purge.get("result").and_then(Value::str) {
            Some("purged") => log(&format!("purged {name} from the trash{was}")),
            other => {
                let reason = purge
                    .get("reason")
                    .and_then(Value::str)
                    .unwrap_or("no reason given");
                log(&format!(
                    "kept {name} in the trash{was} ({}): {reason}",
                    other.unwrap_or("unknown result")
                ));
            }
        }
    }
    removed
}

/// Watch what a fetch or prune changes in each repository: the directories
/// under `refs/remotes`, `packed-refs`, and `reftable`.
fn arm(events: &mut Events, snap: &Snapshot, state: &mut State) {
    if let Err(e) = events.clear() {
        log(&format!("cannot reset the change watches: {e}"));
        return;
    }
    let mut add = |events: &mut Events, path: &Path| {
        if events.len() >= MAX_WATCHES {
            if !state.watch_limit_logged {
                log(&format!(
                    "watching {MAX_WATCHES} paths, the limit; changes in the rest are seen on the next timer"
                ));
                state.watch_limit_logged = true;
            }
            return;
        }
        // A path that vanished between the scan and now is seen on the next scan.
        let _ = events.watch(path);
    };
    for repo in &snap.repos {
        let remotes = repo.common_dir.join("refs").join("remotes");
        let mut stack = vec![(remotes, 0)];
        while let Some((dir, depth)) = stack.pop() {
            if !dir.is_dir() {
                continue;
            }
            add(events, &dir);
            if depth >= 12 {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push((entry.path(), depth + 1));
                    }
                }
            }
        }
        for name in ["packed-refs", "reftable"] {
            let path = repo.common_dir.join(name);
            if path.exists() {
                add(events, &path);
            }
        }
    }
}

/// The next moment a waiting worktree passes `--grace` (its branch landed)
/// or `--idle` (its filesystem is low on space).
fn next_deadline(opts: &Options, snap: &Snapshot, now: i64) -> Option<i64> {
    let low: HashSet<u64> = filesystems(snap)
        .into_iter()
        .filter(|(_, _, free, total)| percent(*free, *total) < opts.min_free)
        .map(|(dev, ..)| dev)
        .collect();
    let mut next: Option<i64> = None;
    for w in &snap.worktrees {
        let Some(t) = w.last_change else { continue };
        let mut times = Vec::new();
        if w.upstream_gone && w.waits_for_idle {
            times.push(t + opts.grace.seconds);
        }
        if w.dev.is_some_and(|d| low.contains(&d)) && (w.waits_for_idle || w.verdict == "candidate")
        {
            times.push(t + opts.idle.seconds);
        }
        for at in times.into_iter().filter(|at| *at > now) {
            next = Some(next.map_or(at, |n: i64| n.min(at)));
        }
    }
    // A little slack, so the scan sees the worktree past the line.
    next.map(|n| n + 30)
}

/// Sleep until something warrants a new scan: a watched change (after it
/// settles), the deadline, a filesystem newly below `--min-free`, or the
/// rediscovery timer.
fn wait(mut events: Option<&mut Events>, opts: &Options, snap: &Snapshot, deadline: Option<i64>) {
    let started = Instant::now();
    let low_before: HashSet<u64> = filesystems(snap)
        .into_iter()
        .filter(|(_, _, free, total)| percent(*free, *total) < opts.min_free)
        .map(|(dev, ..)| dev)
        .collect();
    loop {
        let now = output::now();
        if deadline.is_some_and(|d| now >= d) {
            return;
        }
        let elapsed = started.elapsed();
        if elapsed >= REDISCOVER {
            return;
        }
        let mut step = FREE_CHECK.min(REDISCOVER - elapsed);
        if let Some(d) = deadline {
            step = step.min(Duration::from_secs((d - now).max(1) as u64));
        }
        let changed = match events.as_deref_mut() {
            Some(events) => events.wait(step).unwrap_or_else(|e| {
                log(&format!("waiting for changes failed: {e}"));
                std::thread::sleep(step);
                false
            }),
            None => {
                std::thread::sleep(step);
                false
            }
        };
        if changed {
            // A fetch writes many refs; scan once it has settled.
            let settle = Instant::now();
            if let Some(events) = events.as_deref_mut() {
                while settle.elapsed() < SETTLE_MAX && events.wait(QUIET).unwrap_or(false) {}
            }
            return;
        }
        let low_now = filesystems(snap).into_iter().any(|(dev, _, free, total)| {
            percent(free, total) < opts.min_free && !low_before.contains(&dev)
        });
        if low_now {
            return;
        }
    }
}

/// One watcher per user: a second one would race the first.
fn lock(opts: &Options) -> Result<Option<File>, String> {
    if opts.once {
        return Ok(None);
    }
    let dir = match std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("residue"),
        None => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".local/state/residue"),
            None => return Ok(None),
        },
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("watch.lock");
    let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !try_lock(&file) {
        return Err(format!(
            "another residue watch is already running for this user (lock: {})",
            output::home(&path.to_string_lossy())
        ));
    }
    Ok(Some(file))
}

#[cfg(not(any(target_os = "illumos", target_os = "solaris")))]
fn try_lock(file: &File) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: flock on a descriptor we own; the lock lives as long as `file`.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[cfg(any(target_os = "illumos", target_os = "solaris"))]
fn try_lock(_file: &File) -> bool {
    true
}
