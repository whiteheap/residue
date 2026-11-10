//! Removal records: what residue writes right before it deletes a worktree,
//! so `residue restore` can bring it back.
//!
//! Every deletion (`remove` with or without the trash's purge, `remove
//! --no-trash`, a sweep, and `watch`'s child) first writes a record,
//! `GIT_COMMON_DIR/residue-removed/NAME-STAMP/info`, next to the trash, and
//! a ref, `refs/residue/removed/NAME-STAMP`, at the worktree's HEAD. The
//! record holds where the worktree was, its branch, HEAD and upstream, why
//! it was removed and by which command (`by`), and the ignored entries the
//! deletion took. The ref keeps the commits reachable after the branch is
//! deleted (as after a merge), so `git gc` cannot drop them.
//!
//! The worktree's own Git directory holds commits too: its HEAD reflog
//! (`logs/HEAD`) names every commit it was at, and Git keeps those while
//! the worktree exists. A commit made on a detached HEAD and then left, or
//! the commits before a rebase or an amend, can have no other holder. So
//! the record also keeps a copy of `logs/HEAD`, and one ref,
//! `refs/residue/reflog/NAME-STAMP/N`, at each of the fewest commits that
//! reach every commit that reflog names and no ref outside `refs/residue/`
//! and no worktree's HEAD reaches. Another reflog naming it is no holder:
//! `git branch -D` deletes a branch's reflog with the branch, and reflogs
//! expire. Worktree-private refs and an operation in progress already keep
//! a worktree from removal (`inspect`), so the reflog is all the Git
//! directory adds. In a repository that keeps its refs in reftable, Git
//! reads no `logs/HEAD` file, even one that is there, so residue asks Git
//! which commits that reflog names (`git_head_reflog` says which, and
//! which it cannot), and there is no copy.
//!
//! A removal writes the record and its refs after it moves the worktree
//! aside and before its check at the new path, which drops them again
//! when it fails; the purge writes them after its re-check. Both write
//! them before `git worktree remove`, and if any of it cannot be written,
//! nothing is deleted. `info` is written under another name, synced and
//! renamed into place, the directories are synced, and the refs are made
//! in one transaction with `core.fsync=reference`, so neither is ever half
//! there after a crash. The run holds `info` from before it is in place
//! until the run ends, and its `git worktree remove` holds it while it
//! deletes (`hold`): a restore takes that hold first, so it never moves a
//! worktree back while a removal or its Git may still delete it.
//!
//! `residue restore NAME` recreates the worktree at its path with `git
//! worktree add`, never `--force`, and never moves a branch that exists:
//! the branch at the recorded HEAD or ahead of it is checked out, a branch
//! that is gone is recreated there, and a branch that no longer contains
//! the recorded HEAD keeps its commits while a new branch is made at the
//! recorded HEAD. The recorded reflog goes back in front of the new one
//! while the repository keeps its refs in files. In one that keeps them in
//! reftable when restore runs, Git reads no `logs/HEAD` file, so restore
//! writes none and says so, and a record holding a copy (written before a
//! `git refs migrate`) stays. Ignored entries, such as `node_modules/`,
//! are not restored; the project's own install makes them again. The
//! record and its refs are dropped only once the restored worktree holds
//! everything they held, as Git reads it.
//!
//! A record and its refs expire after `--keep-removed` (default 30 days),
//! which the record keeps (`keep`): every remove run forgets them too,
//! after the same confirmation, once they are older than both its own
//! `--keep-removed` and the record's (`Item::expired`). residue
//! deletes a ref only under `refs/residue/`, only one a record names, and
//! only while it still points where the record says. Once they are gone, a
//! commit only they held (a branch deleted after a squash merge, work only
//! the reflog held) is left to `git gc`: `--keep-removed` is its grace
//! period. A record residue cannot read or forget is a warning, never a
//! failed run.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::activity::{Evidence, Snapshot};
use crate::fingerprint;
use crate::git::{self, Checkout, Entry, RefChange};
use crate::hold::{self, Hold, Taken};
use crate::json::Json;
use crate::output;

pub const DIR: &str = "residue-removed";
/// Where the refs that keep removed worktrees' HEADs live.
pub const REF_PREFIX: &str = "refs/residue/removed/";
/// Where the refs that keep the commits only a removed worktree's reflog
/// held live: `refs/residue/reflog/ENTRY/N`.
pub const REFLOG_PREFIX: &str = "refs/residue/reflog/";
/// The file residue writes into a restored worktree's own Git directory:
/// `watch`'s landed-work rule never removes that worktree again.
pub const MARKER: &str = "residue-restored";
const INFO: &str = "info";
/// `info` is written here first and then renamed, so `info` is whole or
/// absent.
const INFO_NEW: &str = "info.new";
/// A copy of the worktree's own `config.worktree`, when it had one.
const CONFIG: &str = "config.worktree";
/// A copy of the worktree's HEAD reflog, `logs/HEAD` in its Git directory,
/// when it had one and the repository kept its refs in files, the only
/// backend whose reflog Git reads from that file.
const REFLOG: &str = "HEAD.reflog";
/// The value of a record's `hold` line: its writer holds `info` with
/// flock(2) (`hold`).
const HOLD: &str = "flock";
/// What a restore that recreates a worktree says in a repository that
/// keeps its refs in reftable, where Git reads no `logs/HEAD` file to put
/// the old entries in.
const REFTABLE_REFLOG_NOTE: &str = "its HEAD reflog was not put back: in a reftable repository Git adds a reflog entry only as the newest one, so the old entries cannot go back in front of the new one, and `git reflog` there starts at this restore";
/// An entry with no `info` (the run that made it stopped first) and no ref
/// is cleared once its directory is this old, in seconds: a run writing
/// one takes milliseconds, so an hour cannot catch one at work.
pub const INCOMPLETE_AFTER: i64 = 3600;

/// The note for a worktree back at its path that a purge may have deleted
/// part of.
pub const PURGE_STOPPED: &str = "if its purge had started deleting, part of it is gone: `git status` in it lists tracked files that are missing, and `git restore .` brings them back; ignored output comes back from the project's install";

pub fn root(common_dir: &Path) -> PathBuf {
    common_dir.join(DIR)
}

/// Why residue removed a worktree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// `residue remove PATH`.
    Remove,
    /// `residue remove --all`.
    Sweep,
    /// `residue watch`: the branch's upstream was deleted.
    WatchLanded,
    /// `residue watch`: the disk was low.
    WatchLowDisk,
    /// The trash's purge, after `--keep-trash`.
    TrashPurge,
}

impl Reason {
    /// The code in `info` and in JSON. `--removal-reason` takes the watch
    /// ones with hyphens, as options are written.
    pub fn code(self) -> &'static str {
        match self {
            Reason::Remove => "remove",
            Reason::Sweep => "sweep",
            Reason::WatchLanded => "watch_landed",
            Reason::WatchLowDisk => "watch_low_disk",
            Reason::TrashPurge => "trash_purge",
        }
    }

    /// Who removed it and why, as a phrase that follows "removed DATE";
    /// a code a later version wrote is shown as it is.
    pub fn describe(code: &str) -> String {
        match code {
            "remove" => "by residue remove".into(),
            "sweep" => "by a residue remove --all sweep".into(),
            "watch_landed" => "by residue watch: its upstream branch was deleted".into(),
            "watch_low_disk" => "by residue watch: the disk was low".into(),
            "trash_purge" => "from the trash, by its purge after --keep-trash".into(),
            other => format!("({other})"),
        }
    }
}

/// Which residue command removed a worktree: the run that wrote the
/// record. A trash purge's record names the run that purged the entry, so
/// what was freed by hand and what automatic cleanup freed stay apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum By {
    /// `residue remove PATH...`.
    Remove,
    /// `residue remove --all`, by hand or from a scheduler.
    Sweep,
    /// `residue watch`: the `remove` run it starts.
    Watch,
}

impl By {
    /// The code in `info` (`by remove`) and in JSON.
    pub fn code(self) -> &'static str {
        match self {
            By::Remove => "remove",
            By::Sweep => "sweep",
            By::Watch => "watch",
        }
    }
}

/// How long a record is kept, as the removal that wrote it was told:
/// `keep SECONDS TEXT` in `info`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Keep {
    pub seconds: i64,
    /// As the user wrote it: `12w`.
    pub text: String,
}

impl Keep {
    /// The value of a `keep` line, `SECONDS TEXT` with SECONDS above zero;
    /// `None` for anything else. A trash entry's `info` keeps it the same
    /// way.
    pub fn parse(value: &str) -> Option<Keep> {
        let (seconds, shown) = value.split_once(' ').filter(|(_, s)| !s.is_empty())?;
        let seconds = seconds.parse::<i64>().ok().filter(|s| *s > 0)?;
        Some(Keep {
            seconds,
            text: shown.to_string(),
        })
    }
}

/// An ignored entry the deletion took, as the plan measured it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Gone {
    pub path: String,
    pub files: Option<u64>,
    pub bytes: Option<u64>,
}

impl Gone {
    /// `node_modules/ (1204 files, 31 MB)`.
    pub fn sized(&self) -> String {
        match (self.files, self.bytes) {
            (Some(f), Some(b)) => format!(
                "{} ({f} file{}, {})",
                self.path,
                if f == 1 { "" } else { "s" },
                output::bytes(b)
            ),
            _ => self.path.clone(),
        }
    }

    pub fn json(&self) -> Json {
        Json::Obj(vec![
            ("path", Json::str(self.path.clone())),
            ("files", Json::opt(self.files, Json::UInt)),
            ("bytes", Json::opt(self.bytes, Json::UInt)),
        ])
    }
}

/// What `info` records about one removed worktree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Record {
    /// The path the worktree had, where `residue restore` recreates it.
    pub original: PathBuf,
    /// Where it was when the deletion started: the hidden
    /// `.NAME.residue-removing-PID`, or its place in the trash.
    pub aside: PathBuf,
    /// Full ref name, `refs/heads/fix`; `None` for a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
    /// The upstream's full ref name, `refs/remotes/origin/fix`.
    pub upstream: Option<String>,
    /// [`Reason::code`], or a code a later version wrote.
    pub reason: String,
    /// [`By::code`], or a code a later version wrote; `None` for a record
    /// written before residue named the command.
    pub by: Option<String>,
    /// Unix seconds.
    pub removed: i64,
    /// The `--keep-removed` of the removal that wrote it: no run forgets
    /// it before then (`Item::expired`). `None` for a record written
    /// before residue kept it.
    pub keep: Option<Keep>,
    pub allocated: Option<u64>,
    pub reclaimable: Option<u64>,
    /// The ignored entries the deletion took.
    pub gone: Vec<Gone>,
    /// The residue process that wrote it.
    pub pid: Option<u32>,
    pub version: Option<String>,
    /// Its writer holds `info` while it, or the Git it starts, acts on the
    /// worktree (the `hold flock` line, `hold`). A record without it comes
    /// from a release that takes no hold, 0.0.10 and earlier, whose run
    /// only its PID and its Git's arguments show (`unheld`).
    pub hold: bool,
    /// The fewest commits that reach every commit only the worktree's own
    /// reflog held; the Nth has the ref `refs/residue/reflog/ENTRY/N`.
    pub kept: Vec<String>,
}

/// `\` and newline escaped, so one value is one line whatever its bytes.
fn escape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b => out.push(b),
        }
    }
    out
}

fn unescape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut it = bytes.iter();
    while let Some(&b) = it.next() {
        if b == b'\\' {
            match it.next() {
                Some(b'n') => out.push(b'\n'),
                Some(&other) => out.push(other),
                None => out.push(b'\\'),
            }
        } else {
            out.push(b);
        }
    }
    out
}

fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Record {
    fn render(&self, name: &str) -> Vec<u8> {
        let mut out =
            format!("# residue removal record. `residue restore {name}` recreates the worktree.\n")
                .into_bytes();
        let mut line = |key: &str, value: &[u8]| {
            out.extend_from_slice(key.as_bytes());
            out.push(b' ');
            out.extend_from_slice(&escape(value));
            out.push(b'\n');
        };
        line("path", self.original.as_os_str().as_bytes());
        line("aside", self.aside.as_os_str().as_bytes());
        if let Some(b) = &self.branch {
            line("branch", b.as_bytes());
        }
        line("head", self.head.as_bytes());
        if let Some(u) = &self.upstream {
            line("upstream", u.as_bytes());
        }
        line("ref", format!("{REF_PREFIX}{name}").as_bytes());
        line("reason", self.reason.as_bytes());
        if let Some(by) = &self.by {
            line("by", by.as_bytes());
        }
        line(
            "removed",
            format!("{} {}", self.removed, output::rfc3339(self.removed)).as_bytes(),
        );
        if let Some(k) = &self.keep {
            line("keep", format!("{} {}", k.seconds, k.text).as_bytes());
        }
        if let Some(n) = self.allocated {
            line("allocated", n.to_string().as_bytes());
        }
        if let Some(n) = self.reclaimable {
            line("reclaimable", n.to_string().as_bytes());
        }
        for g in &self.gone {
            let n = |v: Option<u64>| v.map_or("-".to_string(), |v| v.to_string());
            line(
                "deleted",
                format!("{} {} {}", n(g.files), n(g.bytes), g.path).as_bytes(),
            );
        }
        if let Some(pid) = self.pid {
            line("pid", pid.to_string().as_bytes());
        }
        if let Some(v) = &self.version {
            line("version", v.as_bytes());
        }
        if self.hold {
            line("hold", HOLD.as_bytes());
        }
        for k in &self.kept {
            line("kept", k.as_bytes());
        }
        out
    }

    fn parse(bytes: &[u8]) -> Result<Record, String> {
        let mut record = Record {
            original: PathBuf::new(),
            aside: PathBuf::new(),
            branch: None,
            head: String::new(),
            upstream: None,
            reason: String::new(),
            by: None,
            removed: 0,
            keep: None,
            allocated: None,
            reclaimable: None,
            gone: Vec::new(),
            pid: None,
            version: None,
            hold: false,
            kept: Vec::new(),
        };
        let (mut original, mut aside, mut removed) = (None, None, None);
        for line in bytes.split(|b| *b == b'\n') {
            if line.is_empty() || line.starts_with(b"#") {
                continue;
            }
            let (key, raw) = match line.iter().position(|b| *b == b' ') {
                Some(i) => (&line[..i], &line[i + 1..]),
                None => (line, &line[line.len()..]),
            };
            let value = unescape(raw);
            let text = || String::from_utf8_lossy(&value).into_owned();
            let number = |what: &str| {
                text()
                    .split(' ')
                    .next()
                    .and_then(|n| n.parse::<i64>().ok())
                    .ok_or_else(|| format!("{what} is not a number"))
            };
            match key {
                b"path" => original = Some(PathBuf::from(OsString::from_vec(value.clone()))),
                b"aside" => aside = Some(PathBuf::from(OsString::from_vec(value.clone()))),
                b"branch" => record.branch = Some(text()),
                b"head" => record.head = text(),
                b"upstream" => record.upstream = Some(text()),
                b"reason" => record.reason = text(),
                b"by" => record.by = Some(text()),
                b"removed" => removed = Some(number("removed")?),
                b"keep" => {
                    record.keep = Some(Keep::parse(&text()).ok_or("keep is not SECONDS DURATION")?);
                }
                b"allocated" => record.allocated = number("allocated")?.try_into().ok(),
                b"reclaimable" => record.reclaimable = number("reclaimable")?.try_into().ok(),
                b"deleted" => {
                    let t = text();
                    let mut parts = t.splitn(3, ' ');
                    let (Some(files), Some(bytes), Some(path)) =
                        (parts.next(), parts.next(), parts.next())
                    else {
                        return Err("a deleted line has fewer than three fields".into());
                    };
                    record.gone.push(Gone {
                        path: path.to_string(),
                        files: files.parse().ok(),
                        bytes: bytes.parse().ok(),
                    });
                }
                b"pid" => record.pid = text().parse().ok(),
                b"version" => record.version = Some(text()),
                // Another kind of hold, from a later release, is one this
                // release cannot see, so only its PID counts.
                b"hold" => record.hold = text() == HOLD,
                b"kept" => {
                    let commit = text();
                    if !is_object_id(&commit) {
                        return Err("a kept line is not an object name".into());
                    }
                    record.kept.push(commit);
                }
                // `ref` is derived from the entry name; keys a later
                // version adds carry nothing this one needs.
                _ => {}
            }
        }
        record.original = original.ok_or("it has no path line")?;
        record.aside = aside.ok_or("it has no aside line")?;
        if !record.original.is_absolute() || !record.aside.is_absolute() {
            return Err("its path or aside is not absolute".into());
        }
        if !is_object_id(&record.head) {
            return Err("its head is not an object name".into());
        }
        if let Some(b) = &record.branch {
            let short = b.strip_prefix("refs/heads/").unwrap_or("");
            if short.is_empty() || short.starts_with('-') {
                return Err(format!("its branch {b:?} is not a local branch"));
            }
        }
        record.removed = removed.ok_or("it has no removed line")?;
        Ok(record)
    }

    pub fn short_branch(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b))
    }

    /// The PID of the run that wrote this record: its `pid` line, or for a
    /// record older than that line, the PID in a removal's aside name.
    fn writer(&self) -> Option<u32> {
        self.pid.or_else(|| crate::remove::pid_of(&self.aside))
    }
}

