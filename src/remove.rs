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
//! or file inside it), the fingerprint, and the walk, which must find no
//! nested repository, mount point or unreadable entry. Only then does
//! `git worktree remove`, without `--force`, delete it; Git applies its own
//! refusals (modified or untracked files, submodules) on top. On any doubt
//! or failure residue moves the worktree back where it was.
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
use crate::hold::{self, Taken};
use crate::inspect::{self, Fingerprint, Kept, Report, Verdict};
use crate::json::Json;
use crate::output::{self, Places, Style, clean};
use crate::policy::Policy;
use crate::removed::{self, Activity, By, Forgot, Reason, Unheld};
use crate::stats;
use crate::trash;

/// The lock reason on a worktree residue has moved aside to delete.
pub const REMOVING: &str = "residue: removing";
const ASIDE: &[u8] = b".residue-removing-";

/// The length in bytes of the longest path the worktree at `original`
/// (canonical) can have where residue deletes it, for the plan to hold
/// the paths inside it against the activity probe's limit there
/// (`Snapshot::evidence_at`). One in the trash is deleted where it is. Any
/// other is moved first, to `.NAME.residue-removing-PID` beside it, a PID
/// of up to ten digits, or with `--trash` to
/// `COMMON_DIR/residue-trash/NAME-STAMP-N/NAME` (`trash::create`), and
/// the longer of the two counts, whichever the run uses.
pub fn deleted_at(original: &Path, common_dir: &Path) -> usize {
    let len = original.as_os_str().len();
    let trash = trash::root(common_dir);
    if original.starts_with(&trash) {
        return len;
    }
    let name = original.file_name().map_or(0, |n| n.len());
    let aside = len + ".".len() + ASIDE.len() + u32::MAX.to_string().len();
    let trashed = trash.as_os_str().len() + "/".len() + name + "-YYYYMMDDTHHMMSSZ-99/".len() + name;
    aside.max(trashed)
}

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

/// The PID in a `.NAME.residue-removing-PID` directory's name: the run
/// that moved it there.
pub fn pid_of(aside: &Path) -> Option<u32> {
    parse_aside(aside).map(|(_, pid)| pid)
}

/// A process with this PID exists (it may belong to another user).
pub fn running(pid: u32) -> bool {
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
    /// Where its Git commands run.
    pub main: PathBuf,
    pub pid: u32,
    /// A process with that PID still runs, so the removal may be in
    /// progress. PIDs are reused, so this is a hint, not proof.
    pub running: bool,
    /// Its directory is still there. If residue stopped during the delete,
    /// part of it may already be gone.
    pub present: bool,
    /// Why Git can no longer use it, when its directory is there and Git
    /// cannot (`unusable`): Git then refuses to move it back or remove it.
    pub unusable: Option<String>,
    /// The removal record that names it, which `residue restore` takes
    /// (`with_records`).
    pub record: Option<PathBuf>,
    /// What may still act on it, from that record (`removed::Activity`):
    /// a run, its Git, or a program Git started holds the record, or the
    /// record comes from a release that takes no hold and its run still
    /// runs. Either way the removal may still be deleting it, or a restore
    /// moving it back. `Err` when residue could not tell, which includes a
    /// record that may name it and cannot be read; `Ok(Idle)` with no
    /// record.
    pub activity: Result<removed::Activity, String>,
    /// That record has its `hold` line, so its hold, not `running`, shows
    /// whether its removal still runs.
    pub held_record: bool,
    /// The one line that recovers it: moves it back, clears Git's record
    /// when the directory is gone and a removal record names it, which
    /// keeps its HEAD and reflog, or, when Git can no longer use it, the
    /// `residue restore` of its record, which says what to clear first.
    /// Empty when Git can no longer use it and no record names it, since no
    /// one command brings it back; when its directory is gone and no record
    /// names it, since clearing Git's record then drops a HEAD and reflog
    /// that nothing else keeps, and the directory may only be moved or on a
    /// disk not mounted (the text gives `missing` and `output::gc_note`
    /// instead); and unless residue could tell that its removal has
    /// stopped, since any move then lets Git delete it where it lands.
    /// Never a deletion.
    pub recovery: String,
    /// The line that clears Git's record of it once its directory is
    /// gone, and of no other worktree: given only where a removal record
    /// names it and keeps what that drops.
    pub clear: String,
    /// The line that moves it back by hand, when its directory is there
    /// and Git can use it: what a refusal names once residue cannot tell
    /// whether its removal still runs. Empty otherwise.
    pub move_back: String,
    /// The lock on Git's record of it, residue's own included: while it
    /// holds, `git gc` keeps that record.
    pub lock: Option<String>,
    /// Its Git directory, `COMMON/worktrees/ID`, when its directory is
    /// gone or Git can no longer use it, and residue found the one that
    /// names it: the text names its HEAD reflog by that ID.
    pub git_dir: Option<PathBuf>,
    /// What is at `original` now, when its directory is there: the move
    /// back stops while anything is (`output::vacant`), so the text says
    /// what instead of naming it.
    pub taken: Option<&'static str>,
    /// What to try first when its directory is gone
    /// (`output::missing_note`).
    pub missing: String,
}

impl Leftover {
    pub fn of(entry: &Entry, main: &Path, common_dir: &Path) -> Option<Leftover> {
        let (original, pid) = parse_aside(&entry.path)?;
        let present = fs::symlink_metadata(&entry.path).is_ok();
        let unusable = unusable(entry);
        let taken = present
            .then(|| fs::symlink_metadata(&original).ok())
            .flatten()
            .map(|m| output::file_kind(&m.file_type()));
        let git_dir = (!present || unusable.is_some())
            .then(|| removed::git_dir_of(common_dir, &[&entry.path]))
            .flatten();
        let git = format!("git -C {} worktree", output::shell(main));
        let path = output::shell(&entry.path);
        let ours = entry.locked.as_deref() == Some(REMOVING);
        let unlock = if ours {
            format!("{git} unlock {path} && ")
        } else {
            String::new()
        };
        // Gone: only a removal record that names it makes `clear` safe to
        // give as the line (`with_records`).
        let recovery = if unusable.is_none() && present {
            recovery(main, &entry.path, &original, ours)
        } else {
            String::new()
        };
        Some(Leftover {
            path: entry.path.clone(),
            original,
            main: main.to_path_buf(),
            pid,
            running: running(pid),
            present,
            unusable,
            record: None,
            activity: Ok(Activity::Idle),
            held_record: false,
            move_back: if present {
                recovery.clone()
            } else {
                String::new()
            },
            recovery,
            clear: format!("{unlock}{git} remove {path}"),
            lock: entry.locked.clone(),
            git_dir,
            taken,
            missing: output::missing_note(main),
        })
    }

    /// The removal record that names it, among the repository's `records`.
    /// When Git can no longer use it, the record's restore is the only line
    /// that works. Unless residue can tell that no run acts on the record
    /// (`Activity::Idle`), no line is safe, so there is none; that includes
    /// a record that may name it and cannot be read. `snapshot` is the
    /// command's activity probe.
    pub fn with_records(mut self, records: &[removed::Item], snapshot: &Snapshot) -> Leftover {
        let named = records
            .iter()
            .find(|i| i.record.as_ref().is_ok_and(|r| r.aside == self.path));
        if let Some(item) = named {
            self.activity = item.activity(Some(snapshot));
            self.held_record = item.record.as_ref().is_ok_and(|r| r.hold);
            self.record = Some(item.dir.clone());
        } else if let Some(item) = records
            .iter()
            .find(|i| i.may_be_unreadable_for(&self.original))
        {
            let e = item.record.as_ref().err().map_or("", String::as_str);
            self.activity = Err(format!(
                "a removal record that may name it cannot be read ({e}), and if that record does not name it, `{}` sets it aside, keeping its files and refs",
                item.set_aside_line()
            ));
        }
        match (&self.activity, &self.record) {
            (Ok(Activity::Idle), Some(dir)) if self.unusable.is_some() => {
                self.recovery = format!("residue restore {}", output::shell(dir));
            }
            // Its directory is gone and its removal record keeps its HEAD
            // and reflog, so clearing Git's record of it loses neither.
            (Ok(Activity::Idle), Some(_)) if !self.present => {
                self.recovery = self.clear.clone();
            }
            (Ok(Activity::Idle), _) => {}
            _ => self.recovery = String::new(),
        }
        self
    }

