//! `.filter(|r| matches!(r.get("state").and_then(Value::str), Some("back" | "unknown")))esidue watch`: stay running and reclaim storage from linked worktrees
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
//! `remove`'s: watch only chooses when to ask. It deletes rather than using
//! the trash, which `--idle` would turn on: freeing the disk is its job, a
//! trashed worktree keeps every byte until a later remove run purges it, and
//! nothing would purge the trash of a repository where watch removes nothing
//! more.
//!
//! Every removal writes a removal record first (`removed`), and its log line
//! ends with the `residue restore` command that brings the worktree back.
//! A worktree `residue restore` put back is never removed again because its
//! work landed (the scan's `restored`, and the child's own check): someone
//! wanted it after it landed. One a removal record names as back at its
//! path (`state` `back`) is kept the same way, which covers a restore stopped
//! before it wrote its marker. Disk pressure still removes it.
//!
//! To see a branch deleted on the remote, it fetches (`Fetcher`): about
//! every `--fetch-every`, `git fetch --prune --no-tags` of each remote that
//! a linked worktree's branch tracks (`git::fetch`, which never prompts). A
//! fetch changes only remote-tracking refs. It is never a reason to remove:
//! it only refreshes the refs that the scan and the removal child read, and
//! a failed fetch is logged and changes nothing. `--no-fetch` turns it off.
//!
//! A removal because work landed also passes `--only-if-upstream-gone`, so
//! the child checks the reason itself, from fresh local refs, at its plan,
//! right before the move and right before the deletion: an upstream that
//! is back by then keeps the worktree. A disk-pressure removal does not
//! need the upstream gone, and does not pass it.
//!
//! It wakes on: a change under a repository's `refs/remotes`, `packed-refs`
//! or `reftable` (a fetch or prune, its own or the user's); a repository's
//! fetch coming due; the moment a waiting worktree passes `--grace` or
//! `--idle`; a filesystem dropping below `--min-free` (free space is read
//! every five minutes, as there is no event for it); and every six hours,
//! to find repositories created under the roots since.
//!
//! Without a ROOT it watches `DEFAULT_ROOTS`, the usual code folders in the
//! home folder that exist.

use crate::cli;
use crate::events::{self, Events};
use crate::git;
use crate::jsonread::{self, Value};
use crate::output;
use crate::run::{self, RunError};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::File;
use std::hash::BuildHasher;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const HELP: &str = "\
residue watch: stay running; clean up when a branch lands or the disk is low.

Usage:
  residue watch [OPTIONS] [ROOT...]

ROOT is a folder of repositories. Without one, the ones of these that exist:
  ~/src ~/code ~/Code ~/dev ~/Developer ~/projects ~/Projects ~/repos ~/git
  ~/github ~/GitHub ~/work ~/Work ~/workspace

It removes a candidate worktree when:
  its branch landed  its upstream branch was deleted; unchanged for --grace
  the disk is low    free space is below --min-free; unchanged for --idle,
                     largest first, until free space is 5 points above it

Options:
  --grace DURATION         Branch landed: keep it if changed within this (1h)
  --idle DURATION          Disk low: keep it if changed within this (2d)
  --min-free PERCENT       The disk is low below PERCENT free (10)
  --keep-removed DURATION  Keep removal records at least this long (30d)
  --fetch-every DURATION   Fetch each repository about this often (30m)
  --no-fetch               Never fetch; a deleted branch shows after your fetch
  --dry-run                Log what it would remove; remove nothing
  --once                   Fetch, decide and act once, then exit
  -h, --help               Show this help

DURATION is a whole number and m, h, d or w: 30m, 1h, 2d, 1w.

Examples:
  residue watch --once --dry-run   preview one pass (it still fetches)
  residue service install          run it from login

Removal:  every check of `residue remove`, then a removal record to restore from
Trash:    not used; entries older than 7d are purged
Kept:     branches and commits; a restored worktree, unless the disk is low
Fetch:    never prompts, so one that needs a password fails and is logged
Details:  https://github.com/whiteheap/residue/blob/main/docs/automation.md
";

