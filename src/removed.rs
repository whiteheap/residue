//! Removal records: what residue writes right before it deletes a worktree,
//! so `residue restore` can bring it back.
//!
//! Every deletion (`remove` with or without the trash's purge, `remove
//! --no-trash`, a sweep, and `watch`'s child) first writes a record,
//! `GIT_COMMON_DIR/residue-removed/NAME-STAMP/info`, next to the trash, and
//! a ref, `refs/residue/removed/NAME-STAMP`, at the worktree's HEAD. The
//! record holds where the worktree was, its branch, HEAD and upstream, why
//! it was removed, and the ignored entries the deletion took. The ref keeps
//! the commits reachable after the branch is deleted (as after a merge), so
//! `git gc` cannot drop them. Both are written after the last check and
//! before `git worktree remove`; if either cannot be written, nothing is
//! deleted.
//!
//! `residue restore NAME` recreates the worktree at its path with `git
//! worktree add`, never `--force`, and never moves a branch that exists:
//! the branch at the recorded HEAD or ahead of it is checked out, a branch
//! that is gone is recreated there, and a branch that no longer contains
//! the recorded HEAD keeps its commits while a new branch is made at the
//! recorded HEAD. Ignored entries, such as `node_modules/`, are not
//! restored; the project's own install makes them again.
//!
//! A record and its ref expire after `--keep-removed` (default 30 days):
//! every remove run that purges the trash forgets them too, after the same
//! confirmation. residue deletes a ref only under `refs/residue/removed/`,
//! only the one a record names, and only while it still points at the
//! recorded HEAD.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::git::{self, Checkout, Entry};
use crate::json::Json;
use crate::output;

pub const DIR: &str = "residue-removed";
/// Where the refs that keep removed worktrees' commits live.
pub const REF_PREFIX: &str = "refs/residue/removed/";
/// The file residue writes into a restored worktree's own Git directory:
/// `watch`'s landed-work rule never removes that worktree again.
pub const MARKER: &str = "residue-restored";
const INFO: &str = "info";
/// A copy of the worktree's own `config.worktree`, when it had one.
const CONFIG: &str = "config.worktree";

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
    /// Unix seconds.
    pub removed: i64,
    pub allocated: Option<u64>,
    pub reclaimable: Option<u64>,
    /// The ignored entries the deletion took.
    pub gone: Vec<Gone>,
    /// The residue process that wrote it.
    pub pid: Option<u32>,
    pub version: Option<String>,
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
        line(
            "removed",
            format!("{} {}", self.removed, output::rfc3339(self.removed)).as_bytes(),
        );
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
            removed: 0,
            allocated: None,
            reclaimable: None,
            gone: Vec::new(),
            pid: None,
            version: None,
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
                b"removed" => removed = Some(number("removed")?),
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

/// One record in a repository.
pub struct Item {
    /// The entry directory's name, also the ref's last component:
    /// `fix-login-20261107T040000Z`.
    pub name: String,
    pub dir: PathBuf,
    pub common_dir: PathBuf,
    /// Where to run `git worktree` commands for this repository.
    pub command_dir: PathBuf,
    pub record: Result<Record, String>,
    /// Git still lists the worktree where the deletion started: the run
    /// stopped before it finished (an interrupted removal), so nothing of
    /// it is gone that Git's own record does not keep.
    pub registered: Option<Entry>,
}

impl Item {
    pub fn ref_name(&self) -> String {
        format!("{REF_PREFIX}{}", self.name)
    }

    pub fn interrupted(&self) -> bool {
        self.registered.is_some()
    }

    pub fn age(&self, now: i64) -> Option<i64> {
        self.record.as_ref().ok().map(|r| now - r.removed)
    }

    pub fn state(&self) -> &'static str {
        if self.interrupted() {
            "interrupted"
        } else {
            "removed"
        }
    }
}

/// The records of the repository at `common_dir`, oldest first. `listed`
/// is every worktree Git lists for it, so a record whose worktree Git still
/// lists where the deletion started is marked interrupted. A record that
/// cannot be read is listed with its error, and never forgotten or used.
pub fn of_repo(repo: &crate::discover::Repo) -> Result<Vec<Item>, String> {
    let listed: Vec<&Entry> = repo.trash.iter().chain(&repo.interrupted).collect();
    list(&repo.common_dir, &repo.command_dir(), &listed)
}