    /// Why no line is safe while something may still act on it.
    fn busy(&self) -> Option<String> {
        let again = "run residue scan again once it ends";
        match &self.activity {
            Ok(Activity::Idle) => None,
            Ok(Activity::Held) => match &self.record {
                Some(dir) if !self.running => Some(format!(
                    "residue run {} has exited, but a process still holds its removal record: the `git worktree remove` it started, a program that Git started, or another residue run (`lsof {}` names it), so Git may still be deleting it; {again}",
                    self.pid,
                    output::shell(&removed::info_of(dir))
                )),
                _ => Some(format!(
                    "a residue run, or the `git worktree remove` it started, still holds its removal record, so it may still be deleting it; {again}"
                )),
            },
            // A PID may be reused: when it alone keeps it, the move back by
            // hand is named for when it is not residue.
            Ok(Activity::Unheld(Unheld::Writer(pid, true))) if !self.move_back.is_empty() => {
                Some(format!(
                    "process {pid}, the residue run that removed it, is from a release that takes no hold and still runs, so it may still be deleting it; {again}; if `ps -p {pid}` shows it is not residue, this moves it back: {}",
                    clean(&self.move_back)
                ))
            }
            Ok(Activity::Unheld(Unheld::Writer(pid, _))) => Some(format!(
                "process {pid}, the residue run that removed it, is from a release that takes no hold and still runs, so it may still be deleting it; {again}; `ps -p {pid}` shows what it is"
            )),
            Ok(Activity::Unheld(Unheld::Git(pid))) => Some(format!(
                "process {pid}, a `git worktree remove` of it, still runs, and the residue run that removed it is from a release that takes no hold, so that Git may still be deleting it; {again}; `ps -p {pid}` shows what it is"
            )),
            Ok(Activity::Unheld(Unheld::Using(pid, command))) => Some(format!(
                "process {pid} ({}) is using it or its Git directory, and the residue run that removed it is from a release that takes no hold, so its removal may still be deleting it; {again}",
                clean(command)
            )),
            // What makes residue unable to tell may never change, such as a
            // probe that cannot see open files, so the move back by hand
            // is named, for once nothing can still be deleting it.
            Err(e) if !self.move_back.is_empty() => Some(format!(
                "residue cannot tell whether its removal still runs: {e}; once nothing can still be deleting it, such as after a restart, this moves it back: {}",
                clean(&self.move_back)
            )),
            Err(e) => Some(format!(
                "residue cannot tell whether its removal still runs: {e}; fix that, then run residue scan again"
            )),
        }
    }

    pub fn text(&self, places: &Places, style: &Style) -> String {
        let head = format!(
            "  {}  {}\n",
            places.show(&self.path),
            style.dim(&format!("was {}", places.show(&self.original))),
        );
        if let Some(why) = self.busy() {
            return format!("{head}    {why}\n");
        }
        // No removal record keeps what clearing Git's record drops, so the
        // text names no line that clears it.
        let gc = || output::gc_note(&self.main, self.lock.as_deref(), self.git_dir.as_deref());
        let line = match (&self.unusable, &self.record) {
            (Some(why), Some(_)) => format!(
                "Git can no longer use it ({why}); restore says what to clear, then recreates it: {}",
                clean(&self.recovery)
            ),
            (Some(why), None) => format!(
                "Git can no longer use it ({why}), and no removal record names it; {}",
                gc()
            ),
            (None, _) if self.present => match self.taken {
                Some(kind) => format!(
                    "{} is {kind} now, so it cannot go back there; move that away, then run residue scan again",
                    places.show(&self.original)
                ),
                None => format!("put it back: {}", clean(&self.recovery)),
            },
            // Gone may be only moved, or on a disk not mounted now: those
            // come first, since clearing Git's record then leaves the
            // directory, once back, no longer a Git worktree.
            (None, Some(_)) => format!(
                "its directory is gone; {}; otherwise `{}` clears Git's record of it, and its removal record keeps its HEAD",
                self.missing,
                clean(&self.clear)
            ),
            (None, None) => format!("its directory is gone; {}; {}", self.missing, gc()),
        };
        // A record with its hold line, free now, shows that its removal has
        // stopped, whatever process has that PID now.
        let line = if self.running && !self.held_record {
            format!(
                "process {} still runs, so residue may still be removing it; if it is not residue, {line}",
                self.pid
            )
        } else {
            line
        };
        let mut out = format!("{head}    {line}\n");
        // A run that stopped after writing its removal record: restore
        // moves it back, with the same checks.
        if let (None, Some(dir)) = (&self.unusable, &self.record) {
            let to = if self.taken.is_some() {
                " --to NEWPATH"
            } else {
                ""
            };
            out.push_str(&format!(
                "    or: residue restore {}{to}\n",
                output::shell(dir)
            ));
        }
        out
    }

    pub fn json(&self) -> Json {
        Json::Obj(vec![
            ("path", Json::path(&self.path)),
            ("original_path", Json::path(&self.original)),
            ("pid", Json::UInt(u64::from(self.pid))),
            ("pid_running", Json::Bool(self.running)),
            (
                "removal_running",
                match &self.activity {
                    Ok(activity) => Json::Bool(*activity != Activity::Idle),
                    Err(_) => Json::Null,
                },
            ),
            ("present", Json::Bool(self.present)),
            ("recovery", Json::str(self.recovery.clone())),
            ("unusable", Json::opt(self.unusable.clone(), Json::Str)),
            (
                "removal_record",
                Json::opt(self.record.as_deref(), Json::path),
            ),
        ])
    }
}

/// Why Git can no longer use the worktree it lists at `entry.path`, whose
/// directory is still there: a `git worktree remove` that stopped part
/// way deleted the `.git` file that ties it to its Git directory, or what
/// Git reads its HEAD from. Git then refuses to move it or remove it
/// ("validation failed"), so the move back `scan` would print cannot work.
pub fn unusable(entry: &Entry) -> Option<String> {
    fs::symlink_metadata(&entry.path).ok()?;
    if !checkout_present(&entry.path) {
        return Some("its .git file is gone".into());
    }
    if entry
        .head
        .as_deref()
        .is_none_or(|h| h.bytes().all(|b| b == b'0'))
    {
        return Some("Git reads no HEAD in its Git directory".into());
    }
    None
}

/// The heading over a list of [`Leftover`]s.
pub const LEFTOVER_TITLE: &str =
    "interrupted removals (moved aside, then stopped; part may already be deleted):";

/// The one line that puts a moved worktree back by hand, which stops
/// while something is at `original` (`output::vacant`).
pub fn recovery(main: &Path, moved: &Path, original: &Path, locked: bool) -> String {
    let git = format!("git -C {} worktree", output::shell(main));
    let unlock = if locked {
        format!("{git} unlock {} && ", output::shell(moved))
    } else {
        String::new()
    };
    format!(
        "{}{unlock}{git} move {} {}",
        output::vacant(original),
        output::shell(moved),
        output::shell(original)
    )
}

enum Outcome {
    /// Deleted; the removal record residue wrote first, which `residue
    /// restore` takes (none for a trash record that held no worktree).
    Removed(Option<PathBuf>),
    /// Moved into the trash; the entry directory.
    Trashed(PathBuf),
    Refused,
    /// A trash entry left alone on purpose, which does not fail the run:
    /// why. Only one on a filesystem with no flock(2), which a run of this
    /// release cannot hold (`Taken::Unsupported`).
    Kept(String),
    Skipped(String),
    Failed(String),
    Invalid(String),
}