/// A process whose arguments residue cannot read that started this many
/// seconds after a record was written is not the run that wrote it, which
/// started before (`writes`). A smaller gap may be a step of the clock, so
/// that process still counts.
const REUSED_AFTER: i64 = 600;

/// How much earlier than a trash entry's `trashed` time the name of a
/// record that residue cannot read may say it was written and still be a
/// purge of that entry (`stopped_purges`): the two runs' clocks may
/// differ, by a step or a wrong clock, and a day covers that.
const NAMED_SLACK: i64 = 86_400;

/// What may still delete the worktree of a record from a release that
/// takes no hold (`unheld`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unheld {
    /// The run that wrote the record, this PID, still runs; `true` when
    /// nothing else here shows that its removal may still run, so the PID
    /// alone keeps it, and a PID may be reused.
    Writer(u32, bool),
    /// A `git worktree remove` of the worktree runs, as the one a run
    /// started still does after that run is killed alone.
    Git(u32),
    /// The activity probe sees this process, with this command, using the
    /// worktree or its Git directory.
    Using(u32, String),
}

/// What may still delete the worktree of the record `item` holds,
/// `record`, while Git lists it at `registered`, where its deletion
/// started, when the record comes from a release that takes no hold (no
/// `hold` line), whose run and Git no hold shows. In turn: the run that
/// wrote it still runs (`writes`); a `git worktree remove` of the
/// worktree runs (`process::removing`); the activity probe (`snapshot`,
/// or one taken now) sees a process using the worktree or its Git
/// directory. A running writer comes first, and the rest is still asked,
/// so that `Unheld::Writer` says whether its PID alone keeps the worktree.
/// `Ok(None)` for a record with its `hold` line, whose hold
/// shows all of that. `Err` when residue cannot tell: another user wrote
/// the record and residue, not root, cannot see that user's processes
/// (`foreign`), it cannot read the arguments of processes, or the probe
/// cannot show that no process uses the worktree (a probe failure, or a
/// partial probe). Once Git no longer lists the worktree, its removal has
/// ended and nothing of it is left to delete, so callers ask only while
/// Git lists it.
pub fn unheld(
    item: &Item,
    record: &Record,
    registered: &Entry,
    snapshot: Option<&Snapshot>,
) -> Result<Option<Unheld>, String> {
    if record.hold {
        return Ok(None);
    }
    foreign(&item.dir.join(INFO))?;
    let me = std::process::id();
    let writer = record
        .writer()
        .filter(|&pid| pid != me && crate::remove::running(pid) && writes(pid, record));
    let others = unheld_others(item, record, registered, snapshot);
    match writer {
        Some(pid) => Ok(Some(Unheld::Writer(pid, matches!(others, Ok(None))))),
        None => others,
    }
}

/// [`unheld`] after the writer: a `git worktree remove` of the worktree,
/// then the activity probe.
fn unheld_others(
    item: &Item,
    record: &Record,
    registered: &Entry,
    snapshot: Option<&Snapshot>,
) -> Result<Option<Unheld>, String> {
    let mut paths = vec![record.aside.clone(), registered.path.clone()];
    paths.extend(git::canonical(&record.aside).ok());
    paths.sort();
    paths.dedup();
    let named: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    if let Some(&pid) = crate::process::removing(&named)?.first() {
        return Ok(Some(Unheld::Git(pid)));
    }
    let taken;
    let snapshot = match snapshot {
        Some(snapshot) => snapshot,
        None => {
            taken = Snapshot::take();
            &taken
        }
    };
    let git_dir = git_dir_of(&item.common_dir, &named);
    let wanted = snapshot.wanted();
    for dir in named.iter().copied().chain(git_dir.as_deref()) {
        // No walk sized this folder, so one runs here, as the plan's does:
        // it says how long the paths inside it are, and which files the
        // probe could not name lie inside it. A folder that is gone holds
        // nothing.
        let reach = match reach(dir, &wanted) {
            Ok(reach) => reach,
            Err(e) => {
                return Err(format!(
                    "the activity probe cannot show that no process is using {}: cannot walk it: {e}",
                    dir.display()
                ));
            }
        };
        match snapshot.evidence(dir, reach.as_ref()) {
            Evidence::InUse(uses) => {
                if let Some((pid, command)) = uses.into_iter().next() {
                    return Ok(Some(Unheld::Using(pid, command)));
                }
            }
            Evidence::NotObserved => {}
            // A probe that failed, or reads too little, shows no use, and
            // that is not evidence of none.
            Evidence::Unknown(why) => {
                return Err(format!(
                    "the activity probe cannot show that no process is using {}: {}",
                    dir.display(),
                    why.detail
                ));
            }
        }
    }
    Ok(None)
}

