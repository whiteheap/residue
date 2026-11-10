//! The trash: worktrees `remove --trash` moved aside instead of deleting.
//!
//! Each entry is a directory `GIT_COMMON_DIR/residue-trash/NAME-STAMP/`
//! that holds the worktree itself and a record, `info`, with its original
//! path, branch, HEAD, the time it was trashed and its size. The worktree
//! stays registered with Git: `git worktree move` put it there, so Git's
//! record follows it, and it is locked with [`LOCK`] so `git worktree
//! prune` and other tools leave it alone. That keeps every way out a Git
//! command: `residue restore` moves it back with `git worktree move`, and
//! the purge deletes it with `git worktree remove` without `--force`. A
//! purge and a restore each hold the entry's `info` while they act on it,
//! and the purge's Git holds it while it deletes (`hold`), so a restore
//! never moves the worktree back while a purge may still delete it.
//!
//! Why the common directory: it belongs to the repository, so the trash is
//! found from any of its worktrees and goes away with it; no worktree's
//! `git status` sees it; and a move there is one rename(2) whenever the
//! worktree is on the repository's filesystem, the usual layout
//! (`.worktrees/NAME` or a sibling directory). A worktree on another
//! filesystem is refused, never copied.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::fingerprint;
use crate::git::{self, Entry};
use crate::hold::{self, Hold, Taken};
use crate::json::Json;
use crate::output;
use crate::removed;

pub const DIR: &str = "residue-trash";
/// The lock reason on every worktree in the trash.
pub const LOCK: &str = "residue: in trash";
const INFO: &str = "info";
/// `info` while it is written: synced, then renamed to `info`, so an entry
/// never has a partial `info`.
const INFO_NEW: &str = "info.new";

pub fn root(common_dir: &Path) -> PathBuf {
    common_dir.join(DIR)
}

/// What `info` records about one trashed worktree.
pub struct Record {
    pub original: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
    /// Unix seconds.
    pub trashed: i64,
    pub allocated: Option<u64>,
    pub reclaimable: Option<u64>,
    /// The `--keep-trash` of the run that trashed it, so no later run
    /// purges it sooner; `None` in an entry an earlier release made, and
    /// the line as written when this release cannot read it (damaged, or
    /// a later release's), which no run purges.
    pub keep: Option<Result<removed::Keep, String>>,
}