impl Outcome {
    fn code(&self) -> &'static str {
        match self {
            Outcome::Removed(_) => "removed",
            Outcome::Trashed(_) => "trashed",
            Outcome::Refused => "refused",
            Outcome::Kept(_) => "kept",
            Outcome::Skipped(_) => "skipped",
            Outcome::Failed(_) => "failed",
            Outcome::Invalid(_) => "invalid",
        }
    }

    fn done(&self) -> bool {
        matches!(self, Outcome::Removed(_) | Outcome::Trashed(_))
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

/// One removal record older than `--keep-removed`, in a repository this
/// run covers: the run forgets it, its ref and then its files, after the
/// same confirmation as everything else.
struct Forget {
    item: removed::Item,
    /// `None` until the run acts; then `Ok` when it was forgotten.
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
    /// `--only-if-upstream-gone`, checked at the plan, in the re-check and
    /// after the move.
    only_if_upstream_gone: bool,
    /// `--keep-removed`: how long a removal record and its ref are kept.
    keep_removed: Span,
    /// Why this run removes, as each removal record says it.
    reason: Reason,
    /// The command this run is, as each removal record says it: `watch`'s
    /// child is the run with `--removal-reason`.
    by: By,
    started: i64,
    /// `--as-of TIME`, or the start when TIME is later: every age this run
    /// judges is measured at it, so a run confirming another run's plan
    /// purges and forgets only what had expired when that plan was made.
    as_of: Option<i64>,
}

impl Mode {
    /// When this run measures ages: `--as-of`, otherwise its start.
    fn ages_at(&self) -> i64 {
        self.as_of.unwrap_or(self.started)
    }
}

pub fn remove(options: &Options) -> u8 {
    let started = output::now();
    let as_of = options.as_of.map(|t| t.min(started));
    let mut policy = match commands::prepare(options) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    policy.idle_at = as_of;
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
        keep_removed: options.keep_removed(),
        reason: options.removal_reason.unwrap_or(if options.all {
            Reason::Sweep
        } else {
            Reason::Remove
        }),
        by: match (options.removal_reason, options.all) {
            (Some(_), _) => By::Watch,
            (None, true) => By::Sweep,
            (None, false) => By::Remove,
        },
        started,
        as_of,
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
    // What each repository keeps like a main checkout, read once for each.
    let mut kept: std::collections::HashMap<&Path, Kept> = std::collections::HashMap::new();
    for t in items.iter().filter_map(|i| i.target.as_ref()) {
        kept.entry(t.repo.common_dir.as_path())
            .or_insert_with(|| Kept::of(&t.repo.main, &t.repo.common_dir));
    }
    let jobs: Vec<inspect::Job> = items
        .iter()
        .filter_map(|i| i.target.as_ref())
        .map(|t| {
            let common = t.repo.common_dir.as_path();
            (&t.entry, common, &kept[common])
        })
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
                if let Err(unmet) = upstream_gone(&report) {
                    item.outcome = Some(Outcome::Skipped(format!(
                        "--only-if-upstream-gone: {}",
                        unmet.text()
                    )));
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
    let mut purges = expired(&repos, &mode.keep, mode.as_of, mode.ages_at(), &mut errors);
    // Problems with removal records: warnings, on stderr and in JSON
    // `warnings`, but never `errors` and never the exit status. A record
    // is about a worktree already gone, and a run that removed its PATHs
    // did what it was asked; otherwise one bad record would fail every run
    // in its repository.
    let mut warnings: Vec<String> = Vec::new();
    // What the plan keeps because its filesystem gives no hold is in the
    // plan's "kept" lists and `kept` in the JSON, never also a warning.
    let mut forgets = expired_records(
        &repos,
        &mode.keep_removed,
        mode.as_of,
        mode.ages_at(),
        &mut warnings,
    );
    let judge = |p: &Purge| p.item.registered.is_some() && p.outcome.is_none();
    let judged: Vec<Entry> = purges
        .iter()
        .filter(|p| judge(p))
        .filter_map(|p| p.item.registered.as_ref().map(as_judged))
        .collect();
    // A trash entry was already removed from its path: nothing keeps it like
    // a main checkout, so it is purged as before.
    let purge_jobs: Vec<inspect::Job> = purges
        .iter()
        .filter(|p| judge(p))
        .zip(&judged)
        .map(|(p, e)| (e, p.item.common_dir.as_path(), &Kept::Nothing))
        .collect();
    let mut purge_reports = inspect::all(&purge_jobs, &snapshot, &purge_policy).into_iter();
    for p in &mut purges {
        if judge(p) {
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
    // Those the plan kept already are not asked about or acted on.
    let forgettable: Vec<usize> = (0..forgets.len())
        .filter(|&i| forgets[i].outcome.is_none())
        .collect();
    if !options.json {
        // The header names the one activity snapshot every check of the plan
        // used, as `scan` and `explain` do.
        print!(
            "{}{}",
            style.dim(&format!(
                "{}{} · activity: {}\n",
                commands::header(mode.started),
                ages_text(&mode),
                clean(&snapshot.scope())
            )),
            plan_text(&items, &purges, &forgets, &errors, &mode, &places, &style)
        );
    }

    let mut declined: Option<String> = None;
    let pending = !ready.is_empty() || !purgeable.is_empty() || !forgettable.is_empty();
    if pending && !options.yes {
        declined = match confirm(ready.len(), purgeable.len(), forgettable.len(), mode.trash) {
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
            for &i in &forgettable {
                forgets[i].outcome = Some(Outcome::Skipped(why.clone()));
            }
        }
    }

    // The stats ledger takes in the removal records and trash entries of
    // these repositories it lacks, before this run forgets any. Nothing it
    // does changes what the run removes.
    let acting = declined.is_none() && pending;
    let seeded = acting.then(|| stats::seed_repos(&repos));

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
        if let Outcome::Kept(why) = &outcome {
            warnings.push(format!(
                "kept the trash entry {}: {why}",
                purges[i].item.dir.display()
            ));
        }
        purges[i].outcome = Some(outcome);
    }
    for &i in &forgettable {
        let f = &mut forgets[i];
        if f.outcome.is_some() {
            continue;
        }
        let outcome = match removed::forget_expired(&f.item) {
            Ok(Forgot::Yes) => Outcome::Removed(None),
            Ok(Forgot::Busy) => Outcome::Skipped(
                "another residue run, a restore, holds it now, so it stays until the next run"
                    .into(),
            ),
            // As the plan's, should its filesystem have changed since.
            Ok(Forgot::Kept(why)) => {
                warnings.push(format!(
                    "kept the removal record {}: {why}",
                    f.item.dir.display()
                ));
                Outcome::Kept(why)
            }
            Err(e) => {
                warnings.push(format!(
                    "could not forget the removal record {}: {e}",
                    f.item.dir.display()
                ));
                Outcome::Failed(e)
            }
        };
        if !options.json {
            println!("{}", forget_line(&f.item, &outcome, &places));
        }
        f.outcome = Some(outcome);
    }

    // A sweep never asked for the worktrees it kept, so only a candidate
    // that was not removed, or a PATH it could not read, makes it fail.
    // The same holds for the trash: an entry the plan kept does not fail
    // the run, an entry it meant to purge and did not does. A removal
    // record it could not read or forget is a warning, never a failure.
    let done = items.iter().all(|i| match &i.outcome {
        Some(o) if o.done() => true,
        Some(Outcome::Refused) => options.all,
        _ => false,
    }) && purges.iter().all(|p| {
        matches!(
            p.outcome,
            Some(Outcome::Removed(_) | Outcome::Refused | Outcome::Kept(_))
        )
    }) && errors.is_empty();
    // After every deletion: a ledger it cannot write is a warning, like a
    // removal record it could not read, and the exit status stays about
    // the PATHs and the trash.
    let run = stats::Run::record(seeded, ledger_lines(&items, &purges, &mode));
    if let Some(e) = &run.error {
        warnings.push(e.clone());
    }
    for warning in &warnings {
        eprintln!("residue: warning: {}", clean(warning));
    }
    if options.json {
        let problems = Problems {
            errors: &errors,
            warnings: &warnings,
        };
        let mut doc = json(
            &items, &purges, &forgets, &problems, &snapshot, &policy, &mode,
        );
        // Added last, so every field before it keeps its place.
        if let Json::Obj(fields) = &mut doc {
            fields.push(("stats", run.json()));
        }
        println!("{}", doc.render());
    } else if declined.as_deref() == Some(NO_TERMINAL) {
        println!("{NO_TERMINAL_TEXT}");
    } else if let Some(why) = declined {
        println!("nothing removed: {}", clean(&why));
    } else if !pending && options.all {
        println!("nothing to remove");
    } else if let Some(line) = run.text(&mode.keep.text) {
        println!("{line}");
    }
    if done { 0 } else { 3 }
}

/// The stats ledger's lines for what this run freed or moved: a deletion
/// that succeeded, with the record it wrote, a move into the trash, and a
/// purge. The plan's reclaimable size, as the removal record keeps it.
fn ledger_lines(items: &[Item], purges: &[Purge], mode: &Mode) -> Vec<stats::Line> {
    let mut lines = Vec::new();
    for item in items {
        let (Some(target), Some(report)) = (&item.target, &item.report) else {
            continue;
        };
        let (event, entry) = match &item.outcome {
            Some(Outcome::Removed(Some(record))) => (stats::Event::Freed, record),
            Some(Outcome::Trashed(entry)) => (stats::Event::Trashed, entry),
            _ => continue,
        };
        lines.push(stats::made(
            event,
            mode.by,
            mode.reason,
            report.usage.as_ref().map(|u| u.reclaimable),
            &target.repo.common_dir,
            &target.canonical,
            entry,
        ));
    }
    for p in purges {
        // An empty record purged (`Removed(None)`) held no worktree.
        let (Some(Outcome::Removed(Some(record))), Ok(trashed)) = (&p.outcome, &p.item.record)
        else {
            continue;
        };
        lines.push(stats::made(
            stats::Event::Freed,
            mode.by,
            Reason::TrashPurge,
            p.report
                .as_ref()
                .and_then(|r| r.usage.as_ref())
                .map(|u| u.reclaimable),
            &p.item.common_dir,
            &trashed.original,
            record,
        ));
    }
    lines
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
                interrupted: t.repo.interrupted.clone(),
            });
        }
    }
    repos
}

