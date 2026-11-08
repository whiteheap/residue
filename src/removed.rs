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
//! there after a crash.
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

use crate::git::{self, Checkout, Entry, RefChange};
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
/// What a restore that recreates a worktree says in a repository that
/// keeps its refs in reftable, where Git reads no `logs/HEAD` file to put
/// the old entries in.
const REFTABLE_REFLOG_NOTE: &str = "its HEAD reflog was not put back: in a reftable repository Git adds a reflog entry only as the newest one, so the old entries cannot go back in front of the new one, and `git reflog` there starts at this restore";
/// An entry with no `info` (the run that made it stopped first) and no ref
/// is cleared once its directory is this old, in seconds: a run writing
/// one takes milliseconds, so an hour cannot catch one at work.
pub const INCOMPLETE_AFTER: i64 = 3600;

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
                    let t = text();
                    let keep = t
                        .split_once(' ')
                        .filter(|(_, shown)| !shown.is_empty())
                        .and_then(|(seconds, shown)| {
                            let seconds = seconds.parse::<i64>().ok().filter(|s| *s > 0)?;
                            Some(Keep {
                                seconds,
                                text: shown.to_string(),
                            })
                        });
                    record.keep = Some(keep.ok_or("keep is not SECONDS DURATION")?);
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
}

impl Item {
    pub fn ref_name(&self) -> String {
        format!("{REF_PREFIX}{}", self.name)
    }

    pub fn interrupted(&self) -> bool {
        self.registered.is_some()
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

    pub fn state(&self) -> &'static str {
        if self.interrupted() {
            "interrupted"
        } else {
            "removed"
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
        let meta = child
            .metadata()
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = child.file_name().to_string_lossy().into_owned();
        if !meta.is_dir() || name.starts_with('.') {
            continue;
        }
        let (record, incomplete) = match fs::read(dir.join(INFO)) {
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
            dir,
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
    kept: Vec<String>,
    command_dir: PathBuf,
    /// The worktree's own Git directory, and the `config.worktree` and HEAD
    /// reflog the record copied from it.
    git_dir: Option<PathBuf>,
    config: Option<Vec<u8>>,
    reflog: Option<Vec<u8>>,
}

impl Written {
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

    /// What the worktree Git still lists, with `head` as its HEAD, lacks
    /// of what this record keeps: `None` when its HEAD contains the
    /// recorded HEAD, its HEAD reflog still begins with the one the record
    /// copied (so it names every commit the record's refs keep), and its
    /// `config.worktree` is the one the record copied. A `git worktree
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
        if let Err(e) = write_entry(&dir, &files, &info) {
            return Err(give_up(e));
        }
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
            config,
            reflog,
        });
    }
    Err(format!(
        "{}: too many records named {base}-{stamp}",
        root.display()
    ))
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
fn write_entry(dir: &Path, files: &[(&str, &[u8])], info: &[u8]) -> Result<(), String> {
    for (file, bytes) in files {
        write_new(&dir.join(file), bytes)
            .map_err(|e| format!("cannot write {}: {e}", dir.join(file).display()))?;
    }
    let staged = dir.join(INFO_NEW);
    write_new(&staged, info).map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
    fs::rename(&staged, dir.join(INFO))
        .map_err(|e| format!("cannot rename {}: {e}", staged.display()))?;
    sync_dir(dir)
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
/// entry stays, and the error says so.
fn discard(dir: &Path) -> Result<(), String> {
    for file in [INFO, INFO_NEW, CONFIG, REFLOG] {
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
/// then drop the record and its refs once the worktree holds all they
/// held. `listed` is every worktree Git lists for the repository, the main
/// one first.
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
    // Read before anything changes: another run may forget this record
    // meanwhile, and nothing else holds the copies.
    let (config, reflog) = read_copies(item)?;
    let mut restored = match &item.registered {
        Some(registered) => move_back(item, record, registered, dest)?,
        None => recreate(item, record, dest, listed)?,
    };
    // A worktree moved back kept its own Git directory, though a removal
    // that failed part way may have deleted its reflog or config.worktree
    // from it. A recreated one has a new one. Either way, until the
    // recorded config.worktree and reflog are in it, and it holds the
    // commits the refs keep, the record stays.
    let recreated = item.registered.is_none();
    let mut keep_record = false;
    match git::identity(&restored.path) {
        Ok(id) => {
            if let Err(e) = mark(&id.git_dir, &format!("removal record {}", item.name)) {
                restored.notes.push(format!(
                    "could not mark it as restored ({e}); `residue watch` may remove it again once its upstream is gone: lock it with `git worktree lock` to keep it"
                ));
            }
            // Git reads a `logs/HEAD` file only while the repository keeps
            // its refs in files. How it keeps them now decides, not how it
            // kept them when the record was written: `git refs migrate`
            // can change it in between.
            let reflog = match git::is_reftable(&restored.path) {
                Ok(false) if recreated => restore_reflog(&reflog, &id.git_dir),
                Ok(false) => reflog_if_lost(&reflog, &id.git_dir),
                Ok(true) => reftable_reflog(item, recreated),
                Err(e) => Err(format!(
                    "its HEAD reflog was not put back: residue could not tell whether the repository keeps its refs in reftable, where Git does not read the file it would write ({e})"
                )),
            };
            for step in [restore_config(&config, &id.git_dir), reflog] {
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
    Ok(restored)
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
    let taken = || {
        "another residue run forgot or restored this removal record after this restore listed it, so residue changed nothing; `residue restore` lists what is left".to_string()
    };
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
        if let How::Branch { tip } = how {
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
/// the directory, and let Git drop its record.
fn clear_steps(main: &Path, aside: &Path, ours: bool) -> String {
    let git = format!("git -C {} worktree", output::shell(main));
    let unlock = if ours {
        format!("run `{git} unlock {}`, ", output::shell(aside))
    } else {
        String::new()
    };
    format!(
        "{unlock}delete {}, run `{git} prune`, then run this restore again",
        aside.display()
    )
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
    let ours = registered.locked.as_deref() == Some(crate::remove::REMOVING);
    if left.running {
        // Git refuses to move a worktree it can no longer use, so there is
        // no command to run while that process may be removing it. PIDs are
        // reused, so the process may be another one that never exits.
        if let Some(why) = &left.unusable {
            return Err(format!(
                "process {pid} still runs, so residue may still be removing it, and Git can no longer use it ({why}); once that process has exited, run this restore again: it says what to clear; if it is not residue (`ps -p {pid}` shows what it is): {}",
                clear_steps(main, aside, ours),
                pid = left.pid
            ));
        }
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
    fn escaping_round_trips() {
        for raw in [&b"plain"[..], b"a\\b", b"a\nb", b"\\n", b"end\\"] {
            assert_eq!(unescape(&escape(raw)), raw);
            assert!(!escape(raw).contains(&b'\n'));
        }
    }
}