/// [`of_repo`] for a repository given by its directories.
pub fn list(common_dir: &Path, command_dir: &Path, listed: &[&Entry]) -> Result<Vec<Item>, String> {
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
        let registered = record.as_ref().ok().and_then(|r| {
            listed
                .iter()
                .find(|e| {
                    e.path == r.aside
                        || git::canonical(&e.path).ok().as_deref() == Some(r.aside.as_path())
                })
                .map(|e| (*e).clone())
        });
        items.push(Item {
            name,
            dir,
            common_dir: common_dir.to_path_buf(),
            command_dir: command_dir.to_path_buf(),
            record,
            registered,
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

/// A record residue has written and not yet given up.
pub struct Written {
    pub name: String,
    pub dir: PathBuf,
    head: String,
    command_dir: PathBuf,
}

impl Written {
    /// Drop the record and its ref: the worktree was not deleted after all.
    pub fn undo(&self) -> Result<(), String> {
        drop_record(&self.command_dir, &self.name, &self.dir, &self.head)
    }
}

/// Write the record for a worktree about to be deleted, then its ref. If
/// either fails, neither is left, and the caller must not delete.
/// `git_dir` is the worktree's own Git directory, whose `config.worktree`,
/// if any, the record keeps a copy of.
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
    let config = match git_dir.map(|d| fs::read(d.join(CONFIG))) {
        None => None,
        Some(Ok(bytes)) => Some(bytes),
        Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => None,
        Some(Err(e)) => return Err(format!("cannot read its config.worktree: {e}")),
    };
    let root = root(common_dir);
    fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let base = entry_base(&record.original);
    let stamp = output::stamp(record.removed);
    for n in 1..100 {
        let name = if n == 1 {
            format!("{base}-{stamp}")
        } else {
            format!("{base}-{stamp}-{n}")
        };
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
        if let Err(e) = write_new(&dir.join(INFO), &record.render(&name)) {
            return Err(give_up(format!(
                "cannot write {}: {e}",
                dir.join(INFO).display()
            )));
        }
        if let Some(bytes) = &config {
            if let Err(e) = write_new(&dir.join(CONFIG), bytes) {
                return Err(give_up(format!(
                    "cannot write {}: {e}",
                    dir.join(CONFIG).display()
                )));
            }
        }
        let full = format!("{REF_PREFIX}{name}");
        if let Err(e) = git::create_ref(command_dir, &full, &record.head) {
            return Err(give_up(format!("cannot create the ref {full}: {e}")));
        }
        return Ok(Written {
            name,
            dir,
            head: record.head.clone(),
            command_dir: command_dir.to_path_buf(),
        });
    }
    Err(format!(
        "{}: too many records named {base}-{stamp}",
        root.display()
    ))
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::File::create_new(path).and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
}

/// Remove an entry's files and its now empty directory, and the records
/// directory once it is empty. Never recursive: anything else left in the
/// entry stays, and the error says so.
fn discard(dir: &Path) -> Result<(), String> {
    for file in [INFO, CONFIG] {
        match fs::remove_file(dir.join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove {}: {e}", dir.join(file).display())),
        }
    }
    fs::remove_dir(dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
    if let Some(root) = dir.parent() {
        crate::trash::remove_dir_if_empty(root)
            .map_err(|e| format!("cannot remove {}: {e}", root.display()))?;
    }
    Ok(())
}

/// The ref first, only while it points at `head`, then the files: a record
/// left without its ref is forgotten again later, a ref left without its
/// record would never be.
fn drop_record(command_dir: &Path, name: &str, dir: &Path, head: &str) -> Result<(), String> {
    let full = format!("{REF_PREFIX}{name}");
    git::delete_ref(command_dir, &full, head)
        .map_err(|e| format!("cannot delete the ref {full}: {e}"))?;
    discard(dir)
}

/// Forget one record: its ref, then its files. An unreadable record is
/// never forgotten, since residue cannot tell which ref it would delete.
pub fn forget(item: &Item) -> Result<(), String> {
    let record = item.record.as_ref().map_err(|e| e.clone())?;
    drop_record(&item.command_dir, &item.name, &item.dir, &record.head)
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
    /// The branch no longer contains the recorded HEAD. It was left alone,
    /// and a new branch was made at the recorded HEAD.
    NewBranch {
        name: String,
        tip: String,
    },
    Detached,
}

impl How {
    pub fn code(&self) -> &'static str {
        match self {
            How::MovedBack(_) => "moved_back",
            How::Branch { .. } => "checked_out_branch",
            How::Recreated => "recreated_branch",
            How::NewBranch { .. } => "new_branch",
            How::Detached => "detached",
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
            "in the trash entry {name} (`residue restore {name}` puts it back)",
            name = output::clean(&name)
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

/// Bring back the worktree one record describes, at `to` or where it was,
/// then drop the record and its ref. `listed` is every worktree Git lists
/// for the repository, the main one first.
pub fn restore(item: &Item, to: Option<&Path>, listed: &[Entry]) -> Result<Restored, String> {
    let record = item.record.as_ref().map_err(|e| e.clone())?;
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
    let restored = match &item.registered {
        Some(registered) => move_back(item, record, registered, dest)?,
        None => recreate(item, record, dest, listed)?,
    };
    let mut restored = restored;
    // A recreated worktree's config.worktree is only in the record until
    // it is back in the Git directory; until then the record stays.
    let config_kept = item.registered.is_some() || !item.dir.join(CONFIG).exists();
    let mut keep_record = false;
    match git::identity(&restored.path) {
        Ok(id) => {
            if let Err(e) = mark(&id.git_dir, &format!("removal record {}", item.name)) {
                restored.notes.push(format!(
                    "could not mark it as restored ({e}); `residue watch` may remove it again once its upstream is gone: lock it with `git worktree lock` to keep it"
                ));
            }
            if item.registered.is_none() {
                match restore_config(&item.dir, &id.git_dir) {
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
            keep_record = !config_kept;
        }
    }
    if keep_record {
        restored.notes.push(format!(
            "the removal record stays at {}, with its config.worktree; a remove run forgets it after --keep-removed",
            item.dir.display()
        ));
    } else if let Err(e) = forget(item) {
        restored.notes.push(format!(
            "could not drop the removal record ({e}); a remove run forgets it after --keep-removed"
        ));
    }
    Ok(restored)
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
        let entry = aside
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Err(format!(
            "its purge from the trash did not finish, so it is still in the trash; `residue restore {}` puts it back",
            output::clean(&entry)
        ));
    }
    let Some(left) = crate::remove::Leftover::of(registered, main) else {
        return Err(format!(
            "Git still lists it at {}, where its removal started; residue cannot tell how it got there, so it leaves it",
            aside.display()
        ));
    };
    if left.running {
        return Err(format!(
            "process {} still runs, so residue may still be removing it; if it is not residue, put it back with: {}",
            left.pid, left.recovery
        ));
    }
    if !left.present {
        return Err(format!(
            "its removal stopped partway and its directory is gone; clear Git's record with `{}`, then run this restore again",
            left.recovery
        ));
    }
    let ours = registered.locked.as_deref() == Some(crate::remove::REMOVING);
    if registered.locked.is_some() && !ours {
        return Err(format!(
            "it is locked by someone else ({}); residue leaves that lock alone",
            registered.locked.as_deref().unwrap_or("")
        ));
    }
    let parent = dest.parent().ok_or("the destination has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let here = fs::symlink_metadata(aside)
        .map_err(|e| format!("{}: {e}", aside.display()))?
        .dev();
    let there = fs::metadata(parent)
        .map_err(|e| format!("{}: {e}", parent.display()))?
        .dev();
    if here != there {
        return Err(format!(
            "{} is on another filesystem than {}; residue moves, never copies",
            parent.display(),
            aside.display()
        ));
    }
    if ours {
        git::unlock_worktree(main, aside)?;
    }
    if let Err(e) = git::move_worktree(main, aside, dest) {
        if ours {
            if let Err(relock) = git::lock_worktree(main, aside, crate::remove::REMOVING) {
                return Err(format!("{e}; and could not lock it again: {relock}"));
            }
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

/// `git worktree add` at `dest`, on the recorded branch when that loses
/// nothing, and never moving a branch that exists.
fn recreate(
    item: &Item,
    record: &Record,
    dest: &Path,
    listed: &[Entry],
) -> Result<Restored, String> {
    let main = &item.command_dir;
    let head = record.head.as_str();
    if git::commit_of(main, head)?.is_none() {
        return Err(format!(
            "commit {} is no longer in the repository: its ref {} was deleted and Git dropped it",
            &head[..12],
            item.ref_name()
        ));
    }
    let checked_out = |full: &str| -> Option<String> {
        let first = listed.first()?;
        let trash_root = crate::trash::root(&item.common_dir);
        listed
            .iter()
            .find(|e| e.branch.as_deref() == Some(full))
            .map(|e| whereabouts(e, first, &trash_root))
    };
    let (how, branch) = match (&record.branch, record.short_branch()) {
        (Some(full), Some(short)) => match git::commit_of(main, full)? {
            None => {
                git::add_worktree(main, dest, &Checkout::NewBranch(short, head))?;
                (How::Recreated, Some(full.clone()))
            }
            Some(tip) if tip == head || git::is_ancestor(main, head, &tip)? => {
                if let Some(at) = checked_out(full) {
                    return Err(format!(
                        "branch {} is checked out {at}; Git allows one worktree per branch",
                        output::clean(short)
                    ));
                }
                git::add_worktree(main, dest, &Checkout::Branch(short))?;
                (How::Branch { tip }, Some(full.clone()))
            }
            Some(tip) => {
                let name = free_branch(main, &format!("{short}-restored"))?;
                git::add_worktree(main, dest, &Checkout::NewBranch(&name, head))?;
                let full = format!("refs/heads/{name}");
                (How::NewBranch { name, tip }, Some(full))
            }
        },
        _ => {
            git::add_worktree(main, dest, &Checkout::Detached(head))?;
            (How::Detached, None)
        }
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
fn restore_config(entry_dir: &Path, git_dir: &Path) -> Result<Option<String>, String> {
    let recorded = match fs::read(entry_dir.join(CONFIG)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read its recorded config.worktree: {e}")),
    };
    let target = git_dir.join(CONFIG);
    match fs::read(&target) {
        Ok(now) if now == recorded => Ok(None),
        Ok(_) => {
            let aside = git_dir.join(format!("{CONFIG}.residue-removed"));
            match write_new(&aside, &recorded) {
                Ok(()) => Ok(Some(format!(
                    "Git gave it a config.worktree that differs from the one it had; the one it had is at {}",
                    aside.display()
                ))),
                Err(e) => Err(format!(
                    "Git gave it a config.worktree that differs from the one it had, and residue could not save that one next to it ({e})"
                )),
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => match write_new(&target, &recorded) {
            Ok(()) => Ok(None),
            Err(e) => Err(format!("could not put back its config.worktree ({e})")),
        },
        Err(e) => Err(format!("could not read {}: {e}", target.display())),
    }
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
        ("state", Json::str(item.state())),
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
        (
            "removed_at",
            Json::opt(r.map(|r| r.removed), |t| Json::str(output::rfc3339(t))),
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
        (
            "worktree_config",
            Json::Bool(item.dir.join(CONFIG).is_file()),
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
            removed: 1_794_000_000,
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
        }
    }

    #[test]
    fn a_record_reads_back_as_written() {
        let r = sample();
        let back = Record::parse(&r.render("fix_login_new-20261107T040000Z")).unwrap();
        assert_eq!(back, r);
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
    fn escaping_round_trips() {
        for raw in [&b"plain"[..], b"a\\b", b"a\nb", b"\\n", b"end\\"] {
            assert_eq!(unescape(&escape(raw)), raw);
            assert!(!escape(raw).contains(&b'\n'));
        }
    }
}