/// The removal records of these repositories at least `keep` old and at
/// least as old as the `--keep-removed` each was written with
/// (`removed::Item::expired`), and the
/// entries with no `info` old enough to clear. A record of an interrupted
/// removal is never forgotten: its worktree is still there, and `residue
/// restore` is how it goes back. An unreadable record is never forgotten
/// either, since residue cannot tell which refs are its own; it goes into
/// `warnings`. One whose filesystem gives no hold is kept from the plan on
/// (`removed::unforgettable`). Under `--as-of`, ages are measured at `now`,
/// that time, and a record is forgotten only when its directory and Git's
/// list of the repository's worktrees last changed before it: a removal
/// still running then, whose record read as interrupted, waits for a later
/// run.
fn expired_records(
    repos: &[Repo],
    keep: &Span,
    as_of: Option<i64>,
    now: i64,
    warnings: &mut Vec<String>,
) -> Vec<Forget> {
    let mut out = Vec::new();
    let forget = |item: removed::Item| Forget {
        outcome: removed::unforgettable(&item).map(Outcome::Kept),
        item,
    };
    for repo in repos {
        let listed = as_of.is_none() || before(as_of, worktrees_changed(&repo.common_dir));
        match removed::of_repo(repo) {
            Ok(list) => {
                for item in list {
                    let settled = listed && before(as_of, item.modified);
                    match &item.record {
                        Err(_) if item.clearable(now) && settled => out.push(forget(item)),
                        Err(_) if item.incomplete => {}
                        Err(e) => warnings.push(format!("removal record: {e}")),
                        Ok(_) if item.interrupted() => {}
                        Ok(_) if item.expired(now, keep.seconds) && settled => {
                            out.push(forget(item));
                        }
                        Ok(_) => {}
                    }
                }
            }
            Err(e) => warnings.push(format!("removal records: {e}")),
        }
    }
    out
}

