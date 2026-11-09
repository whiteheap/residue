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

    /// An entry that holds only its record files: an empty record, or a
    /// bare entry whose `info` is missing or damaged.
    pub fn holds_only_record(&self) -> bool {
        self.is_empty_record() || (self.record.is_err() && self.bare)
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
            Ok(r) => self.is_empty_record() && settled(r.trashed),
            Err(_) => self.bare && self.modified.is_some_and(settled),
        }
    }

    pub fn age(&self, now: i64) -> Option<i64> {
        self.record.as_ref().ok().map(|r| now - r.trashed)
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
        let (record, incomplete, missing_or_damaged) = match fs::read(dir.join(INFO)) {
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
        let bare = missing_or_damaged && holds_only_info(&dir);
        let worktree = record
            .as_ref()
            .ok()
            .and_then(|r| r.original.file_name())
            .map(|n| dir.join(n));
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

/// Remove an entry's record and its now empty directory, and the trash
/// directory once it is empty. Never recursive: if anything else is left
/// in the entry, it stays and the error says so.
pub fn discard(entry: &Path) -> Result<(), String> {
    for file in [INFO, INFO_NEW] {
        match fs::remove_file(entry.join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove {}: {e}", entry.join(file).display())),
        }
    }
    fs::remove_dir(entry).map_err(|e| format!("cannot remove {}: {e}", entry.display()))?;
    if let Some(root) = entry.parent() {
        // Other entries may still be in the trash.
        remove_dir_if_empty(root).map_err(|e| format!("cannot remove {}: {e}", root.display()))?;
    }
    Ok(())
}

/// The entry directory holds nothing but `info` and `info.new`. One that
/// cannot be read counts as holding more.
fn holds_only_info(entry: &Path) -> bool {
    let Ok(listing) = fs::read_dir(entry) else {
        return false;
    };
    let mut only = true;
    for child in listing {
        match child {
            Ok(c) if c.file_name() == INFO || c.file_name() == INFO_NEW => {}
            _ => only = false,
        }
    }
    only
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
/// Returns the restored path, and a note when the record stays behind (the
/// next purge clears it).
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
    let note = discard(&item.dir)
        .err()
        .map(|e| format!("could not remove the trash record ({e}); the next remove run clears it"));
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
    // The folders it makes are on the filesystem of the nearest one above
    // them that exists, as no mount sits on a folder that is not there.
    let missing = parent.ancestors().take_while(|a| !a.exists()).last();
    let existing = missing.and_then(Path::parent).unwrap_or(parent);
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

/// Delete one entry's worktree with `git worktree remove` (no `--force`)
/// and drop the entry. The caller holds the entry and has re-checked it
/// since: `registered` is Git's entry for it from that check, so the lock
/// lifted is the one there now. Git inherits `holds`, the entry's and its
/// removal record's. If Git refuses, the worktree is locked again and
/// stays in the trash.
pub fn purge(item: &Item, registered: &Entry, holds: &[&Hold]) -> Result<(), String> {
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
    discard(&item.dir)
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
    ])
}
