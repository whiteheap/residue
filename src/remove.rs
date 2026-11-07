//! `residue remove`: judge, confirm, re-check, move aside, check again,
//! then let Git delete it, or keep it in the trash.
//!
//! The plan records a fingerprint of each worktree (`inspect::Fingerprint`).
//! Right before acting, residue takes a fresh process snapshot, repeats
//! every check, and compares the fingerprint; any change since the plan
//! skips the worktree. Then `git worktree move` renames it (one rename(2),
//! never a copy) to a hidden sibling, `.NAME.residue-removing-PID`, or into
//! the trash, and locks it. After the rename no new process can reach it by
//! its old path, so residue checks once more at the new path: a fresh
//! process snapshot (which sees any process that still holds a directory
//! or file inside it) and the fingerprint. Only then does `git worktree
//! remove`, without `--force`, delete it; Git applies its own refusals
//! (modified or untracked files, submodules) on top. On any doubt or
//! failure residue moves the worktree back where it was.
//!
//! What remains: from the last check to the end of the delete, a process
//! that opens the hidden directory by its new name, or one the probe cannot
//! see, can still write into it, and that write is deleted with it.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::activity::{Evidence, Snapshot};
use crate::cli::{Options, Span};
use crate::commands::{self, Target, VERSION, fail, locate};
use crate::discover::{self, Repo};
use crate::git::{self, Entry};
use crate::inspect::{self, Report, Verdict};
use crate::json::Json;
use crate::output::{self, Places, Style, clean};
use crate::policy::Policy;
use crate::trash;

/// The lock reason on a worktree residue has moved aside to delete.
pub const REMOVING: &str = "residue: removing";
const ASIDE: &[u8] = b".residue-removing-";

/// `.NAME.residue-removing-PID` next to the worktree.
fn aside_path(original: &Path) -> Option<PathBuf> {
    let mut name = OsString::from(".");
    name.push(original.file_name()?);
    name.push(OsStr::from_bytes(ASIDE));
    name.push(std::process::id().to_string());
    Some(original.with_file_name(name))
}

/// For a directory named `.NAME.residue-removing-PID`, the worktree path
/// it was moved from and the PID of the run that moved it.
fn parse_aside(aside: &Path) -> Option<(PathBuf, u32)> {
    let name = aside.file_name()?.as_bytes();
    let rest = name.strip_prefix(b".")?;
    let at = rest.windows(ASIDE.len()).rposition(|w| w == ASIDE)?;
    let digits = &rest[at + ASIDE.len()..];
    if at == 0 || digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let pid = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((aside.with_file_name(OsStr::from_bytes(&rest[..at])), pid))
}

/// The worktree path a `.NAME.residue-removing-PID` directory was moved
/// from: a residue run stopped before it finished the removal.
pub fn original_of(aside: &Path) -> Option<PathBuf> {
    parse_aside(aside).map(|(original, _)| original)
}

/// A process with this PID exists (it may belong to another user).
fn running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// A worktree a `remove` run moved aside to delete and did not finish:
/// residue was killed, or the machine stopped, partway.
pub struct Leftover {
    pub path: PathBuf,
    pub original: PathBuf,
    pub pid: u32,
    /// A process with that PID still runs, so the removal may be in
    /// progress. PIDs are reused, so this is a hint, not proof.
    pub running: bool,
    /// Its directory is still there. If residue stopped during the delete,
    /// part of it may already be gone.
    pub present: bool,
    /// The one line that recovers it: moves it back, or clears Git's
    /// record when the directory is gone.
    pub recovery: String,
}

impl Leftover {
    pub fn of(entry: &Entry, main: &Path) -> Option<Leftover> {
        let (original, pid) = parse_aside(&entry.path)?;
        let present = fs::symlink_metadata(&entry.path).is_ok();
        let git = format!("git -C {} worktree", output::shell(main));
        let path = output::shell(&entry.path);
        let unlock = if entry.locked.as_deref() == Some(REMOVING) {
            format!("{git} unlock {path} && ")
        } else {
            String::new()
        };
        let recovery = if present {
            format!("{unlock}{git} move {path} {}", output::shell(&original))
        } else {
            format!("{unlock}{git} remove {path}")
        };
        Some(Leftover {
            path: entry.path.clone(),
            original,
            pid,
            running: running(pid),
            present,
            recovery,
        })
    }

    pub fn text(&self, places: &Places, style: &Style) -> String {
        let action = if self.present {
            "put it back"
        } else {
            "its directory is gone; clear Git's record"
        };
        let line = if self.running {
            format!(
                "process {} still runs, so residue may still be removing it; if it is not residue, {action}: {}",
                self.pid,
                clean(&self.recovery)
            )
        } else {
            format!("{action}: {}", clean(&self.recovery))
        };
        format!(
            "  {}  {}\n    {}\n",
            places.show(&self.path),
            style.dim(&format!("was {}", places.show(&self.original))),
            line
        )
    }

    pub fn json(&self) -> Json {
        Json::Obj(vec![
            ("path", Json::path(&self.path)),
            ("original_path", Json::path(&self.original)),
            ("pid", Json::UInt(u64::from(self.pid))),
            ("pid_running", Json::Bool(self.running)),
            ("present", Json::Bool(self.present)),
            ("recovery", Json::str(self.recovery.clone())),
        ])
    }
}