/// The trash entries of these repositories that are at least `keep` old,
/// and entries that hold only their record an hour after they were made
/// (`trash::Item::clearable`), which any purge clears. A younger one may
/// be another run's, between writing `info` and moving the worktree in,
/// so it is left alone, as is one with no `info` yet. Under `--as-of`,
/// ages are measured at `now`, that time, and an entry is purged only when
/// its directory last changed before it: one whose `info` or worktree
/// arrived later waits for a later run.
fn expired(
    repos: &[Repo],
    keep: &Span,
    as_of: Option<i64>,
    now: i64,
    errors: &mut Vec<String>,
) -> Vec<Purge> {
    let mut out = Vec::new();
    for repo in repos {
        match trash::list(&repo.common_dir, &repo.command_dir(), &repo.trash) {
            Ok(list) => {
                for item in list {
                    let settled = before(as_of, item.modified);
                    match &item.record {
                        _ if item.clearable(now) && settled => out.push(Purge {
                            item,
                            report: None,
                            outcome: None,
                        }),
                        Err(_) if item.incomplete && item.bare => {}
                        Err(e) => errors.push(format!("trash entry {}: {e}", item.dir.display())),
                        Ok(_) if item.is_empty_record() => {}
                        Ok(_) if item.age(now).is_some_and(|a| a >= keep.seconds) && settled => {
                            let outcome = if item.registered.is_none() {
                                Some(Outcome::Failed(
                                    "Git no longer lists it as a worktree, so residue cannot remove it with git worktree remove".into(),
                                ))
                            } else {
                                // Kept from the plan on, as `purge` keeps
                                // it, so the prompt does not count it.
                                match hold::take(&trash::info(&item.dir)) {
                                    Ok(Taken::Unsupported(why)) => {
                                        Some(Outcome::Kept(unpurgeable(&why, &item.dir)))
                                    }
                                    _ => None,
                                }
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

/// Without `--as-of`, true. With it, whether `changed`, a modification time
/// in whole seconds, is before TIME, so the change was there when a plan
/// made at TIME read it. An unknown time is not.
fn before(as_of: Option<i64>, changed: Option<i64>) -> bool {
    as_of.is_none_or(|t| changed.is_some_and(|m| m < t))
}

/// When Git's list of a repository's worktrees last changed: the time of
/// `worktrees/` in its common directory, or of the common directory once
/// Git removed `worktrees/` with the last linked worktree.
fn worktrees_changed(common_dir: &Path) -> Option<i64> {
    match fs::metadata(common_dir.join("worktrees")) {
        Ok(m) => Some(m.mtime()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::metadata(common_dir).ok().map(|m| m.mtime())
        }
        Err(_) => None,
    }
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

/// Why a run without `--yes` and without a terminal removes nothing: the
/// JSON `reason`, unchanged. Text prints [`NO_TERMINAL_TEXT`] instead.
const NO_TERMINAL: &str = "stdin is not a terminal, so residue cannot ask for confirmation; pass --yes only from the session that owns these worktrees and knows its task is finished, or from a scheduled `residue remove --all --idle`";

/// The same, as the two lines a text run ends with: what happened, then
/// what to do.
const NO_TERMINAL_TEXT: &str = "nothing removed: stdin is not a terminal, so residue cannot ask\nnext: run it in a terminal, or pass --yes from the session that owns them, once its task is done";

fn confirm(worktrees: usize, purges: usize, forgets: usize, trash: bool) -> Result<bool, String> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err(NO_TERMINAL.into());
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
    if forgets > 0 {
        let what = count(forgets, "removal record", "removal records");
        parts.push(if parts.is_empty() {
            format!("Forget {what}")
        } else {
            format!("forget {what}")
        });
    }
    // A forgotten record's ref no longer keeps its commits; branches do.
    let keeps = if forgets > 0 {
        "Branches stay; a forgotten record cannot be restored."
    } else {
        "Branches and commits stay."
    };
    let prompt = format!("{}? {keeps} Type yes to continue: ", parts.join(" and "));
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

/// Why `--only-if-upstream-gone` keeps a worktree: the local refs say its
/// upstream is not gone, or residue could not read them.
enum Unmet {
    NotGone(String),
    Unreadable(String),
}

impl Unmet {
    fn text(&self) -> &str {
        match self {
            Unmet::NotGone(why) | Unmet::Unreadable(why) => why,
        }
    }
}

/// `--only-if-upstream-gone`: `Ok` only when the local refs show that the
/// branch's upstream is a remote-tracking ref and that it is gone, and
/// `residue restore` did not put the worktree back. A detached HEAD, a
/// branch with no upstream, a local upstream, an upstream that exists, a
/// restored worktree, and a lookup that failed all keep the worktree.
/// `remove` never fetches, so this is the remote as of the last fetch.
fn judge_upstream(
    branch: Option<&str>,
    upstream: &Result<Option<inspect::Upstream>, String>,
    restored: &Result<Option<removed::Mark>, String>,
) -> Result<(), Unmet> {
    // A worktree someone restored was wanted back after its work landed;
    // that rule never removes it again. An explicit remove still does.
    match restored {
        Ok(None) => {}
        Ok(Some(mark)) => {
            return Err(Unmet::NotGone(format!(
                "`residue restore` put it back{}, so its landed work never removes it again; `residue remove PATH` still does",
                mark.at
                    .map(|t| format!(" on {}", output::date(t)))
                    .unwrap_or_default()
            )));
        }
        Err(e) => {
            return Err(Unmet::Unreadable(format!(
                "residue could not read whether `residue restore` put it back: {}",
                clean(e)
            )));
        }
    }
    let Some(branch) = branch else {
        return Err(Unmet::NotGone(
            "it is on a detached HEAD, which has no upstream".into(),
        ));
    };
    match upstream {
        Ok(Some(u)) if u.gone && u.remote() => Ok(()),
        Ok(Some(u)) if u.gone => Err(Unmet::NotGone(format!(
            "its upstream {} is a local branch, not a remote-tracking ref, so its deletion is not work that landed",
            clean(&u.name)
        ))),
        Ok(Some(u)) => Err(Unmet::NotGone(format!(
            "its upstream {} exists in the local refs",
            clean(&u.name)
        ))),
        Ok(None) => Err(Unmet::NotGone(format!(
            "branch {} has no upstream",
            clean(branch)
        ))),
        Err(e) => Err(Unmet::Unreadable(format!(
            "residue could not read the upstream of branch {}: {}",
            clean(branch),
            clean(e)
        ))),
    }
}

/// `judge_upstream` on what a report recorded.
fn upstream_gone(report: &Report) -> Result<(), Unmet> {
    judge_upstream(
        report.entry.short_branch(),
        &report.upstream,
        &report.restored,
    )
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
    // The fresh list says what keeps a worktree like a main checkout now.
    let kept = fresh
        .first()
        .map_or(Kept::Nothing, |main| Kept::of(main, common_dir));
    let now = inspect::worktree(&judged, common_dir, &kept, &snapshot, policy);
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
        upstream_gone(&now).map_err(|unmet| match unmet {
            Unmet::NotGone(why) => Outcome::Skipped(format!("changed since {since}: {why}")),
            Unmet::Unreadable(why) => Outcome::Skipped(format!("--only-if-upstream-gone: {why}")),
        })?;
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
    // `recheck` skips it too without one.
    let planned = match &before.fingerprint {
        Ok(planned) => planned,
        Err(e) => {
            return Outcome::Skipped(format!(
                "residue could not record what the plan showed: {e}"
            ));
        }
    };
    let listed = match recheck(
        original,
        &target.repo.common_dir,
        before,
        policy,
        since,
        /* ours */ None,
        mode.only_if_upstream_gone,
    ) {
        Ok(l) => l,
        Err(outcome) => return outcome,
    };
    let main = target.repo.command_dir();
    // Where it goes: a hidden sibling, or a new trash entry, held until
    // this returns: through the move in, the lock and the check there.
    let (dest, entry, reason, _entry_hold) = if mode.trash {
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
        let (entry, hold) = match trash::create(&target.repo.common_dir, &record) {
            Ok(e) => e,
            Err(e) => return Outcome::Failed(format!("cannot make a trash entry: {e}")),
        };
        let Some(name) = original.file_name() else {
            return Outcome::Failed("the worktree path has no name".into());
        };
        (entry.join(name), Some(entry), trash::LOCK, Some(hold))
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
        (dest, None, REMOVING, None)
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
    let check = || {
        verify_moved(
            &dest,
            before,
            policy,
            since,
            reason,
            mode.only_if_upstream_gone,
        )
    };
    if let Some(entry) = &entry {
        return match check() {
            Ok(()) => Outcome::Trashed(entry.clone()),
            Err(why) => moved.put_back(true, &why),
        };
    }
    // The record and its refs come before the last check, so writing and
    // syncing them does not widen the time between that check and the
    // deletion; if the check fails, they are undone. Without them residue
    // deletes nothing.
    let record = removal_record(
        original,
        &dest,
        &listed,
        before,
        mode.reason,
        mode.by,
        &mode.keep_removed,
    );
    let git_dir = before
        .fingerprint
        .as_ref()
        .ok()
        .map(|f| f.git_dir.as_path());
    let written = match removed::create(&target.repo.common_dir, &main, &record, git_dir) {
        Ok(w) => w,
        Err(e) => {
            return moved.put_back(
                true,
                &format!("could not write its removal record, so residue deleted nothing: {e}"),
            );
        }
    };
    // Git deleted nothing yet, so only the checkout and the Git directory
    // count: a check that failed on an edit of the user's puts it back.
    let both = [original, dest.as_path()];
    if let Err(why) = check() {
        let outcome = moved.put_back(true, &why);
        return settle(
            &written,
            standing(&written, &main, &both, None),
            None,
            outcome,
        );
    }
    if let Err(e) = git::unlock_worktree(&main, &dest) {
        let outcome = moved.put_back(true, &format!("could not unlock it: {e}"));
        return settle(
            &written,
            standing(&written, &main, &both, None),
            None,
            outcome,
        );
    }
    // Git inherits the record's hold, so a restore waits for it even if
    // this process is killed alone.
    let Err(e) = git::remove_worktree(&main, &dest, &[written.hold()]) else {
        return Outcome::Removed(Some(written.dir));
    };
    // A failure can come after Git deleted part of it: its files, its
    // HEAD, its reflog. Then it stays where it is, since Git may not move
    // it, and the record is how it comes back: `residue restore` moves it
    // back and puts back what its Git directory lost. Its files count too:
    // Git may have been stopped partway through them. This one reading
    // decides: the settle of a worktree left lacking uses it, so a later
    // reading cannot drop the record of one that stays aside.
    let files = Files {
        planned,
        policy,
        since,
    };
    let at_dest = standing(&written, &main, &[&dest], Some(&files));
    match at_dest {
        Standing::Lacking(..) => return settle(&written, at_dest, None, Outcome::Failed(e)),
        Standing::Unlisted => return unlisted(&written, &dest, &e),
        Standing::Whole(_) | Standing::Unknown(_) => {}
    }
    let measured_whole = matches!(at_dest, Standing::Whole(_));
    let outcome = match moved.put_back(false, &e) {
        Outcome::Skipped(why) => Outcome::Failed(why),
        other => other,
    };
    // The move back changed where it is, so where it stands is read again;
    // measured whole just now, its files need no second measure, since the
    // move changes none.
    let measure = (!measured_whole).then_some(&files);
    settle(
        &written,
        standing(&written, &main, &both, measure),
        None,
        outcome,
    )
}

/// What the removal record keeps of a worktree about to be deleted at
/// `aside`: `listed` is Git's entry for it, `before` the plan's report,
/// `by` the command this run is, `keep` its `--keep-removed`, which the
/// record keeps so that no later run forgets it sooner.
fn removal_record(
    original: &Path,
    aside: &Path,
    listed: &Entry,
    before: &Report,
    reason: Reason,
    by: By,
    keep: &Span,
) -> removed::Record {
    removed::Record {
        original: original.to_path_buf(),
        aside: aside.to_path_buf(),
        branch: listed.branch.clone(),
        head: listed.head.clone().unwrap_or_default(),
        upstream: before
            .upstream
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|u| u.full.clone()),
        reason: reason.code().to_string(),
        by: Some(by.code().to_string()),
        removed: output::now(),
        keep: Some(removed::Keep {
            seconds: keep.seconds,
            text: keep.text.clone(),
        }),
        allocated: before.usage.as_ref().map(|u| u.allocated),
        reclaimable: before.usage.as_ref().map(|u| u.reclaimable),
        gone: before
            .ignored
            .iter()
            .map(|i| removed::Gone {
                path: i.path.clone(),
                files: i.files,
                bytes: i.bytes,
            })
            .collect(),
        pid: Some(std::process::id()),
        version: Some(VERSION.to_string()),
        hold: true,
        // `removed::create` finds them, right before it writes the record.
        kept: Vec::new(),
    }
}

/// A worktree's checkout is there: its directory, with the `.git` file
/// that ties it to its Git directory.
fn checkout_present(path: &Path) -> bool {
    fs::symlink_metadata(path.join(".git")).is_ok_and(|m| m.is_file())
}

/// What is left of a worktree whose deletion did not complete, as far as
/// its removal record is concerned.
enum Standing<'a> {
    /// Git lists it at this path, with its checkout and everything the
    /// record keeps: nothing of it is gone.
    Whole(&'a Path),
    /// Git lists it at this path, but this part of it is gone.
    Lacking(&'a Path, String),
    /// Git no longer lists it at any of the paths.
    Unlisted,
    /// residue could not list the worktrees.
    Unknown(String),
}

/// The plan's measure of a worktree's files, to compare once `git
/// worktree remove` has run: Git deletes the files before its own
/// directory, so a Git stopped partway (residue's `REMOVE_TIMEOUT`, or a
/// kill of Git alone) leaves part of them gone and the rest intact.
struct Files<'a> {
    planned: &'a Fingerprint,
    policy: &'a Policy,
    since: &'a str,
}

/// Where Git lists the worktree among `paths`, and whether all the record
/// keeps is still there: its checkout (the `.git` file that ties it to its
/// Git directory), a Git directory Git can use, and in it what
/// `Written::lacks` checks. With `files`, once Git ran, every file the
/// plan measured counts too: any difference from the plan, or a measure
/// that fails, is `Lacking`.
/// Git deletes the checkout, then its own directory for the worktree, each
/// in the order the filesystem lists the files and stopping at the first
/// it cannot delete, so it can list a worktree part of which is gone.
/// Another writer can add a worktree at one of the paths meanwhile (a
/// `git worktree add` at the path a purge's trash entry came from): a
/// listed path counts as this worktree only when its Git directory is the
/// one the record was written for (`Written::is_its_git_dir`), so another
/// worktree's HEAD never stands in for this one's. A listed path whose
/// Git directory residue cannot read may be this worktree, part of which
/// is gone: it is the answer unless another path is this worktree.
fn standing<'a>(
    written: &removed::Written,
    main: &Path,
    paths: &[&'a Path],
    files: Option<&Files>,
) -> Standing<'a> {
    let list = match git::worktrees(main) {
        Ok(list) => list,
        Err(e) => return Standing::Unknown(e),
    };
    let mut unsure = None;
    for &at in paths {
        let Some(entry) = list
            .iter()
            .find(|e| e.path == at || git::canonical(&e.path).ok().as_deref() == Some(at))
        else {
            continue;
        };
        if !checkout_present(at) {
            unsure.get_or_insert(Standing::Lacking(at, "its files are gone".to_string()));
            continue;
        }
        // Git lists a worktree whose `commondir` is gone with its HEAD, but
        // cannot use it.
        let id = match git::identity(at) {
            Ok(id) => id,
            Err(e) => {
                unsure.get_or_insert(Standing::Lacking(
                    at,
                    format!("Git can no longer use it ({e})"),
                ));
                continue;
            }
        };
        if !written.is_its_git_dir(&id.git_dir) {
            continue;
        }
        if let Some(why) = written.lacks(entry.head.as_deref()) {
            return Standing::Lacking(at, why);
        }
        let Some(files) = files else {
            return Standing::Whole(at);
        };
        return match files
            .planned
            .again(at, entry.head.clone(), files.policy, &[])
            .0
        {
            Ok(now) => match files.planned.changed(&now, files.since) {
                None => Standing::Whole(at),
                Some(why) => Standing::Lacking(at, format!("part of its files may be gone: {why}")),
            },
            Err(e) => Standing::Lacking(
                at,
                format!("residue could not measure its files to tell whether any are gone ({e})"),
            ),
        };
    }
    unsure.unwrap_or(Standing::Unlisted)
}

/// After a deletion that did not complete, from one reading of where the
/// worktree stands (`standing`, with `files` once Git ran): when nothing
/// of it is gone and it is at a path that is its own (its original path,
/// or a purge's trash entry), its removal record is dropped. Otherwise the
/// record stays, and the outcome says so: it is how the worktree comes
/// back. One left whole under the hidden name keeps its record too, so
/// that `residue restore` and `residue scan` still find it. `entry` is the
/// trash entry a purge's worktree is still in, whose restore brings it
/// back while Git lists it there. The run holds the record, and a purge
/// the entry, so no restore moved the worktree meanwhile.
fn settle(
    written: &removed::Written,
    standing: Standing,
    entry: Option<&Path>,
    outcome: Outcome,
) -> Outcome {
    let restore = output::shell(&written.dir);
    let note = match standing {
        Standing::Whole(at) if original_of(at).is_some() => format!(
            "it is whole at {}, and its removal record stays: residue restore {restore} moves it back",
            at.display()
        ),
        Standing::Whole(_) => match written.undo() {
            Ok(()) => return outcome,
            Err(e) => format!("its removal record stays ({e})"),
        },
        Standing::Lacking(at, why) => format!(
            "Git still lists it at {}, but {why}; its removal record keeps its HEAD and reflog: residue restore {}",
            at.display(),
            entry.map_or(restore, output::shell)
        ),
        Standing::Unlisted => format!(
            "Git no longer lists it, so part of it may be gone; its removal record keeps its HEAD and reflog: residue restore {restore}"
        ),
        Standing::Unknown(e) => format!(
            "residue could not list the worktrees to tell whether any of it is gone ({e}); its removal record keeps its HEAD and reflog: residue restore {restore}"
        ),
    };
    match outcome {
        Outcome::Skipped(why) => Outcome::Skipped(with_note(why, &note)),
        Outcome::Failed(why) => Outcome::Failed(with_note(why, &note)),
        other => other,
    }
}

/// After a `git worktree remove` that failed once Git had deleted its own
/// directory for the worktree, which it does even when it could not delete
/// every file: Git no longer lists it, so no `git worktree move` brings it
/// back, and what Git did not delete stays under the hidden name. The
/// removal record stays, and the reason ends with its restore, as for a
/// worktree Git deleted part of (`settle`): that restore recreates the
/// worktree at its original path, and says what is left at the hidden one.
fn unlisted(written: &removed::Written, dest: &Path, e: &str) -> Outcome {
    let left = match fs::symlink_metadata(dest) {
        Ok(_) => format!("what Git did not delete is still at {}", dest.display()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            format!("nothing of it is left at {}", dest.display())
        }
        Err(err) => format!(
            "residue cannot tell what is left at {} ({err})",
            dest.display()
        ),
    };
    Outcome::Failed(format!(
        "{e}; Git no longer lists it, so part of it is gone, and {left}; its removal record keeps its HEAD and reflog: residue restore {}",
        output::shell(&written.dir)
    ))
}

/// Where the line that recovers a worktree residue could not move back
/// starts in an outcome (`Moved::stuck`).
const RECOVER_WITH: &str = ". Recover with: ";

/// `why` with `note` added. The line that recovers the worktree stays last,
/// so a copy of it to the end runs nothing else.
fn with_note(why: String, note: &str) -> String {
    match why.rsplit_once(RECOVER_WITH) {
        Some((head, line)) => format!("{head}; {note}{RECOVER_WITH}{line}"),
        None => format!("{why}; {note}"),
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
            "{why}; residue could not move it back to {} ({cause}). It is at {}{RECOVER_WITH}{}",
            self.original.display(),
            self.dest.display(),
            recovery(self.main, self.dest, self.original, locked || relocked)
        ))
    }
}

/// After the move: Git lists the worktree at its new path, locked by
/// residue, with the HEAD and branch of the plan; no process the probe can
/// see is inside it; its fingerprint still matches the plan; the walk
/// finds no nested repository, mount point or unreadable entry there; and
/// it has no operation in progress and no private ref, as the plan's
/// `operation` and `private_refs` checks found.
fn verify_moved(
    dest: &Path,
    before: &Report,
    policy: &Policy,
    since: &str,
    reason: &str,
    only_if_upstream_gone: bool,
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
    let planned = before
        .fingerprint
        .as_ref()
        .map_err(|e| format!("residue could not record what the plan showed: {e}"))?;
    // The snapshot is taken before the walk, as the plan's was, and read
    // after it: the walk at the new path says how long the worktree's
    // paths are now and which files the probe could not name lie inside
    // it. A use the probe saw still comes first among the reasons.
    let snapshot = Snapshot::take();
    let (now, reach) = planned.again(dest, listed.head.clone(), policy, &snapshot.wanted());
    match snapshot.evidence(dest, reach.as_ref()) {
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
    let now = now?;
    if let Some(why) = planned.changed(&now, since) {
        return Err(why);
    }
    // After the measuring, which can take seconds: a bisect, or a private
    // ref, started during the move or since is deleted with the worktree,
    // and `git gc` then drops a commit only that ref kept.
    let found = inspect::operations(&planned.git_dir)?;
    if !found.is_empty() {
        return Err(format!(
            "{} in progress after residue moved it aside",
            found.join(", ")
        ));
    }
    let refs = inspect::private_refs(dest)
        .map_err(|e| format!("cannot list its private refs after moving it aside: {e}"))?;
    if !refs.is_empty() {
        return Err(format!(
            "it has worktree-private refs after residue moved it aside, which its deletion would delete: {}",
            inspect::examples(&refs, 5)
        ));
    }
    // Measuring again can take seconds on a large worktree, so read the
    // upstream once more, last, right before the deletion.
    if only_if_upstream_gone {
        let live = match &listed.branch {
            Some(branch) => inspect::upstream(dest, branch),
            None => Ok(None),
        };
        let mark = removed::marked(&planned.git_dir);
        judge_upstream(listed.short_branch(), &live, &mark).map_err(|unmet| match unmet {
            Unmet::NotGone(why) => format!("changed after residue moved it aside: {why}"),
            Unmet::Unreadable(why) => format!("--only-if-upstream-gone: {why}"),
        })?;
    }
    Ok(())
}

/// Why a trash entry whose filesystem gives no flock(2) lock stays, on
/// every run, which does not fail for it. Only an older release made it
/// there, and this run cannot keep a restore away while it deletes.
fn unpurgeable(why: &str, dir: &Path) -> String {
    format!(
        "{why}, so residue cannot hold it against a restore while it purges; `residue restore {}` names the steps to move it back by hand",
        output::shell(dir)
    )
}

/// Hold one expired trash entry, re-check it, then delete it with `git
/// worktree remove`. The hold comes first, so no restore moves the
/// worktree between the check and the deletion, nor while Git deletes.
fn purge(p: &Purge, policy: &Policy, mode: &Mode) -> Outcome {
    // One that holds only its record has no worktree to delete or restore,
    // and may have no `info` to hold; the hour since it was made keeps it
    // from being a run's still moving a worktree in (`trash::Item`).
    if p.item.holds_only_record() {
        return match trash::discard(&p.item.dir) {
            Ok(()) => Outcome::Removed(None),
            Err(e) => Outcome::Failed(e),
        };
    }
    let held = match hold::take(&trash::info(&p.item.dir)) {
        Ok(Taken::Held(held)) => held,
        Ok(Taken::Busy) => {
            return Outcome::Skipped(
                "a residue run, or the `git worktree remove` it started, is purging or restoring it now".into(),
            );
        }
        Ok(Taken::Gone) => {
            return Outcome::Skipped(
                "another residue run purged or restored it since the plan".into(),
            );
        }
        // Only an older release made it there, and this run cannot keep a
        // restore away while it deletes: it stays, every run, and the run
        // does not fail for it (a warning says so).
        Ok(Taken::Unsupported(why)) => return Outcome::Kept(unpurgeable(&why, &p.item.dir)),
        Err(e) => return Outcome::Failed(e),
    };
    let (Some(worktree), Some(before), Ok(trashed)) = (&p.item.worktree, &p.report, &p.item.record)
    else {
        return Outcome::Failed("internal: no report".into());
    };
    let planned = match &before.fingerprint {
        Ok(planned) => planned,
        Err(e) => {
            return Outcome::Skipped(format!(
                "residue could not record what the plan showed: {e}"
            ));
        }
    };
    let path = git::canonical(worktree).unwrap_or_else(|_| worktree.clone());
    let listed = match recheck(
        &path,
        &p.item.common_dir,
        before,
        policy,
        mode.since,
        Some(trash::LOCK),
        /* only_if_upstream_gone */ false,
    ) {
        Ok(l) => l,
        Err(outcome) => return outcome,
    };
    // A purge deletes too, so it records first, as a removal does.
    let record = removal_record(
        &trashed.original,
        &path,
        &listed,
        before,
        Reason::TrashPurge,
        mode.by,
        &mode.keep_removed,
    );
    let written = match removed::create(
        &p.item.common_dir,
        &p.item.command_dir,
        &record,
        Some(planned.git_dir.as_path()),
    ) {
        Ok(w) => w,
        Err(e) => {
            return Outcome::Failed(format!(
                "could not write its removal record, so residue deleted nothing: {e}"
            ));
        }
    };
    // The lock lifted is the one the re-check read, under the hold.
    match trash::purge(&p.item, &listed, &[&held, written.hold()]) {
        Ok(()) => Outcome::Removed(Some(written.dir)),
        // Its files count whether or not Git ran: the re-check found them
        // as the plan did a moment ago, so a difference keeps the record.
        Err(e) => {
            let files = Files {
                planned,
                policy,
                since: mode.since,
            };
            let at = standing(&written, &p.item.command_dir, &[&path], Some(&files));
            settle(&written, at, Some(&p.item.dir), Outcome::Failed(e))
        }
    }
}

fn plan_text(
    items: &[Item],
    purges: &[Purge],
    forgets: &[Forget],
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
    if !ready.is_empty() {
        let title = if mode.trash {
            format!("will move to the trash ({}):", ready.len())
        } else {
            format!("will remove ({}):", ready.len())
        };
        out.push_str(&format!("\n{}\n", style.bold(&title)));
        if mode.trash {
            out.push_str(&style.dim(&format!(
                "  put one back: residue restore · purged after {} (--keep-trash), which frees the disk\n",
                mode.keep.text
            )));
        } else {
            out.push_str(&style.dim(&format!(
                "  `residue restore` brings one back for {} (--keep-removed), without its ignored files\n",
                mode.keep_removed.text
            )));
        }
        // Every worktree here passed the activity check against the one
        // snapshot in the header, so its caveat is said once.
        out.push_str(&style.dim(
            "  no process observed using them; processes residue cannot see are not checked\n",
        ));
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
            out.push_str(&purge_summary(&purges[i], mode.ages_at(), places, style));
        }
    }
    let (pending, kept): (Vec<&Forget>, Vec<&Forget>) =
        forgets.iter().partition(|f| f.outcome.is_none());
    if !pending.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            style.bold(&format!(
                "will forget removal records ({}), older than {} (--keep-removed):",
                pending.len(),
                mode.keep_removed.text
            ))
        ));
        out.push_str(
            &style.dim("  after this, `residue restore` cannot bring these back; branches stay\n"),
        );
        for f in pending {
            out.push_str(&forget_summary(&f.item, mode.ages_at(), places, style));
        }
    }
    if !kept.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            style.bold(&format!("kept removal records ({}):", kept.len()))
        ));
        for f in kept {
            let why = match &f.outcome {
                Some(Outcome::Kept(why)) => clean(why),
                _ => String::new(),
            };
            out.push_str(&format!(
                "  {}  {}\n",
                style.bold(&clean(&f.item.name)),
                style.dim(&why)
            ));
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
                (Some(Outcome::Failed(e) | Outcome::Kept(e)), _) => clean(e),
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

/// One trash entry the plan purges, its age at `now`, when the run measures
/// ages (`Mode::ages_at`), as the header says.
fn purge_summary(p: &Purge, now: i64, places: &Places, style: &Style) -> String {
    let Ok(r) = &p.item.record else {
        return format!(
            "  {}\n    {}\n",
            style.bold(&places.show(&p.item.dir)),
            style.dim("holds no worktree, and its record is missing or damaged")
        );
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

/// `; restore: residue restore PATH` for a deletion residue recorded.
fn restore_hint(record: &Option<PathBuf>) -> String {
    record
        .as_ref()
        .map(|r| format!("; restore: residue restore {}", output::shell(r)))
        .unwrap_or_default()
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
        Outcome::Removed(record) => format!(
            "removed {shown}{branch}{}{}",
            reclaimable("reclaimable"),
            restore_hint(record)
        ),
        Outcome::Trashed(entry) => format!(
            "moved {shown} to the trash{branch}; restore: residue restore {}",
            output::shell(entry)
        ),
        Outcome::Skipped(why) => format!("skipped {shown}: {}", clean(why)),
        Outcome::Failed(why) => format!("failed {shown}: {}", clean(why)),
        Outcome::Refused | Outcome::Kept(_) | Outcome::Invalid(_) => format!("not removed {shown}"),
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
        Outcome::Removed(_) if p.item.holds_only_record() => {
            format!("cleared the empty trash record {name}{was}")
        }
        Outcome::Removed(record) => format!(
            "purged {name} from the trash{was}{}{}",
            p.item
                .record
                .as_ref()
                .ok()
                .and_then(|r| r.reclaimable)
                .map(|b| format!("; about {} reclaimable", output::bytes(b)))
                .unwrap_or_default(),
            restore_hint(record)
        ),
        Outcome::Skipped(why) | Outcome::Kept(why) => {
            format!("kept {name} in the trash: {}", clean(why))
        }
        Outcome::Failed(why) => format!("failed to purge {name}: {}", clean(why)),
        _ => format!("kept {name} in the trash"),
    }
}

fn forget_line(item: &removed::Item, outcome: &Outcome, places: &Places) -> String {
    let name = clean(&item.name);
    let was = item
        .record
        .as_ref()
        .map(|r| format!(" (was {})", places.show(&r.original)))
        .unwrap_or_default();
    match outcome {
        Outcome::Removed(_) => format!("forgot the removal record {name}{was}"),
        Outcome::Skipped(why)
        | Outcome::Kept(why)
        | Outcome::Failed(why)
        | Outcome::Invalid(why) => {
            format!("kept the removal record {name}{was}: {}", clean(why))
        }
        _ => format!("kept the removal record {name}{was}"),
    }
}

/// One removal record the plan forgets, its age at `now`, as for a purge.
fn forget_summary(item: &removed::Item, now: i64, places: &Places, style: &Style) -> String {
    let Ok(r) = &item.record else {
        return String::new();
    };
    let branch = r
        .short_branch()
        .map(|b| format!("[{}]", clean(b)))
        .unwrap_or_default();
    format!(
        "  {}  {}\n    was {}\n",
        style.bold(&clean(&item.name)),
        style.dim(&branch),
        style.dim(&format!(
            "{} · removed {}",
            places.show(&r.original),
            output::age(now - r.removed)
        ))
    )
}

fn reason_json(outcome: &Option<Outcome>) -> Json {
    match outcome {
        Some(Outcome::Removed(_) | Outcome::Trashed(_)) => Json::Null,
        Some(Outcome::Kept(r) | Outcome::Skipped(r) | Outcome::Failed(r) | Outcome::Invalid(r)) => {
            Json::str(r.clone())
        }
        Some(Outcome::Refused) => Json::str("one or more checks did not pass"),
        None => Json::str("not attempted"),
    }
}

/// What a remove run lists apart from its results: `errors`, what it
/// could not read, each of which makes it exit 3, and `warnings`, the
/// removal records it could not read or forget, which never change the
/// exit status.
struct Problems<'a> {
    errors: &'a [String],
    warnings: &'a [String],
}

fn json(
    items: &[Item],
    purges: &[Purge],
    forgets: &[Forget],
    problems: &Problems,
    snapshot: &Snapshot,
    policy: &Policy,
    mode: &Mode,
) -> Json {
    let record_json = |outcome: &Option<Outcome>| match outcome {
        Some(Outcome::Removed(Some(record))) => Json::path(record),
        _ => Json::Null,
    };
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
                ("removal_record", record_json(&item.outcome)),
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
                Some(Outcome::Removed(_)) => "purged",
                Some(Outcome::Refused) => "kept",
                Some(o) => o.code(),
                None => "not_attempted",
            };
            Json::Obj(vec![
                ("result", Json::str(result)),
                ("reason", reason_json(&p.outcome)),
                ("entry", trash::item_json(&p.item)),
                ("removal_record", record_json(&p.outcome)),
                (
                    "worktree",
                    Json::opt(p.report.as_ref(), output::report_json),
                ),
            ])
        })
        .collect();
    let forgotten = forgets
        .iter()
        .map(|f| {
            let result = match &f.outcome {
                Some(Outcome::Removed(_)) => "forgotten",
                Some(o) => o.code(),
                None => "not_attempted",
            };
            Json::Obj(vec![
                ("result", Json::str(result)),
                ("reason", reason_json(&f.outcome)),
                ("entry", removed::item_json(&f.item)),
            ])
        })
        .collect();
    Json::Obj(vec![
        ("schema", Json::str(output::SCHEMA_REMOVE)),
        ("version", Json::str(VERSION)),
        ("generated_at", Json::str(output::rfc3339(mode.started))),
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
        (
            "removal_records",
            Json::Obj(vec![
                ("reason", Json::str(mode.reason.code())),
                ("by", Json::str(mode.by.code())),
                ("keep", Json::str(mode.keep_removed.text.clone())),
                ("keep_seconds", Json::Int(mode.keep_removed.seconds)),
            ]),
        ),
        ("results", Json::Arr(results)),
        ("purged", Json::Arr(purged)),
        ("forgotten", Json::Arr(forgotten)),
        (
            "errors",
            Json::Arr(
                problems
                    .errors
                    .iter()
                    .map(|e| Json::str(e.clone()))
                    .collect(),
            ),
        ),
        (
            "warnings",
            Json::Arr(
                problems
                    .warnings
                    .iter()
                    .map(|w| Json::str(w.clone()))
                    .collect(),
            ),
        ),
        // Added after the others, so every field before it keeps its place.
        ("as_of", Json::str(output::rfc3339(mode.ages_at()))),
    ])
}