/// What a walk of `dir` reached (`fsinfo::Reach`), or `None` when `dir`
/// is gone.
fn reach(dir: &Path, wanted: &[(u64, u64)]) -> std::io::Result<Option<crate::fsinfo::Reach>> {
    let walked = fs::canonicalize(dir).and_then(|canonical| {
        crate::fsinfo::measure(&canonical, &[], &|_| false, &|_| false, wanted)
    });
    match walked {
        Ok(usage) => Ok(Some(usage.reach)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether the live process `pid`, the one `record` names, may be the run
/// that wrote it. With its arguments readable, it is when they hold
/// `remove` (`process::removes`), whenever it started: a step of the
/// clock moves the start time Linux gives. Without them (another user's
/// process), it is unless it started well after the record
/// (`REUSED_AFTER`), where the platform says when (`process::started`).
fn writes(pid: u32, record: &Record) -> bool {
    match crate::process::arguments(pid) {
        Some(args) => crate::process::removes(&args),
        None => crate::process::started(pid)
            .is_none_or(|at| at <= record.removed.saturating_add(REUSED_AFTER)),
    }
}

/// `Err` when the record `info` belongs to another user and residue does
/// not run as root: it cannot read the arguments or the open files of that
/// user's processes on every platform, so a run or a Git of theirs, such
/// as one `sudo residue remove` started, would go unseen.
fn foreign(info: &Path) -> Result<(), String> {
    // SAFETY: geteuid only returns this process's effective user.
    let me = unsafe { libc::geteuid() };
    if me == 0 {
        return Ok(());
    }
    let owner = fs::metadata(info)
        .map_err(|e| format!("cannot read {}: {e}", info.display()))?
        .uid();
    if owner == me {
        return Ok(());
    }
    // No `sudo` hint: a run as another user, root included, leaves files
    // that user owns, such as the restored marker, in this Git directory.
    let who = if owner == 0 {
        "root".to_string()
    } else {
        format!("uid {owner}")
    };
    Err(format!(
        "{who} wrote it, and residue runs as uid {me}, so it cannot see that user's processes; run residue as that user"
    ))
}

/// The Git directory, `GIT_COMMON_DIR/worktrees/ID`, whose `gitdir` names
/// the `.git` of one of `paths`. Git writes that path absolute, or
/// relative to the Git directory with `worktree.useRelativePaths`.
pub fn git_dir_of(common_dir: &Path, paths: &[&Path]) -> Option<PathBuf> {
    let wanted: Vec<PathBuf> = paths.iter().map(|p| p.join(".git")).collect();
    for child in fs::read_dir(common_dir.join("worktrees")).ok()?.flatten() {
        let dir = child.path();
        let Ok(bytes) = fs::read(dir.join("gitdir")) else {
            continue;
        };
        let named = Path::new(OsStr::from_bytes(bytes.trim_ascii_end()));
        if wanted.contains(&lexical(&dir.join(named))) {
            return Some(dir);
        }
    }
    None
}

/// `path` with its `.` and `..` components resolved by name alone.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// A restore's refusal while something may still delete the worktree of
/// the record `item` holds, `record`, which Git lists at `registered`
/// (`unheld`). `what` names the record. When residue cannot tell, the
/// refusal names the move back by hand, for once nothing can still be
/// deleting it.
fn refuse_unheld(
    item: &Item,
    record: &Record,
    registered: &Entry,
    what: &str,
) -> Result<(), String> {
    match unheld_refusal(item, record, registered, what) {
        None => Ok(()),
        Some(unsettled) => Err(unsettled.restore_text()),
    }
}

/// What keeps a restore or a purge off the worktree of `record`, which a
/// release that takes no hold wrote, while something may still delete it
/// (`unheld`); `None` when nothing can. Each caller words it.
fn unheld_refusal(
    item: &Item,
    record: &Record,
    registered: &Entry,
    what: &str,
) -> Option<Unsettled> {
    let found = match unheld(item, record, registered, None) {
        Ok(None) => return None,
        Ok(Some(found)) => Ok(found),
        Err(e) => Err(e),
    };
    Some(Unsettled::Unheld {
        what: what.to_string(),
        release: record
            .version
            .as_deref()
            .map_or("an older residue".to_string(), |v| {
                format!("residue {}", output::clean(v))
            }),
        found,
        move_back: move_back_line(&item.command_dir, registered, &record.original),
    })
}

/// Why `stopped_purges` found that a purge of the entry may not have
/// stopped, or could not tell, before anything changed. A restore says it
/// one way (`restore_text`), a purge another (`purge_text`).
pub enum Unsettled {
    /// A process holds `what`, a purge's record, at `info`; `pid` is the
    /// one the record names.
    Held {
        what: String,
        info: PathBuf,
        pid: Option<u32>,
    },
    /// `what` comes from `release`, which takes no hold, and something may
    /// still delete the worktree (`found`), or residue cannot tell; the
    /// move back by hand is `move_back`.
    Unheld {
        what: String,
        release: String,
        found: Result<Unheld, String>,
        move_back: String,
    },
    /// The record `name` may be one of this entry's purges, and cannot be
    /// read (`error`); `why` residue cannot set it by.
    Unreadable {
        name: String,
        error: String,
        why: Unread,
    },
    /// What residue could not read or take, in words for any caller.
    Error(String),
}

/// Why an unreadable record that may be a purge's of the entry stops it.
pub enum Unread {
    /// A process holds it, at this `info`.
    Held(PathBuf),
    /// Something may still delete the worktree: this.
    Deleting(String),
    /// No record that can be read names the worktree, so this one may be
    /// its purge's; the line that sets it aside, keeping its files and refs.
    NotNamed(String),
}

impl Unsettled {
    /// Whether it ends without a person: a process that holds a record, or
    /// may still delete the worktree, ends. An unreadable record that no
    /// readable one explains stays until someone reads or sets it aside.
    pub fn passes(&self) -> bool {
        !matches!(
            self,
            Unsettled::Unreadable {
                why: Unread::NotNamed(_),
                ..
            }
        )
    }

    fn cannot(name: &str, error: &str) -> String {
        format!(
            "residue cannot read the removal record {name} ({error}), so it cannot tell whether a purge of this entry stopped partway, or still runs"
        )
    }

    /// What a restore says, having changed nothing.
    pub fn restore_text(&self) -> String {
        match self {
            Unsettled::Held { what, info, pid } => busy_note_of(*pid, info, what),
            Unsettled::Unheld {
                what,
                release,
                found,
                move_back,
            } => match found {
                // A PID may be reused: when it alone keeps the worktree, the
                // move back by hand is named for when it is not residue.
                Ok(Unheld::Writer(pid, true)) => format!(
                    "{release} wrote {what}, takes no hold, and its process {pid} still runs, so it may still be deleting the worktree; run this restore again once it ends; if `ps -p {pid}` shows it is not residue, this moves it back: {move_back}"
                ),
                Ok(Unheld::Writer(pid, false)) => format!(
                    "{release} wrote {what}, takes no hold, and its process {pid} still runs, so it may still be deleting the worktree; run this restore again once it ends; `ps -p {pid}` shows what it is"
                ),
                Ok(Unheld::Git(pid)) => format!(
                    "{release} wrote {what} and takes no hold, and process {pid}, a `git worktree remove` of the worktree, still runs, so it may still be deleting it; run this restore again once it ends; `ps -p {pid}` shows what it is"
                ),
                Ok(Unheld::Using(pid, command)) => format!(
                    "{release} wrote {what} and takes no hold, and process {pid} ({}) is using the worktree or its Git directory, so it may still be deleting it; run this restore again once it ends",
                    output::clean(command)
                ),
                Err(e) => format!(
                    "{release} wrote {what} and takes no hold, and residue cannot tell whether anything still deletes the worktree ({e}), so it changed nothing; once nothing can, such as after a restart, this moves it back: {move_back}"
                ),
            },
            Unsettled::Unreadable { name, error, why } => {
                let cannot = Self::cannot(name, error);
                match why {
                    Unread::Held(info) => format!(
                        "{cannot}, and a process holds that record (`lsof {}` names it); residue changed nothing; run this restore again once that process ends",
                        output::shell(info)
                    ),
                    Unread::Deleting(why) => {
                        format!("{cannot}, and {why}; residue changed nothing")
                    }
                    Unread::NotNamed(line) => format!(
                        "{cannot}; residue changed nothing; if that record is not about this entry, `{line}` sets it aside, keeping its files and refs"
                    ),
                }
            }
            Unsettled::Error(e) => e.clone(),
        }
    }

    /// What a purge says as it leaves the entry in the trash.
    pub fn purge_text(&self) -> String {
        let again = "a later remove run tries the purge again";
        match self {
            Unsettled::Held { what, info, .. } => format!(
                "a residue run, or the `git worktree remove` it started, still holds {what} (`lsof {}` names it): it may still be deleting the worktree, or restoring or forgetting the record; {again} once that ends",
                output::shell(info)
            ),
            Unsettled::Unheld {
                what,
                release,
                found,
                ..
            } => match found {
                Ok(Unheld::Writer(pid, _)) => format!(
                    "{release} wrote {what}, takes no hold, and its process {pid} still runs, so it may still be deleting the worktree; {again} once it ends; `ps -p {pid}` shows what it is"
                ),
                Ok(Unheld::Git(pid)) => format!(
                    "{release} wrote {what} and takes no hold, and process {pid}, a `git worktree remove` of the worktree, still runs, so it may still be deleting it; {again} once it ends"
                ),
                Ok(Unheld::Using(pid, command)) => format!(
                    "{release} wrote {what} and takes no hold, and process {pid} ({}) is using the worktree or its Git directory, so it may still be deleting it; {again} once it ends",
                    output::clean(command)
                ),
                Err(e) => format!(
                    "{release} wrote {what} and takes no hold, and residue cannot tell whether anything still deletes the worktree ({e}); {again}"
                ),
            },
            Unsettled::Unreadable { name, error, why } => {
                let cannot = Self::cannot(name, error);
                match why {
                    Unread::Held(info) => format!(
                        "{cannot}, and a process holds that record (`lsof {}` names it); {again} once that process ends",
                        output::shell(info)
                    ),
                    Unread::Deleting(why) => format!("{cannot}, and {why}; {again} once that ends"),
                    Unread::NotNamed(line) => format!(
                        "{cannot}, so the entry stays until that record can be read; if it is not about this entry, `{line}` sets it aside, keeping its files and refs"
                    ),
                }
            }
            Unsettled::Error(e) => e.clone(),
        }
    }
}

/// The commands that move the worktree Git lists at `registered` back to
/// `original` by hand, lifting residue's own lock on it first.
pub fn move_back_line(main: &Path, registered: &Entry, original: &Path) -> String {
    let git = format!("git -C {} worktree", output::shell(main));
    let path = output::shell(&registered.path);
    let ours = matches!(
        registered.locked.as_deref(),
        Some(crate::remove::REMOVING | crate::trash::LOCK)
    );
    let unlock = if ours {
        format!("{git} unlock {path} && ")
    } else {
        String::new()
    };
    format!(
        "{}{unlock}{git} move {path} {}",
        output::vacant(original),
        output::shell(original)
    )
}

/// The command that recreates by hand, at its original path, the worktree
/// of `item`'s record, which Git no longer lists: the `git worktree add`
/// that `recreate` runs (`choose`), after `output::vacant`, so it stops
/// before Git makes a branch while anything is at that path. `Err` says
/// why no line can bring it back.
fn recreate_line(item: &Item, record: &Record) -> Result<String, String> {
    let listed = git::worktrees(&item.command_dir)?;
    let choice = choose(item, record, &listed)?;
    let path = &record.original;
    let words = match choice.checkout(&record.head) {
        Checkout::Branch(branch) => format!("{} {}", output::shell(path), output::word(branch)),
        Checkout::NewBranch(branch, commit) => format!(
            "-b {} {} {}",
            output::word(branch),
            output::shell(path),
            output::word(commit)
        ),
        Checkout::Detached(commit) => {
            format!("--detach {} {}", output::shell(path), output::word(commit))
        }
    };
    Ok(format!(
        "{}git -C {} worktree add {words}",
        output::vacant(path),
        output::shell(&item.command_dir)
    ))
}

/// How a restore recreates the worktree of a record Git no longer lists
/// (`choose`): what `recreate` runs and what `recreate_line` names.
enum Choice {
    /// The branch is gone: a branch of its name again, at the recorded
    /// HEAD, so Git never takes one from a remote instead.
    Again(String),
    /// The branch is at the recorded HEAD or ahead of it, and no worktree
    /// has it checked out: that branch, at `tip`.
    Branch { short: String, tip: String },
    /// The branch, at `tip`, no longer contains the recorded HEAD, or a
    /// worktree has it checked out (`elsewhere` says where): `name`, the
    /// first free `BRANCH-restored`, at the recorded HEAD. The branch and
    /// that worktree stay as they are.
    NewBranch {
        name: String,
        tip: String,
        elsewhere: Option<String>,
    },
    /// The record names no branch: detached at the recorded HEAD.
    Detached,
}

impl Choice {
    fn checkout<'a>(&'a self, head: &'a str) -> Checkout<'a> {
        match self {
            Choice::Again(short) => Checkout::NewBranch(short, head),
            Choice::Branch { short, .. } => Checkout::Branch(short),
            Choice::NewBranch { name, .. } => Checkout::NewBranch(name, head),
            Choice::Detached => Checkout::Detached(head),
        }
    }
}

/// The one decision of how the worktree of `item`'s record comes back
/// with `git worktree add`, from Git's worktrees as `listed` now. It never
/// moves, resets or deletes a branch, and never touches another worktree.
fn choose(item: &Item, record: &Record, listed: &[Entry]) -> Result<Choice, String> {
    let main = &item.command_dir;
    let head = record.head.as_str();
    if git::commit_of(main, head)?.is_none() {
        return Err(format!(
            "commit {} is no longer in the repository: its ref {} was deleted and Git dropped it",
            &head[..head.len().min(12)],
            item.ref_name()
        ));
    }
    let (Some(full), Some(short)) = (&record.branch, record.short_branch()) else {
        return Ok(Choice::Detached);
    };
    let Some(tip) = git::commit_of(main, full)? else {
        return Ok(Choice::Again(short.to_string()));
    };
    let contains = tip == head || git::is_ancestor(main, head, &tip)?;
    let elsewhere = listed.first().and_then(|first| {
        let trash_root = crate::trash::root(&item.common_dir);
        listed
            .iter()
            .find(|e| e.branch.as_deref() == Some(full.as_str()))
            .map(|e| whereabouts(e, first, &trash_root))
    });
    if contains && elsewhere.is_none() {
        return Ok(Choice::Branch {
            short: short.to_string(),
            tip,
        });
    }
    Ok(Choice::NewBranch {
        name: free_branch(main, &format!("{short}-restored"))?,
        tip,
        elsewhere: if contains { elsewhere } else { None },
    })
}

/// What a restore says when a run holds a record: `info` is its file.
/// Once the run that wrote it has exited, what holds it is its Git, a
/// program that Git started, or another run that restores or forgets the
/// record, and `lsof` names it.
fn busy_note(record: Option<&Record>, info: &Path, what: &str) -> String {
    busy_note_of(record.and_then(|r| r.pid), info, what)
}

/// [`busy_note`] for the record that names `pid`.
fn busy_note_of(pid: Option<u32>, info: &Path, what: &str) -> String {
    let lsof = format!("`lsof {}` names it", output::shell(info));
    match pid {
        Some(pid) if !crate::remove::running(pid) => format!(
            "the residue run that wrote {what} (process {pid}) has exited, but another process still holds it ({lsof}): the `git worktree remove` it started, or a program that Git started, may still be deleting the worktree, or another residue run is restoring or forgetting the record; run this restore again once that process ends"
        ),
        _ => format!(
            "a residue run, or the `git worktree remove` it started, still holds {what} ({lsof}): it may still be deleting the worktree, or restoring or forgetting the record; run this restore again once it ends"
        ),
    }
}

/// The part of an entry name that comes from the worktree's directory
/// name: letters, digits, `.`, `_` and `-`, so the entry name is also a
/// valid ref name; anything else becomes `_`, and leading dots go.
fn entry_base(original: &Path) -> String {
    let raw = original
        .file_name()
        .map(OsStr::to_string_lossy)
        .unwrap_or_default();
    let mut base = String::new();
    for c in raw.chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            c
        } else {
            '_'
        };
        // `..` is not allowed in a ref name.
        if c == '.' && base.ends_with('.') {
            continue;
        }
        base.push(c);
    }
    let base = base.trim_start_matches('.').to_string();
    if base.is_empty() {
        "worktree".to_string()
    } else {
        base
    }
}

/// [`Item::named_at`] for a record entry named `name`.
fn named_at(name: &str, original: &Path) -> Option<i64> {
    let rest = name
        .strip_prefix(&entry_base(original))?
        .strip_prefix('-')?;
    let (stamp, n) = rest.split_at_checked(16)?;
    let numbered = n
        .strip_prefix('-')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
    if !n.is_empty() && !numbered {
        return None;
    }
    output::unstamp(stamp)
}

/// One record in a repository.
pub struct Item {
    /// The entry directory's name, also the last part of its refs' names:
    /// `fix-login-20261107T040000Z`.
    pub name: String,
    pub dir: PathBuf,
    pub common_dir: PathBuf,
    /// Where to run `git worktree` commands for this repository.
    pub command_dir: PathBuf,
    pub record: Result<Record, String>,
    /// The entry has no `info`: the run that made it stopped before writing
    /// it, so it never deleted anything.
    pub incomplete: bool,
    /// The entry holds a copy of the worktree's `config.worktree`, as it
    /// did when it was listed.
    pub worktree_config: bool,
    /// The entry holds a copy of the worktree's HEAD reflog.
    pub head_reflog: bool,
    /// When the entry directory last changed, in Unix seconds.
    pub modified: Option<i64>,
    /// Git still lists the worktree where the deletion started: the run
    /// did not finish (an interrupted removal). A `git worktree remove`
    /// that failed part way can have deleted part of its files and of its
    /// Git directory, which the record keeps.
    pub registered: Option<Entry>,
    /// Whether Git lists this record's worktree at its original path
    /// again (`is_back`).
    pub back: Back,
    /// Its fingerprint (`fingerprint::entry`), from the `info` read here:
    /// `id` in its JSON, which `restore --expect` compares.
    pub id: String,
}

/// Whether Git lists a record's worktree at its original path again.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Back {
    /// No worktree with its checkout is there, or one on another branch,
    /// or one whose HEAD does not contain the recorded one.
    No,
    /// One is there, on the recorded branch (detached when the record
    /// is), at the recorded HEAD or ahead of it.
    Yes,
    /// One is there on the recorded branch, but Git could not compare its
    /// HEAD with the recorded one: residue cannot tell, so it calls the
    /// record neither back nor removed.
    Unknown(String),
}

/// Where a removal record stands, read from its files and from Git's list
/// of worktrees when it is listed. `item_json` gives it as `state`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// The entry has no `info`: its run stopped before it deleted anything.
    Incomplete,
    /// Its `info` cannot be read, so residue never uses or forgets it.
    Unreadable,
    /// Git still lists the worktree where the deletion started.
    Interrupted,
    /// Git lists the worktree at its original path again.
    Back,
    /// A worktree is at its original path on its branch, and residue
    /// could not tell whether it holds the recorded HEAD (`Back::Unknown`).
    Unknown,
    /// The worktree is gone; `residue restore` recreates it.
    Removed,
}

impl State {
    pub fn code(self) -> &'static str {
        match self {
            State::Incomplete => "incomplete",
            State::Unreadable => "unreadable",
            State::Interrupted => "interrupted",
            State::Back => "back",
            State::Unknown => "unknown",
            State::Removed => "removed",
        }
    }
}

impl Item {
    pub fn ref_name(&self) -> String {
        format!("{REF_PREFIX}{}", self.name)
    }

    pub fn interrupted(&self) -> bool {
        self.registered.is_some()
    }

    /// A record with an `info` residue cannot read, whose name it could
    /// have been given for a worktree at `original` (`named_at`): residue
    /// cannot tell whether it names that worktree, or whether a run holds
    /// it.
    pub fn may_be_unreadable_for(&self, original: &Path) -> bool {
        self.record.is_err() && !self.incomplete && self.named_at(original).is_some()
    }

    /// When this record was written, as its name says, if `create` could
    /// have given it that name for a worktree at `original`: every release
    /// names a record `BASE-STAMP` or `BASE-STAMP-N`, BASE from the
    /// worktree's directory name (`entry_base`), STAMP the removal's time
    /// by the clock of the run that wrote it (`output::stamp`), N from 2.
    pub fn named_at(&self, original: &Path) -> Option<i64> {
        named_at(&self.name, original)
    }

    /// The command that sets this record's directory aside, out of
    /// residue's list, keeping its files and its refs: for a record residue
    /// cannot read, which then no longer blocks a restore.
    pub fn set_aside_line(&self) -> String {
        let to = self
            .common_dir
            .join(format!("residue-damaged-{}", self.name));
        format!("mv {} {}", output::shell(&self.dir), output::shell(&to))
    }

    /// A run with `--keep-removed` of `keep` seconds forgets this record:
    /// it is at least that old and at least as old as the `--keep-removed`
    /// of the removal that wrote it, so no run forgets a record before the
    /// time it was written with. A record with no `keep` line (residue
    /// 0.0.7 and earlier) has only the run's.
    pub fn expired(&self, now: i64, keep: i64) -> bool {
        self.record.as_ref().ok().is_some_and(|r| {
            let own = r.keep.as_ref().map_or(0, |k| k.seconds);
            now - r.removed >= keep.max(own)
        })
    }

    pub fn state(&self) -> State {
        match (&self.record, self.incomplete) {
            (Err(_), true) => State::Incomplete,
            (Err(_), false) => State::Unreadable,
            (Ok(_), _) if self.interrupted() => State::Interrupted,
            (Ok(_), _) if self.back == Back::Yes => State::Back,
            (Ok(_), _) if matches!(self.back, Back::Unknown(_)) => State::Unknown,
            (Ok(_), _) => State::Removed,
        }
    }

    /// An entry with no `info` that a remove run may clear: old enough that
    /// no run can still be writing it. Its refs are checked when it goes.
    pub fn clearable(&self, now: i64) -> bool {
        self.incomplete && self.modified.is_some_and(|m| now - m >= INCOMPLETE_AFTER)
    }
}

/// The refs a record has: its HEAD's, then one for each kept commit, each
/// with the commit it must point at.
fn refs_of(name: &str, head: &str, kept: &[String]) -> Vec<(String, String)> {
    let mut refs = vec![(format!("{REF_PREFIX}{name}"), head.to_string())];
    for (i, commit) in kept.iter().enumerate() {
        refs.push((format!("{REFLOG_PREFIX}{name}/{}", i + 1), commit.clone()));
    }
    refs
}

/// The records of the repository at `common_dir`, oldest first. `listed`
/// is every worktree Git lists for it, so a record whose worktree Git still
/// lists where the deletion started is marked interrupted, and one whose
/// worktree Git lists at its original path again is marked back. A record
/// that cannot be read is listed with its error, and never forgotten or
/// used.
pub fn of_repo(repo: &crate::discover::Repo) -> Result<Vec<Item>, String> {
    let listed: Vec<&Entry> = repo.trash.iter().chain(&repo.interrupted).collect();
    list(&repo.common_dir, &repo.command_dir(), &listed, &repo.linked)
}