/// The heading over a list of [`Leftover`]s.
pub const LEFTOVER_TITLE: &str = "interrupted removals (a residue run moved these aside to delete them and stopped; if it stopped during the delete, part may be gone):";

/// The one line that puts a moved worktree back by hand.
pub fn recovery(main: &Path, moved: &Path, original: &Path, locked: bool) -> String {
    let git = format!("git -C {} worktree", output::shell(main));
    let unlock = if locked {
        format!("{git} unlock {} && ", output::shell(moved))
    } else {
        String::new()
    };
    format!(
        "{unlock}{git} move {} {}",
        output::shell(moved),
        output::shell(original)
    )
}

enum Outcome {
    Removed,
    /// Moved into the trash; the entry directory.
    Trashed(PathBuf),
    Refused,
    Skipped(String),
    Failed(String),
    Invalid(String),
}

impl Outcome {
    fn code(&self) -> &'static str {
        match self {
            Outcome::Removed => "removed",
            Outcome::Trashed(_) => "trashed",
            Outcome::Refused => "refused",
            Outcome::Skipped(_) => "skipped",
            Outcome::Failed(_) => "failed",
            Outcome::Invalid(_) => "invalid",
        }
    }

    fn done(&self) -> bool {
        matches!(self, Outcome::Removed | Outcome::Trashed(_))
    }
}

struct Item {
    arg: PathBuf,
    target: Option<Target>,
    report: Option<Report>,
    outcome: Option<Outcome>,
}

/// One entry of the trash old enough to purge, in a repository this run
/// covers.
struct Purge {
    item: trash::Item,
    report: Option<Report>,
    outcome: Option<Outcome>,
}

/// How this run treats the worktrees it removes.
struct Mode {
    trash: bool,
    keep: Span,
    /// `you confirmed`, or `the plan` under `--yes`.
    since: &'static str,
    /// `--all`: the PATHs are scan roots.
    sweep: bool,
    /// `--only-if-upstream-gone`, checked at the plan and in the re-check.
    only_if_upstream_gone: bool,
    started: i64,
}

pub fn remove(options: &Options) -> u8 {
    let started = output::now();
    let policy = match commands::prepare(options) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let mode = Mode {
        trash: options.trash(),
        keep: options.keep_trash(),
        since: if options.yes {
            "the plan"
        } else {
            "you confirmed"
        },
        sweep: options.all,
        only_if_upstream_gone: options.only_if_upstream_gone,
        started,
    };
    let places = Places::new();
    let style = Style::stdout();
    let (mut items, mut errors, repos) = if options.all {
        sweep(&options.paths)
    } else {
        let items = named(&options.paths);
        let repos = repos_of(&items);
        (items, Vec::new(), repos)
    };

    let snapshot = Snapshot::take();
    let jobs: Vec<(&Entry, &Path)> = items
        .iter()
        .filter_map(|i| i.target.as_ref())
        .map(|t| (&t.entry, t.repo.common_dir.as_path()))
        .collect();
    let mut reports = inspect::all(&jobs, &snapshot, &policy).into_iter();
    for item in &mut items {
        if item.target.is_some() {
            let Some(report) = reports.next() else {
                item.outcome = Some(Outcome::Failed("internal: no report".into()));
                continue;
            };
            if report.verdict != Verdict::Candidate {
                item.outcome = Some(Outcome::Refused);
            } else if mode.only_if_upstream_gone {
                if let Err(why) = upstream_gone(&report) {
                    item.outcome =
                        Some(Outcome::Skipped(format!("--only-if-upstream-gone: {why}")));
                }
            }
            item.report = Some(report);
        }
    }

    // --keep-trash is the only clock for the trash. --idle does not apply
    // there: the move into the trash rewrote each entry's gitdir, so its
    // last change is the time it was trashed, and --idle would only stretch
    // --keep-trash without saying so.
    let mut purge_policy = policy.clone();
    purge_policy.idle = None;
    let mut purges = expired(&repos, &mode.keep, started, &mut errors);
    let judged: Vec<Entry> = purges
        .iter()
        .filter_map(|p| p.item.registered.as_ref().map(as_judged))
        .collect();
    let purge_jobs: Vec<(&Entry, &Path)> = purges
        .iter()
        .filter(|p| p.item.registered.is_some())
        .zip(&judged)
        .map(|(p, e)| (e, p.item.common_dir.as_path()))
        .collect();
    let mut purge_reports = inspect::all(&purge_jobs, &snapshot, &purge_policy).into_iter();
    for p in &mut purges {
        if p.item.registered.is_some() {
            let report = purge_reports.next();
            if report
                .as_ref()
                .is_none_or(|r| r.verdict != Verdict::Candidate)
            {
                p.outcome = Some(Outcome::Refused);
            }
            p.report = report;
        }
    }

    let ready: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].outcome.is_none())
        .collect();
    let purgeable: Vec<usize> = (0..purges.len())
        .filter(|&i| purges[i].outcome.is_none())
        .collect();
    if !options.json {
        print!(
            "{}",
            plan_text(&items, &purges, &errors, &mode, &places, &style)
        );
    }

    let mut declined: Option<String> = None;
    if (!ready.is_empty() || !purgeable.is_empty()) && !options.yes {
        declined = match confirm(ready.len(), purgeable.len(), mode.trash) {
            Ok(true) => None,
            Ok(false) => Some("not confirmed".to_string()),
            Err(e) => Some(e),
        };
        if let Some(why) = &declined {
            for &i in &ready {
                items[i].outcome = Some(Outcome::Skipped(why.clone()));
            }
            for &i in &purgeable {
                purges[i].outcome = Some(Outcome::Skipped(why.clone()));
            }
        }
    }

    for &i in &ready {
        if items[i].outcome.is_some() {
            continue;
        }
        let outcome = execute(&items[i], &policy, &mode);
        if !options.json {
            println!("{}", outcome_line(&items[i], &outcome, &places));
        }
        items[i].outcome = Some(outcome);
    }
    for &i in &purgeable {
        if purges[i].outcome.is_some() {
            continue;
        }
        let outcome = purge(&purges[i], &purge_policy, &mode);
        if !options.json {
            println!("{}", purge_line(&purges[i], &outcome, &places));
        }
        purges[i].outcome = Some(outcome);
    }

    // A sweep never asked for the worktrees it kept, so only a candidate
    // that was not removed, or a PATH it could not read, makes it fail.
    // The same holds for the trash: an entry the plan kept does not fail
    // the run, an entry it meant to purge and did not does.
    let done = items.iter().all(|i| match &i.outcome {
        Some(o) if o.done() => true,
        Some(Outcome::Refused) => options.all,
        _ => false,
    }) && purges
        .iter()
        .all(|p| matches!(p.outcome, Some(Outcome::Removed) | Some(Outcome::Refused)))
        && errors.is_empty();
    if options.json {
        println!(
            "{}",
            json(&items, &purges, &errors, &snapshot, &policy, &mode, started).render()
        );
    } else if let Some(why) = declined {
        println!("nothing removed: {}", clean(&why));
    } else if ready.is_empty() && purgeable.is_empty() && options.all {
        println!("nothing to remove");
    }
    if done { 0 } else { 3 }
}