impl Record {
    fn render(&self, entry: &Path) -> Vec<u8> {
        let mut out = format!(
            "# residue trash entry. `residue restore {}` moves the worktree back.\n",
            entry.display()
        )
        .into_bytes();
        out.extend_from_slice(b"path ");
        out.extend_from_slice(self.original.as_os_str().as_bytes());
        out.push(b'\n');
        if let Some(b) = &self.branch {
            out.extend_from_slice(format!("branch {b}\n").as_bytes());
        }
        if let Some(h) = &self.head {
            out.extend_from_slice(format!("head {h}\n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trashed {} {}\n",
                self.trashed,
                output::rfc3339(self.trashed)
            )
            .as_bytes(),
        );
        // A release before this one ignores the line, and purges the entry
        // after its own --keep-trash.
        if let Some(Ok(k)) = &self.keep {
            out.extend_from_slice(format!("keep {} {}\n", k.seconds, k.text).as_bytes());
        }
        if let Some(n) = self.allocated {
            out.extend_from_slice(format!("allocated {n}\n").as_bytes());
        }
        if let Some(n) = self.reclaimable {
            out.extend_from_slice(format!("reclaimable {n}\n").as_bytes());
        }
        out
    }

    fn parse(bytes: &[u8]) -> Result<Record, String> {
        let mut original = None;
        let mut record = Record {
            original: PathBuf::new(),
            branch: None,
            head: None,
            trashed: 0,
            allocated: None,
            reclaimable: None,
            keep: None,
        };
        let mut trashed = None;
        for line in bytes.split(|b| *b == b'\n') {
            if line.is_empty() || line.starts_with(b"#") {
                continue;
            }
            let (key, value) = match line.iter().position(|b| *b == b' ') {
                Some(i) => (&line[..i], &line[i + 1..]),
                None => (line, &line[line.len()..]),
            };
            let text = || String::from_utf8_lossy(value).into_owned();
            let number = |what: &str| {
                text()
                    .split(' ')
                    .next()
                    .and_then(|n| n.parse::<i64>().ok())
                    .ok_or_else(|| format!("{what} is not a number"))
            };
            match key {
                b"path" => original = Some(PathBuf::from(OsString::from_vec(value.to_vec()))),
                b"branch" => record.branch = Some(text()),
                b"head" => record.head = Some(text()),
                b"trashed" => trashed = Some(number("trashed")?),
                // One this release cannot read keeps the entry readable,
                // so it can be restored, and never expiring.
                b"keep" => record.keep = Some(removed::Keep::parse(&text()).ok_or_else(text)),
                b"allocated" => record.allocated = number("allocated")?.try_into().ok(),
                b"reclaimable" => record.reclaimable = number("reclaimable")?.try_into().ok(),
                // Keys a later version adds carry nothing this one needs.
                _ => {}
            }
        }
        record.original = original.ok_or("it has no path line")?;
        if !record.original.is_absolute() {
            return Err("its path is not absolute".into());
        }
        record.trashed = trashed.ok_or("it has no trashed line")?;
        Ok(record)
    }
}

/// One entry of a repository's trash.
pub struct Item {
    /// The entry directory's name: `fix-login-20261107T040000Z`.
    pub name: String,
    pub dir: PathBuf,
    pub common_dir: PathBuf,
    /// Where to run `git worktree` commands for this repository.
    pub command_dir: PathBuf,
    pub record: Result<Record, String>,
    /// The worktree inside the entry, named like the original.
    pub worktree: Option<PathBuf>,
    /// Git's record of that worktree, when Git lists it.
    pub registered: Option<Entry>,
    /// The entry has no `info`: the run that made it is writing it, or
    /// stopped before it finished.
    pub incomplete: bool,
    /// Its `info` is missing or does not parse, and the entry holds nothing
    /// but `info` and `info.new`. False for a readable record.
    pub bare: bool,
    /// The entry directory's modification time, in Unix seconds.
    pub modified: Option<i64>,
    /// What an entry that does not hold its worktree, or whose `info` is
    /// missing or damaged, holds besides `info` and `info.new`, by name,
    /// sorted; `None` when residue could not list it. residue never puts
    /// anything else there and never deletes it. Empty, and not read, for
    /// an entry that holds its worktree: nothing decided there needs it.
    pub extra: Option<Vec<String>>,
    /// Its fingerprint (`fingerprint::entry`), from the `info` read here:
    /// `id` in its JSON, which `restore --expect` compares.
    pub id: String,
}

impl Item {
    /// The worktree directory is still in the entry.
    pub fn holds_worktree(&self) -> bool {
        self.worktree
            .as_ref()
            .is_some_and(|w| fs::symlink_metadata(w).is_ok())
    }

    /// A record whose worktree is not in the entry: it never arrived
    /// (residue stopped between writing `info` and the move), is arriving
    /// now (a run is between the two), or was deleted by hand.
    pub fn is_empty_record(&self) -> bool {
        self.record.is_ok() && !self.holds_worktree() && self.registered.is_none()
    }

    /// An entry that holds only its record files: an empty record with
    /// nothing else beside it, or a bare entry whose `info` is missing or
    /// damaged.
    pub fn holds_only_record(&self) -> bool {
        (self.is_empty_record() && self.extra.as_ref().is_some_and(Vec::is_empty))
            || (self.record.is_err() && self.bare)
    }

    /// An entry that holds only its record files and is old enough that no
    /// run can still be moving a worktree into it, so a purge clears it.
    /// The run that made it moves the worktree in milliseconds after it
    /// writes `info`; clearing it in between would leave that worktree in
    /// the trash with no `info`. An empty record's age is its `trashed`
    /// time, a bare one's its directory's.
    pub fn clearable(&self, now: i64) -> bool {
        let settled = |since: i64| now - since >= removed::INCOMPLETE_AFTER;
        match &self.record {
            Ok(r) => self.holds_only_record() && settled(r.trashed),
            Err(_) => self.bare && self.modified.is_some_and(settled),
        }
    }