/// The records of purges of the trash worktree at `aside`, which came from
/// `original`, that Git still lists there, read now, each held. `listed`
/// is every worktree Git lists for the repository now. A purge holds the
/// trash entry while it, or its Git, runs, so to a restore that holds the
/// entry each one is a purge that stopped partway. The restore refuses,
/// having changed nothing, while one may not have stopped: another run
/// holds it, or it comes from a release that takes no hold and something
/// may still delete the worktree (`unheld`). A record that may be one but
/// cannot be read refuses it too while a run holds it, while something
/// may still delete the worktree, or while no record that can be read
/// names the worktree, unless its name says it was written before the
/// entry's `trashed` time, by more than a clock can be off.
pub fn stopped_purges(
    common_dir: &Path,
    command_dir: &Path,
    aside: &Path,
    original: &Path,
    trashed: i64,
    listed: &[Entry],
) -> Result<Vec<(Item, Hold)>, Unsettled> {
    let listed: Vec<&Entry> = listed.iter().collect();
    let aside_now = git::canonical(aside).ok();
    let items = list(common_dir, command_dir, &listed, &[]).map_err(Unsettled::Error)?;
    let ours = |item: &Item| {
        item.interrupted()
            && item
                .record
                .as_ref()
                .is_ok_and(|r| r.aside == aside || aside_now.as_ref() == Some(&r.aside))
    };
    let named = items.iter().any(ours);
    let mut stopped = Vec::new();
    for item in &items {
        // A purge of the entry comes after the entry: a record named for an
        // earlier time, by more than a clock can be off, is another one's.
        let before = item
            .named_at(original)
            .is_some_and(|at| at < trashed.saturating_sub(NAMED_SLACK));
        if !item.may_be_unreadable_for(original) || before {
            continue;
        }
        let unreadable = |why: Unread| Unsettled::Unreadable {
            name: item.name.clone(),
            error: item.record.as_ref().err().cloned().unwrap_or_default(),
            why,
        };
        let info = item.dir.join(INFO);
        match hold::take(&info).map_err(Unsettled::Error)? {
            Taken::Busy => return Err(unreadable(Unread::Held(info))),
            Taken::Held(_) => {}
            // Forgotten since the listing.
            Taken::Gone => continue,
            Taken::Unsupported(why) => {
                return Err(Unsettled::Error(unsupported_note(&why, None)));
            }
        }
        // Its release, and so whether a free hold shows its run, is
        // unknown: a `git worktree remove` of the worktree refuses too, and
        // so does a record of another user, whose Git residue cannot see.
        let paths: Vec<&Path> = [Some(aside), aside_now.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let deleting = match crate::process::removing(&paths) {
            Ok(found) => found.first().map(|pid| {
                format!("process {pid}, a `git worktree remove` of the worktree, still runs")
            }),
            Err(e) => Some(e),
        };
        if let Some(why) = deleting.or_else(|| foreign(&info).err()) {
            return Err(unreadable(Unread::Deleting(why)));
        }
        if !named {
            return Err(unreadable(Unread::NotNamed(item.set_aside_line())));
        }
    }
    for item in items {
        if !ours(&item) {
            continue;
        }
        let info = item.dir.join(INFO);
        let what = format!("{}, the removal record of a purge of this entry", item.name);
        match hold::take(&info).map_err(Unsettled::Error)? {
            Taken::Held(hold) => {
                if let (Ok(record), Some(registered)) = (&item.record, &item.registered) {
                    if let Some(unsettled) = unheld_refusal(&item, record, registered, &what) {
                        return Err(unsettled);
                    }
                }
                stopped.push((item, hold));
            }
            Taken::Busy => {
                return Err(Unsettled::Held {
                    pid: item.record.as_ref().ok().and_then(|r| r.pid),
                    what,
                    info,
                });
            }
            // Forgotten since the listing: nothing of it is left to settle.
            Taken::Gone => {}
            Taken::Unsupported(why) => {
                return Err(Unsettled::Error(unsupported_note(&why, None)));
            }
        }
    }
    Ok(stopped)
}

/// Before a purge of the trash worktree at `path` writes its own record:
/// the records of earlier purges of the same entry that Git still lists
/// there (`stopped_purges`, each held), which a purge stopped before or
/// while its Git ran leaves, or one whose re-check failed after something
/// changed what the record keeps. A record whose worktree still holds all
/// it keeps (`still_holds`) is forgotten: the purge's own record keeps the
/// same, so one worktree has one record. Any other stays, and so does the
/// entry, at `entry`, until it is restored: the error says why, and names
/// that record.
pub fn settle_before_purge(
    stopped: Vec<(Item, Hold)>,
    path: &Path,
    entry: &Path,
) -> Result<(), (String, PathBuf)> {
    let restore = output::shell(entry);
    for (item, _held) in stopped {
        let lacks = item
            .record
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|record| still_holds(&item, record, path));
        if let Some(why) = lacks.unwrap_or_else(Some) {
            return Err((
                format!(
                    "the removal record {} of an earlier purge of this entry stays, since {why}, so the entry stays until you restore it: residue restore {restore}",
                    item.name
                ),
                item.dir,
            ));
        }
        if let Err(e) = forget(&item) {
            return Err((
                format!(
                    "could not forget {}, the removal record of an earlier purge of this entry ({e}), so the entry stays until a later remove run forgets it, or you restore it: residue restore {restore}",
                    item.name
                ),
                item.dir,
            ));
        }
    }
    Ok(())
}

/// What the worktree at `path`, still where the record's purge started,
/// lacks of what the record keeps, or `None`: its HEAD contains the
/// recorded HEAD, its HEAD reflog names every commit the record's refs
/// keep, as Git reads it (`unnamed_kept`), and its config.worktree is the
/// record's copy, when the record has one.
fn still_holds(item: &Item, record: &Record, path: &Path) -> Result<Option<String>, String> {
    let short = |c: &str| c[..c.len().min(12)].to_string();
    let now = git::commit_of(path, "HEAD")?.ok_or("it has no HEAD commit")?;
    if !git::is_ancestor(path, &record.head, &now)? {
        return Ok(Some(format!(
            "the worktree's HEAD {} does not contain the recorded HEAD {}",
            short(&now),
            short(&record.head)
        )));
    }
    let missing = unnamed_kept(record, path, &item.command_dir)?.len();
    if missing > 0 {
        return Ok(Some(format!(
            "the worktree's HEAD reflog does not name {missing} of the commits the record keeps"
        )));
    }
    let (config, _) = read_copies(item)?;
    if let Some(copy) = config.map_err(|e| format!("its copy of config.worktree: {e}"))? {
        let at = git::identity(path)?.git_dir.join(CONFIG);
        match fs::read(&at) {
            Ok(now) if now == copy => {}
            Ok(_) => {
                return Ok(Some(
                    "the worktree's config.worktree differs from the record's copy".into(),
                ));
            }
            Err(e) => return Ok(Some(format!("cannot read {}: {e}", at.display()))),
        }
    }
    Ok(None)
}

/// The names of the records of purges of the trash worktree at any of
/// `asides` that are not in `seen`, read now. Once a trash restore has
/// moved the worktree, a purge record it did not read under its hold
/// (`stopped_purges`) is one of a release that takes no hold, written
/// while the restore ran; its Git may have deleted part of the worktree
/// before the move. Its writer may still run, so the record stays.
pub fn raced_purges(
    common_dir: &Path,
    command_dir: &Path,
    asides: &[PathBuf],
    seen: &[String],
) -> Result<Vec<String>, String> {
    Ok(list(common_dir, command_dir, &[], &[])?
        .into_iter()
        .filter(|item| {
            item.record
                .as_ref()
                .is_ok_and(|r| r.reason == Reason::TrashPurge.code() && asides.contains(&r.aside))
                && !seen.contains(&item.name)
        })
        .map(|item| item.name)
        .collect())
}

/// [`of_repo`] for a repository given by its directories. `live` is the
/// linked worktrees Git lists outside the trash and the hidden names.
fn list(
    common_dir: &Path,
    command_dir: &Path,
    listed: &[&Entry],
    live: &[Entry],
) -> Result<Vec<Item>, String> {
    let root = root(common_dir);
    let listing = match fs::read_dir(&root) {
        Ok(l) => l,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", root.display())),
    };
    let mut items = Vec::new();
    for child in listing {
        let child = child.map_err(|e| format!("{}: {e}", root.display()))?;
        let dir = child.path();
        let meta = child
            .metadata()
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = child.file_name().to_string_lossy().into_owned();
        if !meta.is_dir() || name.starts_with('.') {
            continue;
        }
        let info = fs::read(dir.join(INFO));
        let id = fingerprint::entry("removed", &dir, info.as_deref().ok());
        let (record, incomplete) = match info {
            Ok(bytes) => (
                Record::parse(&bytes).map_err(|e| format!("{}: {e}", dir.join(INFO).display())),
                false,
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (
                Err(format!(
                    "{}: it has no info; the run that made it stopped before it deleted anything",
                    dir.display()
                )),
                true,
            ),
            Err(e) => (Err(format!("{}: {e}", dir.join(INFO).display())), false),
        };
        let registered = record.as_ref().ok().and_then(|r| {
            listed
                .iter()
                .find(|e| {
                    e.path == r.aside
                        || git::canonical(&e.path).ok().as_deref() == Some(r.aside.as_path())
                })
                .map(|e| (*e).clone())
        });
        let back = match (&registered, &record) {
            (None, Ok(r)) => is_back(r, live, command_dir),
            _ => Back::No,
        };
        items.push(Item {
            name,
            common_dir: common_dir.to_path_buf(),
            command_dir: command_dir.to_path_buf(),
            record,
            incomplete,
            worktree_config: dir.join(CONFIG).is_file(),
            head_reflog: dir.join(REFLOG).is_file(),
            modified: Some(meta.mtime()),
            registered,
            back,
            dir,
            id,
        });
    }
    items.sort_by_key(|i| {
        (
            i.record.as_ref().map_or(i64::MIN, |r| r.removed),
            i.name.clone(),
        )
    });
    Ok(items)
}

/// Git lists a worktree at the record's original path whose checkout is
/// there (the `.git` file that ties it to its Git directory), and whose
/// HEAD is the recorded HEAD, on any branch or detached, or contains it on
/// the recorded branch (detached when the record is). A restore that kept
/// its record, one stopped before it wrote its marker, a worktree added
/// there by hand, such as by the `BRANCH-restored` line a restore names
/// where it can hold no record, a move back whose record could not be
/// dropped, and a trash restore that lands during a purge all leave that.
/// A new worktree there at another commit on another branch is not one,
/// and neither is a locked one whose folder is gone, which Git never
/// marks prunable. One whose HEAD Git cannot compare with the recorded one
/// is `Back::Unknown`.
fn is_back(record: &Record, live: &[Entry], command_dir: &Path) -> Back {
    let Some(entry) = at_original(record, live) else {
        return Back::No;
    };
    if !fs::symlink_metadata(entry.path.join(".git")).is_ok_and(|m| m.is_file()) {
        return Back::No;
    }
    let Some(head) = entry.head.as_deref() else {
        return Back::No;
    };
    if head == record.head {
        return Back::Yes;
    }
    if entry.branch != record.branch {
        return Back::No;
    }
    match git::is_ancestor(command_dir, &record.head, head) {
        Ok(true) => Back::Yes,
        Ok(false) => Back::No,
        Err(e) => Back::Unknown(format!(
            "Git lists a worktree at {} on its branch, and residue could not compare its HEAD with the recorded {} ({e})",
            record.original.display(),
            &record.head[..record.head.len().min(12)]
        )),
    }
}

/// The worktree Git lists at the record's original path, not prunable.
fn at_original<'a>(record: &Record, live: &'a [Entry]) -> Option<&'a Entry> {
    live.iter().find(|e| {
        e.prunable.is_none()
            && (e.path == record.original
                || git::canonical(&e.path).ok().as_deref() == Some(record.original.as_path()))
    })
}

/// A record residue has written and not yet given up.
pub struct Written {
    pub name: String,
    pub dir: PathBuf,
    head: String,
    kept: Vec<String>,
    command_dir: PathBuf,
    /// The worktree's own Git directory, and the `config.worktree` and HEAD
    /// reflog the record copied from it.
    git_dir: Option<PathBuf>,
    /// That directory as it was when the record was written: its canonical
    /// path, device and inode, so [`Written::is_its_git_dir`] can tell it
    /// from a directory Git made again under the same name.
    git_dir_id: Option<(PathBuf, u64, u64)>,
    /// That directory had an index.
    index: bool,
    config: Option<Vec<u8>>,
    reflog: Option<Vec<u8>>,
    /// The hold on its `info`, taken before `info` was in place.
    hold: Hold,
}

impl Written {
    /// The hold on the record, which the `git worktree remove` that deletes
    /// its worktree inherits.
    pub fn hold(&self) -> &Hold {
        &self.hold
    }

    /// Drop the record and its refs: the worktree was not deleted after
    /// all. Only once [`Written::lacks`] finds nothing missing.
    pub fn undo(&self) -> Result<(), String> {
        drop_record(
            &self.command_dir,
            &self.name,
            &self.dir,
            &refs_of(&self.name, &self.head, &self.kept),
        )
    }

    /// Whether `git_dir`, the Git directory of a worktree Git lists, is
    /// this worktree's own: the same canonical path and the same directory
    /// (device and inode), not another worktree added at the same path, or
    /// one Git made again under the same name once this one's was deleted.
    /// When residue could not read it as the record was written, no
    /// directory is, so the record stays.
    pub fn is_its_git_dir(&self, git_dir: &Path) -> bool {
        self.git_dir_id.is_some() && dir_id(git_dir) == self.git_dir_id
    }

    /// What the worktree Git still lists, with `head` as its HEAD, lacks
    /// of what this record keeps: `None` when its HEAD contains the
    /// recorded HEAD, its HEAD reflog still begins with the one the record
    /// copied (so it names every commit the record's refs keep), its index
    /// is there if it was, and its `config.worktree` is the one the record
    /// copied. The record holds no copy of the index, but a worktree that
    /// lost it is not whole: it stays aside, and its restore says how to
    /// write the index again (`index_note`). A `git worktree
    /// remove` that failed part way may have deleted any of them, in the
    /// order the filesystem lists them, while Git still lists the worktree.
    pub fn lacks(&self, head: Option<&str>) -> Option<String> {
        let short = |c: &str| c[..c.len().min(12)].to_string();
        let head = head.filter(|h| is_object_id(h) && h.bytes().any(|b| b != b'0'));
        let Some(head) = head else {
            return Some(format!(
                "its HEAD is gone (Git reads no commit there, not the recorded {})",
                short(&self.head)
            ));
        };
        match git::is_ancestor(&self.command_dir, &self.head, head) {
            Ok(true) => {}
            Ok(false) => {
                return Some(format!(
                    "its HEAD is {}, which does not contain the recorded {}",
                    short(head),
                    short(&self.head)
                ));
            }
            Err(e) => return Some(format!("residue could not compare its HEAD ({e})")),
        }
        let Some(git_dir) = &self.git_dir else {
            return None;
        };
        let now = |file: &Path| read_if_present(Some(git_dir.join(file)));
        match (&self.reflog, now(&Path::new("logs").join("HEAD"))) {
            (None, Ok(_)) => {}
            (Some(saved), Ok(Some(now))) if now.starts_with(saved) => {}
            (Some(_), Ok(_)) => return Some("part of its HEAD reflog is gone".to_string()),
            (_, Err(e)) => return Some(format!("residue could not read its HEAD reflog ({e})")),
        }
        if self.index {
            match fs::symlink_metadata(git_dir.join("index")) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Some("its index is gone".to_string());
                }
                Err(e) => return Some(format!("residue could not check its index ({e})")),
            }
        }
        match now(Path::new(CONFIG)) {
            Ok(now) if now == self.config => None,
            Ok(_) => Some(format!("its {CONFIG} is gone or changed")),
            Err(e) => Some(format!("residue could not read its {CONFIG} ({e})")),
        }
    }
}