/// A trash entry as `inspect` should judge it: residue's own trash lock is
/// not a reason to keep it, any other lock is.
fn as_judged(entry: &Entry) -> Entry {
    let mut e = entry.clone();
    if trash::locked_by_residue(&e) {
        e.locked = None;
    }
    e
}

/// The repositories of the named worktrees, each once.
fn repos_of(items: &[Item]) -> Vec<Repo> {
    let mut repos: Vec<Repo> = Vec::new();
    for t in items.iter().filter_map(|i| i.target.as_ref()) {
        if !repos.iter().any(|r| r.common_dir == t.repo.common_dir) {
            repos.push(Repo {
                common_dir: t.repo.common_dir.clone(),
                main: t.repo.main.clone(),
                linked: Vec::new(),
                trash: t.repo.trash.clone(),
                interrupted: Vec::new(),
            });
        }
    }
    repos
}

/// The trash entries of these repositories that are at least `keep` old,
/// and records that hold no worktree, which any purge clears.
fn expired(repos: &[Repo], keep: &Span, now: i64, errors: &mut Vec<String>) -> Vec<Purge> {
    let mut out = Vec::new();
    for repo in repos {
        match trash::list(&repo.common_dir, &repo.command_dir(), &repo.trash) {
            Ok(list) => {
                for item in list {
                    match &item.record {
                        Err(e) => errors.push(format!("trash entry {}: {e}", item.dir.display())),
                        Ok(_) if item.is_empty_record() => out.push(Purge {
                            item,
                            report: None,
                            outcome: None,
                        }),
                        Ok(_) if item.age(now).is_some_and(|a| a >= keep.seconds) => {
                            let outcome = if item.registered.is_none() {
                                Some(Outcome::Failed(
                                    "Git no longer lists it as a worktree, so residue cannot remove it with git worktree remove".into(),
                                ))
                            } else {
                                None
                            };
                            out.push(Purge {
                                item,
                                report: None,
                                outcome,
                            });
                        }
                        Ok(_) => {}
                    }
                }
            }
            Err(e) => errors.push(e),
        }
    }
    out
}

/// The worktree PATHs named on the command line, each resolved to one
/// linked worktree, duplicates dropped.
fn named(paths: &[PathBuf]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for arg in paths {
        let mut item = Item {
            arg: arg.clone(),
            target: None,
            report: None,
            outcome: None,
        };
        match locate(arg) {
            Ok(t)
                if items
                    .iter()
                    .any(|i| i.target.as_ref().map(|x| &x.canonical) == Some(&t.canonical)) =>
            {
                continue;
            }
            Ok(t) => match moved_by_residue(&t) {
                Some(why) => item.outcome = Some(Outcome::Invalid(why)),
                None => item.target = Some(t),
            },
            Err(e) => item.outcome = Some(Outcome::Invalid(e)),
        }
        items.push(item);
    }
    items
}