    /// An entry no purge can clear, which every run keeps: why, naming what
    /// it holds. An empty record as old as a clearable one that also holds
    /// something else, or that residue could not list; or an entry with no
    /// `info` that holds anything but `info.new`, which no run of this
    /// release leaves (`discard` keeps `info` beside anything else) and
    /// none is making (a run writes `info` before it moves a worktree in).
    pub fn stranded(&self, now: i64) -> Option<String> {
        match &self.record {
            Ok(r) if self.is_empty_record() && now - r.trashed >= removed::INCOMPLETE_AFTER => {
                match &self.extra {
                    Some(names) if names.is_empty() => None,
                    Some(names) => Some(beside(names)),
                    None => Some(
                        "its worktree is gone, but residue could not list what else the entry holds, so the entry stays".into(),
                    ),
                }
            }
            Ok(_) => None,
            Err(_) if self.incomplete && !self.bare => Some(match &self.extra {
                Some(names) => format!(
                    "it has no info, so residue cannot tell what it holds ({}) and leaves it; once that is moved or deleted, a later remove run clears the entry",
                    crate::inspect::examples(names, 5)
                ),
                None => "it has no info, and residue could not list what it holds, so the entry stays".into(),
            }),
            Err(_) => None,
        }
    }

    /// At least `keep` old, and at least as old as the `--keep-trash` of
    /// the run that trashed it, the longer of the two, as a removal
    /// record's `--keep-removed` (`removed::Item::expired`). An entry
    /// without one, from 0.0.13 or earlier, has only `keep`; one whose
    /// `keep` this release cannot read never expires.
    pub fn expired(&self, now: i64, keep: i64) -> bool {
        self.record.as_ref().is_ok_and(|r| match &r.keep {
            Some(Err(_)) => false,
            Some(Ok(own)) => now - r.trashed >= keep.max(own.seconds),
            None => now - r.trashed >= keep,
        })
    }
}

/// The entries in the trash of the repository at `common_dir`, oldest
/// first. `registered` is the repository's trash worktrees as Git lists
/// them (`discover::Repo::trash`). An entry whose record cannot be read is
/// listed with its error and never purged.
pub fn list(
    common_dir: &Path,
    command_dir: &Path,
    registered: &[Entry],
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
        let is_dir = child
            .file_type()
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .is_dir();
        let name = child.file_name().to_string_lossy().into_owned();
        if !is_dir || name.starts_with('.') {
            continue;
        }
        let modified = child.metadata().ok().map(|m| m.mtime());
        // `missing_or_damaged`: an `info` residue can tell it did not finish
        // (none, or one that does not parse), as opposed to one it could
        // not read at all.
        let info = fs::read(dir.join(INFO));
        let id = fingerprint::entry("trash", &dir, info.as_deref().ok());
        let (record, incomplete, missing_or_damaged) = match info {
            Ok(bytes) => {
                let record =
                    Record::parse(&bytes).map_err(|e| format!("{}: {e}", dir.join(INFO).display()));
                let damaged = record.is_err();
                (record, false, damaged)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (
                Err(format!(
                    "{}: it has no info, so residue cannot tell what it holds",
                    dir.display()
                )),
                true,
                true,
            ),
            Err(e) => (
                Err(format!("{}: {e}", dir.join(INFO).display())),
                false,
                false,
            ),
        };
        let name_in = record.as_ref().ok().and_then(|r| r.original.file_name());
        let worktree = name_in.map(|n| dir.join(n));
        let holds_worktree = worktree
            .as_ref()
            .is_some_and(|w| fs::symlink_metadata(w).is_ok());
        // Listed only where a decision reads it, so `scan` reads no more
        // of an entry that holds its worktree.
        let extra = if missing_or_damaged || (record.is_ok() && !holds_worktree) {
            besides_record(&dir).ok().map(|names| {
                names
                    .into_iter()
                    .filter(|n| Some(n.as_os_str()) != name_in)
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
        } else {
            Some(Vec::new())
        };
        let bare = missing_or_damaged && extra.as_ref().is_some_and(Vec::is_empty);
        let registered = worktree.as_ref().and_then(|w| {
            registered
                .iter()
                .find(|e| {
                    e.path == *w || git::canonical(&e.path).ok().as_deref() == Some(w.as_path())
                })
                .cloned()
        });
        items.push(Item {
            name,
            dir,
            common_dir: common_dir.to_path_buf(),
            command_dir: command_dir.to_path_buf(),
            record,
            worktree,
            registered,
            incomplete,
            bare,
            modified,
            extra,
            id,
        });
    }
    items.sort_by_key(|i| {
        (
            i.record.as_ref().map_or(i64::MIN, |r| r.trashed),
            i.name.clone(),
        )
    });
    Ok(items)
}

/// Make a new entry for a worktree at `original`, with its record written,
/// and return the entry directory and the hold on it, taken before its
/// `info` is in place. The worktree is not moved yet; the caller moves it
/// to `entry.join(original's name)` and checks it there while it keeps
/// the hold, so no restore or purge acts on the entry meanwhile, and on a
/// filesystem without flock(2) nothing is made or moved.
pub fn create(common_dir: &Path, record: &Record) -> Result<(PathBuf, Hold), String> {
    let name = record
        .original
        .file_name()
        .ok_or("the worktree path has no name")?;
    if record.original.as_os_str().as_bytes().contains(&b'\n') {
        return Err("its path contains a newline, which the trash record cannot hold".into());
    }
    let root = root(common_dir);
    let new_root = !root.is_dir();
    fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let stamp = output::stamp(record.trashed);
    for n in 1..100 {
        let mut entry_name = OsString::from(name);
        entry_name.push(format!("-{stamp}"));
        if n > 1 {
            entry_name.push(format!("-{n}"));
        }
        // Trash entries and removal records share one list in `residue
        // restore`, so a name a record already has is skipped.
        if fs::symlink_metadata(removed::root(common_dir).join(&entry_name)).is_ok() {
            continue;
        }
        let entry = root.join(&entry_name);
        match fs::create_dir(&entry) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot create {}: {e}", entry.display())),
        }
        // As a removal record's: `info.new`, synced and held, renamed, then
        // the directories synced, so a power loss leaves a whole `info` or
        // none, and no restore finds the new entry free.
        let staged = entry.join(INFO_NEW);
        let written = hold::write_new(&staged, &record.render(&entry))
            .map_err(|e| format!("cannot write {}: {e}", staged.display()))
            .and_then(|hold| {
                fs::rename(&staged, entry.join(INFO))
                    .map_err(|e| format!("cannot rename {}: {e}", staged.display()))?;
                removed::sync_dir(&entry)?;
                removed::sync_dir(&root)?;
                if new_root {
                    removed::sync_dir(common_dir)?;
                }
                Ok(hold)
            });
        return match written {
            Ok(hold) => Ok((entry, hold)),
            Err(message) => Err(match discard(&entry) {
                Ok(()) => message,
                Err(more) => format!("{message}; {more}"),
            }),
        };
    }
    Err(format!(
        "{}: too many entries named {}-{stamp}",
        root.display(),
        name.to_string_lossy()
    ))
}

/// Why an entry whose worktree is gone stays: it also holds `names`.
pub fn beside(names: &[String]) -> String {
    format!(
        "its worktree is gone, but the entry also holds {}, which residue never deletes, so the entry stays; once that is moved or deleted, a later remove run clears it",
        crate::inspect::examples(names, 5)
    )
}

/// Why [`discard`] left an entry where it is.
pub enum Stays {
    /// The entry holds these besides its record, which residue never
    /// deletes, so it removed nothing: the record stays with them.
    Holds(PathBuf, Vec<String>),
    /// Listing or removing it failed.
    Failed(String),
}

impl std::fmt::Display for Stays {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stays::Holds(entry, names) => write!(
                f,
                "the trash entry {} stays, with its record: it also holds {}, which residue never deletes",
                entry.display(),
                crate::inspect::examples(names, 5)
            ),
            Stays::Failed(e) => f.write_str(e),
        }
    }
}