/// Write the record for the worktree at `record.aside`, about to be
/// deleted, then its refs. If any of it fails, none of it is left, and the
/// caller must not delete. `git_dir` is the worktree's own Git directory,
/// whose `config.worktree` and HEAD reflog the record keeps copies of.
pub fn create(
    common_dir: &Path,
    command_dir: &Path,
    record: &Record,
    git_dir: Option<&Path>,
) -> Result<Written, String> {
    if !is_object_id(&record.head) {
        return Err(format!(
            "{:?} is not a commit residue can keep",
            record.head
        ));
    }
    let config = read_if_present(git_dir.map(|d| d.join(CONFIG)))
        .map_err(|e| format!("cannot read its config.worktree: {e}"))?;
    // Git reads a `logs/HEAD` file only while the repository keeps its
    // refs in files. In reftable one is not the worktree's reflog (the
    // restore of residue 0.0.7 and earlier wrote one there after a `git
    // refs migrate`), so it is not copied, and `reflog_only` asks Git.
    let reftable = git::is_reftable(command_dir).map_err(|e| {
        format!("cannot tell whether the repository keeps its refs in reftable: {e}")
    })?;
    let reflog = if reftable {
        None
    } else {
        read_if_present(git_dir.map(|d| d.join("logs").join("HEAD")))
            .map_err(|e| format!("cannot read its HEAD reflog: {e}"))?
    };
    let mut record = record.clone();
    record.kept = reflog_only(reflog.as_deref(), &record.aside, command_dir)
        .map_err(|e| format!("cannot tell which commits only its reflog holds: {e}"))?;
    let root = root(common_dir);
    let new_root = match fs::create_dir(&root) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(format!("cannot create {}: {e}", root.display())),
    };
    let trash_root = crate::trash::root(common_dir);
    let base = entry_base(&record.original);
    let stamp = output::stamp(record.removed);
    for n in 1..100 {
        let name = if n == 1 {
            format!("{base}-{stamp}")
        } else {
            format!("{base}-{stamp}-{n}")
        };
        // Trash entries and records share one list in `residue restore`,
        // so a name either already has is skipped.
        if fs::symlink_metadata(trash_root.join(&name)).is_ok() {
            continue;
        }
        let dir = root.join(&name);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
        }
        let give_up = |message: String| match discard(&dir) {
            Ok(()) => message,
            Err(more) => format!("{message}; {more}"),
        };
        let mut files = Vec::new();
        if let Some(bytes) = &config {
            files.push((CONFIG, bytes.as_slice()));
        }
        if let Some(bytes) = &reflog {
            files.push((REFLOG, bytes.as_slice()));
        }
        let info = record.render(&name);
        let hold = match write_entry(&dir, &files, &info) {
            Ok(hold) => hold,
            Err(e) => return Err(give_up(e)),
        };
        let synced = sync_dir(&root).and_then(|()| {
            if new_root {
                sync_dir(common_dir)
            } else {
                Ok(())
            }
        });
        if let Err(e) = synced {
            return Err(give_up(e));
        }
        let refs = refs_of(&name, &record.head, &record.kept);
        let creates: Vec<RefChange> = refs
            .iter()
            .map(|(name, commit)| RefChange::Create { name, commit })
            .collect();
        if let Err(e) = git::change_refs(command_dir, &creates) {
            return Err(give_up(format!("cannot create its refs: {e}")));
        }
        return Ok(Written {
            name,
            dir,
            head: record.head.clone(),
            kept: record.kept.clone(),
            command_dir: command_dir.to_path_buf(),
            git_dir: git_dir.map(Path::to_path_buf),
            git_dir_id: git_dir.and_then(dir_id),
            index: git_dir.is_some_and(|d| fs::symlink_metadata(d.join("index")).is_ok()),
            config,
            reflog,
            hold,
        });
    }
    Err(format!(
        "{}: too many records named {base}-{stamp}",
        root.display()
    ))
}

/// A directory's canonical path, device and inode; `None` when it cannot
/// be read.
fn dir_id(dir: &Path) -> Option<(PathBuf, u64, u64)> {
    let path = fs::canonicalize(dir).ok()?;
    let meta = fs::metadata(&path).ok()?;
    meta.is_dir().then(|| (path, meta.dev(), meta.ino()))
}

/// The file at `path`, `None` when there is no path or no file.
fn read_if_present(path: Option<PathBuf>) -> io::Result<Option<Vec<u8>>> {
    match path.map(fs::read) {
        None => Ok(None),
        Some(Ok(bytes)) => Ok(Some(bytes)),
        Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Some(Err(e)) => Err(e),
    }
}

/// The copies first, then `info`, written as `info.new` and renamed, then
/// the directory synced: an entry with `info` has every copy it names.
/// `info.new` is held before the rename, so `info` is held from the moment
/// it is there.
fn write_entry(dir: &Path, files: &[(&str, &[u8])], info: &[u8]) -> Result<Hold, String> {
    for (file, bytes) in files {
        write_new(&dir.join(file), bytes)
            .map_err(|e| format!("cannot write {}: {e}", dir.join(file).display()))?;
    }
    let staged = dir.join(INFO_NEW);
    let hold = hold::write_new(&staged, info)
        .map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
    fs::rename(&staged, dir.join(INFO))
        .map_err(|e| format!("cannot rename {}: {e}", staged.display()))?;
    sync_dir(dir)?;
    Ok(hold)
}

/// The commits the worktree's HEAD reflog holds that no ref outside
/// `refs/residue/` and no worktree's HEAD reaches (the record's own ref
/// keeps its HEAD). Another reflog naming one does not count: a branch's
/// reflog goes with `git branch -D`, and any reflog expires. Of those, the
/// fewest that reach them all, so one ref each keeps every one. `reflog`
/// is the bytes of its `logs/HEAD`, where Git keeps it in a files
/// repository; without them Git says which commits it names
/// (`git_head_reflog`). `main` is where Git commands for the repository
/// run.
fn reflog_only(reflog: Option<&[u8]>, worktree: &Path, main: &Path) -> Result<Vec<String>, String> {
    let named = match reflog {
        Some(bytes) => reflog_ids(bytes),
        None => git_head_reflog(worktree, main)?,
    };
    let named: Vec<&str> = named.iter().map(String::as_str).collect();
    let lost = git::unreferenced(main, &named)?;
    git::independent(main, &lost)
}

/// The commits Git reads the HEAD reflog of the worktree at `worktree` as
/// naming, once each: each entry's new value and the oldest entry's old
/// value. Git reads no other old value on its own, and one after a gap in
/// the middle (an entry expired, or deleted with `git reflog delete`)
/// names a commit no entry names as new. So every commit the reflogs Git
/// reads at `worktree` name that the ones it reads at `main` do not counts
/// too: only the worktree's own reflog names it. Left out is such an old
/// value that another reflog also names, a branch's say. At a removal,
/// that reflog keeps it after the removal, and a later `git branch -D` can
/// lose it; at a restore, the record that pins it stays.
fn git_head_reflog(worktree: &Path, main: &Path) -> Result<Vec<String>, String> {
    let shared: HashSet<String> = git::reflogs_commits(main)?.into_iter().collect();
    let own = git::reflogs_commits(worktree)?
        .into_iter()
        .filter(|id| !shared.contains(id));
    let mut seen = HashSet::new();
    Ok(git::head_reflog_commits(worktree)?
        .into_iter()
        .chain(own)
        .filter(|id| seen.insert(id.clone()))
        .collect())
}

/// Every commit a reflog's bytes name, the old and the new value of each
/// entry, once each, in lowercase; never the null id. A line Git would not
/// parse names nothing, as in `git reflog`.
fn reflog_ids(bytes: &[u8]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for line in bytes.split(|&b| b == b'\n') {
        for field in line.split(|&b| b == b' ').take(2) {
            let Ok(id) = std::str::from_utf8(field) else {
                break;
            };
            if !is_object_id(id) {
                break;
            }
            let id = id.to_ascii_lowercase();
            if id.bytes().any(|b| b != b'0') && seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }
    ids
}

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::File::create_new(path).and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
}

/// Sync a directory, so entries just made or renamed in it survive a power
/// loss. A filesystem that cannot sync a directory (some network and FUSE
/// ones answer EINVAL, ENOTSUP or EBADF) is not an error: POSIX leaves a
/// directory's sync to the filesystem, and the files themselves are synced.
pub(crate) fn sync_dir(dir: &Path) -> Result<(), String> {
    match fs::File::open(dir).and_then(|f| f.sync_all()) {
        Ok(()) => Ok(()),
        Err(e)
            if e.raw_os_error().is_some_and(|code| {
                [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP, libc::EBADF].contains(&code)
            }) =>
        {
            Ok(())
        }
        Err(e) => Err(format!("cannot sync {}: {e}", dir.display())),
    }
}

/// Remove an entry's files and its now empty directory, and the records
/// directory once it is empty. Never recursive: anything else left in the
/// entry stays, and the error says so. An entry another run discarded
/// first is gone, as this one wanted.
fn discard(dir: &Path) -> Result<(), String> {
    for file in [INFO, INFO_NEW, CONFIG, REFLOG] {
        match fs::remove_file(dir.join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove {}: {e}", dir.join(file).display())),
        }
    }
    match fs::remove_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove {}: {e}", dir.display())),
    }
    if let Some(root) = dir.parent() {
        crate::trash::remove_dir_if_empty(root)
            .map_err(|e| format!("cannot remove {}: {e}", root.display()))?;
    }
    Ok(())
}

/// The refs first, then the files: a record left without its refs is
/// forgotten again later, a ref left without its record never would be.
/// `expected` is every ref the record has, with its commit. Each of them
/// that exists is deleted, in one transaction, only while it points at its
/// commit; a ref under the record's names that it does not list, or one
/// that points elsewhere, keeps the record and its refs as they are.
fn drop_record(
    command_dir: &Path,
    name: &str,
    dir: &Path,
    expected: &[(String, String)],
) -> Result<(), String> {
    let patterns = [
        format!("{REF_PREFIX}{name}"),
        format!("{REFLOG_PREFIX}{name}"),
    ];
    let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
    let found = git::refs_named(command_dir, &patterns)?;
    let mut deletes = Vec::new();
    for (ref_name, now) in &found {
        match expected.iter().find(|(r, _)| r == ref_name) {
            Some((_, commit)) if commit == now => deletes.push(RefChange::Delete {
                name: ref_name,
                commit,
            }),
            Some((_, commit)) => {
                return Err(format!(
                    "the ref {ref_name} points at {now}, not at the recorded {commit}, so residue leaves it and the record"
                ));
            }
            None => {
                return Err(format!(
                    "the ref {ref_name} is not one the record names, so residue leaves it and the record"
                ));
            }
        }
    }
    git::change_refs(command_dir, &deletes).map_err(|e| format!("cannot delete its refs: {e}"))?;
    discard(dir)
}

/// Forget one record: its refs, then its files. An unreadable record is
/// never forgotten, since residue cannot tell which refs are its own; an
/// entry with no `info` has no refs of its own, so it goes only while it
/// has none.
pub fn forget(item: &Item) -> Result<(), String> {
    match &item.record {
        Ok(r) => drop_record(
            &item.command_dir,
            &item.name,
            &item.dir,
            &refs_of(&item.name, &r.head, &r.kept),
        ),
        Err(_) if item.incomplete => drop_record(&item.command_dir, &item.name, &item.dir, &[]),
        Err(e) => Err(e.clone()),
    }
}

/// What [`forget_expired`] did.
pub enum Forgot {
    /// The record is gone, forgotten by this run or since the listing.
    Yes,
    /// A run holds it now, a restore, so it stays until the next run.
    Busy,
    /// It stays on every run, and why: its filesystem gives no hold
    /// (`unforgettable`).
    Kept(String),
}

/// Forget a record a remove run found expired, holding it first: a run
/// that holds it now, a restore, is using it, so it stays. One forgotten
/// since the listing is gone, as this run wanted. An entry with no `info`
/// has nothing to hold.
pub fn forget_expired(item: &Item) -> Result<Forgot, String> {
    if item.incomplete {
        return forget(item).map(|()| Forgot::Yes);
    }
    match hold::take(&item.dir.join(INFO))? {
        Taken::Held(_hold) => forget(item).map(|()| Forgot::Yes),
        Taken::Busy => Ok(Forgot::Busy),
        Taken::Gone => Ok(Forgot::Yes),
        Taken::Unsupported(why) => Ok(Forgot::Kept(kept_unheld(item, &why))),
    }
}

/// Why the record of `item` stays on every run, when its filesystem gives
/// no hold: the plan says so, by an instant hold as `scan` takes, so the
/// plan, the prompt, the run and its JSON agree. `None` otherwise, a
/// failed open included, which the forget itself then reports.
pub fn unforgettable(item: &Item) -> Option<String> {
    if item.incomplete {
        return None;
    }
    match hold::take(&item.dir.join(INFO)) {
        Ok(Taken::Unsupported(why)) => Some(kept_unheld(item, &why)),
        _ => None,
    }
}

fn kept_unheld(item: &Item, why: &str) -> String {
    let kept = format!(
        "{why}, so residue cannot tell whether a restore uses it, and keeps it, with the commits it keeps"
    );
    if item.registered.is_none() && item.back == Back::Yes {
        return format!("{kept}; Git lists its worktree at its original path again");
    }
    format!(
        "{kept}; `residue restore {}` names the steps that bring its worktree back by hand",
        output::word(&item.name)
    )
}

// ---------------------------------------------------------------- restore

/// How `restore` brought a worktree back.
pub enum How {
    /// An interrupted removal: moved back from where the run left it.
    MovedBack(PathBuf),
    /// The recorded branch, at the recorded HEAD or ahead of it.
    Branch {
        tip: String,
    },
    /// The branch was gone; it was created again at the recorded HEAD.
    Recreated,
    /// The branch no longer contains the recorded HEAD, or a worktree has
    /// it checked out (`elsewhere` says where). It was left alone, and a
    /// new branch was made at the recorded HEAD.
    NewBranch {
        name: String,
        tip: String,
        elsewhere: Option<String>,
    },
    Detached,
    /// Git listed it at its path already, on its branch or detached as the
    /// record is, at `tip`: the restore put back only what it lacked.
    AlreadyBack {
        tip: String,
    },
}

impl How {
    pub fn code(&self) -> &'static str {
        match self {
            How::MovedBack(_) => "moved_back",
            How::Branch { .. } => "checked_out_branch",
            How::Recreated => "recreated_branch",
            How::NewBranch { .. } => "new_branch",
            How::Detached => "detached",
            How::AlreadyBack { .. } => "already_back",
        }
    }
}

pub struct Restored {
    pub path: PathBuf,
    pub how: How,
    /// The branch the restored worktree is on, full ref name.
    pub branch: Option<String>,
    /// Things the user should know that do not make the restore fail.
    pub notes: Vec<String>,
}

/// Why a restore, of a removal record or a trash entry, brought nothing
/// back.
pub enum NotRestored {
    /// It refused, or a step failed: why, and what it changed.
    Refused(String),
    /// Under `restore --expect ID`, the entry it holds is not ID. It
    /// changed nothing.
    Other,
}

impl From<String> for NotRestored {
    fn from(why: String) -> NotRestored {
        NotRestored::Refused(why)
    }
}