/// A worktree residue moved aside itself is never removed by name: one in
/// the trash leaves only through the purge or `residue restore`, and one an
/// interrupted run left under a hidden name waits for a person.
fn moved_by_residue(t: &Target) -> Option<String> {
    if t.canonical.starts_with(trash::root(&t.repo.common_dir)) {
        return Some(
            "it is in the trash; `residue restore` puts it back, and a remove run purges it after --keep-trash".into(),
        );
    }
    original_of(&t.canonical).map(|original| {
        format!(
            "a residue run moved it aside and stopped before it finished; `residue scan` prints the command that puts it back at {}",
            original.display()
        )
    })
}

/// `--all`: every linked worktree of the repositories a scan of PATH
/// finds, the PATHs or repositories it could not read, and the
/// repositories themselves.
fn sweep(roots: &[PathBuf]) -> (Vec<Item>, Vec<String>, Vec<Repo>) {
    let found = discover::find(roots);
    let mut items = Vec::new();
    for repo in &found.repos {
        for (i, entry) in repo.linked.iter().enumerate() {
            let others = repo
                .linked
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, e)| e.clone())
                .collect();
            // A stale entry has no directory to resolve; its report says so.
            let canonical = git::canonical(&entry.path).unwrap_or_else(|_| entry.path.clone());
            items.push(Item {
                arg: entry.path.clone(),
                target: Some(Target {
                    repo: Repo {
                        common_dir: repo.common_dir.clone(),
                        main: repo.main.clone(),
                        linked: others,
                        trash: repo.trash.clone(),
                        interrupted: repo.interrupted.clone(),
                    },
                    entry: entry.clone(),
                    canonical,
                }),
                report: None,
                outcome: None,
            });
        }
    }
    (items, found.errors, found.repos)
}

fn confirm(worktrees: usize, purges: usize, trash: bool) -> Result<bool, String> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err(
            "stdin is not a terminal, so residue cannot ask for confirmation; pass --yes only from the session that owns these worktrees and knows its task is finished, or from a scheduled `residue remove --all --idle`"
                .into(),
        );
    }
    let style = Style::stderr();
    let mut err = io::stderr();
    let count = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut parts = Vec::new();
    if worktrees > 0 {
        parts.push(if trash {
            format!(
                "Move {} to the trash",
                count(worktrees, "worktree", "worktrees")
            )
        } else {
            format!(
                "Remove {} with `git worktree remove`",
                count(worktrees, "worktree", "worktrees")
            )
        });
    }
    if purges > 0 {
        let what = count(purges, "trash entry", "trash entries");
        parts.push(if parts.is_empty() {
            format!("Purge {what}")
        } else {
            format!("purge {what}")
        });
    }
    let prompt = format!(
        "{}? Branches and commits stay. Type yes to continue: ",
        parts.join(" and ")
    );
    write!(err, "{}", style.bold(&prompt))
        .and_then(|_| err.flush())
        .map_err(|e| format!("cannot write the prompt: {e}"))?;
    let mut answer = String::new();
    stdin
        .lock()
        .read_line(&mut answer)
        .map_err(|e| format!("cannot read the answer: {e}"))?;
    Ok(answer.trim().eq_ignore_ascii_case("yes"))
}

/// `--only-if-upstream-gone`: `Ok` only when the local refs show that this
/// worktree's branch has an upstream and that it is gone. A detached HEAD,
/// a branch with no upstream, an upstream that exists, and a lookup that
/// failed all keep the worktree. residue never fetches, so this is the
/// remote as of the last fetch.
fn upstream_gone(report: &Report) -> Result<(), String> {
    let Some(branch) = report.entry.short_branch() else {
        return Err("it is on a detached HEAD, which has no upstream".into());
    };
    match &report.upstream {
        Ok(Some(u)) if u.gone => Ok(()),
        Ok(Some(u)) => Err(format!(
            "its upstream {} exists in the local refs",
            clean(&u.name)
        )),
        Ok(None) => Err(format!("branch {} has no upstream", clean(branch))),
        Err(e) => Err(format!(
            "residue could not read the upstream of branch {}: {}",
            clean(branch),
            clean(e)
        )),
    }
}

/// The worktree at `path` as Git lists it now, judged again against a
/// fresh snapshot and compared with the plan's report. `Ok` when nothing
/// changed. `ours` is a lock residue placed itself, which does not count.
/// With `only_if_upstream_gone`, the fresh report must also still show the
/// branch's upstream gone.
fn recheck(
    path: &Path,
    common_dir: &Path,
    before: &Report,
    policy: &Policy,
    since: &str,
    ours: Option<&str>,
    only_if_upstream_gone: bool,
) -> Result<Entry, Outcome> {
    let fresh = git::worktrees(path).map_err(Outcome::Failed)?;
    let listed = fresh
        .iter()
        .find(|e| git::canonical(&e.path).ok().as_deref() == Some(path))
        .ok_or_else(|| Outcome::Skipped("Git no longer lists it as a worktree".into()))?;
    let mut judged = listed.clone();
    if judged.locked.is_some() && judged.locked.as_deref() == ours {
        judged.locked = None;
    }
    compare_entry(&before.entry, &judged, since)?;
    let snapshot = Snapshot::take();
    let now = inspect::worktree(&judged, common_dir, &snapshot, policy);
    if now.verdict != Verdict::Candidate {
        let reasons: Vec<String> = now.blocking().map(|c| c.short.clone()).collect();
        return Err(Outcome::Skipped(format!(
            "changed since {since}: {}",
            reasons.join(", ")
        )));
    }
    let planned = before.fingerprint.as_ref().map_err(|e| {
        Outcome::Skipped(format!(
            "residue could not record what the plan showed: {e}"
        ))
    })?;
    let measured = now
        .fingerprint
        .as_ref()
        .map_err(|e| Outcome::Skipped(format!("residue could not measure it again: {e}")))?;
    if let Some(why) = planned.changed(measured, since) {
        return Err(Outcome::Skipped(why));
    }
    if only_if_upstream_gone {
        upstream_gone(&now)
            .map_err(|why| Outcome::Skipped(format!("changed since {since}: {why}")))?;
    }
    Ok(listed.clone())
}