impl Stays {
    /// What a run whose worktree left the entry says of the entry: the
    /// worktree is gone or back, so what is left is not it.
    pub fn note(&self) -> String {
        match self {
            Stays::Holds(..) => {
                format!("{self}; once that is moved or deleted, a later remove run clears it")
            }
            Stays::Failed(e) => format!("residue could not clear its trash entry ({e})"),
        }
    }
}

/// Remove an entry's record and its now empty directory, and the trash
/// directory once it is empty. Never recursive, and the entry is listed
/// first: when it holds anything but `info` and `info.new`, a worktree
/// Git left there included, nothing is removed, so the record stays with
/// what it is about and `residue restore` still finds them. Something
/// that arrives between the listing and the rmdir(2) stays too, and the
/// error says so.
pub fn discard(entry: &Path) -> Result<(), Stays> {
    let names = besides_record(entry)
        .map_err(|e| Stays::Failed(format!("cannot list {}: {e}", entry.display())))?;
    if !names.is_empty() {
        return Err(Stays::Holds(
            entry.to_path_buf(),
            names
                .iter()
                .map(|n| n.to_string_lossy().into_owned())
                .collect(),
        ));
    }
    for file in [INFO, INFO_NEW] {
        match fs::remove_file(entry.join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Stays::Failed(format!(
                    "cannot remove {}: {e}",
                    entry.join(file).display()
                )));
            }
        }
    }
    fs::remove_dir(entry)
        .map_err(|e| Stays::Failed(format!("cannot remove {}: {e}", entry.display())))?;
    if let Some(root) = entry.parent() {
        // Other entries may still be in the trash.
        remove_dir_if_empty(root)
            .map_err(|e| Stays::Failed(format!("cannot remove {}: {e}", root.display())))?;
    }
    Ok(())
}