impl NotRestored {
    /// Under `--expect ID`, the entry at `dir`, whose `info` is at `info`,
    /// read now that the restore holds it: refused when it is not ID.
    pub fn check(expect: Option<&str>, kind: &str, dir: &Path, info: &Path) -> Result<(), Self> {
        let Some(want) = expect else {
            return Ok(());
        };
        if fingerprint::entry_now(kind, dir, info)? == want {
            Ok(())
        } else {
            Err(NotRestored::Other)
        }
    }
}

/// Where a worktree Git lists is, for a message: in the trash, left by an
/// interrupted removal, the main checkout, or a path.
fn whereabouts(entry: &Entry, main: &Entry, trash_root: &Path) -> String {
    let path = git::canonical(&entry.path).unwrap_or_else(|_| entry.path.clone());
    if path.starts_with(trash_root) {
        let name = path
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!(
            "in the trash entry {} (`residue restore {}` puts it back)",
            output::clean(&name),
            output::word(&name)
        )
    } else if crate::remove::original_of(&path).is_some() {
        format!(
            "in a worktree an interrupted removal left at {}",
            path.display()
        )
    } else if entry.path == main.path {
        format!("in the main checkout at {}", path.display())
    } else {
        format!("in the worktree at {}", path.display())
    }
}

/// The restore found the record gone since its listing.
const TAKEN: &str = "another residue run forgot or restored this removal record after this restore listed it, so residue changed nothing; `residue restore` lists what is left";

/// Take the hold on a record before a restore acts on it. Held, it means
/// that no removal, no `git worktree remove` one started and no other
/// restore is acting on the record or its worktree, unless the record
/// comes from a release that takes no hold: `restore` then asks `unheld`
/// too, while Git lists the worktree where its deletion started.
fn take(item: &Item) -> Result<Hold, String> {
    let info = item.dir.join(INFO);
    match hold::take(&info)? {
        Taken::Held(hold) => Ok(hold),
        Taken::Busy => Err(busy_note(
            item.record.as_ref().ok(),
            &info,
            "this removal record",
        )),
        Taken::Gone => Err(TAKEN.into()),
        Taken::Unsupported(why) => Err(unsupported_note(&why, manual(item))),
    }
}

/// What to do by hand with the worktree of `item`'s record where the
/// filesystem gives no hold: move it back while Git lists it where its
/// deletion started; nothing when Git lists it at its original path
/// again; otherwise recreate it, the way `recreate` would.
fn manual(item: &Item) -> Option<Manual> {
    let record = item.record.as_ref().ok()?;
    if let Some(registered) = &item.registered {
        return Some(Manual::MoveBack(move_back_line(
            &item.command_dir,
            registered,
            &record.original,
        )));
    }
    // Its worktree is gone, and residue cannot forget the record there
    // either (`forget_expired`), so its refs stay.
    Some(match &item.back {
        Back::Yes => Manual::Back,
        Back::Unknown(e) => Manual::Recreate(Err(output::clean(e))),
        Back::No => Manual::Recreate(recreate_line(item, record)),
    })
}

/// What a restore where the filesystem gives no hold names to do by hand.
pub enum Manual {
    /// The line that moves the worktree, which Git still lists, back.
    MoveBack(String),
    /// The line that recreates the worktree of a record Git no longer
    /// lists, whose refs still keep its commits, or why none can.
    Recreate(Result<String, String>),
    /// Git lists the worktree at its original path again.
    Back,
}

/// What a restore says where the filesystem gives no hold: it changed
/// nothing, and `manual`, when there is one, says what to do by hand.
/// That filesystem has only entries a release that takes no hold made,
/// and only its runs, or a hand, may still act on them.
pub fn unsupported_note(why: &str, manual: Option<Manual>) -> String {
    const STAYS: &str = "residue cannot forget it there either";
    let manual = match manual {
        None => String::new(),
        Some(Manual::MoveBack(line)) => format!(
            "; once no residue run acts on it, this moves the worktree back by hand: {line}"
        ),
        Some(Manual::Recreate(Ok(line))) => format!(
            "; once no residue run acts on it, this recreates the worktree by hand: {line}; the record still keeps the commits of its HEAD reflog, and {STAYS}"
        ),
        Some(Manual::Recreate(Err(e))) => format!(
            "; residue names no line that recreates the worktree ({e}); the record still keeps the commits of its HEAD reflog, and {STAYS}"
        ),
        Some(Manual::Back) => format!(
            "; Git lists its worktree at its original path again, and the record stays, since {STAYS}"
        ),
    };
    format!("{why}, so residue cannot hold it against another run, and changed nothing{manual}")
}

/// What may still act on a record's worktree now, as far as residue can
/// tell (`Item::activity`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Activity {
    /// No run holds the record, and nothing else shows that its removal
    /// still runs.
    Idle,
    /// A run, its Git, or a program that Git started holds it.
    Held,
    /// No one holds it, but it comes from a release that takes no hold, Git
    /// still lists its worktree where the deletion started, and something
    /// may still delete it (`unheld`).
    Unheld(Unheld),
}

impl Item {
    /// Whether a run may still act on this record's worktree now. It takes
    /// the hold and lets go of it at once (`hold::held`). `snapshot` is the
    /// command's activity probe, when it took one.
    pub fn activity(&self, snapshot: Option<&Snapshot>) -> Result<Activity, String> {
        if hold::held(&self.dir.join(INFO))? {
            return Ok(Activity::Held);
        }
        let (Ok(record), Some(registered)) = (&self.record, &self.registered) else {
            return Ok(Activity::Idle);
        };
        Ok(unheld(self, record, registered, snapshot)?.map_or(Activity::Idle, Activity::Unheld))
    }
}

/// The `info` of the record entry `dir`.
pub fn info_of(dir: &Path) -> PathBuf {
    dir.join(INFO)
}

/// Bring back the worktree one record describes, at `to` or where it was,
/// then drop the record and its refs once the worktree holds all they
/// held. It holds the record first, then reads Git's list of worktrees
/// again: the listing that chose the record may have come before its
/// removal ended. While Git lists the worktree where its deletion started,
/// a record from a release that takes no hold also refuses while anything
/// may still delete it (`unheld`). A worktree that is back at its path already
/// (`Back::Yes`: a restore that stopped after `git worktree add`, one that
/// kept its record, or a `git worktree add` by hand) gets the steps it may
/// lack: the restored mark, its config.worktree, the recorded HEAD reflog
/// in front of its own. Each leaves alone what is already there, and the
/// record goes only once the worktree holds what it keeps, as for any
/// restore. Under `--expect ID` (`expect`), it reads the record again once
/// it holds it, and refuses, having changed nothing, when it is not ID.
pub fn restore(
    item: &Item,
    to: Option<&Path>,
    expect: Option<&str>,
) -> Result<Restored, NotRestored> {
    let record = item.record.as_ref().map_err(|e| e.clone())?;
    let hold = take(item)?;
    NotRestored::check(expect, "removed", &item.dir, &item.dir.join(INFO))?;
    let listed = git::worktrees(&item.command_dir)?;
    let registered = listed
        .iter()
        .find(|e| {
            e.path == record.aside
                || git::canonical(&e.path).ok().as_deref() == Some(record.aside.as_path())
        })
        .cloned();
    if let Some(registered) = &registered {
        refuse_unheld(item, record, registered, "this removal record")?;
    }
    let purged = registered
        .as_ref()
        .is_some_and(|e| purged_to_git_dir(item, record, e));
    let standing = match registered {
        Some(_) => Back::No,
        None => is_back(record, &listed, &item.command_dir),
    };
    let back = to.is_none() && standing == Back::Yes;
    if let (None, Back::Unknown(e)) = (to, &standing) {
        return Err(format!(
            "{e}, so it cannot tell whether that worktree holds what this record keeps; move that worktree away, or restore somewhere else with --to PATH"
        ).into());
    }
    let dest = to.unwrap_or(&record.original);
    match fs::symlink_metadata(dest) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        _ if back => {}
        Ok(_) => {
            return Err(format!(
                "{} exists; move it away first, or restore somewhere else with --to PATH",
                dest.display()
            )
            .into());
        }
        Err(e) => return Err(format!("cannot check {}: {e}", dest.display()).into()),
    }
    // Read before anything changes: another run may forget this record
    // meanwhile, and nothing else holds the copies.
    let copies = read_copies(item)?;
    let mut restored = match &registered {
        _ if back => {
            let mut restored = already_back(record, &listed)?;
            // A purge's Git may have deleted part of it before it came
            // back: moved back by hand, or by a trash restore it raced.
            if record.reason == Reason::TrashPurge.code() {
                restored.notes.push(PURGE_STOPPED.into());
            }
            restored
        }
        Some(registered) if purged => {
            let note = clear_purged(item, registered, &hold)?;
            // Read again: the list before named the trash worktree, on
            // the branch the recreated one checks out.
            let mut restored = recreate(item, record, dest, &git::worktrees(&item.command_dir)?)?;
            restored.notes.extend(note);
            restored
        }
        Some(registered) => move_back(item, record, registered, dest)?,
        None => {
            let mut restored = recreate(item, record, dest, &listed)?;
            // A removal whose Git stopped partway, once it no longer listed
            // the worktree, leaves what it did not delete aside, unlisted.
            if record.aside != dest && fs::symlink_metadata(&record.aside).is_ok() {
                restored.notes.push(format!(
                    "what its removal did not delete is still at {}; residue leaves it there for you to look through and delete",
                    record.aside.display()
                ));
            }
            restored
        }
    };
    let recreated = (registered.is_none() || purged) && !back;
    finish(item, record, &mut restored, &copies, recreated, back);
    Ok(restored)
}

/// A purge of a trash worktree that stopped once Git had deleted its
/// files: Git still lists it in the trash, but nothing is at its path, so
/// the trash restore has nothing to move back, and this record is how it
/// comes back. The trash entry may be gone too, with no restore of its
/// own to send the user to.
fn purged_to_git_dir(item: &Item, record: &Record, registered: &Entry) -> bool {
    let trash = crate::trash::root(&item.common_dir);
    (record.aside.starts_with(&trash) || registered.path.starts_with(&trash))
        && fs::symlink_metadata(&registered.path)
            .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
}

/// Clear what Git keeps of the trash worktree a purge stopped deleting
/// (`purged_to_git_dir`), so that the restore can recreate it: under the
/// record's hold, `record_hold`, and its trash entry's, `git worktree
/// remove` finds no files at its path and deletes only its own directory
/// for it, as the purge's Git would have; then the entry, which holds only
/// its `info`, goes. A lock residue's trash put on it is lifted first, and
/// put back if Git refuses; anyone else's keeps it. Returns a note when
/// the entry stays.
fn clear_purged(
    item: &Item,
    registered: &Entry,
    record_hold: &Hold,
) -> Result<Option<String>, String> {
    let main = &item.command_dir;
    let entry = registered
        .path
        .parent()
        .ok_or("the trash worktree has no entry")?;
    let info = crate::trash::info(entry);
    let entry_hold = match hold::take(&info)? {
        Taken::Held(h) => Some(h),
        Taken::Busy => {
            return Err(format!(
                "a residue run, or the `git worktree remove` it started, is purging or restoring its trash entry now (`lsof {}` names it); run this restore again once it ends",
                output::shell(&info)
            ));
        }
        Taken::Gone => None,
        Taken::Unsupported(why) => return Err(unsupported_note(&why, None)),
    };
    let ours = crate::trash::locked_by_residue(registered);
    match registered.locked.as_deref() {
        None => {}
        Some(_) if ours => git::unlock_worktree(main, &registered.path)?,
        Some(reason) => {
            return Err(format!(
                "Git still lists it in the trash, locked by someone else ({reason}); residue leaves that lock alone"
            ));
        }
    }
    let mut holds = vec![record_hold];
    holds.extend(entry_hold.as_ref());
    if let Err(e) = git::remove_worktree(main, &registered.path, &holds) {
        let relocked = match ours {
            true => git::lock_worktree(main, &registered.path, crate::trash::LOCK)
                .err()
                .map_or(String::new(), |r| {
                    format!("; and could not lock it again: {r}")
                }),
            false => String::new(),
        };
        return Err(format!(
            "its purge from the trash stopped after Git deleted its files, and residue could not clear what Git kept of it: {e}{relocked}"
        ));
    }
    if entry_hold.is_none() {
        return Ok(None);
    }
    Ok(crate::trash::discard(entry).err().map(|stays| stays.note()))
}

/// After `residue restore ENTRY` moved a trash entry's worktree to `path`:
/// `item` is the record of a purge of that entry that stopped partway,
/// which may have deleted its HEAD reflog or config.worktree, held by the
/// caller (`stopped_purges`). What it lacks of them goes back, then the
/// record goes once the worktree holds everything it keeps; otherwise the
/// record stays. Returns the notes.
pub fn after_trash_restore(item: &Item, path: &Path) -> Vec<String> {
    let Ok(record) = &item.record else {
        return Vec::new();
    };
    let copies = match read_copies(item) {
        Ok(copies) => copies,
        Err(e) => {
            return match fs::symlink_metadata(item.dir.join(INFO)) {
                // Its `info` is gone: deleted by hand, or forgotten by a
                // residue that takes no hold. Nothing of it is left to put
                // back.
                Err(gone) if gone.kind() == io::ErrorKind::NotFound => Vec::new(),
                _ => vec![format!(
                    "the removal record {} stays, since residue could not read its copies ({e})",
                    item.name
                )],
            };
        }
    };
    let mut restored = Restored {
        path: path.to_path_buf(),
        how: How::MovedBack(record.aside.clone()),
        branch: None,
        notes: Vec::new(),
    };
    // The trash restore marked it already.
    finish(item, record, &mut restored, &copies, false, true);
    restored.notes
}