/// HEAD, branch and lock as Git lists them, against the plan.
fn compare_entry(before: &Entry, now: &Entry, since: &str) -> Result<(), Outcome> {
    let short = |h: &Option<String>| {
        h.as_deref()
            .map_or("none".to_string(), |h| h[..h.len().min(12)].to_string())
    };
    if now.head != before.head {
        return Err(Outcome::Skipped(format!(
            "HEAD changed since {since} ({} -> {})",
            short(&before.head),
            short(&now.head)
        )));
    }
    if now.branch != before.branch {
        let name = |b: &Option<String>| b.as_deref().map_or("detached".to_string(), str::to_string);
        return Err(Outcome::Skipped(format!(
            "branch changed since {since} ({} -> {})",
            name(&before.branch),
            name(&now.branch)
        )));
    }
    if now.locked != before.locked {
        return Err(Outcome::Skipped(format!(
            "lock changed since {since}{}",
            now.locked
                .as_deref()
                .filter(|r| !r.is_empty())
                .map(|r| format!(" (locked: {r})"))
                .unwrap_or_default()
        )));
    }
    Ok(())
}

/// The device of `path` and of the directory it would move into.
fn same_filesystem(path: &Path, into: &Path) -> Result<bool, String> {
    let a = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let b = fs::metadata(into).map_err(|e| format!("{}: {e}", into.display()))?;
    Ok(a.dev() == b.dev())
}

/// Re-check one confirmed worktree, move it aside, check it again, then
/// delete it or leave it in the trash.
fn execute(item: &Item, policy: &Policy, mode: &Mode) -> Outcome {
    let (Some(target), Some(before)) = (&item.target, &item.report) else {
        return Outcome::Failed("internal: no target".into());
    };
    let since = mode.since;
    let original = target.canonical.as_path();
    let listed = match recheck(
        original,
        &target.repo.common_dir,
        before,
        policy,
        since,
        None,
        mode.only_if_upstream_gone,
    ) {
        Ok(l) => l,
        Err(outcome) => return outcome,
    };
    let main = target.repo.command_dir();

    // Where it goes: a hidden sibling, or a new trash entry.
    let (dest, entry, reason) = if mode.trash {
        match same_filesystem(original, &target.repo.common_dir) {
            Ok(true) => {}
            Ok(false) => {
                return Outcome::Skipped(format!(
                    "the trash ({}) is on another filesystem, and residue moves, never copies; run without --trash (or with --no-trash) to delete it",
                    trash::root(&target.repo.common_dir).display()
                ));
            }
            Err(e) => return Outcome::Failed(e),
        }
        let record = trash::Record {
            original: original.to_path_buf(),
            branch: listed.branch.clone(),
            head: listed.head.clone(),
            trashed: output::now(),
            allocated: before.usage.as_ref().map(|u| u.allocated),
            reclaimable: before.usage.as_ref().map(|u| u.reclaimable),
        };
        let entry = match trash::create(&target.repo.common_dir, &record) {
            Ok(e) => e,
            Err(e) => return Outcome::Failed(format!("cannot make a trash entry: {e}")),
        };
        let Some(name) = original.file_name() else {
            return Outcome::Failed("the worktree path has no name".into());
        };
        (entry.join(name), Some(entry), trash::LOCK)
    } else {
        let Some(dest) = aside_path(original) else {
            return Outcome::Failed("the worktree path has no name".into());
        };
        let Some(parent) = original.parent() else {
            return Outcome::Failed("the worktree path has no parent".into());
        };
        match same_filesystem(original, parent) {
            Ok(true) => {}
            Ok(false) => {
                return Outcome::Skipped(
                    "it is a mount point, so residue cannot move it aside before deleting it"
                        .into(),
                );
            }
            Err(e) => return Outcome::Failed(e),
        }
        match fs::symlink_metadata(&dest) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Outcome::Skipped(format!(
                    "{} already exists, so residue cannot move it aside",
                    dest.display()
                ));
            }
            Err(e) => return Outcome::Failed(format!("{}: {e}", dest.display())),
        }
        (dest, None, REMOVING)
    };
    let moved = Moved {
        main: &main,
        dest: &dest,
        original,
        reason,
        entry: entry.as_deref(),
    };
    if let Err(e) = git::move_worktree(&main, original, &dest) {
        return Outcome::Failed(moved.drop_entry(format!("could not move it aside: {e}")));
    }
    if let Err(e) = git::lock_worktree(&main, &dest, reason) {
        return moved.put_back(false, &format!("could not lock it: {e}"));
    }
    if let Err(why) = verify_moved(&dest, before, policy, since, reason) {
        return moved.put_back(true, &why);
    }
    if let Some(entry) = entry {
        return Outcome::Trashed(entry);
    }
    if let Err(e) = git::unlock_worktree(&main, &dest) {
        return moved.put_back(true, &format!("could not unlock it: {e}"));
    }
    match git::remove_worktree(&main, &dest) {
        Ok(()) => Outcome::Removed,
        Err(e) => match moved.put_back(false, &e) {
            Outcome::Skipped(why) => Outcome::Failed(why),
            other => other,
        },
    }
}