/// The names in an entry directory besides `info` and `info.new`, sorted.
fn besides_record(entry: &Path) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for child in fs::read_dir(entry)? {
        let name = child?.file_name();
        if name != INFO && name != INFO_NEW {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Remove a directory if it is empty; one that still has entries stays and
/// is not an error. POSIX lets rmdir(2) report a non-empty directory as
/// ENOTEMPTY or EEXIST, and illumos uses EEXIST, which Rust calls
/// `AlreadyExists`. rmdir never reports EEXIST for anything else. One
/// another run removed first is gone, as this one wanted.
pub fn remove_dir_if_empty(dir: &Path) -> io::Result<()> {
    match fs::remove_dir(dir) {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::DirectoryNotEmpty
                    | io::ErrorKind::AlreadyExists
                    | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

/// The lock on a trashed worktree is residue's own, so residue may lift it.
pub fn locked_by_residue(entry: &Entry) -> bool {
    entry.locked.as_deref() == Some(LOCK)
}

/// The `info` of the entry at `entry`, which a run holds while it purges
/// or restores the entry (`hold`).
pub fn info(entry: &Path) -> PathBuf {
    entry.join(INFO)
}

/// Take the hold on an entry before a restore acts on it. Held, it means
/// that no removal moving a worktree in, no purge, no `git worktree
/// remove` one started and no other restore is acting on the entry. Once
/// the run that holds it has exited, what holds it is its Git or a
/// program that Git started, which `lsof` names.
pub fn take(item: &Item) -> Result<Hold, String> {
    let info = info(&item.dir);
    match hold::take(&info)? {
        Taken::Held(hold) => Ok(hold),
        Taken::Busy => Err(format!(
            "a residue run, or the `git worktree remove` it started, is purging or restoring this trash entry now, or moving a worktree into it (`lsof {}` names it); run this restore again once it ends",
            output::shell(&info)
        )),
        Taken::Gone => Err(
            "another residue run purged or restored this trash entry after this restore listed it, so residue changed nothing; `residue restore` lists what is left".into(),
        ),
        Taken::Unsupported(why) => Err(removed::unsupported_note(
            &why,
            match (&item.record, &item.registered) {
                (Ok(record), Some(registered)) => {
                    Some(removed::Manual::MoveBack(removed::move_back_line(
                        &item.command_dir,
                        registered,
                        &record.original,
                    )))
                }
                _ => None,
            },
        )),
    }
}

/// Why [`restore`] did not move the worktree back.
pub enum NotBack {
    /// A check refused, before Git changed anything.
    Refused(String),
    /// Git's unlock or move failed.
    Git(Box<GitFailed>),
}

/// What a restore whose Git unlock or move failed, `error`, changed.
pub struct GitFailed {
    pub error: String,
    /// The lock it lifted first and could not put back, with why: its
    /// reason, then Git's error.
    pub lifted: Option<(String, String)>,
    /// The first folder it made for the destination, which stays.
    pub created: Option<PathBuf>,
    /// The destination, when Git renamed the worktree's directory there
    /// and stopped before it recorded the new path.
    pub moved: Option<PathBuf>,
    /// The lock it lifted is back on the worktree.
    pub relocked: bool,
}

impl From<String> for NotBack {
    fn from(why: String) -> NotBack {
        NotBack::Refused(why)
    }
}

/// Put one entry's worktree back where it was, or at `to`, re-registered
/// by `git worktree move`, and drop the entry. The caller holds the entry
/// (`take`) and passes every worktree Git lists for the repository, read
/// since then (`listed`), so the lock it lifts is the one there now.
/// Returns the restored path, and a note when the entry stays behind,
/// naming what it still holds ([`Stays::note`]).
pub fn restore(
    item: &Item,
    to: Option<&Path>,
    listed: &[Entry],
    _held: &Hold,
) -> Result<(PathBuf, Option<String>), NotBack> {
    let (worktree, registered, dest, created) = restorable(item, to, listed)?;
    let main = &item.command_dir;
    let lock = registered.locked.as_deref();
    if lock.is_some() {
        git::unlock_worktree(main, worktree).map_err(|error| {
            NotBack::Git(Box::new(GitFailed {
                error,
                lifted: None,
                created: created.clone(),
                moved: None,
                relocked: false,
            }))
        })?;
    }
    if let Err(error) = git::move_worktree(main, worktree, dest) {
        let lifted = match lock {
            Some(reason) => git::lock_worktree(main, worktree, LOCK)
                .err()
                .map(|relock| (reason.to_string(), relock)),
            None => None,
        };
        // Git renames the directory before it records the new path, and
        // may stop between the two.
        let moved = (fs::symlink_metadata(worktree).is_err() && fs::symlink_metadata(dest).is_ok())
            .then(|| dest.to_path_buf());
        return Err(NotBack::Git(Box::new(GitFailed {
            error,
            relocked: lock.is_some() && lifted.is_none(),
            lifted,
            created,
            moved,
        })));
    }
    let note = discard(&item.dir).err().map(|stays| stays.note());
    Ok((dest.to_path_buf(), note))
}

/// What [`restore`] checks before Git changes anything: the entry's
/// worktree, Git's entry for it, where it goes, and the first folder it
/// made for that, if any. It checks that the destination is on the
/// trash's filesystem before it makes a folder, by the nearest folder
/// above it that exists.
fn restorable<'a>(
    item: &'a Item,
    to: Option<&'a Path>,
    listed: &'a [Entry],
) -> Result<(&'a Path, &'a Entry, &'a Path, Option<PathBuf>), String> {
    let record = item.record.as_ref().map_err(|e| e.clone())?;
    let worktree = item
        .worktree
        .as_ref()
        .ok_or("the entry names no worktree")?;
    if !item.holds_worktree() {
        return Err(format!(
            "{} is not there; the entry holds only its record",
            worktree.display()
        ));
    }
    let registered = listed.iter().find(|e| {
        e.path == *worktree || git::canonical(&e.path).ok().as_deref() == Some(worktree.as_path())
    });
    let Some(registered) = registered else {
        return Err(format!(
            "Git no longer lists {} as a worktree. If the repository moved, `git -C {} worktree repair {}` reconnects it; then restore again",
            worktree.display(),
            output::shell(&item.command_dir),
            output::shell(worktree)
        ));
    };
    match &registered.locked {
        Some(_) if !locked_by_residue(registered) => {
            return Err(format!(
                "it is locked by someone else ({}); residue leaves that lock alone",
                registered.locked.as_deref().unwrap_or("")
            ));
        }
        _ => {}
    }
    let dest = to.unwrap_or(&record.original);
    match fs::symlink_metadata(dest) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(format!(
                "{} exists; move it away first, or restore somewhere else with --to PATH",
                dest.display()
            ));
        }
        Err(e) => return Err(format!("cannot check {}: {e}", dest.display())),
    }
    let parent = dest.parent().ok_or("the destination has no parent")?;
    let (existing, missing) = nearest_existing(parent);
    let here = fs::symlink_metadata(worktree)
        .map_err(|e| format!("{}: {e}", worktree.display()))?
        .dev();
    let there = fs::metadata(existing)
        .map_err(|e| format!("{}: {e}", existing.display()))?
        .dev();
    if here != there {
        return Err(format!(
            "{} is on another filesystem than the trash; residue moves, never copies",
            existing.display()
        ));
    }
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    Ok((worktree, registered, dest, missing.map(Path::to_path_buf)))
}

/// The nearest folder at or above `parent` that exists, and the topmost
/// one under it that does not, if any. The folders a restore makes on the
/// way to `parent` are on the first one's filesystem, as no mount sits on
/// a folder that is not there, so a restore checks it before it makes any.
pub fn nearest_existing(parent: &Path) -> (&Path, Option<&Path>) {
    let missing = parent.ancestors().take_while(|a| !a.exists()).last();
    (missing.and_then(Path::parent).unwrap_or(parent), missing)
}

/// Delete one entry's worktree with `git worktree remove` (no `--force`)
/// and drop the entry. The caller holds the entry and has re-checked it
/// since: `registered` is Git's entry for it from that check, so the lock
/// lifted is the one there now. Git inherits `holds`, the entry's and its
/// removal record's. If Git refuses, the worktree is locked again and
/// stays in the trash. Once Git deleted it, an entry [`discard`] leaves
/// is the `Ok` value: the worktree is gone either way.
pub fn purge(item: &Item, registered: &Entry, holds: &[&Hold]) -> Result<Option<Stays>, String> {
    let worktree = item
        .worktree
        .as_ref()
        .ok_or("the entry names no worktree")?;
    let main = &item.command_dir;
    if locked_by_residue(registered) {
        git::unlock_worktree(main, worktree)?;
    } else if let Some(reason) = &registered.locked {
        return Err(format!("it is locked by someone else ({reason})"));
    }
    if let Err(e) = git::remove_worktree(main, worktree, holds) {
        if let Err(relock) = git::lock_worktree(main, worktree, LOCK) {
            return Err(format!("{e}; and could not lock it again: {relock}"));
        }
        return Err(e);
    }
    Ok(discard(&item.dir).err())
}

/// The `--keep-trash` a record keeps, when this release can read it.
fn own_keep(record: Option<&Record>) -> Option<&removed::Keep> {
    record
        .and_then(|r| r.keep.as_ref())
        .and_then(|k| k.as_ref().ok())
}

pub fn item_json(item: &Item) -> Json {
    let record = item.record.as_ref().ok();
    Json::Obj(vec![
        ("kind", Json::str("trash")),
        ("entry", Json::str(item.name.clone())),
        ("entry_path", Json::path(&item.dir)),
        (
            "worktree_path",
            Json::opt(item.worktree.as_deref(), Json::path),
        ),
        (
            "original_path",
            Json::opt(record.map(|r| r.original.as_path()), Json::path),
        ),
        (
            "branch",
            Json::opt(record.and_then(|r| r.branch.clone()), Json::Str),
        ),
        (
            "head",
            Json::opt(record.and_then(|r| r.head.clone()), Json::Str),
        ),
        (
            "trashed_at",
            Json::opt(record.map(|r| r.trashed), |t| Json::str(output::rfc3339(t))),
        ),
        (
            "allocated_bytes",
            Json::opt(record.and_then(|r| r.allocated), Json::UInt),
        ),
        (
            "reclaimable_bytes",
            Json::opt(record.and_then(|r| r.reclaimable), Json::UInt),
        ),
        ("registered", Json::Bool(item.registered.is_some())),
        (
            "error",
            match &item.record {
                Err(e) => Json::str(e.clone()),
                Ok(_) => Json::Null,
            },
        ),
        // The `--keep-trash` it was trashed with, as a removal record's
        // `keep`; null for an entry an earlier release made, or one whose
        // keep line this release cannot read.
        (
            "keep",
            Json::opt(own_keep(record), |k| Json::str(k.text.clone())),
        ),
        (
            "keep_seconds",
            Json::opt(own_keep(record), |k| Json::Int(k.seconds)),
        ),
        // Added last, so every field before it keeps its place.
        ("id", Json::str(item.id.clone())),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(keep: Option<(i64, &str)>) -> Record {
        Record {
            original: PathBuf::from("/src/app/.worktrees/w"),
            branch: Some("refs/heads/w".into()),
            head: Some("0123456789abcdef0123456789abcdef01234567".into()),
            trashed: 1_000,
            allocated: Some(10),
            reclaimable: Some(8),
            keep: keep.map(|(seconds, text)| {
                Ok(removed::Keep {
                    seconds,
                    text: text.into(),
                })
            }),
        }
    }

    /// An entry of `record`, holding its worktree, as `list` makes it.
    fn item(record: Record) -> Item {
        Item {
            name: "w-19700101T001640Z".into(),
            dir: PathBuf::from("/src/app/.git/residue-trash/w-19700101T001640Z"),
            common_dir: PathBuf::from("/src/app/.git"),
            command_dir: PathBuf::from("/src/app"),
            record: Ok(record),
            worktree: None,
            registered: None,
            incomplete: false,
            bare: false,
            modified: Some(1_000),
            extra: Some(Vec::new()),
            id: String::new(),
        }
    }

    /// An empty folder of the test's own.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "residue-trash-{test}-{}-{}",
            std::process::id(),
            output::now()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn info_keeps_the_keep_trash_it_was_written_with() {
        let entry = Path::new("/src/app/.git/residue-trash/w-19700101T001640Z");
        let bytes = record(Some((2_592_000, "30d"))).render(entry);
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("\nkeep 2592000 30d\n"), "{text}");
        let back = Record::parse(&bytes).unwrap();
        let keep = back.keep.unwrap().unwrap();
        assert_eq!((keep.seconds, keep.text.as_str()), (2_592_000, "30d"));
        // An entry an earlier release made has none, and reads as before.
        let old = Record::parse(&record(None).render(entry)).unwrap();
        assert!(old.keep.is_none());
        assert!(
            !String::from_utf8(record(None).render(entry))
                .unwrap()
                .contains("keep")
        );
        // One this release cannot read leaves the record readable, so it
        // can be restored, and it never expires.
        let damaged = text.replace("keep 2592000 30d", "keep soon");
        let read = Record::parse(damaged.as_bytes()).unwrap();
        assert!(matches!(&read.keep, Some(Err(line)) if line == "soon"));
        assert!(!item(read).expired(1_000 + 520 * 7 * 86_400, 86_400));
    }

    #[test]
    fn an_entry_expires_at_the_longer_of_the_run_s_keep_and_its_own() {
        let day = 86_400;
        let own = item(record(Some((30 * day, "30d"))));
        let trashed = 1_000;
        // A run with 7d keeps it until its own 30d have passed.
        assert!(!own.expired(trashed + 10 * day, 7 * day));
        assert!(!own.expired(trashed + 10 * day, day));
        assert!(own.expired(trashed + 30 * day, 7 * day));
        // A run with a longer keep keeps it longer.
        assert!(!own.expired(trashed + 31 * day, 60 * day));
        // One without its own keep has only the run's.
        let old = item(record(None));
        assert!(old.expired(trashed + 7 * day, 7 * day));
        assert!(!old.expired(trashed + 7 * day - 1, 7 * day));
    }

    #[test]
    fn discard_removes_nothing_from_an_entry_that_holds_more_than_its_record() {
        let dir = scratch("discard");
        let entry = dir.join("w-19700101T001640Z");
        fs::create_dir(&entry).unwrap();
        fs::write(
            entry.join(INFO),
            b"path /src/app/.worktrees/w\ntrashed 1000 x\n",
        )
        .unwrap();
        fs::write(entry.join("stray.txt"), b"stray").unwrap();
        match discard(&entry) {
            Err(Stays::Holds(at, names)) => {
                assert_eq!(at, entry);
                assert_eq!(names, vec!["stray.txt".to_string()]);
            }
            Err(Stays::Failed(e)) => panic!("failed: {e}"),
            Ok(()) => panic!("discarded an entry that holds stray.txt"),
        }
        // The record stays with what is left, and the entry is whole.
        assert!(entry.join(INFO).is_file());
        assert!(entry.join("stray.txt").is_file());
        // Once only the record is left, it goes, and so does the trash.
        fs::remove_file(entry.join("stray.txt")).unwrap();
        discard(&entry).map_err(|e| e.to_string()).unwrap();
        assert!(!entry.exists());
        assert!(!dir.exists());
    }

    #[test]
    fn an_entry_no_purge_can_clear_says_what_it_holds() {
        let hour = removed::INCOMPLETE_AFTER;
        // An empty record that also holds a file: kept, naming it, once it
        // is as old as a clearable one.
        let mut beside = item(record(None));
        beside.extra = Some(vec!["stray.txt".into()]);
        assert!(beside.is_empty_record() && !beside.holds_only_record());
        assert!(!beside.clearable(1_000 + hour));
        assert!(beside.stranded(1_000 + hour - 1).is_none());
        assert!(beside.stranded(1_000 + hour).unwrap().contains("stray.txt"));
        // With only its record, it is cleared instead.
        let only = item(record(None));
        assert!(only.stranded(1_000 + hour).is_none());
        assert!(only.clearable(1_000 + hour));
        // No info, and more than info.new: kept at any age.
        let mut lost = item(record(None));
        lost.record = Err("it has no info".into());
        lost.incomplete = true;
        lost.extra = Some(vec!["w".into()]);
        assert!(lost.stranded(0).unwrap().contains("it has no info"));
        // No info and nothing else: bare, which a purge clears an hour on.
        lost.extra = Some(Vec::new());
        lost.bare = true;
        assert!(lost.stranded(0).is_none());
        // A damaged info beside other files stays an error, as before.
        let mut damaged = item(record(None));
        damaged.record = Err("it has no path line".into());
        damaged.extra = Some(vec!["w".into()]);
        assert!(damaged.stranded(0).is_none());
    }
}