/// The folders in the home folder that `watch` and `service install` use
/// when no ROOT is given, those that exist. Each is searched the way any
/// ROOT is (`discover`: four levels, no symbolic links below it, one
/// filesystem, hidden folders, `node_modules`, `target` and `Library`
/// skipped). Never the home folder itself, which holds far more than code,
/// and nothing under `Documents`, `Desktop` or `Downloads`, which macOS
/// guards with a permission prompt for a background process.
pub const DEFAULT_ROOTS: &[&str] = &[
    "src",
    "code",
    "Code",
    "dev",
    "Developer",
    "projects",
    "Projects",
    "repos",
    "git",
    "github",
    "GitHub",
    "work",
    "Work",
    "workspace",
];

/// How often `watch` fetches each repository when `--fetch-every` is not
/// given.
pub const DEFAULT_FETCH_EVERY: &str = "30m";

/// The `DEFAULT_ROOTS` that exist, canonical, each once (on a
/// case-insensitive filesystem `~/code` and `~/Code` are one folder). A name
/// that resolves to the home folder or above it is skipped.
pub fn default_roots() -> Result<Vec<PathBuf>, String> {
    let no_home = "no ROOT given, and HOME names no home folder: name the folder that holds your repositories";
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or(no_home)?;
    let home = home.canonicalize().unwrap_or(home);
    // HOME=/ (some daemons and minimal containers) would make `/dev` a code
    // folder.
    if home.parent().is_none() {
        return Err(no_home.into());
    }
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for name in DEFAULT_ROOTS {
        let Ok(path) = home.join(name).canonicalize() else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || home.starts_with(&path) {
            continue;
        }
        if seen.insert((meta.dev(), meta.ino())) {
            roots.push(path);
        }
    }
    Ok(roots)
}

/// The usage error, one line, when no ROOT is given and no default root
/// exists. `command` is what to run with a ROOT (`residue watch`).
fn no_default_roots(command: &str) -> String {
    let names: Vec<String> = DEFAULT_ROOTS.iter().map(|n| format!("~/{n}")).collect();
    format!(
        "none of the usual code folders exists in your home folder ({}); name yours: {command} ~/path/to/repos",
        names.join(", ")
    )
}

/// The options of `watch` that take a value. `roots_in` skips them.
const VALUE_OPTIONS: &[&str] = &[
    "--grace",
    "--idle",
    "--min-free",
    "--keep-removed",
    "--fetch-every",
];

/// The value of the option `flag` in the arguments of a `watch` command
/// line (after `watch`), as written; the last one when it is given twice,
/// as `Options::parse` takes it. `service status` reads the installed
/// service's settings with it.
pub fn option_in(args: &[String], flag: &str) -> Option<String> {
    let mut value = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if VALUE_OPTIONS.contains(&arg.as_str()) {
            let v = it.next();
            if arg == flag {
                value = v.cloned();
            }
        }
    }
    value
}

/// The ROOTs in the arguments of a `watch` command line (after `watch`), as
/// written: every argument that is neither an option nor an option's
/// value. `service status` reads them back from the installed service.
pub fn roots_in(args: &[String]) -> Vec<String> {
    let mut roots = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if VALUE_OPTIONS.contains(&arg.as_str()) {
            it.next();
        } else if !(arg.starts_with('-') && arg != "-") {
            roots.push(arg.clone());
        }
    }
    roots
}

#[derive(Clone)]
pub struct Span {
    pub seconds: i64,
    pub text: String,
}

impl Span {
    pub fn parse(flag: &str, text: &str) -> Result<Span, String> {
        let invalid =
            || format!("{flag} {text:?}: use a whole number and a unit, such as 30m, 1h, 2d or 1w");
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
    /// `--keep-removed`, passed to each removal child: how long the
    /// removal records of the repositories it removes in are kept. Parsed
    /// as `residue remove` parses it.
    pub keep_removed: cli::Span,
    /// `None` with `--no-fetch`.
    pub fetch_every: Option<Span>,
    pub dry_run: bool,
    pub once: bool,
    pub roots: Vec<PathBuf>,
    /// No ROOT was given: `roots` are the `DEFAULT_ROOTS` that exist.
    pub default_roots: bool,
}

impl Options {
    /// `None` when the arguments ask for help.
    pub fn parse(args: &[OsString]) -> Result<Option<Options>, String> {
        let mut grace = Span::parse("--grace", "1h")?;
        let mut idle = Span::parse("--idle", "2d")?;
        let mut min_free = 10.0;
        let mut keep_removed = cli::Span::parse("--keep-removed", cli::DEFAULT_KEEP_REMOVED)?;
        let mut fetch_every = Some(Span::parse("--fetch-every", DEFAULT_FETCH_EVERY)?);
        let mut fetch_every_given = false;
        let mut no_fetch = false;
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
                "--keep-removed" => {
                    keep_removed = cli::Span::parse("--keep-removed", &value("--keep-removed")?)?;
                }
                "--fetch-every" => {
                    fetch_every = Some(Span::parse("--fetch-every", &value("--fetch-every")?)?);
                    fetch_every_given = true;
                }
                "--no-fetch" => no_fetch = true,
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
        if no_fetch {
            if fetch_every_given {
                return Err("--no-fetch and --fetch-every cannot both be given".into());
            }
            fetch_every = None;
        }
        let default_roots = roots.is_empty();
        Ok(Some(Options {
            grace,
            idle,
            min_free,
            keep_removed,
            fetch_every,
            dry_run,
            once,
            roots,
            default_roots,
        }))
    }