/// A worktree residue has moved aside: where it was, where it is, the lock
/// reason residue gives it there, and its trash entry, if any.
struct Moved<'a> {
    main: &'a Path,
    dest: &'a Path,
    original: &'a Path,
    reason: &'static str,
    entry: Option<&'a Path>,
}

impl Moved<'_> {
    /// Drop the trash entry made for this move, adding any failure to `why`.
    fn drop_entry(&self, why: String) -> String {
        match self.entry.map(trash::discard) {
            Some(Err(e)) => format!("{why}; {e}"),
            _ => why,
        }
    }

    /// Move the worktree back to where it was, and report `why` residue
    /// stopped. If the move back fails, it stays locked with residue's own
    /// reason, so no later run removes it, and the outcome names the one
    /// line that recovers it.
    fn put_back(&self, locked: bool, why: &str) -> Outcome {
        // `git worktree move` into an existing directory moves the worktree
        // inside it, so something new at the original path stops the move.
        match fs::symlink_metadata(self.original) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return self.stuck(locked, why, "something exists at that path again"),
            Err(e) => return self.stuck(locked, why, &e.to_string()),
        }
        let unlocked = if locked {
            git::unlock_worktree(self.main, self.dest)
        } else {
            Ok(())
        };
        match unlocked.and_then(|()| git::move_worktree(self.main, self.dest, self.original)) {
            Ok(()) => Outcome::Skipped(self.drop_entry(format!("{why}; it is back in place"))),
            Err(e) => self.stuck(locked, why, &e),
        }
    }

    /// The worktree could not go back: lock it with residue's reason, so
    /// no later run removes it, and name the line that recovers it.
    fn stuck(&self, locked: bool, why: &str, cause: &str) -> Outcome {
        let relocked = git::lock_worktree(self.main, self.dest, self.reason).is_ok();
        Outcome::Failed(format!(
            "{why}; residue could not move it back to {} ({cause}). It is at {}. Recover with: {}",
            self.original.display(),
            self.dest.display(),
            recovery(self.main, self.dest, self.original, locked || relocked)
        ))
    }
}

