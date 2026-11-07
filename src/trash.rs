//! The trash: worktrees `remove --trash` moved aside instead of deleting.
//!
//! Each entry is a directory `GIT_COMMON_DIR/residue-trash/NAME-STAMP/`
//! that holds the worktree itself and a record, `info`, with its original
//! path, branch, HEAD, the time it was trashed and its size. The worktree
//! stays registered with Git: `git worktree move` put it there, so Git's
//! record follows it, and it is locked with [`LOCK`] so `git worktree
//! prune` and other tools leave it alone. That keeps every way out a Git
//! command: `residue restore` moves it back with `git worktree move`, and
//! the purge deletes it with `git worktree remove` without `--force`.
//!
//! Why the common directory: it belongs to the repository, so the trash is
//! found from any of its worktrees and goes away with it; no worktree's
//! `git status` sees it; and a move there is one rename(2) whenever the
//! worktree is on the repository's filesystem, the usual layout
//! (`.worktrees/NAME` or a sibling directory). A worktree on another
//! filesystem is refused, never copied.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::git::{self, Entry};
use crate::json::Json;
use crate::output;

pub const DIR: &str = "residue-trash";
/// The lock reason on every worktree in the trash.
pub const LOCK: &str = "residue: in trash";
const INFO: &str = "info";

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
}

impl Item {
    /// The worktree directory is still in the entry.
    pub fn holds_worktree(&self) -> bool {
        self.worktree
            .as_ref()
            .is_some_and(|w| fs::symlink_metadata(w).is_ok())
    }

    /// A record whose worktree never arrived (residue stopped between
    /// writing `info` and the move) or was deleted by hand: nothing to
    /// keep, so any purge clears it.
    pub fn is_empty_record(&self) -> bool {
        self.record.is_ok() && !self.holds_worktree() && self.registered.is_none()
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
        let record = match fs::read(dir.join(INFO)) {
            Ok(bytes) => {
                Record::parse(&bytes).map_err(|e| format!("{}: {e}", dir.join(INFO).display()))
            }
            Err(e) => Err(format!("{}: {e}", dir.join(INFO).display())),
        };
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
/// and return the entry directory. The worktree is not moved yet; the
/// caller moves it to `entry.join(original's name)`.
pub fn create(common_dir: &Path, record: &Record) -> Result<PathBuf, String> {
    let name = record
        .original
        .file_name()
        .ok_or("the worktree path has no name")?;
    if record.original.as_os_str().as_bytes().contains(&b'\n') {
        return Err("its path contains a newline, which the trash record cannot hold".into());
    }
    let root = root(common_dir);
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
        if fs::symlink_metadata(crate::removed::root(common_dir).join(&entry_name)).is_ok() {
            continue;
        }
        let entry = root.join(&entry_name);
        match fs::create_dir(&entry) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot create {}: {e}", entry.display())),
        }
        let written = fs::File::create_new(entry.join(INFO)).and_then(|mut f| {
            f.write_all(&record.render(&entry))
                .and_then(|()| f.sync_all())
        });
        if let Err(e) = written {
            let message = format!("cannot write {}: {e}", entry.join(INFO).display());
            return Err(match discard(&entry) {
                Ok(()) => message,
                Err(more) => format!("{message}; {more}"),
            });
        }
        return Ok(entry);
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
    match fs::remove_file(entry.join(INFO)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove {}: {e}", entry.join(INFO).display())),
    }
    fs::remove_dir(entry).map_err(|e| format!("cannot remove {}: {e}", entry.display()))?;
    if let Some(root) = entry.parent() {
        // Other entries may still be in the trash.
        remove_dir_if_empty(root).map_err(|e| format!("cannot remove {}: {e}", root.display()))?;
    }
    Ok(())
}

/// Remove a directory if it is empty; one that still has entries stays and
/// is not an error. POSIX lets rmdir(2) report a non-empty directory as
/// ENOTEMPTY or EEXIST, and illumos uses EEXIST, which Rust calls
/// `AlreadyExists`. rmdir never reports EEXIST for anything else.
pub fn remove_dir_if_empty(dir: &Path) -> io::Result<()> {
    match fs::remove_dir(dir) {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists
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

/// Put one entry's worktree back where it was, or at `to`, re-registered
/// by `git worktree move`, and drop the entry. Returns the restored path,
/// and a note when the record stays behind (the next purge clears it).
pub fn restore(item: &Item, to: Option<&Path>) -> Result<(PathBuf, Option<String>), String> {
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
    let Some(registered) = &item.registered else {
        return Err(format!(
            "Git no longer lists {} as a worktree. If the repository moved, run `git worktree repair {}` in it, then restore again",
            worktree.display(),
            worktree.display()
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
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let here = fs::symlink_metadata(worktree)
        .map_err(|e| format!("{}: {e}", worktree.display()))?
        .dev();
    let there = fs::metadata(parent)
        .map_err(|e| format!("{}: {e}", parent.display()))?
        .dev();
    if here != there {
        return Err(format!(
            "{} is on another filesystem than the trash; residue moves, never copies",
            parent.display()
        ));
    }
    let main = &item.command_dir;
    let was_locked = registered.locked.is_some();
    if was_locked {
        git::unlock_worktree(main, worktree)?;
    }
    if let Err(e) = git::move_worktree(main, worktree, dest) {
        if was_locked {
            if let Err(relock) = git::lock_worktree(main, worktree, LOCK) {
                return Err(format!("{e}; and could not lock it again: {relock}"));
            }
        }
        return Err(e);
    }
    let note = discard(&item.dir)
        .err()
        .map(|e| format!("could not remove the trash record ({e}); the next remove run clears it"));
    Ok((dest.to_path_buf(), note))
}

/// Delete one entry's worktree with `git worktree remove` (no `--force`)
/// and drop the entry. The caller has re-checked it. If Git refuses, the
/// worktree is locked again and stays in the trash.
pub fn purge(item: &Item) -> Result<(), String> {
    if item.is_empty_record() {
        return discard(&item.dir);
    }
    let worktree = item
        .worktree
        .as_ref()
        .ok_or("the entry names no worktree")?;
    let registered = item
        .registered
        .as_ref()
        .ok_or("Git no longer lists it as a worktree")?;
    let main = &item.command_dir;
    if locked_by_residue(registered) {
        git::unlock_worktree(main, worktree)?;
    } else if let Some(reason) = &registered.locked {
        return Err(format!("it is locked by someone else ({reason})"));
    }
    if let Err(e) = git::remove_worktree(main, worktree) {
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