    /// The arguments that reproduce these options, for a service definition.
    /// The roots are always written out, default ones included, so the
    /// service watches the same folders on every start.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "watch".to_string(),
            "--grace".into(),
            self.grace.text.clone(),
            "--idle".into(),
            self.idle.text.clone(),
            "--min-free".into(),
            format!("{}", self.min_free),
            "--keep-removed".into(),
            self.keep_removed.text.clone(),
        ];
        match &self.fetch_every {
            Some(every) => args.extend(["--fetch-every".into(), every.text.clone()]),
            None => args.push("--no-fetch".into()),
        }
        if self.dry_run {
            args.push("--dry-run".into());
        }
        args.extend(self.roots.iter().map(|r| r.to_string_lossy().into_owned()));
        args
    }

    /// When no ROOT was given, watch the `DEFAULT_ROOTS` that exist. The
    /// error is one line that names the folders it looked for and how to
    /// pass a ROOT to `command`.
    pub fn use_default_roots(&mut self, command: &str) -> Result<(), String> {
        if !self.default_roots {
            return Ok(());
        }
        self.roots = default_roots()?;
        if self.roots.is_empty() {
            return Err(no_default_roots(command));
        }
        Ok(())
    }

    /// The roots for a message: `~/src, ~/Work`.
    pub fn shown_roots(&self) -> String {
        self.roots
            .iter()
            .map(|r| output::home(&r.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// How often `wait` reads free space for `--min-free`: one `statvfs` per
/// filesystem, under 0.01 s of CPU in five minutes at rest. No event serves
/// instead on Linux or macOS. Linux has none. macOS defines
/// `VQ_FREE_SPACE_CHANGE` for an `EVFILT_FS` kqueue, but on macOS 26.3.1
/// APFS sent none while 10.25 GB was written and deleted, only
/// `VQ_PURGEABLE_SPACE_CHANGE` (2026-11-08). A longer interval only delays
/// the low-disk rule, and that write took 9 s.
const FREE_CHECK: Duration = Duration::from_secs(5 * 60);
const REDISCOVER: Duration = Duration::from_secs(6 * 60 * 60);
const QUIET: Duration = Duration::from_secs(20);
const SETTLE_MAX: Duration = Duration::from_secs(2 * 60);
const MAX_WATCHES: usize = 4_000;

pub fn main(args: &[OsString]) -> u8 {
    let mut opts = match Options::parse(args) {
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
    if let Err(message) = opts.use_default_roots("residue watch") {
        eprintln!("residue watch: {}", output::clean(&message));
        return 2;
    }
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
    // One line of labels and values; `residue watch --help` explains each.
    log(&format!(
        "watching {} ({}{}) · removes: landed and unchanged {}, or below {}% free and unchanged {} · removal records: {} · fetch: {}{}",
        opts.shown_roots(),
        events::BACKEND,
        if opts.default_roots {
            "; the usual code folders; name ROOTs to change"
        } else {
            ""
        },
        opts.grace.text,
        opts.min_free,
        opts.idle.text,
        opts.keep_removed.text,
        match &opts.fetch_every {
            Some(every) => format!("every {}", every.text),
            None => "off (--no-fetch)".to_string(),
        },
        if opts.dry_run {
            " · dry run: removes nothing"
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
    let mut fetcher = Fetcher::new(opts.fetch_every.clone());
    let mut rescans_in_a_row = 0;
    loop {
        let mut snap = match scan(&exe, &opts) {
            Ok(snap) => snap,
            Err(e) => match failed(e, opts.once) {
                Some(code) => return code,
                None => continue,
            },
        };
        // Repositories never fetched (all of them, on the first pass) and
        // those whose fetch came due while scanning: fetch, and judge with
        // the refs it brought.
        if fetcher.run(&snap) {
            snap = match scan(&exe, &opts) {
                Ok(snap) => snap,
                Err(e) => match failed(e, opts.once) {
                    Some(code) => return code,
                    None => continue,
                },
            };
        }
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
        wait(events.as_mut(), &opts, &snap, deadline, &mut fetcher);
    }
}

/// What to do after a failed scan: `Some(code)` to exit with, or `None` to
/// try again after a pause.
fn failed(error: ScanError, once: bool) -> Option<u8> {
    match error {
        ScanError::Gone => {
            log(
                "the residue binary is gone, most likely upgraded; exiting so the service manager starts the new one",
            );
            Some(1)
        }
        ScanError::Failed(message) => {
            log(&format!("scan failed: {message}"));
            if once {
                return Some(1);
            }
            std::thread::sleep(FREE_CHECK);
            None
        }
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
    /// Its upstream is a remote-tracking ref that is gone: the branch was
    /// deleted on a remote, as after a merged pull request.
    upstream_gone: bool,
    last_change: Option<i64>,
    bytes: u64,
    /// The only checks that fail are `idle`: it becomes a candidate once it
    /// has been unchanged long enough.
    waits_for_idle: bool,
    /// `residue restore` put it back (or residue could not read whether
    /// it did), or a removal record names it as back at its path: the
    /// landed-work rule never removes it again.
    restored: bool,
    /// The filesystem it is on.
    dev: Option<u64>,
}

struct Repo {
    common_dir: PathBuf,
    /// Where to run Git for the whole repository: the main checkout, or the
    /// common directory when the main checkout is gone or bare.
    dir: PathBuf,
    /// The branches (`refs/heads/fix`) of its linked worktrees whose
    /// upstream is a remote-tracking ref: the ones a fetch can show landed.
    /// A restored worktree's is left out: the landed rule never removes it,
    /// so no fetch is needed for it.
    branches: BTreeSet<String>,
    /// Those upstreams (`refs/remotes/origin/fix`).
    upstreams: HashSet<String>,
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
        let worktrees = repo.get("worktrees").map(Value::items).unwrap_or(&[]);
        let back = back_paths(repo);
        let put_back = |w: &Value| restored(w) || is_back(w, &back);
        if let Some(common_dir) = repo.get("common_dir").and_then(Value::str) {
            let common_dir = PathBuf::from(common_dir);
            let dir = repo
                .get("main_path")
                .and_then(Value::str)
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| common_dir.clone());
            let mut branches = BTreeSet::new();
            let mut upstreams = HashSet::new();
            for w in worktrees.iter().filter(|w| !put_back(w)) {
                let branch = w.get("branch").and_then(Value::str);
                let upstream = w.get("upstream_ref").and_then(Value::str);
                if let (Some(branch), Some(upstream)) = (branch, upstream) {
                    if upstream.starts_with("refs/remotes/") {
                        branches.insert(branch.to_string());
                        upstreams.insert(upstream.to_string());
                    }
                }
            }
            snap.repos.push(Repo {
                common_dir,
                dir,
                branches,
                upstreams,
            });
        }
        for w in worktrees {
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
                upstream_gone: w.get("upstream_gone").and_then(Value::bool) == Some(true)
                    && w.get("upstream_ref")
                        .and_then(Value::str)
                        .is_some_and(|r| r.starts_with("refs/remotes/")),
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
                restored: put_back(w),
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
        .filter(|w| w.verdict == "candidate" && w.upstream_gone && !w.restored)
        .collect();
    for w in &landed {
        tried.insert(&w.path);
    }
    if !landed.is_empty() {
        removed += remove(
            exe,
            opts,
            &landed,
            &opts.grace,
            /* require_upstream_gone */ true,
            |w| {
                format!(
                    "{} was deleted, unchanged for {}",
                    w.upstream.as_deref().unwrap_or("(unknown)"),
                    opts.grace.text
                )
            },
        );
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
                "{}: {pct:.1}% free, below {}%, and no candidate unchanged for {}",
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
        removed += remove(
            exe,
            opts,
            &chosen,
            &opts.idle,
            /* require_upstream_gone */ false,
            |_| {
                format!(
                    "{pct:.1}% free, below {}%, unchanged for {}",
                    opts.min_free, opts.idle.text
                )
            },
        );
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
    require_upstream_gone: bool,
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
    if require_upstream_gone {
        cmd.arg("--only-if-upstream-gone");
    }
    // How long the records it writes, and the older ones in these
    // repositories, are kept.
    cmd.args(["--keep-removed", &opts.keep_removed.text]);
    // Why, as each removal record says it.
    cmd.args([
        "--removal-reason",
        if require_upstream_gone {
            "watch-landed"
        } else {
            "watch-low-disk"
        },
    ]);
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
                    "removed {} ({size}): {reason}{}",
                    output::home(path),
                    restore_hint(result)
                ));
            }
            // --no-trash makes this unreachable today; if a remove run ever
            // trashes instead, the worktree is gone from its path all the
            // same, and the log says where it went.
            Some("trashed") => {
                removed += 1;
                let entry = result.get("trash_entry").and_then(Value::str).unwrap_or("");
                log(&format!(
                    "moved {} to the trash ({size}): {reason}; restore: residue restore {}",
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
            Some("purged") => log(&format!(
                "purged {name} from the trash{was}{}",
                restore_hint(purge)
            )),
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
    // And it forgets the removal records older than --keep-removed there.
    for forgot in doc.get("forgotten").map(Value::items).unwrap_or(&[]) {
        let entry = forgot.get("entry");
        let name = entry
            .and_then(|e| e.get("entry"))
            .and_then(Value::str)
            .unwrap_or("(unnamed record)");
        let was = entry
            .and_then(|e| e.get("original_path"))
            .and_then(Value::str)
            .map(|p| format!(" (was {})", output::home(p)))
            .unwrap_or_default();
        match forgot.get("result").and_then(Value::str) {
            Some("forgotten") => log(&format!("forgot the removal record {name}{was}")),
            other => {
                let reason = forgot
                    .get("reason")
                    .and_then(Value::str)
                    .unwrap_or("no reason given");
                log(&format!(
                    "kept the removal record {name}{was} ({}): {reason}",
                    other.unwrap_or("unknown result")
                ));
            }
        }
    }
    // What it could not read (`errors`, which failed the run), and the
    // removal records it could not read or forget and a stats ledger it
    // could not write (`warnings`, which never do): the log is the only
    // place a service user sees either.
    for (key, kind) in [("errors", ""), ("warnings", "warning: ")] {
        for line in doc.get(key).map(Value::items).unwrap_or(&[]) {
            if let Some(text) = line.str() {
                log(&format!("residue remove: {kind}{text}"));
            }
        }
    }
    if let Some(stats) = doc.get("stats") {
        log_freed(stats);
    }
    removed
}

/// What a removal child freed, and the total on this machine, from its
/// `stats`. The date is the log's: UTC. A ledger it could not write is in
/// its `warnings`, logged above.
fn log_freed(stats: &Value) {
    let number = |key: &str| {
        stats
            .get(key)
            .and_then(Value::i64)
            .and_then(|n| u64::try_from(n).ok())
    };
    if let (Some(bytes), Some(1..)) = (number("freed_bytes"), number("freed_worktrees")) {
        let total = match (
            number("total_freed_bytes"),
            stats.get("since").and_then(Value::str),
        ) {
            (Some(total), Some(since)) => format!(
                " (about {} on this machine since {})",
                output::bytes(total),
                since.get(..10).unwrap_or(since)
            ),
            _ => String::new(),
        };
        log(&format!("freed about {}{total}", output::bytes(bytes)));
    }
}

/// `; restore: residue restore RECORD` for a result that names the removal
/// record its deletion wrote.
fn restore_hint(result: &Value) -> String {
    result
        .get("removal_record")
        .and_then(Value::str)
        .map(|r| format!("; restore: residue restore {}", output::shell(Path::new(r))))
        .unwrap_or_default()
}

/// The scan says `residue restore` put this worktree back, or cannot read
/// whether it did (`restored_error`): either way the landed rule keeps it.
fn restored(worktree: &Value) -> bool {
    ["restored", "restored_error"]
        .iter()
        .any(|k| worktree.get(k).is_some_and(|v| !matches!(v, Value::Null)))
}

/// The original paths of the repository's removal records whose worktree
/// is back there (`state` `back`): a restore stopped before it wrote its
/// marker, or one that kept its record, or a move back whose record could
/// not be dropped; and of those residue cannot tell are back (`state`
/// `unknown`). The landed rule keeps those worktrees as it keeps a marked
/// one, while the record lives.
fn back_paths(repo: &Value) -> Vec<PathBuf> {
    repo.get("removed")
        .map(Value::items)
        .unwrap_or(&[])
        .iter()
        .filter(|r| {
            matches!(
                r.get("state").and_then(Value::str),
                Some("back" | "unknown")
            )
        })
        .filter_map(|r| r.get("original_path").and_then(Value::str))
        .map(PathBuf::from)
        .collect()
}

/// A worktree at one of `back`, by its path as the scan gives it or as
/// the filesystem resolves it.
fn is_back(worktree: &Value, back: &[PathBuf]) -> bool {
    if back.is_empty() {
        return false;
    }
    let Some(path) = worktree.get("path").and_then(Value::str).map(Path::new) else {
        return false;
    };
    back.iter().any(|b| b == path) || std::fs::canonicalize(path).is_ok_and(|p| back.contains(&p))
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
        if w.upstream_gone && w.waits_for_idle && !w.restored {
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
/// settles), the deadline, a filesystem newly below `--min-free`, the
/// rediscovery timer, or a fetch that changed a remote-tracking ref. A fetch
/// that comes due runs here, and one that changed nothing keeps waiting.
fn wait(
    mut events: Option<&mut Events>,
    opts: &Options,
    snap: &Snapshot,
    deadline: Option<i64>,
    fetcher: &mut Fetcher,
) {
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
        let fetch_at = fetcher.next(snap);
        if fetch_at.is_some_and(|t| now >= t) {
            if fetcher.run(snap) {
                return;
            }
            continue;
        }
        let mut step = FREE_CHECK.min(REDISCOVER - elapsed);
        for at in [deadline, fetch_at].into_iter().flatten() {
            step = step.min(Duration::from_secs((at - now).max(1) as u64));
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

/// Where `watch` keeps its lock: `$XDG_STATE_HOME/residue/watch.lock`, or
/// `~/.local/state/residue/watch.lock`.
fn lock_path() -> Option<PathBuf> {
    let dir = match std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("residue"),
        None => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?)
            .join(".local/state/residue"),
    };
    Some(dir.join("watch.lock"))
}

/// One watcher per user: a second one would race the first. A moment of
/// contention is not a second watcher (`lock_held` takes the lock briefly
/// to look), so it tries for a second before giving up.
fn lock(opts: &Options) -> Result<Option<File>, String> {
    if opts.once {
        return Ok(None);
    }
    let Some(path) = lock_path() else {
        return Ok(None);
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    for attempt in 0..10 {
        if try_lock(&file, libc::LOCK_EX) {
            return Ok(Some(file));
        }
        if attempt < 9 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Err(format!(
        "another residue watch is already running for this user (lock: {})",
        output::home(&path.to_string_lossy())
    ))
}

/// Whether a `residue watch` of this user runs now, by its lock. `None`
/// when residue cannot tell (the lock cannot be read, or this system has no
/// `flock`). It never creates the lock file: no file means no watch ever
/// ran with this state folder.
pub fn lock_held() -> Option<bool> {
    if cfg!(any(target_os = "illumos", target_os = "solaris")) {
        return None;
    }
    let path = lock_path()?;
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(false),
        Err(_) => return None,
    };
    // A shared lock is enough to see an exclusive one; closing `file`
    // releases it at once.
    Some(!try_lock(&file, libc::LOCK_SH))
}

#[cfg(not(any(target_os = "illumos", target_os = "solaris")))]
fn try_lock(file: &File, kind: libc::c_int) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: flock on a descriptor we own; the lock lives as long as `file`.
    unsafe { libc::flock(file.as_raw_fd(), kind | libc::LOCK_NB) == 0 }
}

#[cfg(any(target_os = "illumos", target_os = "solaris"))]
fn try_lock(_file: &File, _kind: libc::c_int) -> bool {
    true
}

/// Fetches each repository's tracked remotes on its own clock.
///
/// A repository is due at once when watch first sees it (at start, or when
/// the six-hourly rediscovery finds it), then about every `--fetch-every`:
/// each next time is drawn between 90% and 110% of it, so repositories
/// drift apart instead of all fetching in the same minute. Fetches run one
/// at a time. Only the remotes that a linked worktree's branch tracks
/// through a remote-tracking upstream are fetched (`git::upstream_remotes`),
/// and a repository with none is never fetched: no other ref decides
/// whether work landed.
struct Fetcher {
    every: Option<Span>,
    /// When each repository, by its common directory, is next due, in Unix
    /// seconds.
    due: HashMap<PathBuf, i64>,
    /// The failure last logged for a repository and remote, so a remote
    /// that stays unreachable is logged when the error changes, not on
    /// every try.
    failing: HashMap<(PathBuf, String), String>,
    random: std::collections::hash_map::RandomState,
    draws: u64,
}

impl Fetcher {
    fn new(every: Option<Span>) -> Fetcher {
        Fetcher {
            every,
            due: HashMap::new(),
            failing: HashMap::new(),
            random: std::collections::hash_map::RandomState::new(),
            draws: 0,
        }
    }

    /// The repositories of `snap` that a fetch could tell something.
    fn fetched<'a>(&self, snap: &'a Snapshot) -> impl Iterator<Item = &'a Repo> {
        let on = self.every.is_some();
        snap.repos
            .iter()
            .filter(move |r| on && !r.branches.is_empty())
    }

    /// The next moment a repository of `snap` is due, if any.
    fn next(&self, snap: &Snapshot) -> Option<i64> {
        self.fetched(snap)
            .map(|r| self.due.get(&r.common_dir).copied().unwrap_or(0))
            .min()
    }

    /// `every`, scaled by a random factor between 0.9 and 1.1.
    fn interval(&mut self, every: &Span) -> i64 {
        self.draws += 1;
        let bits = self.random.hash_one(self.draws) >> 11;
        let unit = bits as f64 / (1u64 << 53) as f64;
        ((every.seconds as f64) * (0.9 + 0.2 * unit))
            .round()
            .max(60.0) as i64
    }

    /// Fetch every repository of `snap` that is due. `true` when a
    /// remote-tracking ref changed, or when residue could not tell: the
    /// caller scans again.
    fn run(&mut self, snap: &Snapshot) -> bool {
        let Some(every) = self.every.clone() else {
            return false;
        };
        let known: HashSet<&Path> = snap.repos.iter().map(|r| r.common_dir.as_path()).collect();
        self.due.retain(|dir, _| known.contains(dir.as_path()));
        self.failing
            .retain(|(dir, _), _| known.contains(dir.as_path()));
        let mut changed = false;
        let repos: Vec<&Repo> = self.fetched(snap).collect();
        for repo in repos {
            if self
                .due
                .get(&repo.common_dir)
                .is_some_and(|t| *t > output::now())
            {
                continue;
            }
            changed |= self.fetch(repo, &every);
            let next = output::now() + self.interval(&every);
            self.due.insert(repo.common_dir.clone(), next);
        }
        changed
    }

    /// Fetch the tracked remotes of one repository. `true` when a
    /// remote-tracking ref changed, or when residue could not tell.
    fn fetch(&mut self, repo: &Repo, every: &Span) -> bool {
        let shown = output::home(&repo.dir.to_string_lossy());
        let remotes = match git::upstream_remotes(&repo.dir, &repo.branches) {
            Ok(remotes) => remotes,
            Err(e) => {
                self.failed(
                    repo,
                    "",
                    &format!("cannot list the remotes to fetch in {shown}: {e}"),
                    every,
                );
                return false;
            }
        };
        let before = git::remote_refs(&repo.dir);
        let mut fetched = false;
        for remote in &remotes {
            match git::fetch(&repo.dir, remote) {
                Ok(()) => {
                    if self
                        .failing
                        .remove(&(repo.common_dir.clone(), remote.clone()))
                        .is_some()
                    {
                        log(&format!("fetch of {remote} in {shown} works again"));
                    }
                    fetched = true;
                }
                Err(e) => self.failed(
                    repo,
                    remote,
                    &format!(
                        "fetch of {remote} in {shown} failed; its refs stay as they were ({e})"
                    ),
                    every,
                ),
            }
        }
        if !fetched {
            return false;
        }
        let (Ok(before), Ok(after)) = (before, git::remote_refs(&repo.dir)) else {
            return true;
        };
        for name in before.keys().filter(|r| !after.contains_key(*r)) {
            if repo.upstreams.contains(name) {
                log(&format!(
                    "fetched {shown}: {} is gone from the remote",
                    name.strip_prefix("refs/remotes/").unwrap_or(name)
                ));
            }
        }
        before != after
    }

    /// Log a failure once, until it changes or the fetch works again.
    fn failed(&mut self, repo: &Repo, remote: &str, message: &str, every: &Span) {
        let key = (repo.common_dir.clone(), remote.to_string());
        if self.failing.get(&key).map(String::as_str) == Some(message) {
            return;
        }
        log(&format!("{message}; next try in about {}", every.text));
        self.failing.insert(key, message.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(roots: &[&str]) -> Options {
        Options {
            grace: Span::parse("--grace", "1h").unwrap(),
            idle: Span::parse("--idle", "2d").unwrap(),
            min_free: 10.0,
            keep_removed: cli::Span::parse("--keep-removed", "30d").unwrap(),
            fetch_every: Some(Span::parse("--fetch-every", "30m").unwrap()),
            dry_run: true,
            once: false,
            roots: roots.iter().map(PathBuf::from).collect(),
            default_roots: false,
        }
    }

    /// `service status` and a second `service install` read the roots back
    /// from what `to_args` wrote, whatever the options.
    #[test]
    fn roots_in_reads_back_what_to_args_wrote() {
        let roots = ["/home/a/src", "/home/a/my repos", "/srv/-dash"];
        let mut opts = options(&roots);
        for fetch in [Some(Span::parse("--fetch-every", "5m").unwrap()), None] {
            opts.fetch_every = fetch;
            let args = opts.to_args();
            assert_eq!(args[0], "watch");
            assert_eq!(roots_in(&args[1..]), roots);
        }
    }

    #[test]
    fn to_args_records_the_fetch_setting() {
        let mut opts = options(&["/r"]);
        assert!(
            opts.to_args()
                .windows(2)
                .any(|w| w == ["--fetch-every", "30m"])
        );
        opts.fetch_every = None;
        let args = opts.to_args();
        assert!(args.contains(&"--no-fetch".to_string()));
        assert!(!args.contains(&"--fetch-every".to_string()));
    }

    /// `service status` reads `--keep-removed` back from what `to_args`
    /// wrote, and never takes its value for a root.
    #[test]
    fn option_in_reads_back_keep_removed() {
        let mut opts = options(&["/r"]);
        opts.keep_removed = cli::Span::parse("--keep-removed", "12w").unwrap();
        let args = opts.to_args();
        assert_eq!(
            option_in(&args[1..], "--keep-removed").as_deref(),
            Some("12w")
        );
        assert_eq!(roots_in(&args[1..]), ["/r"]);
        let old: Vec<String> = ["--grace", "1h", "--no-fetch", "/r"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(option_in(&old, "--keep-removed"), None);
    }

    #[test]
    fn keep_removed_is_parsed_as_remove_parses_it() {
        let parse = |v: &str| {
            let args: Vec<OsString> = ["--keep-removed", v, "/"]
                .iter()
                .map(OsString::from)
                .collect();
            Options::parse(&args).map(|o| o.map(|o| o.keep_removed.seconds))
        };
        assert_eq!(parse("2w"), Ok(Some(14 * 86_400)));
        for bad in ["0d", "30", "1y", "101000w"] {
            assert!(parse(bad).is_err(), "{bad} was accepted");
        }
        let none: Vec<OsString> = vec![OsString::from("/")];
        let opts = Options::parse(&none).unwrap().unwrap();
        assert_eq!(opts.keep_removed.text, cli::DEFAULT_KEEP_REMOVED);
    }

    #[test]
    fn no_fetch_and_fetch_every_conflict() {
        let args: Vec<OsString> = ["--no-fetch", "--fetch-every", "5m", "/"]
            .iter()
            .map(OsString::from)
            .collect();
        assert!(Options::parse(&args).is_err());
    }

    #[test]
    fn help_lists_every_default_root() {
        for name in DEFAULT_ROOTS {
            assert!(
                HELP.split_whitespace().any(|w| w == format!("~/{name}")),
                "HELP does not list ~/{name}"
            );
        }
    }
}