/// The rest of a restore, once the worktree is at `restored.path`. A
/// worktree moved back kept its own Git directory, though a removal that
/// failed part way may have deleted its reflog or config.worktree from it.
/// A `recreated` one has a new one. One already back has either: only what
/// it lacks goes in. Either way, until the recorded config.worktree and
/// reflog are in it, and it holds the commits the refs keep, the record
/// stays. With `keep_mark`, a restored mark already there stays.
fn finish(
    item: &Item,
    record: &Record,
    restored: &mut Restored,
    (config, reflog): &(Saved, Saved),
    recreated: bool,
    keep_mark: bool,
) {
    let mut keep_record = false;
    match git::identity(&restored.path) {
        Ok(id) => {
            // A mark already there stays: it says when it was restored.
            let marking = if keep_mark && matches!(marked(&id.git_dir), Ok(Some(_))) {
                Ok(())
            } else {
                mark(&id.git_dir, &format!("removal record {}", item.name))
            };
            if let Err(e) = marking {
                restored.notes.push(format!(
                    "could not mark it as restored ({e}); `residue watch` may remove it again once its upstream is gone: lock it with `git worktree lock` to keep it"
                ));
            }
            if !recreated {
                restored.notes.extend(index_note(&id.git_dir));
            }
            // Git reads a `logs/HEAD` file only while the repository keeps
            // its refs in files. How it keeps them now decides, not how it
            // kept them when the record was written: `git refs migrate`
            // can change it in between.
            let reflog = match git::is_reftable(&restored.path) {
                Ok(false) if recreated => restore_reflog(reflog, &id.git_dir),
                Ok(false) => reflog_if_lost(reflog, &id.git_dir),
                Ok(true) => reftable_reflog(item, recreated),
                Err(e) => Err(format!(
                    "its HEAD reflog was not put back: residue could not tell whether the repository keeps its refs in reftable, where Git does not read the file it would write ({e})"
                )),
            };
            for step in [restore_config(config, &id.git_dir), reflog] {
                match step {
                    Ok(None) => {}
                    Ok(Some(note)) => restored.notes.push(note),
                    Err(note) => {
                        restored.notes.push(note);
                        keep_record = true;
                    }
                }
            }
        }
        Err(e) => {
            restored.notes.push(format!(
                "could not find its Git directory to mark it as restored ({e}); `residue watch` may remove it again once its upstream is gone: lock it with `git worktree lock` to keep it"
            ));
            keep_record = item.worktree_config || item.head_reflog;
        }
    }
    if !keep_record {
        if let Err(note) = holds(record, &restored.path, &item.command_dir, &mut restored.how) {
            restored.notes.push(note);
            keep_record = true;
        }
    }
    if keep_record {
        // Said only from Git's refs as they are now: another run may have
        // forgotten the record while this one restored it.
        let note = match refs_gone(item) {
            Ok(false) => format!(
                "the removal record stays at {}, with its refs, so nothing it keeps is lost; a remove run forgets it after --keep-removed",
                item.dir.display()
            ),
            Ok(true) => taken_note(item, record, &restored.path),
            Err(e) => format!(
                "the removal record at {} should stay, with its refs, but residue could not read them to check ({e})",
                item.dir.display()
            ),
        };
        restored.notes.push(note);
    } else if let Err(e) = forget(item) {
        // Gone already: another run forgot it, and the worktree holds
        // everything it kept.
        if fs::symlink_metadata(&item.dir).is_ok() {
            restored.notes.push(format!(
                "could not drop the removal record ({e}); a remove run forgets it after --keep-removed"
            ));
        }
    }
}

/// What a restore says of a worktree it put back whose Git directory has
/// no index: a removal that stopped partway can delete it, and then `git
/// status` lists every tracked file as deleted, and `git restore .`, which
/// copies from the index, brings none back.
fn index_note(git_dir: &Path) -> Option<String> {
    match fs::symlink_metadata(git_dir.join("index")) {
        Ok(_) => None,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(
            "it has no index, which a removal stopped partway can delete: run `git reset` in it first, which writes the index again from HEAD and changes no file".into(),
        ),
        Err(e) => Some(format!("residue could not check its index ({e})")),
    }
}

/// A recorded copy, read before a restore changes anything: `None` when
/// the record has none.
type Saved = Result<Option<Vec<u8>>, String>;

/// The record's `config.worktree` and HEAD reflog copies, the ones the
/// listing saw, read before the restore changes anything. `info`, or a
/// copy the listing saw, that is gone now means another run forgot or
/// restored the record after this one listed it: the restore stops, having
/// changed nothing. A copy that cannot be read for another reason is that
/// step's error, which keeps the record, as before.
fn read_copies(item: &Item) -> Result<(Saved, Saved), String> {
    let taken = || TAKEN.to_string();
    match fs::symlink_metadata(item.dir.join(INFO)) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(taken()),
        Err(e) => {
            return Err(format!(
                "cannot check {}: {e}",
                item.dir.join(INFO).display()
            ));
        }
    }
    let read = |file: &str, listed: bool| -> Result<Saved, String> {
        if !listed {
            return Ok(Ok(None));
        }
        match fs::read(item.dir.join(file)) {
            Ok(bytes) => Ok(Ok(Some(bytes))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(taken()),
            Err(e) => Ok(Err(e.to_string())),
        }
    };
    Ok((
        read(CONFIG, item.worktree_config)?,
        read(REFLOG, item.head_reflog)?,
    ))
}

/// No ref under the record's names exists: another run forgot it. A
/// forget deletes them all in one transaction, before the files.
fn refs_gone(item: &Item) -> Result<bool, String> {
    let patterns = [
        format!("{REF_PREFIX}{}", item.name),
        format!("{REFLOG_PREFIX}{}", item.name),
    ];
    let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
    Ok(git::refs_named(&item.command_dir, &patterns)?.is_empty())
}

/// What a restore says when the record it would keep is gone: another
/// run forgot it meanwhile, so its refs no longer pin anything, and the
/// commits only the old reflog held that the restored worktree's reflog
/// does not name can go with `git gc`.
fn taken_note(item: &Item, record: &Record, path: &Path) -> String {
    let head = format!(
        "another residue run forgot the removal record {} while this restore ran, so its refs are gone",
        item.name
    );
    match unnamed_kept(record, path, &item.command_dir) {
        Ok(lost) if lost.is_empty() => format!("{head}; the worktree holds every commit they kept"),
        Ok(lost) => format!(
            "{head}: {} {} only the old reflog held {} no longer kept, and `git gc` can drop {}: {}; `git branch NAME COMMIT` keeps one",
            lost.len(),
            if lost.len() == 1 { "commit" } else { "commits" },
            if lost.len() == 1 { "is" } else { "are" },
            if lost.len() == 1 { "it" } else { "them" },
            lost.join(", ")
        ),
        Err(e) => format!("{head}, and residue could not check what the worktree holds ({e})"),
    }
}

/// The commits the record keeps that the HEAD reflog of the worktree at
/// `path` does not name, as Git reads it (`git_head_reflog`).
fn unnamed_kept(record: &Record, path: &Path, main: &Path) -> Result<Vec<String>, String> {
    if record.kept.is_empty() {
        return Ok(Vec::new());
    }
    let named: HashSet<String> = git_head_reflog(path, main)?.into_iter().collect();
    Ok(record
        .kept
        .iter()
        .filter(|k| !named.contains(&k.to_ascii_lowercase()))
        .cloned()
        .collect())
}

/// What a restore does with the recorded HEAD reflog in a repository that
/// keeps its refs in reftable now, where Git reads no `logs/HEAD` file and
/// adds an entry only as the newest one. A recreated worktree's reflog
/// starts at this restore, so the restore says so, and a record holding a
/// copy, written while the repository kept its refs in files, stays with
/// it. A worktree moved back kept its reflog in its own Git directory;
/// `holds` asks Git whether it still names every kept commit.
fn reftable_reflog(item: &Item, recreated: bool) -> Result<Option<String>, String> {
    match (recreated, item.head_reflog) {
        (false, _) => Ok(None),
        (true, false) => Ok(Some(REFTABLE_REFLOG_NOTE.into())),
        (true, true) => Err(REFTABLE_REFLOG_NOTE.into()),
    }
}

/// The restored worktree at `path` holds what the record's refs keep: its
/// HEAD contains the recorded HEAD (a branch moved between the check and
/// `git worktree add` would not), and its own HEAD reflog names every kept
/// commit: that reflog, not a branch's, is what kept them before the
/// removal. Git says which commits the reflog names (`git_head_reflog`),
/// never a `logs/HEAD` file, which Git does not read in a reftable
/// repository. `main` is where Git commands for the repository run. A
/// branch checkout's tip in `how` becomes the HEAD it found, so the report
/// says where the worktree is. `Err` is a note saying what it lacks.
fn holds(record: &Record, path: &Path, main: &Path, how: &mut How) -> Result<(), String> {
    let short = |c: &str| c[..c.len().min(12)].to_string();
    let mut check = || -> Result<Option<String>, String> {
        let now = git::commit_of(path, "HEAD")?.ok_or("it has no HEAD commit")?;
        if let How::Branch { tip } | How::AlreadyBack { tip } = how {
            tip.clone_from(&now);
        }
        if !git::is_ancestor(path, &record.head, &now)? {
            return Ok(Some(format!(
                "it is at {}, which does not contain the recorded HEAD {}: its branch moved while residue restored it, and the record's ref still holds {}",
                short(&now),
                short(&record.head),
                short(&record.head)
            )));
        }
        let missing = unnamed_kept(record, path, main)?.len();
        Ok((missing > 0).then(|| {
            format!(
                "its reflog does not name {missing} of the {} commits only the old reflog held",
                record.kept.len()
            )
        }))
    };
    match check() {
        Ok(None) => Ok(()),
        Ok(Some(lack)) => Err(lack),
        Err(e) => Err(format!(
            "could not check that it holds what the record keeps ({e})"
        )),
    }
}

/// How to clear what is left of a worktree Git can no longer use, so that
/// restore can recreate it: lift residue's lock when it holds one, delete
/// the directory, then have Git drop its record of this one worktree.
/// It names that one worktree: `git worktree prune` acts on every
/// unlocked worktree whose directory is missing, this one or not.
fn clear_steps(main: &Path, aside: &Path, ours: bool) -> String {
    let git = format!("git -C {} worktree", output::shell(main));
    let path = output::shell(aside);
    let unlock = if ours {
        format!("run `{git} unlock {path}`, ")
    } else {
        String::new()
    };
    format!(
        "{unlock}delete {}, run `{git} remove {path}`, then run this restore again",
        aside.display()
    )
}

/// The worktree Git lists at the record's original path, which the listing
/// found back there (`is_back`): read again from `listed`, as Git lists it
/// now, so a worktree moved away, or switched to another branch at another
/// commit, meanwhile is not taken for it. `holds` then checks its HEAD.
fn already_back(record: &Record, listed: &[Entry]) -> Result<Restored, String> {
    let entry = at_original(record, listed)
        .filter(|e| e.branch == record.branch || e.head.as_deref() == Some(record.head.as_str()))
        .ok_or_else(|| {
            format!(
                "Git no longer lists its worktree at {} on its branch or at its HEAD; run residue restore again to see where it stands",
                record.original.display()
            )
        })?;
    Ok(Restored {
        path: entry.path.clone(),
        how: How::AlreadyBack {
            tip: entry.head.clone().unwrap_or_default(),
        },
        branch: entry.branch.clone(),
        notes: Vec::new(),
    })
}

/// A worktree an interrupted run left where the deletion started: move it
/// back with `git worktree move`, the same command `residue scan` prints
/// for an interrupted removal.
fn move_back(
    item: &Item,
    record: &Record,
    registered: &Entry,
    dest: &Path,
) -> Result<Restored, String> {
    let aside = &record.aside;
    let main = &item.command_dir;
    if aside.starts_with(crate::trash::root(&item.common_dir)) {
        // A purge that stopped deleting it, part of which Git can no
        // longer use: the trash restore cannot move that, and once it is
        // deleted this restore recreates it (`purged_to_git_dir`).
        if let Some(why) = crate::remove::unusable(registered) {
            return Err(format!(
                "its purge from the trash stopped partway, and Git can no longer use what is left of it ({why}); the record keeps its HEAD and reflog: delete {}, then run this restore again, which recreates it",
                registered.path.display()
            ));
        }
        let entry = aside
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Err(format!(
            "its purge from the trash did not finish, so it is still in the trash; `residue restore {}` puts it back",
            output::word(&entry)
        ));
    }
    let Some(left) = crate::remove::Leftover::of(registered, main, &item.common_dir) else {
        return Err(format!(
            "Git still lists it at {}, where its removal started; residue cannot tell how it got there, so it leaves it",
            aside.display()
        ));
    };
    // Read under the record's hold, which no removal or Git holds now (or,
    // for a record without a hold line, with nothing `unheld` sees): only
    // the user locks or unlocks it meanwhile, and never residue's own lock.
    let ours = registered.locked.as_deref() == Some(crate::remove::REMOVING);
    if !left.present {
        return Err(format!(
            "its removal stopped partway and its directory is gone; {}; otherwise clear Git's record with `{}`, then run this restore again, which recreates it from this record",
            left.missing, left.clear
        ));
    }
    if registered.locked.is_some() && !ours {
        return Err(format!(
            "it is locked by someone else ({}); residue leaves that lock alone",
            registered.locked.as_deref().unwrap_or("")
        ));
    }
    let parent = dest.parent().ok_or("the destination has no parent")?;
    // Checked before it makes any folder, so a refusal leaves none.
    let (existing, _) = crate::trash::nearest_existing(parent);
    let here = fs::symlink_metadata(aside)
        .map_err(|e| format!("{}: {e}", aside.display()))?
        .dev();
    let there = fs::metadata(existing)
        .map_err(|e| format!("{}: {e}", existing.display()))?
        .dev();
    if here != there {
        return Err(format!(
            "{} is on another filesystem than {}; residue moves, never copies",
            existing.display(),
            aside.display()
        ));
    }
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    if ours {
        git::unlock_worktree(main, aside)?;
    }
    if let Err(e) = git::move_worktree(main, aside, dest) {
        if ours {
            if let Err(relock) = git::lock_worktree(main, aside, crate::remove::REMOVING) {
                return Err(format!("{e}; and could not lock it again: {relock}"));
            }
        }
        // A `git worktree remove` that failed part way can delete its HEAD
        // or the file that ties it to the repository, and leave the rest:
        // Git then can neither move nor remove it. The record still keeps
        // its HEAD and reflog, so once Git's leftovers are cleared, this
        // restore recreates it.
        // A `.git` file it lost leaves `git rev-parse` to find the repository
        // above it, so that answer alone does not show it.
        if let Some(why) = git::identity(aside).err().or_else(|| left.unusable.clone()) {
            return Err(format!(
                "{e}; Git can no longer use it ({why}), so it can neither move it back nor remove it; the record keeps its HEAD and reflog: {}",
                clear_steps(main, aside, ours)
            ));
        }
        return Err(e);
    }
    // Git deletes the checkout before its own record, so a removal killed
    // during the delete leaves part of the files.
    let notes = vec![
        "if its removal had started deleting, part of it is gone: `git status` in it lists tracked files that are missing, and `git restore .` brings them back; ignored output comes back from the project's install".into(),
    ];
    Ok(Restored {
        path: dest.to_path_buf(),
        how: How::MovedBack(aside.clone()),
        branch: registered.branch.clone(),
        notes,
    })
}

/// `git worktree add` at `dest`, as `choose` decides: on the recorded
/// branch when that loses nothing, and never moving a branch that exists.
fn recreate(
    item: &Item,
    record: &Record,
    dest: &Path,
    listed: &[Entry],
) -> Result<Restored, String> {
    let choice = choose(item, record, listed)?;
    git::add_worktree(&item.command_dir, dest, &choice.checkout(&record.head))?;
    let (how, branch) = match choice {
        Choice::Again(_) => (How::Recreated, record.branch.clone()),
        Choice::Branch { tip, .. } => (How::Branch { tip }, record.branch.clone()),
        Choice::NewBranch {
            name,
            tip,
            elsewhere,
        } => {
            let full = format!("refs/heads/{name}");
            (
                How::NewBranch {
                    name,
                    tip,
                    elsewhere,
                },
                Some(full),
            )
        }
        Choice::Detached => (How::Detached, None),
    };
    let mut notes = Vec::new();
    if matches!(how, How::Recreated) {
        if let Some(up) = &record.upstream {
            let shown = up.strip_prefix("refs/remotes/").unwrap_or(up);
            notes.push(format!(
                "its upstream was {}; set it again with `git branch --set-upstream-to`, or push with `git push -u`",
                output::clean(shown)
            ));
        }
    }
    Ok(Restored {
        path: dest.to_path_buf(),
        how,
        branch,
        notes,
    })
}