/// After the move: Git lists the worktree at its new path, locked by
/// residue, with the HEAD and branch of the plan; no process the probe can
/// see is inside it; and its fingerprint still matches the plan.
fn verify_moved(
    dest: &Path,
    before: &Report,
    policy: &Policy,
    since: &str,
    reason: &str,
) -> Result<(), String> {
    let list = git::worktrees(dest)?;
    let listed = list
        .iter()
        .find(|e| git::canonical(&e.path).ok().as_deref() == Some(dest))
        .ok_or("Git does not list it at its new path")?;
    if listed.locked.as_deref() != Some(reason) {
        return Err("its lock changed after residue moved it aside".into());
    }
    let mut judged = listed.clone();
    judged.locked = None;
    if let Err(Outcome::Skipped(why)) = compare_entry(&before.entry, &judged, since) {
        return Err(why);
    }
    let snapshot = Snapshot::take();
    match snapshot.evidence(dest) {
        Evidence::NotObserved => {}
        Evidence::InUse(procs) => {
            let names: Vec<String> = procs
                .iter()
                .map(|(pid, cmd)| format!("{cmd}[{pid}]"))
                .collect();
            return Err(format!(
                "in use by {} after residue moved it aside",
                names.join(", ")
            ));
        }
        Evidence::Unknown(u) => {
            return Err(format!(
                "activity unknown after residue moved it aside: {} ({})",
                u.detail, u.reason
            ));
        }
    }
    let planned = before
        .fingerprint
        .as_ref()
        .map_err(|e| format!("residue could not record what the plan showed: {e}"))?;
    let now = planned
        .again(dest, listed.head.clone(), policy)
        .map_err(|e| format!("cannot measure it after moving it aside: {e}"))?;
    match planned.changed(&now, since) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// Re-check one expired trash entry, then delete it with `git worktree
/// remove`.
fn purge(p: &Purge, policy: &Policy, mode: &Mode) -> Outcome {
    if !p.item.is_empty_record() {
        let (Some(worktree), Some(before)) = (&p.item.worktree, &p.report) else {
            return Outcome::Failed("internal: no report".into());
        };
        let path = git::canonical(worktree).unwrap_or_else(|_| worktree.clone());
        if let Err(outcome) = recheck(
            &path,
            &p.item.common_dir,
            before,
            policy,
            mode.since,
            Some(trash::LOCK),
            false,
        ) {
            return outcome;
        }
    }
    match trash::purge(&p.item) {
        Ok(()) => Outcome::Removed,
        Err(e) => Outcome::Failed(e),
    }
}

fn plan_text(
    items: &[Item],
    purges: &[Purge],
    errors: &[String],
    mode: &Mode,
    places: &Places,
    style: &Style,
) -> String {
    let ready: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].outcome.is_none())
        .collect();
    let purgeable: Vec<usize> = (0..purges.len())
        .filter(|&i| purges[i].outcome.is_none())
        .collect();
    let mut out = String::new();
    out.push_str(&style.dim(&format!("{}\n", commands::header(mode.started))));
    if !ready.is_empty() {
        let title = if mode.trash {
            format!("will move to the trash ({}):", ready.len())
        } else {
            format!("will remove ({}):", ready.len())
        };
        out.push_str(&format!("\n{}\n", style.bold(&title)));
        if mode.trash {
            out.push_str(&style.dim(&format!(
                "  `residue restore` puts one back. A remove run purges entries older than {} (--keep-trash), which frees the disk.\n",
                mode.keep.text
            )));
        }
        for &i in &ready {
            let (Some(t), Some(r)) = (&items[i].target, &items[i].report) else {
                continue;
            };
            let size = r.usage.as_ref().map_or(String::new(), output::usage_phrase);
            let branch = r
                .entry
                .short_branch()
                .map_or("detached".to_string(), |b| format!("[{}]", clean(b)));
            out.push_str(&format!(
                "  {}  {}\n    {}\n",
                style.bold(&places.show(&t.canonical)),
                style.dim(&branch),
                size
            ));
            let deleted: Vec<String> = r.ignored.iter().map(|i| i.sized()).collect();
            if !deleted.is_empty() {
                out.push_str(&format!(
                    "    {} ignored: {}\n",
                    if mode.trash { "keeps" } else { "deletes" },
                    clean(&deleted.join(", "))
                ));
            }
            if let (Ok(Some(n)), Some(b)) = (&r.local_commits, r.entry.short_branch()) {
                if *n > 0 {
                    out.push_str(&format!(
                        "    {n} commit{} on no remote-tracking ref {} on branch {}\n",
                        if *n == 1 { "" } else { "s" },
                        if *n == 1 { "stays" } else { "stay" },
                        clean(b)
                    ));
                }
            }
            if let Some(scope) = r.checks.iter().find(|c| c.id == "activity") {
                out.push_str(&format!("    {}\n", style.dim(&clean(&scope.detail))));
            }
        }
    }
    if !purgeable.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            style.bold(&format!(
                "will purge from the trash ({}), older than {}:",
                purgeable.len(),
                mode.keep.text
            ))
        ));
        for &i in &purgeable {
            out.push_str(&purge_summary(&purges[i], places, style));
        }
    }
    let refused: Vec<&Item> = items.iter().filter(|i| i.outcome.is_some()).collect();
    if !refused.is_empty() {
        let title = if mode.sweep {
            format!("kept ({}):", refused.len())
        } else {
            "not removed:".to_string()
        };
        out.push_str(&format!("\n{}\n", style.bold(&title)));
        for item in refused {
            let shown = item.target.as_ref().map_or_else(
                || clean(&item.arg.to_string_lossy()),
                |t| places.show(&t.canonical),
            );
            let why = match (&item.outcome, &item.report) {
                (Some(Outcome::Invalid(e) | Outcome::Skipped(e)), _) => clean(e),
                (_, Some(r)) => refusal(r),
                _ => String::new(),
            };
            out.push_str(&format!("  {shown}  {}\n", style.dim(&why)));
        }
        out.push_str(&style.dim("  details: residue explain PATH\n"));
    }
    let kept: Vec<&Purge> = purges.iter().filter(|p| p.outcome.is_some()).collect();
    if !kept.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            style.bold(&format!("kept in the trash ({}):", kept.len()))
        ));
        for p in kept {
            let why = match (&p.outcome, &p.report) {
                (Some(Outcome::Failed(e)), _) => clean(e),
                (_, Some(r)) => refusal(r),
                _ => String::new(),
            };
            out.push_str(&format!(
                "  {}  {}\n",
                places.show(&p.item.dir),
                style.dim(&why)
            ));
        }
    }
    if !errors.is_empty() {
        out.push_str(&format!("\n{}\n", style.bold("could not inspect:")));
        for e in errors {
            out.push_str(&format!("  {}\n", clean(e)));
        }
    }
    out.push('\n');
    out
}

/// `keep: 1 untracked · locked: resume later`, or why a stale entry is stale.
fn refusal(r: &Report) -> String {
    let reasons: Vec<String> = match &r.stale_reason {
        Some(why) => vec![clean(why)],
        None => r.blocking().map(|c| clean(&c.short)).collect(),
    };
    format!("{}: {}", r.verdict.code(), reasons.join(" · "))
}

fn purge_summary(p: &Purge, places: &Places, style: &Style) -> String {
    let now = output::now();
    let Ok(r) = &p.item.record else {
        return String::new();
    };
    let branch = r
        .branch
        .as_deref()
        .map(|b| format!("[{}]", clean(b.strip_prefix("refs/heads/").unwrap_or(b))))
        .unwrap_or_default();
    let detail = if p.item.is_empty_record() {
        "holds no worktree, only its record".to_string()
    } else {
        format!(
            "trashed {}{}",
            output::age(now - r.trashed),
            r.reclaimable
                .map(|b| format!(" · about {} reclaimable", output::bytes(b)))
                .unwrap_or_default()
        )
    };
    format!(
        "  {}  {}\n    was {}\n",
        style.bold(&places.show(&p.item.dir)),
        style.dim(&branch),
        style.dim(&format!("{} · {detail}", places.show(&r.original)))
    )
}