/// ` · ages as of 14:01:07 +02:00` in the header when `--as-of` set a time
/// before the run's start; empty otherwise.
fn ages_text(mode: &Mode) -> String {
    match mode.as_of {
        Some(t) if t < mode.started - 86_400 => format!(" · ages as of {}", output::date(t)),
        Some(t) if t < mode.started => format!(" · ages as of {}", output::clock(t)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn a_worktree_is_deleted_at_its_hidden_name_or_its_trash_entry_whichever_is_longer() {
        let common = Path::new("/r/.git");
        let at = |original: &str| super::deleted_at(Path::new(original), common);
        // Deep below a short trash: its hidden name is the longer.
        assert_eq!(
            at("/a/b/c/d/e/f/g/h/w"),
            "/a/b/c/d/e/f/g/h/.w.residue-removing-4294967295".len()
        );
        // A long name: its trash entry, which holds the name twice.
        let long = "n".repeat(40);
        assert_eq!(
            at(&format!("/r/.worktrees/{long}")),
            format!("/r/.git/residue-trash/{long}-YYYYMMDDTHHMMSSZ-99/{long}").len()
        );
        // Already in the trash: the purge deletes it where it is.
        let entry = "/r/.git/residue-trash/w-20261109T154958Z/w";
        assert_eq!(at(entry), entry.len());
    }
}