/// `wanted`, or `wanted-2`, `wanted-3`, ...: the first name no local
/// branch has.
fn free_branch(dir: &Path, wanted: &str) -> Result<String, String> {
    for n in 1..100 {
        let name = if n == 1 {
            wanted.to_string()
        } else {
            format!("{wanted}-{n}")
        };
        if !git::ref_exists(dir, &format!("refs/heads/{name}"))? {
            return Ok(name);
        }
    }
    Err(format!("too many branches named {wanted}-N"))
}

/// Put the recorded `config.worktree` into the restored worktree's Git
/// directory. When Git already made one (it copies the main worktree's)
/// that differs, Git's stays and the recorded one is saved next to it.
/// `Ok` with a note when the recorded one is in the Git directory either
/// way; `Err` when it is not, so the record, which still holds it, must
/// stay.
fn restore_config(recorded: &Saved, git_dir: &Path) -> Result<Option<String>, String> {
    let recorded = match recorded {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("could not read its recorded config.worktree: {e}")),
    };
    let target = git_dir.join(CONFIG);
    match fs::read(&target) {
        Ok(now) if now == *recorded => Ok(None),
        Ok(_) => {
            let aside = git_dir.join(format!("{CONFIG}.residue-removed"));
            match write_new(&aside, recorded) {
                Ok(()) => Ok(Some(format!(
                    "Git gave it a config.worktree that differs from the one it had; the one it had is at {}",
                    aside.display()
                ))),
                Err(e) => Err(format!(
                    "Git gave it a config.worktree that differs from the one it had, and residue could not save that one next to it ({e})"
                )),
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => match write_new(&target, recorded) {
            Ok(()) => Ok(None),
            Err(e) => Err(format!("could not put back its config.worktree ({e})")),
        },
        Err(e) => Err(format!("could not read {}: {e}", target.display())),
    }
}

/// For a worktree moved back: its HEAD reflog still begins with the one
/// the record copied, unless a `git worktree remove` that failed part way
/// deleted it; then the recorded one goes back in front, as for a
/// recreated worktree.
fn reflog_if_lost(saved: &Saved, git_dir: &Path) -> Result<Option<String>, String> {
    let saved_bytes = match saved {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("could not read its recorded HEAD reflog: {e}")),
    };
    let log = git_dir.join("logs").join("HEAD");
    match read_if_present(Some(log.clone())) {
        Ok(Some(now)) if now.starts_with(saved_bytes) => Ok(None),
        Ok(_) => restore_reflog(saved, git_dir),
        Err(e) => Err(format!("could not read {}: {e}", log.display())),
    }
}

/// Put the recorded HEAD reflog back, in front of the entry `git worktree
/// add` wrote, so `git reflog` in the restored worktree shows its history
/// and Git keeps the commits only that reflog held. The whole file is
/// written under another name and renamed into place. `Err` when it is not
/// back, so the record, which still holds it, must stay.
fn restore_reflog(saved: &Saved, git_dir: &Path) -> Result<Option<String>, String> {
    let mut bytes = match saved {
        Ok(Some(bytes)) => bytes.clone(),
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("could not read its recorded HEAD reflog: {e}")),
    };
    let logs = git_dir.join("logs");
    let target = logs.join("HEAD");
    let fail = |e: String| format!("could not put back its HEAD reflog ({e})");
    match fs::read(&target) {
        Ok(now) => {
            if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(&now);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(fail(format!("{}: {e}", target.display()))),
    }
    fs::create_dir_all(&logs).map_err(|e| fail(format!("{}: {e}", logs.display())))?;
    let staged = logs.join("HEAD.residue-new");
    match fs::remove_file(&staged) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(fail(format!("{}: {e}", staged.display()))),
    }
    write_new(&staged, &bytes).map_err(|e| fail(format!("{}: {e}", staged.display())))?;
    if let Err(e) = fs::rename(&staged, &target) {
        let left = fs::remove_file(&staged)
            .err()
            .map(|r| format!("; {} is left: {r}", staged.display()))
            .unwrap_or_default();
        return Err(fail(format!("{}: {e}{left}", target.display())));
    }
    sync_dir(&logs).map_err(fail)?;
    Ok(None)
}

// ---------------------------------------------------------------- marker

/// When and from what `residue restore` put a worktree back.
pub struct Mark {
    /// Unix seconds; `None` when the file does not say.
    pub at: Option<i64>,
    /// `removal record NAME` or `trash entry NAME`.
    pub from: String,
}

/// Mark a worktree as restored, in its own Git directory.
pub fn mark(git_dir: &Path, from: &str) -> Result<(), String> {
    let now = output::now();
    let text = format!(
        "# `residue restore` put this worktree back; `residue watch` never removes it again because its work landed.\nrestored {now} {}\nfrom {}\n",
        output::rfc3339(now),
        from.replace('\n', " ")
    );
    let path = git_dir.join(MARKER);
    fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The mark in a worktree's own Git directory, if `residue restore` left
/// one. The file is the mark: one whose content residue cannot parse still
/// counts.
pub fn marked(git_dir: &Path) -> Result<Option<Mark>, String> {
    let path = git_dir.join(MARKER);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut mark = Mark {
        at: None,
        from: String::new(),
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("restored ") {
            mark.at = rest.split(' ').next().and_then(|n| n.parse().ok());
        } else if let Some(rest) = line.strip_prefix("from ") {
            mark.from = rest.to_string();
        }
    }
    Ok(Some(mark))
}

pub fn mark_json(mark: &Result<Option<Mark>, String>) -> (Json, Json) {
    match mark {
        Ok(Some(m)) => (
            Json::Obj(vec![
                ("unix", Json::opt(m.at, Json::Int)),
                ("at", Json::opt(m.at, |t| Json::str(output::rfc3339(t)))),
                ("from", Json::str(m.from.clone())),
            ]),
            Json::Null,
        ),
        Ok(None) => (Json::Null, Json::Null),
        Err(e) => (Json::Null, Json::str(e.clone())),
    }
}

// ---------------------------------------------------------------- JSON

/// One removal record, as `residue restore --json` lists it.
pub fn item_json(item: &Item) -> Json {
    let r = item.record.as_ref().ok();
    Json::Obj(vec![
        ("kind", Json::str("removed")),
        ("entry", Json::str(item.name.clone())),
        ("entry_path", Json::path(&item.dir)),
        ("ref", Json::str(item.ref_name())),
        ("state", Json::str(item.state().code())),
        (
            "original_path",
            Json::opt(r.map(|r| r.original.as_path()), Json::path),
        ),
        (
            "aside_path",
            Json::opt(r.map(|r| r.aside.as_path()), Json::path),
        ),
        (
            "branch",
            Json::opt(r.and_then(|r| r.branch.clone()), Json::Str),
        ),
        ("head", Json::opt(r.map(|r| r.head.clone()), Json::Str)),
        (
            "upstream",
            Json::opt(r.and_then(|r| r.upstream.clone()), Json::Str),
        ),
        ("reason", Json::opt(r.map(|r| r.reason.clone()), Json::Str)),
        ("by", Json::opt(r.and_then(|r| r.by.clone()), Json::Str)),
        (
            "removed_at",
            Json::opt(r.map(|r| r.removed), |t| Json::str(output::rfc3339(t))),
        ),
        // The `--keep-removed` it was written with; null for a record
        // written before residue kept it.
        (
            "keep",
            Json::opt(r.and_then(|r| r.keep.as_ref()), |k| {
                Json::str(k.text.clone())
            }),
        ),
        (
            "keep_seconds",
            Json::opt(r.and_then(|r| r.keep.as_ref()), |k| Json::Int(k.seconds)),
        ),
        (
            "allocated_bytes",
            Json::opt(r.and_then(|r| r.allocated), Json::UInt),
        ),
        (
            "reclaimable_bytes",
            Json::opt(r.and_then(|r| r.reclaimable), Json::UInt),
        ),
        (
            "not_kept",
            Json::Arr(r.map_or_else(Vec::new, |r| r.gone.iter().map(Gone::json).collect())),
        ),
        ("worktree_config", Json::Bool(item.worktree_config)),
        ("head_reflog", Json::Bool(item.head_reflog)),
        (
            "reflog_refs",
            Json::Arr(r.map_or_else(Vec::new, |r| {
                refs_of(&item.name, &r.head, &r.kept)
                    .into_iter()
                    .skip(1)
                    .map(|(name, commit)| {
                        Json::Obj(vec![
                            ("ref", Json::str(name)),
                            ("commit", Json::str(commit)),
                        ])
                    })
                    .collect()
            })),
        ),
        ("incomplete", Json::Bool(item.incomplete)),
        // Why residue cannot tell whether the worktree is back (`unknown`).
        (
            "back_error",
            match &item.back {
                Back::Unknown(e) => Json::str(e.clone()),
                Back::No | Back::Yes => Json::Null,
            },
        ),
        (
            "pid",
            Json::opt(r.and_then(|r| r.pid), |p| Json::UInt(u64::from(p))),
        ),
        (
            "residue_version",
            Json::opt(r.and_then(|r| r.version.clone()), Json::Str),
        ),
        (
            "error",
            match &item.record {
                Err(e) => Json::str(e.clone()),
                Ok(_) => Json::Null,
            },
        ),
        // Added last, so every field before it keeps its place.
        ("id", Json::str(item.id.clone())),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Record {
        Record {
            original: PathBuf::from("/src/app/.worktrees/fix login\nnew"),
            aside: PathBuf::from("/src/app/.worktrees/.fix login\nnew.residue-removing-42"),
            branch: Some("refs/heads/fix".into()),
            head: "0f607062a2d10dfb8bc578c32a96c976609da819".into(),
            upstream: Some("refs/remotes/origin/fix".into()),
            reason: Reason::WatchLanded.code().into(),
            by: Some(By::Watch.code().into()),
            removed: 1_794_000_000,
            keep: Some(Keep {
                seconds: 12 * 604_800,
                text: "12w".into(),
            }),
            allocated: Some(4096),
            reclaimable: Some(2048),
            gone: vec![
                Gone {
                    path: "node_modules/".into(),
                    files: Some(3),
                    bytes: Some(120),
                },
                Gone {
                    path: "odd name\\with\nnewline/".into(),
                    files: None,
                    bytes: None,
                },
            ],
            pid: Some(42),
            version: Some("0.0.7".into()),
            hold: true,
            kept: vec![
                "1f607062a2d10dfb8bc578c32a96c976609da819".into(),
                "2f607062a2d10dfb8bc578c32a96c976609da819".into(),
            ],
        }
    }

    #[test]
    fn a_record_reads_back_as_written() {
        let r = sample();
        let back = Record::parse(&r.render("fix_login_new-20261107T040000Z")).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn a_record_without_a_by_line_still_reads() {
        let r = Record {
            by: None,
            ..sample()
        };
        let text = r.render("fix-20261107T040000Z");
        assert!(!text.split(|b| *b == b'\n').any(|l| l.starts_with(b"by ")));
        assert_eq!(Record::parse(&text).unwrap(), r);
    }

    /// A record residue 0.0.7 wrote has no `keep` line: it reads, and only
    /// the run's `--keep-removed` decides when it is forgotten.
    #[test]
    fn keep_is_the_floor_of_every_run() {
        let item = |keep: Option<Keep>| Item {
            name: "x".into(),
            dir: PathBuf::from("/src/app/.git/residue-removed/x"),
            common_dir: PathBuf::from("/src/app/.git"),
            command_dir: PathBuf::from("/src/app"),
            record: Ok(Record { keep, ..sample() }),
            incomplete: false,
            worktree_config: false,
            head_reflog: false,
            modified: None,
            registered: None,
            back: Back::No,
            id: String::new(),
        };
        let day = 86_400;
        let removed = sample().removed;
        let twelve_weeks = item(sample().keep);
        // A plain run (30d) on day 31 leaves a record written with 12w.
        assert!(!twelve_weeks.expired(removed + 31 * day, 30 * day));
        assert!(!twelve_weeks.expired(removed + 84 * day - 1, 30 * day));
        assert!(twelve_weeks.expired(removed + 84 * day, 30 * day));
        // A run with a longer --keep-removed still waits for its own.
        assert!(!twelve_weeks.expired(removed + 84 * day, 100 * day));
        let old = item(None);
        assert!(old.expired(removed + 30 * day, 30 * day));
        assert!(!old.expired(removed + 30 * day - 1, 30 * day));

        let full = String::from_utf8(sample().render("x")).unwrap();
        let without: String = full
            .lines()
            .filter(|l| !l.starts_with("keep "))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(Record::parse(without.as_bytes()).unwrap().keep, None);
        for bad in ["keep 0 0d", "keep 12w", "keep -5 1d", "keep 7257600"] {
            let text = full.replace("keep 7257600 12w", bad);
            assert!(Record::parse(text.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_record_without_its_essentials_is_an_error() {
        let full = String::from_utf8(sample().render("x")).unwrap();
        for key in ["path ", "aside ", "head ", "removed "] {
            let without: String = full
                .lines()
                .filter(|l| !l.starts_with(key))
                .map(|l| format!("{l}\n"))
                .collect();
            assert!(Record::parse(without.as_bytes()).is_err(), "{key}");
        }
        let detached = full.replace("branch refs/heads/fix\n", "");
        assert_eq!(Record::parse(detached.as_bytes()).unwrap().branch, None);
        for bad in [
            full.replace("head 0f6", "head --upload-pack=x 0f6"),
            full.replace("branch refs/heads/fix", "branch refs/heads/-x"),
            full.replace("branch refs/heads/fix", "branch refs/tags/v1"),
            full.replace("path /src", "path src"),
        ] {
            assert!(Record::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn entry_names_are_valid_ref_components() {
        let cases = [
            ("fix-login", "fix-login"),
            ("my wt", "my_wt"),
            (".hidden", "hidden"),
            ("a..b", "a.b"),
            ("x~1^2:3?*[\\", "x_1_2_3____"),
            ("...", "worktree"),
            ("ünïcode", "_n_code"),
        ];
        for (name, want) in cases {
            assert_eq!(entry_base(&Path::new("/r").join(name)), want, "{name}");
        }
    }

    #[test]
    fn a_record_name_says_when_it_was_written() {
        for t in [0, 951_782_400, 1_735_689_599, 1_790_000_000, 4_102_444_800] {
            assert_eq!(output::unstamp(&output::stamp(t)), Some(t), "{t}");
        }
        for bad in [
            "20261307T000000Z",
            "20261107T240000Z",
            "20261107 040000Z",
            "2026110T040000Z",
        ] {
            assert_eq!(output::unstamp(bad), None, "{bad}");
        }
        let w = Path::new("/r/w");
        let at = output::unstamp("20250101T000000Z");
        assert!(at.is_some());
        assert_eq!(named_at("w-20250101T000000Z", w), at);
        assert_eq!(named_at("w-20250101T000000Z-12", w), at);
        for other in [
            "w-20250101T000000Z-",
            "w-20250101T000000Z-x",
            "w-login-20250101T000000Z",
            "wx-20250101T000000Z",
            "w-20250101T000000Z-20250102T000000Z",
        ] {
            assert_eq!(named_at(other, w), None, "{other}");
        }
        let login = Path::new("/r/w-login");
        assert_eq!(named_at("w-login-20250101T000000Z", login), at);
    }

    #[test]
    fn escaping_round_trips() {
        for raw in [&b"plain"[..], b"a\\b", b"a\nb", b"\\n", b"end\\"] {
            assert_eq!(unescape(&escape(raw)), raw);
            assert!(!escape(raw).contains(&b'\n'));
        }
    }
}