fn outcome_line(item: &Item, outcome: &Outcome, places: &Places) -> String {
    let shown = item.target.as_ref().map_or_else(
        || clean(&item.arg.to_string_lossy()),
        |t| places.show(&t.canonical),
    );
    let branch = item
        .report
        .as_ref()
        .and_then(|r| r.entry.short_branch())
        .map(|b| format!(" (branch {} kept)", clean(b)))
        .unwrap_or_default();
    let reclaimable = |label: &str| {
        item.report
            .as_ref()
            .and_then(|r| r.usage.as_ref())
            .map(|u| format!("; about {} {label}", output::bytes(u.reclaimable)))
            .unwrap_or_default()
    };
    match outcome {
        Outcome::Removed => format!("removed {shown}{branch}{}", reclaimable("reclaimable")),
        Outcome::Trashed(entry) => format!(
            "moved {shown} to the trash{branch}; put it back with: residue restore {}",
            output::shell(entry)
        ),
        Outcome::Skipped(why) => format!("skipped {shown}: {}", clean(why)),
        Outcome::Failed(why) => format!("failed {shown}: {}", clean(why)),
        Outcome::Refused | Outcome::Invalid(_) => format!("not removed {shown}"),
    }
}

fn purge_line(p: &Purge, outcome: &Outcome, places: &Places) -> String {
    let name = clean(&p.item.name);
    let was = p
        .item
        .record
        .as_ref()
        .map(|r| format!(" (was {})", places.show(&r.original)))
        .unwrap_or_default();
    match outcome {
        Outcome::Removed if p.item.is_empty_record() => {
            format!("cleared the empty trash record {name}{was}")
        }
        Outcome::Removed => format!(
            "purged {name} from the trash{was}{}",
            p.item
                .record
                .as_ref()
                .ok()
                .and_then(|r| r.reclaimable)
                .map(|b| format!("; about {} reclaimable", output::bytes(b)))
                .unwrap_or_default()
        ),
        Outcome::Skipped(why) => format!("kept {name} in the trash: {}", clean(why)),
        Outcome::Failed(why) => format!("failed to purge {name}: {}", clean(why)),
        _ => format!("kept {name} in the trash"),
    }
}

fn reason_json(outcome: &Option<Outcome>) -> Json {
    match outcome {
        Some(Outcome::Removed | Outcome::Trashed(_)) => Json::Null,
        Some(Outcome::Skipped(r) | Outcome::Failed(r) | Outcome::Invalid(r)) => {
            Json::str(r.clone())
        }
        Some(Outcome::Refused) => Json::str("one or more checks did not pass"),
        None => Json::str("not attempted"),
    }
}

fn json(
    items: &[Item],
    purges: &[Purge],
    errors: &[String],
    snapshot: &Snapshot,
    policy: &Policy,
    mode: &Mode,
    started: i64,
) -> Json {
    let results = items
        .iter()
        .map(|item| {
            Json::Obj(vec![
                ("path", Json::path(&item.arg)),
                (
                    "result",
                    Json::str(item.outcome.as_ref().map_or("not_attempted", Outcome::code)),
                ),
                ("reason", reason_json(&item.outcome)),
                (
                    "trash_entry",
                    match &item.outcome {
                        Some(Outcome::Trashed(entry)) => Json::path(entry),
                        _ => Json::Null,
                    },
                ),
                (
                    "worktree",
                    Json::opt(item.report.as_ref(), output::report_json),
                ),
            ])
        })
        .collect();
    let purged = purges
        .iter()
        .map(|p| {
            let result = match &p.outcome {
                Some(Outcome::Removed) => "purged",
                Some(Outcome::Refused) => "kept",
                Some(o) => o.code(),
                None => "not_attempted",
            };
            Json::Obj(vec![
                ("result", Json::str(result)),
                ("reason", reason_json(&p.outcome)),
                ("entry", trash::item_json(&p.item)),
                (
                    "worktree",
                    Json::opt(p.report.as_ref(), output::report_json),
                ),
            ])
        })
        .collect();
    Json::Obj(vec![
        ("schema", Json::str(output::SCHEMA_REMOVE)),
        ("version", Json::str(VERSION)),
        ("generated_at", Json::str(output::rfc3339(started))),
        ("activity", output::activity_json(snapshot)),
        (
            "ignored_environment_rules",
            output::ignored_environment_json(policy),
        ),
        ("rule_sources", output::policy_sources_json(policy)),
        ("idle", output::idle_json(policy)),
        (
            "trash",
            Json::Obj(vec![
                ("enabled", Json::Bool(mode.trash)),
                ("keep", Json::str(mode.keep.text.clone())),
                ("keep_seconds", Json::Int(mode.keep.seconds)),
            ]),
        ),
        ("results", Json::Arr(results)),
        ("purged", Json::Arr(purged)),
        (
            "errors",
            Json::Arr(errors.iter().map(|e| Json::str(e.clone())).collect()),
        ),
    ])
}
