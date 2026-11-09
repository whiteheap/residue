//! Disk usage of one worktree, and free space on its filesystem.
//!
//! The walk never follows symbolic links and never crosses into another
//! filesystem. It opens each folder relative to its parent's open
//! descriptor with `O_NOFOLLOW`, so a folder that becomes a symbolic link,
//! or another folder, before the walk opens it is reported as changed and
//! never read. A folder already open is read through its descriptor, as
//! any walk reads it: renamed away or mounted over after that, it is still
//! the folder the listing saw. It records what would make a removal
//! surprising: nested repositories, mount points and entries it could not
//! read. The same walk attributes files and bytes to each ignored entry (a
//! bucket), and judges the contents of a collapsed ignored directory that
//! a path rule reaches into, so the worktree is read once.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::fs;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

/// The most entries residue judges one by one under a collapsed ignored
/// directory. Past it the directory is not called regenerable.
pub const EXPAND_LIMIT: u64 = 100_000;

/// How many levels of folders one walk keeps open: the worktree (level 0)
/// to level `HELD - 1`. The walk opens each folder from its parent's
/// descriptor, so it keeps a folder open while subfolders of it wait to be
/// listed: about one descriptor per level of the path it is on. A folder
/// deeper than that is closed once listed, unless it is one of the
/// [`REOPENED`] kept in [`Reopened`], and its subfolders are opened from
/// the deepest folder still open above them, one name at a time, each
/// checked against the device and inode its listing showed
/// (`Pending::open`). So a walk holds at most `HELD + REOPENED + 2`
/// descriptors however deep the tree, 22, and `scan`'s 8 workers at most
/// 176: launchd starts a login service with a soft limit of 256, and Git's
/// pipes need some of the rest. The limit is not raised, because nothing
/// needs more. The deepest folder in five large local worktrees and
/// checkouts (41,650 to 112,595 entries, 2026-11-08) was 14 levels down,
/// so in practice no folder is reopened.
const HELD: usize = 16;

/// How the walk opens a folder to list it: for reading, as opendir(3)
/// does, and never through a symbolic link.
const DIR: libc::c_int =
    libc::O_RDONLY | libc::O_NONBLOCK | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// How the walk opens a folder above the worktree, only to look the next
/// name up in it: with search permission alone, as a lookup of the full
/// path needs. `O_PATH` on Linux; `O_SEARCH` on macOS, FreeBSD and illumos
/// (macOS 12 and earlier ignore its `O_EXEC` bit and open for reading, as
/// xnu-8019's `open1` shows). NetBSD, OpenBSD and DragonFly document no
/// such flag, so there a folder above the worktree must also be readable,
/// or the worktree's contents are `unknown`.
#[cfg(any(target_os = "linux", target_os = "android"))]
const ABOVE: libc::c_int = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "illumos",
    target_os = "solaris"
))]
const ABOVE: libc::c_int =
    libc::O_SEARCH | libc::O_NONBLOCK | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "illumos",
    target_os = "solaris"
)))]
const ABOVE: libc::c_int = DIR;

#[derive(Clone, Default)]
pub struct Usage {
    /// Sum of file lengths, each hard-linked inode counted once.
    pub apparent: u64,
    /// Blocks allocated, each hard-linked inode counted once.
    pub allocated: u64,
    /// Blocks that removal frees: excludes inodes that also have links
    /// outside the worktree (a pnpm store, for example). Copy-on-write
    /// clones and snapshots are not detected, so even this can overstate.
    pub reclaimable: u64,
    pub entries: u64,
    pub newest_mtime: Option<i64>,
    /// The entry with that mtime (the first one seen, on a tie).
    pub newest_path: Option<PathBuf>,
    pub unreadable: u64,
    pub first_error: Option<String>,
    /// Each entry named `.git`, in any case, other than the root's own
    /// link: another repository or checkout, or files Git does not list.
    pub nested_repos: Vec<PathBuf>,
    pub mount_points: Vec<PathBuf>,
    /// One result per bucket passed to [`measure`], in the same order.
    pub buckets: Vec<BucketUsage>,
    pub reach: Reach,
}

/// What the walk reached that the activity check judges with it
/// (`activity::Snapshot::evidence`).
#[derive(Clone, Default)]
pub struct Reach {
    /// The length in bytes of the longest path the walk listed, the root's
    /// own included: a process probe names a path whole only up to a
    /// limit.
    pub longest: usize,
    /// Each `(device, inode)` of the `wanted` list passed to [`measure`]
    /// that the walk listed, the root included, in the order it found them.
    pub found: Vec<(u64, u64)>,
    /// The first entry the walk could not read, with why: one of `wanted`
    /// may lie behind it.
    pub unread: Option<String>,
}

impl Reach {
    fn note(&mut self, path: &Path, meta: &Stat, wanted: &[(u64, u64)]) {
        self.longest = self.longest.max(path.as_os_str().len());
        if !wanted.is_empty() {
            let id = (meta.dev(), meta.ino());
            if wanted.binary_search(&id).is_ok() && !self.found.contains(&id) {
                self.found.push(id);
            }
        }
    }
}

/// An ignored entry whose contents the walk attributes to it.
pub struct Bucket {
    /// Relative to the worktree root, without a trailing slash.
    pub rel: PathBuf,
    /// Judge every entry below it with the rule matcher (a collapsed
    /// directory that a path rule reaches into).
    pub expand: bool,
}

#[derive(Clone, Default)]
pub struct BucketUsage {
    /// False when the walk never reached the entry (it vanished, lies
    /// behind an unreadable directory or a mount point, or Git spelled its
    /// path in a way the walk did not find). `inspect::judge` then calls it
    /// not judged, whatever the rules say.
    pub seen: bool,
    /// Non-directory entries, each hard-linked inode counted once.
    pub files: u64,
    pub bytes: u64,
    /// The newest modification time of the entry or anything below it, in
    /// seconds and nanoseconds, so `remove` can tell that it changed even
    /// when its file count and size did not.
    pub newest: Option<(i64, i64)>,
    pub expansion: Expansion,
    /// A regular file or a symbolic link is at or below the entry, outside
    /// any `.git` entry, every hard link counted: the kinds Git lists, so
    /// Git lists a directory that holds one (#83).
    pub holds_file: bool,
    /// An entry named `.git`, in any case, is at or below the entry. Git
    /// skips that name, and lists a directory holding nothing else only
    /// when it makes a nested repository, which only Git decides.
    pub holds_git: bool,
    /// Part of the entry was not read: an unreadable entry or a mount point
    /// is at or below it.
    pub incomplete: bool,
    /// The first environment file (`policy::env_file`) strictly inside the
    /// entry, outside `.git` entries and package folders, that no rule names
    /// itself, relative to the worktree root.
    pub env_file: Option<String>,
}

#[derive(Clone, Default)]
pub enum Expansion {
    #[default]
    NotRequested,
    /// Every non-directory entry below it matches a rule.
    Matched,
    /// The first entry no rule matches, relative to the worktree root.
    Unmatched(String),
    /// More than [`EXPAND_LIMIT`] entries.
    OverLimit,
    /// Part of it could not be read.
    Unreadable(String),
}

struct Shared {
    seen: u64,
    links: u64,
    blocks: u64,
}

/// Where a walked entry sits relative to the buckets.
#[derive(Clone, Copy)]
struct Place {
    bucket: Option<usize>,
    /// A rule already matched this entry or one of its ancestors below the
    /// bucket, so nothing under it needs judging.
    covered: bool,
    /// The entry is, or is inside, an entry named `.git` below the bucket.
    in_git: bool,
    /// The entry is, or is inside, a package folder (`policy::PACKAGE_DIRS`).
    in_package: bool,
}

/// Measures the worktree at `root`, which must be canonical
/// (`git::canonical`): the walk refuses a symbolic link anywhere on it.
/// `matches` is the rule matcher (`Rules::matches`), and `named` says
/// whether a rule names a file itself (`Rules::names_file`). `wanted` is a
/// sorted list of `(device, inode)` pairs whose presence the walk reports
/// in `Usage::reach` (`activity::Snapshot::wanted`).
pub fn measure(
    root: &Path,
    buckets: &[Bucket],
    matches: &dyn Fn(&str) -> bool,
    named: &dyn Fn(&str) -> bool,
    wanted: &[(u64, u64)],
) -> io::Result<Usage> {
    let (top, opened) = open_root(root)?;
    let device = top.dev();
    let mut usage = Usage {
        buckets: vec![BucketUsage::default(); buckets.len()],
        ..Usage::default()
    };
    usage.reach.note(root, &top, wanted);
    let lookup: HashMap<PathBuf, usize> = buckets
        .iter()
        .enumerate()
        .map(|(i, b)| (root.join(&b.rel), i))
        .collect();
    let mut walked = vec![0u64; buckets.len()];
    let mut shared: HashMap<(u64, u64), Shared> = HashMap::new();
    account(&mut usage, &mut shared, &top, root);
    let top_fd = match opened {
        Ok(fd) => Rc::new(fd),
        Err(e) => {
            // As a folder below it that fails to open: one unreadable entry.
            usage.unreadable_entry(root, &e);
            return Ok(usage);
        }
    };
    let mut next = Some(Folder {
        below: Parent::Open(Rc::clone(&top_fd)),
        fd: top_fd,
        path: root.to_path_buf(),
        depth: 0,
        place: Place {
            bucket: None,
            covered: false,
            in_git: false,
            in_package: false,
        },
    });
    let mut stack: Vec<Pending> = Vec::new();
    let mut kept = Reopened::default();
    let mut buf = Vec::new();
    loop {
        let folder = match next.take() {
            Some(folder) => folder,
            None => {
                let Some(pending) = stack.pop() else {
                    break;
                };
                match pending.open(&mut kept) {
                    Ok(folder) => folder,
                    Err((path, place, e)) => {
                        usage.unreadable_entry(&path, &e);
                        judge_unreadable(&mut usage, buckets, place, &path, root, &e);
                        continue;
                    }
                }
            }
        };
        let Folder {
            fd,
            path: dir,
            depth,
            place,
            below,
        } = folder;
        let listing = list(fd.as_fd(), &dir, &mut buf);
        let before = stack.len();
        let listing = match listing {
            Ok(listing) => listing,
            Err(e) => {
                usage.unreadable_entry(&dir, &e);
                judge_unreadable(&mut usage, buckets, place, &dir, root, &e);
                continue;
            }
        };
        for item in listing {
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    usage.unreadable_entry(&dir, &e);
                    judge_unreadable(&mut usage, buckets, place, &dir, root, &e);
                    continue;
                }
            };
            let path = item.path;
            // Neither listing follows symbolic links.
            let meta = match item.stat {
                Ok(meta) => meta,
                Err(e) => {
                    usage.unreadable_entry(&path, &e);
                    judge_unreadable(&mut usage, buckets, place, &path, root, &e);
                    continue;
                }
            };
            usage.reach.note(&path, &meta, wanted);
            // Any case of `.git`, at any level, as Git skips the name when
            // `core.ignorecase` is set, which hides the files inside from
            // `git status` (#102). Only the root's own link, named exactly
            // `.git`, is not one. A case-sensitive configuration lists a
            // `.GIT` folder's files, so there this keeps a worktree that
            // need not be kept: the safe direction. So does the root's link
            // stored in another case, on a case-insensitive file system.
            if item.git_name && (dir != root || path.file_name().is_some_and(|n| n != ".git")) {
                usage.nested_repos.push(path.clone());
            }
            let mut here = place;
            if here.bucket.is_none() {
                // Git's path, which can spell the names another way (#103),
                // by its bytes.
                let found = lookup
                    .get(&path)
                    .or_else(|| git_path(&path, root).and_then(|rel| lookup.get(&root.join(rel))));
                if let Some(&i) = found {
                    here.bucket = Some(i);
                    usage.buckets[i].seen = true;
                    if buckets[i].expand {
                        usage.buckets[i].expansion = Expansion::Matched;
                    }
                }
            } else if let Some(i) = here.bucket {
                // An entry strictly inside a bucket.
                if buckets[i].expand && !here.covered {
                    walked[i] += 1;
                    let state = &mut usage.buckets[i].expansion;
                    if walked[i] > EXPAND_LIMIT {
                        if matches!(state, Expansion::Matched) {
                            *state = Expansion::OverLimit;
                        }
                        here.covered = true;
                    } else if matches!(state, Expansion::Matched) {
                        // A rule matches Git's spelling, and also the disk's.
                        if relative(&path, root).is_some_and(|rel| matches(&rel))
                            || git_spelling(&path, root).is_some_and(|rel| matches(&rel))
                        {
                            here.covered = true;
                        } else if !meta.is_dir() || meta.dev() != device {
                            // A file no rule matches, or a mount point whose
                            // contents the walk cannot judge.
                            *state = Expansion::Unmatched(display(&path, root));
                        }
                    }
                }
            }
            if meta.is_dir() && meta.dev() != device {
                if let Some(i) = here.bucket {
                    usage.buckets[i].incomplete = true;
                }
                usage.mount_points.push(path);
                continue;
            }
            let first = account(&mut usage, &mut shared, &meta, &path);
            if let Some(i) = here.bucket {
                let stamp = (meta.mtime(), meta.mtime_nsec());
                let newest = &mut usage.buckets[i].newest;
                if newest.is_none_or(|n| stamp > n) {
                    *newest = Some(stamp);
                }
                if item.git_name {
                    usage.buckets[i].holds_git = true;
                    here.in_git = true;
                }
                if meta.is_file() && !here.in_git {
                    usage.buckets[i].holds_file = true;
                    // Strictly inside the bucket: the bucket entry itself
                    // is judged by the rules alone, as before.
                    if place.bucket.is_some()
                        && !here.in_package
                        && usage.buckets[i].env_file.is_none()
                        && path.file_name().is_some_and(crate::policy::env_file)
                        && !relative(&path, root).is_some_and(|rel| named(&rel))
                        && !git_spelling(&path, root).is_some_and(|rel| named(&rel))
                    {
                        usage.buckets[i].env_file = Some(display(&path, root));
                    }
                }
                if !meta.is_dir() && first {
                    usage.buckets[i].files += 1;
                    usage.buckets[i].bytes += meta.len();
                }
            }
            if meta.is_dir()
                && path
                    .file_name()
                    .is_some_and(|n| crate::policy::PACKAGE_DIRS.iter().any(|p| n == *p))
            {
                here.in_package = true;
            }
            if meta.is_dir() {
                match leaf(&path) {
                    Ok(name) => stack.push(Pending {
                        path,
                        name,
                        id: (meta.dev(), meta.ino()),
                        depth: depth + 1,
                        place: here,
                        parent: below.clone(),
                    }),
                    Err(e) => {
                        usage.unreadable_entry(&path, &e);
                        judge_unreadable(&mut usage, buckets, here, &path, root, &e);
                    }
                }
            }
        }
        // A folder deeper than HELD stays open only while subfolders of it
        // wait, in `kept`; `below` keeps one above HELD open.
        if let Parent::Below(_, step) = &below {
            if stack.len() > before {
                kept.push(Rc::clone(step), fd, Some(stack.len() - before));
            }
        }
    }
    for inode in shared.values() {
        if inode.seen >= inode.links {
            usage.reclaimable += inode.blocks;
        }
    }
    Ok(usage)
}

/// A folder the walk has open and is about to list.
struct Folder {
    fd: Rc<OwnedFd>,
    path: PathBuf,
    depth: usize,
    place: Place,
    /// Where its subfolders are opened from.
    below: Parent,
}

/// A folder the walk has seen in its parent's listing and not yet opened.
struct Pending {
    path: PathBuf,
    /// Its name in its parent.
    name: CString,
    /// The device and inode its parent's listing showed.
    id: (u64, u64),
    depth: usize,
    place: Place,
    parent: Parent,
}

/// Where the walk opens a folder from.
#[derive(Clone)]
enum Parent {
    /// Its parent, still open.
    Open(Rc<OwnedFd>),
    /// Its parent is deeper than [`HELD`], so it stays open only while
    /// [`Reopened`] keeps it: the deepest folder held above it, and its
    /// parent as the last of the folders from there down, each as its own
    /// parent listed it.
    Below(Rc<OwnedFd>, Rc<Step>),
}

/// A folder deeper than [`HELD`], as its parent's listing showed it.
struct Step {
    name: CString,
    id: (u64, u64),
    /// The folder above it, `None` for the first one below the deepest
    /// folder held.
    up: Option<Rc<Step>>,
    /// How far below the deepest folder held it is: 1 for the first.
    level: usize,
}

/// The folders deeper than [`HELD`] that the walk keeps open, shallowest
/// first: at most [`REOPENED`] folders of the path it is on. The walk is
/// depth-first, so a folder waiting to be opened always has its parent on
/// that path, and when that parent is kept here the folder opens from it,
/// rather than reopening every folder from the deepest one held. A folder
/// with no subfolders left waiting is dropped first, so what stays is the
/// folders that subfolders still wait in. An entry is matched by the `Rc`
/// of its step, which it holds, so that address cannot be reused for
/// another folder while it is kept.
#[derive(Default)]
struct Reopened(Vec<Kept>);

struct Kept {
    step: Rc<Step>,
    fd: Rc<OwnedFd>,
    /// How many subfolders of it wait to be opened, when the walk knows:
    /// it does for a folder it listed, not for one it reopened.
    waiting: Option<usize>,
}

/// How many folders [`Reopened`] keeps. Measured 2026-11-08 on a 41-level
/// chain under `node_modules` ending in a folder of 3,000 leaf folders,
/// 1,000 folders three deep, 500 six deep, 30 by 30 and 12 by 12 by 12:
/// with 4, `explain` was as fast as opening each folder by its full path
/// on all five. With 2 the 12 by 12 by 12 tree was slower. Without it,
/// every folder past [`HELD`] reopened its whole chain, 5 to 9 times the
/// system time.
const REOPENED: usize = 4;

impl Reopened {
    /// Keeps the folders that `to` is or lies below, drops the rest, and
    /// returns the deepest one kept. When that is `to`, one subfolder of it
    /// is being opened, so one fewer waits.
    fn keep_above(&mut self, to: &Rc<Step>) -> Option<(usize, Rc<OwnedFd>)> {
        while let Some(kept) = self.0.last_mut() {
            let mut at = to;
            while at.level > kept.step.level {
                match &at.up {
                    Some(up) => at = up,
                    None => break,
                }
            }
            if Rc::ptr_eq(at, &kept.step) {
                if Rc::ptr_eq(to, &kept.step) {
                    kept.waiting = kept.waiting.map(|n| n.saturating_sub(1));
                }
                return Some((kept.step.level, Rc::clone(&kept.fd)));
            }
            self.0.pop();
        }
        None
    }

    /// Keeps `fd`, the folder at `step`, as the deepest. It drops the
    /// folders no subfolder waits in, then the shallowest past
    /// [`REOPENED`].
    fn push(&mut self, step: Rc<Step>, fd: Rc<OwnedFd>, waiting: Option<usize>) {
        self.0.retain(|kept| kept.waiting != Some(0));
        if self.0.len() == REOPENED {
            self.0.remove(0);
        }
        self.0.push(Kept { step, fd, waiting });
    }
}

/// Opens the folder at `to` from the deepest folder kept above it, or from
/// `base`, the deepest folder held, one name at a time, each checked
/// against the device and inode its listing showed (`open_listed`), and
/// keeps each one it opens. A failure gives the level of the folder that
/// failed.
fn reopen(
    base: &Rc<OwnedFd>,
    to: &Rc<Step>,
    kept: &mut Reopened,
) -> Result<Rc<OwnedFd>, (usize, io::Error)> {
    let (have, mut at) = kept.keep_above(to).unwrap_or_else(|| (0, Rc::clone(base)));
    let mut missing = Vec::with_capacity(to.level - have);
    let mut step = to;
    while step.level > have {
        missing.push(step);
        match &step.up {
            Some(up) => step = up,
            None => break,
        }
    }
    for step in missing.into_iter().rev() {
        let fd = open_listed(at.as_fd(), &step.name, step.id).map_err(|e| (step.level, e))?;
        at = Rc::new(fd);
        kept.push(Rc::clone(step), Rc::clone(&at), None);
    }
    Ok(at)
}

impl Pending {
    /// Opens it, and returns where its subfolders will be opened from. A
    /// failure names the folder that failed, this one or a folder above it
    /// that had to be reopened, with the place to judge it in.
    fn open(self, kept: &mut Reopened) -> Result<Folder, (PathBuf, Place, io::Error)> {
        let fd = match &self.parent {
            Parent::Open(at) => {
                // Every folder kept is in a subtree the walk has finished.
                kept.0.clear();
                open_listed(at.as_fd(), &self.name, self.id)
            }
            Parent::Below(base, parent) => match reopen(base, parent, kept) {
                Ok(at) => open_listed(at.as_fd(), &self.name, self.id),
                Err((level, e)) => {
                    // The folder at `level` is `parent.level - level + 1` up.
                    let up = self.path.ancestors().nth(parent.level - level + 1);
                    let path = up.unwrap_or(&self.path).to_path_buf();
                    return Err((path, self.place, e));
                }
            },
        };
        let fd = match fd {
            Ok(fd) => Rc::new(fd),
            Err(e) => return Err((self.path, self.place, e)),
        };
        let below = if self.depth < HELD {
            Parent::Open(Rc::clone(&fd))
        } else {
            let (base, up) = match self.parent {
                Parent::Open(base) => (base, None),
                Parent::Below(base, parent) => (base, Some(parent)),
            };
            let level = up.as_ref().map_or(1, |up| up.level + 1);
            let step = Step {
                name: self.name,
                id: self.id,
                up,
                level,
            };
            Parent::Below(base, Rc::new(step))
        };
        Ok(Folder {
            fd,
            path: self.path,
            depth: self.depth,
            place: self.place,
            below,
        })
    }
}

/// The last component of a listed entry's path: the name its listing gave,
/// which holds no `/` and is never `.` or `..`.
fn leaf(path: &Path) -> io::Result<CString> {
    CString::new(path.file_name().unwrap_or_default().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains a NUL byte"))
}

/// Opens the worktree's folder from `/`, one name at a time, each relative
/// to the folder before it and never through a symbolic link. `root` is
/// canonical, so a symbolic link on it means the path changed since. An
/// `Err` is a folder above the worktree that fails to open, or a worktree
/// that is not a folder. Otherwise the worktree's own lstat comes back
/// with its open, which can fail on its own: a worktree residue may look
/// up but not read is still one entry, counted unreadable.
fn open_root(root: &Path) -> io::Result<(Stat, io::Result<OwnedFd>)> {
    let mut names = Vec::new();
    for part in root.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => names.push(name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a canonical path", root.display()),
                ));
            }
        }
    }
    if !root.has_root() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not an absolute path", root.display()),
        ));
    }
    let Some((last, above)) = names.split_last() else {
        // SAFETY: open reads the NUL-terminated literal; the descriptor it
        // returns is ours.
        let fd = unsafe { libc::open(c"/".as_ptr(), DIR) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the descriptor is open and nothing else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        return Ok((fstat(fd.as_fd())?, Ok(fd)));
    };
    // SAFETY: as above.
    let fd = unsafe { libc::open(c"/".as_ptr(), ABOVE) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor is open and nothing else owns it.
    let mut at = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut reached = PathBuf::from("/");
    let at_path = |reached: &Path, e: io::Error| {
        io::Error::new(e.kind(), format!("{}: {e}", reached.display()))
    };
    for name in above {
        reached.push(name);
        at = leaf(&reached)
            .and_then(|c| open_dir(at.as_fd(), &c, ABOVE))
            .map_err(|e| at_path(&reached, e))?;
    }
    reached.push(last);
    let name = leaf(&reached).map_err(|e| at_path(&reached, e))?;
    let st = lstat_raw(at.as_fd(), &name).map_err(|e| at_path(&reached, e))?;
    match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => {}
        libc::S_IFLNK => return Err(at_path(&reached, changed("it is a symbolic link now"))),
        _ => return Err(at_path(&reached, changed("it is not a folder now"))),
    }
    let top = Stat::from(&st);
    Ok((top, open_listed(at.as_fd(), &name, (top.dev(), top.ino()))))
}

/// Opens a folder the walk listed, relative to the folder it was listed
/// in, and checks that it is still the one the listing saw: another folder
/// renamed into its place, or a file system mounted on it, has another
/// device or inode.
fn open_listed(at: BorrowedFd, name: &CStr, id: (u64, u64)) -> io::Result<OwnedFd> {
    let fd = open_dir(at, name, DIR)?;
    let now = fstat(fd.as_fd())?;
    if (now.dev(), now.ino()) != id {
        return Err(changed("another folder is in its place"));
    }
    Ok(fd)
}

/// `openat(at, name, flags)`, where `flags` hold `O_DIRECTORY` and
/// `O_NOFOLLOW`. When it fails, lstat says what is there now, because the
/// error for a symbolic link differs by system: `ELOOP` on Linux, OpenBSD
/// and illumos, `EMLINK` on FreeBSD and DragonFly, `EFTYPE` on NetBSD, and
/// `ENOTDIR` on macOS with `O_DIRECTORY` set. A folder that became a link
/// or a file is a change during the walk; any other failure keeps its own
/// error.
fn open_dir(at: BorrowedFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: openat reads the NUL-terminated name, which outlives the
    // call; the descriptor it returns is ours.
    let fd = unsafe { libc::openat(at.as_raw_fd(), name.as_ptr(), flags) };
    if fd >= 0 {
        // SAFETY: the descriptor is open and nothing else owns it.
        return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    let error = io::Error::last_os_error();
    match lstat_raw(at, name).map(|st| st.st_mode & libc::S_IFMT) {
        Ok(libc::S_IFLNK) => Err(changed("it is a symbolic link now")),
        Ok(kind) if kind != libc::S_IFDIR => Err(changed("it is not a folder now")),
        _ => Err(error),
    }
}

fn changed(now: &str) -> io::Error {
    io::Error::other(format!("changed during the walk: {now}"))
}

/// Part of a bucket being judged could not be read, so it cannot be called
/// regenerable.
fn judge_unreadable(
    usage: &mut Usage,
    buckets: &[Bucket],
    place: Place,
    path: &Path,
    root: &Path,
    error: &io::Error,
) {
    if let Some(i) = place.bucket {
        usage.buckets[i].incomplete = true;
        let state = &mut usage.buckets[i].expansion;
        if buckets[i].expand && !place.covered && matches!(state, Expansion::Matched) {
            *state = Expansion::Unreadable(format!("{}: {error}", display(path, root)));
        }
    }
}

/// The path relative to the worktree root, when it is valid UTF-8. A path
/// that is not cannot match a rule, so it stays unmatched.
fn relative(path: &Path, root: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .and_then(|rel| rel.to_str())
        .map(str::to_string)
}

/// The path relative to the worktree root as Git prints it, when that is
/// not how the disk spells it. On macOS, where `git init` sets
/// `core.precomposeunicode`, Git precomposes (NFC) each name it reads that
/// holds a non-ASCII byte, one name at a time, with `iconv` from UTF-8-MAC
/// to UTF-8, and keeps a name the conversion refuses as stored
/// (`precompose_utf8_readdir` in Git's `compat/precompose_utf8.c`). A name
/// stored decomposed (NFD) then never equals Git's path byte for byte
/// (#103). Apple's converter refuses a name with a character above U+FFFF,
/// such as an emoji, so one path can hold a name Git composed and a name it
/// kept: this converts each name on its own, as Git does. Bytes, as Git
/// prints them, so a name that is not UTF-8, which Git keeps too, cannot
/// lose the match. None when no name changes: an ASCII path, one whose
/// every name the conversion leaves alone or refuses, and on other
/// systems, where Git prints every name as stored. The walk tries the
/// disk's spelling first, which is Git's when the setting is off.
fn git_path(path: &Path, root: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(root).ok()?.as_os_str().as_bytes();
    if rel.is_ascii() || !cfg!(target_os = "macos") {
        return None;
    }
    let spelled = spell_names(rel, precompose)?;
    Some(PathBuf::from(OsStr::from_bytes(&spelled)))
}

/// [`git_path`] as text, for the rule matchers: a path that is not UTF-8
/// matches no rule.
fn git_spelling(path: &Path, root: &Path) -> Option<String> {
    git_path(path, root)?.into_os_string().into_string().ok()
}

/// `rel` with each name that holds a non-ASCII byte replaced by what
/// `convert` makes of it, and each name it refuses or leaves alone kept as
/// it is. None when no name changes.
fn spell_names(rel: &[u8], convert: impl Fn(&[u8]) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(rel.len());
    let mut changed = false;
    for (i, name) in rel.split(|&b| b == b'/').enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        match (!name.is_ascii()).then(|| convert(name)).flatten() {
            Some(spelled) if spelled != name => {
                out.extend_from_slice(&spelled);
                changed = true;
            }
            _ => out.extend_from_slice(name),
        }
    }
    changed.then_some(out)
}

#[cfg(not(target_os = "macos"))]
fn precompose(_: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(target_os = "macos")]
use nfc::precompose;

#[cfg(target_os = "macos")]
mod nfc {
    use std::ffi::{c_char, c_int, c_void};

    // <iconv.h>, in the libiconv every macOS has, which Git uses for this and
    // residue already links through the libc crate. That crate's bindings
    // are deprecated, as it means to stop linking the library, so the three
    // calls are declared here.
    #[link(name = "iconv")]
    unsafe extern "C" {
        fn iconv_open(tocode: *const c_char, fromcode: *const c_char) -> *mut c_void;
        fn iconv(
            cd: *mut c_void,
            inbuf: *mut *mut c_char,
            inbytesleft: *mut usize,
            outbuf: *mut *mut c_char,
            outbytesleft: *mut usize,
        ) -> usize;
        fn iconv_close(cd: *mut c_void) -> c_int;
    }

    /// Git's own conversion of one name, called as Git calls it: the name
    /// with its NUL, through a fresh descriptor (about 0.6 us on Apple
    /// silicon). None when iconv refuses it, as Apple's converter refuses a
    /// character above U+FFFF; Git then keeps the name as stored. Never
    /// given a whole path: one refused name would lose the names Git
    /// composed beside it.
    pub fn precompose(name: &[u8]) -> Option<Vec<u8>> {
        // SAFETY: both arguments are NUL-terminated literals.
        let cd = unsafe { iconv_open(c"UTF-8".as_ptr(), c"UTF-8-MAC".as_ptr()) };
        if cd.addr() == usize::MAX {
            return None;
        }
        let mut input = Vec::with_capacity(name.len() + 1);
        input.extend_from_slice(name);
        input.push(0);
        // Composing never lengthens a name; a conversion that would is
        // refused, and the name keeps the disk's spelling.
        let mut out = vec![0u8; input.len()];
        let mut in_ptr = input.as_mut_ptr().cast::<c_char>();
        let mut in_left = input.len();
        let mut out_ptr = out.as_mut_ptr().cast::<c_char>();
        let mut out_left = out.len();
        // SAFETY: iconv reads at most `in_left` bytes from `input` and writes
        // at most `out_left` into `out`, both ours, advancing the two
        // pointers within them; `cd` is open until the close below.
        let done = unsafe { iconv(cd, &mut in_ptr, &mut in_left, &mut out_ptr, &mut out_left) };
        // SAFETY: `cd` came from iconv_open and is closed once.
        unsafe { iconv_close(cd) };
        if done == usize::MAX || in_left != 0 {
            return None;
        }
        out.truncate(out.len() - out_left);
        if out.pop() != Some(0) || out == name {
            return None;
        }
        Some(out)
    }
}

fn display(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Add one entry to the totals. Returns false for a hard-linked inode seen
/// before, so per-bucket counts take each inode once too.
fn account(
    usage: &mut Usage,
    shared: &mut HashMap<(u64, u64), Shared>,
    meta: &Stat,
    path: &Path,
) -> bool {
    usage.entries += 1;
    let mtime = meta.mtime();
    if usage.newest_mtime.is_none_or(|m| mtime > m) {
        usage.newest_mtime = Some(mtime);
        usage.newest_path = Some(path.to_path_buf());
    }
    let blocks = meta.blocks().saturating_mul(512);
    if !meta.is_dir() && meta.nlink() > 1 {
        let inode = shared.entry((meta.dev(), meta.ino())).or_insert(Shared {
            seen: 0,
            links: meta.nlink(),
            blocks,
        });
        inode.seen += 1;
        if inode.seen == 1 {
            usage.apparent += meta.len();
            usage.allocated += blocks;
            return true;
        }
        return false;
    }
    usage.apparent += meta.len();
    usage.allocated += blocks;
    usage.reclaimable += blocks;
    true
}

/// The stat and readdir calls the walk makes. On glibc, `stat`, `fstatat`
/// and `readdir` are the 32-bit-offset calls on a 32-bit target, unless
/// the libc crate is built with `gnu_file_offset_bits64`: a file over
/// 2 GiB or an inode number above 2^32 there fails with `EOVERFLOW`. So on
/// glibc the walk calls the 64-bit ones at every pointer width, as std
/// does, and CI's 64-bit Linux runners run that path. musl and the other
/// systems have only 64-bit offsets.
mod sys {
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    pub use libc::{fstat, fstatat, readdir, stat};
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    pub use libc::{fstat64 as fstat, fstatat64 as fstatat, readdir64 as readdir, stat64 as stat};
}

/// The fields of `lstat` the walk reads for one entry.
#[derive(Clone, Copy)]
struct Stat {
    dir: bool,
    /// A regular file or a symbolic link.
    file: bool,
    dev: u64,
    ino: u64,
    nlink: u64,
    /// 512-byte blocks allocated, as `st_blocks`.
    blocks: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
}

impl Stat {
    fn is_dir(&self) -> bool {
        self.dir
    }
    fn is_file(&self) -> bool {
        self.file
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn ino(&self) -> u64 {
        self.ino
    }
    fn nlink(&self) -> u64 {
        self.nlink
    }
    fn blocks(&self) -> u64 {
        self.blocks
    }
    fn len(&self) -> u64 {
        self.len
    }
    fn mtime(&self) -> i64 {
        self.mtime
    }
    fn mtime_nsec(&self) -> i64 {
        self.mtime_nsec
    }
}

impl From<&sys::stat> for Stat {
    // stat's field types differ by platform (st_dev is i32 on macOS and
    // u64 on Linux, st_nlink u16 to u64), so each is widened with `as`, as
    // std's MetadataExt widens it; on some platforms that is the same type.
    #[allow(clippy::unnecessary_cast)]
    fn from(st: &sys::stat) -> Stat {
        #[cfg(target_os = "netbsd")]
        let mtime_nsec = st.st_mtimensec;
        #[cfg(not(target_os = "netbsd"))]
        let mtime_nsec = st.st_mtime_nsec;
        Stat {
            dir: st.st_mode & libc::S_IFMT == libc::S_IFDIR,
            file: matches!(st.st_mode & libc::S_IFMT, libc::S_IFREG | libc::S_IFLNK),
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
            nlink: st.st_nlink as u64,
            blocks: st.st_blocks as u64,
            len: st.st_size as u64,
            mtime: st.st_mtime as i64,
            mtime_nsec: mtime_nsec as i64,
        }
    }
}

/// `fstat` of an open folder.
fn fstat(fd: BorrowedFd) -> io::Result<Stat> {
    // SAFETY: fstat writes into the zeroed struct we own.
    let mut st: sys::stat = unsafe { std::mem::zeroed() };
    if unsafe { sys::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Stat::from(&st))
}

/// `lstat` of one entry, relative to its folder.
fn lstat_at(at: BorrowedFd, name: &CStr) -> io::Result<Stat> {
    lstat_raw(at, name).map(|st| Stat::from(&st))
}

fn lstat_raw(at: BorrowedFd, name: &CStr) -> io::Result<sys::stat> {
    // SAFETY: fstatat reads the NUL-terminated name, which outlives the
    // call, and writes into the zeroed struct we own.
    let mut st: sys::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        sys::fstatat(
            at.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

/// One entry of a directory listing.
struct Listed {
    path: PathBuf,
    /// The entry is named `.git` in any case, as Git matches the name when
    /// `core.ignorecase` is set.
    git_name: bool,
    stat: io::Result<Stat>,
}

/// The entries of one directory, in the order the file system lists them.
/// An `Err` item is a listing error, after which the listing ends; an entry
/// whose `stat` failed carries that error.
type Listing = Vec<io::Result<Listed>>;

/// The listing through readdir(3), then one lstat per entry relative to
/// the folder. readdir reads a copy of the descriptor, which closedir
/// closes, because the walk keeps the folder's own descriptor to open its
/// subfolders from.
fn list_readdir(fd: BorrowedFd, dir: &Path) -> io::Result<Listing> {
    struct Dir(*mut libc::DIR);

    impl Drop for Dir {
        fn drop(&mut self) {
            // SAFETY: the stream is ours, open, and closed only here.
            unsafe { libc::closedir(self.0) };
        }
    }

    let copy = fd.try_clone_to_owned()?;
    // SAFETY: fdopendir takes the open descriptor; on success the stream
    // owns it.
    let stream = unsafe { libc::fdopendir(copy.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    // closedir closes it now.
    std::mem::forget(copy);
    let stream = Dir(stream);
    let mut out = Vec::new();
    loop {
        clear_errno();
        // SAFETY: the stream is open; the entry stays valid until the next
        // readdir on it.
        let entry = unsafe { sys::readdir(stream.0) };
        if entry.is_null() {
            // readdir returns null at the end too, and leaves errno alone.
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(0) {
                out.push(Err(e));
            }
            break;
        }
        // SAFETY: d_name is NUL-terminated within the entry readdir
        // returned. It is read through a raw pointer, because illumos
        // declares it one byte long.
        let name = unsafe { CStr::from_ptr((&raw const (*entry).d_name).cast()) };
        let bytes = name.to_bytes();
        // The walk must never descend these two.
        if bytes == b"." || bytes == b".." {
            continue;
        }
        out.push(Ok(Listed {
            git_name: bytes.eq_ignore_ascii_case(b".git"),
            path: dir.join(OsStr::from_bytes(bytes)),
            stat: lstat_at(fd, name),
        }));
    }
    Ok(out)
}

/// Sets this thread's errno to 0, so the end of a listing can be told
/// apart from a listing error.
fn clear_errno() {
    // SAFETY: each returns this thread's errno, which lives as long as the
    // thread.
    unsafe {
        #[cfg(any(target_os = "linux", target_os = "dragonfly"))]
        let errno = libc::__errno_location();
        #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
        let errno = libc::__error();
        #[cfg(any(target_os = "netbsd", target_os = "openbsd", target_os = "android"))]
        let errno = libc::__errno();
        #[cfg(any(target_os = "illumos", target_os = "solaris"))]
        let errno = libc::___errno();
        *errno = 0;
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "illumos",
    target_os = "solaris"
)))]
compile_error!("residue's walk needs this system's errno: add it to fsinfo::clear_errno");

#[cfg(not(target_os = "macos"))]
fn list(fd: BorrowedFd, dir: &Path, _buf: &mut Vec<u8>) -> io::Result<Listing> {
    list_readdir(fd, dir)
}

/// On macOS, one `getattrlistbulk` call returns a batch of entries with
/// their attributes, where `readdir` plus `fstatat` costs one system call
/// per entry: `du` lists this way through fts. A regular file's fields come
/// from the batch. Any other entry, or one the call reports an error for,
/// is read with `fstatat`, exactly as the standard library reads it. A
/// directory the call refuses gets the readdir listing.
#[cfg(target_os = "macos")]
fn list(fd: BorrowedFd, dir: &Path, buf: &mut Vec<u8>) -> io::Result<Listing> {
    bulk::list(fd, dir, buf)
}

#[cfg(target_os = "macos")]
mod bulk {
    use super::{Listed, Listing, Stat, list_readdir, lstat_at};
    use std::ffi::{CStr, OsStr};
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    // <sys/attr.h> and <sys/vnode.h>; the libc crate does not have these.
    const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;
    const VREG: u32 = 1;
    /// Bytes per call: room for several hundred entries.
    const BUF_LEN: usize = 128 * 1024;

    const COMMON: libc::attrgroup_t = libc::ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_ERROR
        | libc::ATTR_CMN_NAME
        | libc::ATTR_CMN_DEVID
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_MODTIME
        | libc::ATTR_CMN_FILEID;
    const FILE: libc::attrgroup_t =
        libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;

    pub fn list(fd: BorrowedFd, dir: &Path, buf: &mut Vec<u8>) -> io::Result<Listing> {
        let mut attrs = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: COMMON,
            volattr: 0,
            dirattr: 0,
            fileattr: FILE,
            forkattr: 0,
        };
        buf.resize(BUF_LEN, 0);
        let mut out = Vec::new();
        let mut first = true;
        loop {
            // SAFETY: the call reads `attrs` and writes at most `buf.len()`
            // bytes into `buf`; both are ours and outlive it.
            let count = unsafe {
                libc::getattrlistbulk(
                    fd.as_raw_fd(),
                    (&raw mut attrs).cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    0,
                )
            };
            if count < 0 {
                // A file system without the call, or a directory it refuses
                // (one without search permission, say), gets the readdir
                // listing, so each entry reports what readdir and lstat
                // report. getattrlistbulk(2) restarts a listing with an lseek
                // to 0, and readdir must not continue one it began.
                if first {
                    // SAFETY: lseek on our open descriptor.
                    if unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET) } < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    return list_readdir(fd, dir);
                }
                out.push(Err(io::Error::last_os_error()));
                break;
            }
            if count == 0 {
                break;
            }
            first = false;
            let mut at = 0;
            for _ in 0..count {
                let Some((entry, len)) = buf.get(at..).and_then(parse) else {
                    out.push(Err(io::Error::other(
                        "getattrlistbulk returned an entry residue cannot read",
                    )));
                    return Ok(out);
                };
                at += len;
                let name = entry.name.to_bytes();
                // readdir skips these two, and the walk must never descend them.
                if name == b"." || name == b".." {
                    continue;
                }
                let path = dir.join(OsStr::from_bytes(name));
                let stat = match entry.stat() {
                    Some(stat) => Ok(stat),
                    None => lstat_at(fd, entry.name),
                };
                out.push(Ok(Listed {
                    git_name: name.eq_ignore_ascii_case(b".git"),
                    path,
                    stat,
                }));
            }
        }
        Ok(out)
    }

    /// One entry of a batch: the attributes it returned.
    struct Entry<'a> {
        name: &'a CStr,
        error: u32,
        objtype: Option<u32>,
        dev: Option<i32>,
        mtime: Option<(i64, i64)>,
        ino: Option<u64>,
        nlink: Option<u32>,
        alloc: Option<i64>,
        len: Option<i64>,
    }

    impl Entry<'_> {
        /// The fields of a regular file, when the batch returned all of
        /// them without an error.
        fn stat(&self) -> Option<Stat> {
            if self.error != 0 || self.objtype? != VREG {
                return None;
            }
            let (mtime, mtime_nsec) = self.mtime?;
            Some(Stat {
                dir: false,
                file: true,
                // As std's MetadataExt::dev widens st_dev.
                dev: self.dev? as u64,
                ino: self.ino?,
                nlink: u64::from(self.nlink?),
                // Allocation is in whole file system blocks, so this is
                // exact; st_blocks counts the same allocation.
                blocks: u64::try_from(self.alloc?).ok()?.div_ceil(512),
                len: u64::try_from(self.len?).ok()?,
                mtime,
                mtime_nsec,
            })
        }
    }

    fn take<const N: usize>(rec: &[u8], at: &mut usize) -> Option<[u8; N]> {
        let bytes = rec.get(*at..at.checked_add(N)?)?;
        *at += N;
        bytes.try_into().ok()
    }

    /// Reads the entry at the start of `buf`, and returns it with its
    /// length. The layout is getattrlist(2)'s: the length, the returned
    /// attribute set, the error code, then each returned attribute in bit
    /// order, every one a multiple of 4 bytes and 4-byte aligned.
    fn parse(buf: &[u8]) -> Option<(Entry<'_>, usize)> {
        let len = u32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
        let rec = buf.get(..len)?;
        let mut at = 4;
        let common = u32::from_ne_bytes(take(rec, &mut at)?);
        let _vol = take::<4>(rec, &mut at)?;
        let _dir = take::<4>(rec, &mut at)?;
        let file = u32::from_ne_bytes(take(rec, &mut at)?);
        let _fork = take::<4>(rec, &mut at)?;
        let mut entry = Entry {
            name: c"",
            error: 0,
            objtype: None,
            dev: None,
            mtime: None,
            ino: None,
            nlink: None,
            alloc: None,
            len: None,
        };
        if common & ATTR_CMN_ERROR != 0 {
            entry.error = u32::from_ne_bytes(take(rec, &mut at)?);
        }
        if common & libc::ATTR_CMN_NAME == 0 {
            return None;
        }
        // An attrreference_t: the name's offset from this field, and its
        // length with the terminating NUL.
        let field = at;
        let offset = i32::from_ne_bytes(take(rec, &mut at)?);
        let name_len = u32::from_ne_bytes(take(rec, &mut at)?) as usize;
        let start = field.checked_add_signed(isize::try_from(offset).ok()?)?;
        let name = rec.get(start..start.checked_add(name_len)?)?;
        entry.name = CStr::from_bytes_until_nul(name).ok()?;
        if entry.name.is_empty() {
            return None;
        }
        if common & libc::ATTR_CMN_DEVID != 0 {
            entry.dev = Some(i32::from_ne_bytes(take(rec, &mut at)?));
        }
        if common & libc::ATTR_CMN_OBJTYPE != 0 {
            entry.objtype = Some(u32::from_ne_bytes(take(rec, &mut at)?));
        }
        if common & libc::ATTR_CMN_MODTIME != 0 {
            let sec = i64::from_ne_bytes(take(rec, &mut at)?);
            let nsec = i64::from_ne_bytes(take(rec, &mut at)?);
            entry.mtime = Some((sec, nsec));
        }
        if common & libc::ATTR_CMN_FILEID != 0 {
            entry.ino = Some(u64::from_ne_bytes(take(rec, &mut at)?));
        }
        if file & libc::ATTR_FILE_LINKCOUNT != 0 {
            entry.nlink = Some(u32::from_ne_bytes(take(rec, &mut at)?));
        }
        if file & libc::ATTR_FILE_ALLOCSIZE != 0 {
            entry.alloc = Some(i64::from_ne_bytes(take(rec, &mut at)?));
        }
        if file & libc::ATTR_FILE_DATALENGTH != 0 {
            entry.len = Some(i64::from_ne_bytes(take(rec, &mut at)?));
        }
        Some((entry, len))
    }
}

impl Usage {
    fn unreadable_entry(&mut self, path: &Path, error: &io::Error) {
        self.unreadable += 1;
        if self.first_error.is_none() {
            self.first_error = Some(format!("{}: {error}", path.display()));
            self.reach.unread = self.first_error.clone();
        }
    }
}

#[derive(Clone, Copy)]
pub struct Space {
    pub device: u64,
    pub free: u64,
    pub total: u64,
    pub free_inodes: u64,
    pub total_inodes: u64,
}

// statvfs field types differ by platform (u32 on macOS, u64 on 64-bit Linux),
// so u64::from is the identity on some targets and a widening on others.
#[allow(clippy::useless_conversion)]
pub fn space(path: &Path) -> io::Result<Space> {
    let device = fs::metadata(path)?.dev();
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: statvfs writes into the zeroed struct we own and reads the
    // NUL-terminated path, which outlives the call.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let fragment = u64::from(stat.f_frsize);
    Ok(Space {
        device,
        free: u64::from(stat.f_bavail).saturating_mul(fragment),
        total: u64::from(stat.f_blocks).saturating_mul(fragment),
        free_inodes: u64::from(stat.f_favail),
        total_inodes: u64::from(stat.f_files),
    })
}

#[cfg(test)]
mod tests {
    //! The walk's folder opens, on real folders in a temporary directory:
    //! a folder swapped for a symbolic link or another folder after its
    //! parent was listed is reported as changed and never read, a walk
    //! deeper than [`HELD`] reads every level and keeps the same checks,
    //! and a worktree that can be looked up but not read is one unreadable
    //! entry. And Git's spelling of a path, name by name, with the walk
    //! finding an ignored entry by it.

    #[cfg(target_os = "macos")]
    use super::git_spelling;
    use super::{
        Bucket, HELD, Parent, Pending, Place, REOPENED, Reopened, Step, lstat_at, measure,
        open_listed, open_root, spell_names,
    };
    use std::ffi::CString;
    use std::fs;
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    /// An empty folder of the test's own, by its canonical path, as the
    /// walk's callers pass it.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "residue-walk-{test}-{}-{}",
            std::process::id(),
            crate::output::now()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    fn c(name: &str) -> CString {
        CString::new(name).unwrap()
    }

    const TOP: Place = Place {
        bucket: None,
        covered: false,
        in_git: false,
        in_package: false,
    };

    /// The folder at `dir`, opened as the walk opens a worktree.
    fn open(dir: &Path) -> OwnedFd {
        open_root(dir).unwrap().1.unwrap()
    }

    /// The device and inode of `name` in `dir`, as the listing records it.
    fn id(dir: &Path, name: &str) -> (u64, u64) {
        let st = lstat_at(open(dir).as_fd(), &c(name)).unwrap();
        (st.dev(), st.ino())
    }

    /// The steps from the folder below `base` down to `to`, each as its
    /// parent lists it.
    fn steps(base: &Path, to: &Path) -> Rc<Step> {
        let mut up: Option<Rc<Step>> = None;
        let mut at = base.to_path_buf();
        for (i, part) in to.strip_prefix(base).unwrap().iter().enumerate() {
            let name = part.to_str().unwrap();
            let step = Step {
                name: c(name),
                id: id(&at, name),
                up: up.take(),
                level: i + 1,
            };
            up = Some(Rc::new(step));
            at.push(part);
        }
        up.unwrap()
    }

    /// Entries below `dir` and itself, counted with std, never following
    /// a link, and every entry named `.git` in any case, as the walk
    /// lists them.
    fn count(dir: &Path, repos: &mut Vec<PathBuf>) -> u64 {
        let mut n = 1;
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().as_bytes().eq_ignore_ascii_case(b".git") {
                repos.push(entry.path());
            }
            if entry.file_type().unwrap().is_dir() {
                n += count(&entry.path(), repos);
            } else {
                n += 1;
            }
        }
        n
    }

    #[test]
    fn a_folder_swapped_after_its_listing_is_changed_and_never_read() {
        let root = scratch("swap");
        let w = root.join("w");
        fs::create_dir_all(w.join("a")).unwrap();
        fs::write(w.join("a").join("inside"), "x").unwrap();
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(elsewhere.join(".git")).unwrap();
        fs::write(elsewhere.join("secret"), "y").unwrap();

        // The walk has listed `w` and recorded `a` as a folder.
        let parent = Rc::new(open(&w));
        let listed = id(&w, "a");
        let pending = |parent: &Rc<_>| Pending {
            path: w.join("a"),
            name: c("a"),
            id: listed,
            depth: 1,
            place: TOP,
            parent: Parent::Open(Rc::clone(parent)),
        };
        let mut kept = Reopened::default();
        assert!(
            pending(&parent).open(&mut kept).is_ok(),
            "the folder as listed"
        );

        // Now `a` is a symbolic link to a folder outside the worktree.
        fs::rename(w.join("a"), root.join("a-before")).unwrap();
        symlink(&elsewhere, w.join("a")).unwrap();
        let e = open_listed(parent.as_fd(), &c("a"), listed).unwrap_err();
        assert_eq!(
            e.to_string(),
            "changed during the walk: it is a symbolic link now"
        );
        let Err((path, _, e)) = pending(&parent).open(&mut kept) else {
            panic!("the walk opened a symbolic link");
        };
        assert_eq!(path, w.join("a"));
        assert!(e.to_string().starts_with("changed during the walk"), "{e}");
        // A symbolic link to the very folder the listing saw is not it either.
        fs::remove_file(w.join("a")).unwrap();
        symlink(root.join("a-before"), w.join("a")).unwrap();
        assert!(
            pending(&parent).open(&mut kept).is_err(),
            "a link to the same folder"
        );

        // Another folder in its place: a new inode.
        fs::remove_file(w.join("a")).unwrap();
        fs::create_dir(w.join("a")).unwrap();
        let Err((_, _, e)) = pending(&parent).open(&mut kept) else {
            panic!("the walk read another folder as the one it listed");
        };
        assert_eq!(
            e.to_string(),
            "changed during the walk: another folder is in its place"
        );

        // A file in its place.
        fs::remove_dir(w.join("a")).unwrap();
        fs::write(w.join("a"), "z").unwrap();
        let Err((_, _, e)) = pending(&parent).open(&mut kept) else {
            panic!("the walk opened a file as a folder");
        };
        assert_eq!(
            e.to_string(),
            "changed during the walk: it is not a folder now"
        );

        // A worktree path with a symbolic link on it is refused whole.
        symlink(&w, root.join("link")).unwrap();
        let Err(e) = measure(&root.join("link"), &[], &|_| false, &|_| false, &[]) else {
            panic!("the walk opened a worktree through a symbolic link");
        };
        assert!(e.to_string().contains("symbolic link"), "{e}");

        // The walk itself reads the link as one entry and nothing behind it.
        fs::remove_file(w.join("a")).unwrap();
        symlink(&elsewhere, w.join("a")).unwrap();
        let usage = measure(&w, &[], &|_| false, &|_| false, &[]).unwrap();
        assert_eq!(usage.entries, 2, "w and the link");
        assert!(usage.nested_repos.is_empty());
        assert_eq!(usage.unreadable, 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_walk_deeper_than_the_folders_it_holds_reads_every_level() {
        let root = scratch("deep");
        let w = root.join("w");
        // HELD * 2 + 3 levels below `w`, a file in each, two folders and a
        // nested repository at the bottom.
        let levels = HELD * 2 + 3;
        let mut dir = w.clone();
        let mut chain = Vec::new();
        for level in 1..=levels {
            dir.push(format!("d{level}"));
            chain.push(dir.clone());
        }
        fs::create_dir_all(dir.join("left")).unwrap();
        fs::create_dir_all(dir.join("right").join(".git")).unwrap();
        for d in &chain {
            fs::write(d.join("f"), "12345").unwrap();
        }
        // Below HELD, three branches deeper than the folders kept open
        // there, with a file at every level and a nested repository at the
        // bottom of the middle one, and many empty siblings beside them.
        let fork = &chain[HELD + 2];
        for branch in 0..3 {
            let mut at = fork.join(format!("b{branch}"));
            for level in 0..REOPENED + 2 {
                at.push(format!("x{level}"));
                fs::create_dir_all(&at).unwrap();
                fs::write(at.join("g"), "1").unwrap();
            }
            if branch == 1 {
                fs::create_dir(at.join(".git")).unwrap();
            }
        }
        for sibling in 0..20 {
            fs::create_dir(fork.join(format!("s{sibling}"))).unwrap();
        }
        let mut repos = Vec::new();
        let entries = count(&w, &mut repos);
        let usage = measure(&w, &[], &|_| false, &|_| false, &[]).unwrap();
        assert_eq!(usage.unreadable, 0, "{:?}", usage.first_error);
        assert_eq!(usage.entries, entries);
        let mut found = usage.nested_repos.clone();
        found.sort();
        repos.sort();
        assert_eq!(found, repos);
        assert_eq!(repos.len(), 2, "right and the middle branch");

        // A folder below HELD opens from the deepest folder held, one name
        // at a time, each checked against its listing.
        let base_path = &chain[HELD - 2];
        let base = Rc::new(open(base_path));
        let parent = steps(base_path, &chain[levels - 2]);
        let last = id(&chain[levels - 2], &format!("d{levels}"));
        let pending = || Pending {
            path: dir.clone(),
            name: c(&format!("d{levels}")),
            id: last,
            depth: levels,
            place: TOP,
            parent: Parent::Below(Rc::clone(&base), Rc::clone(&parent)),
        };
        assert!(
            pending().open(&mut Reopened::default()).is_ok(),
            "the folder as listed"
        );

        // With folders of another branch kept, a folder opens from the
        // deepest folder held, not from one of them.
        let mut kept = Reopened::default();
        let b0 = fork.join("b0").join("x0").join("x1");
        let pending_in = |at: &Path, kept: &mut Reopened| {
            let up = at.parent().unwrap();
            let name = at.file_name().unwrap().to_str().unwrap();
            Pending {
                path: at.to_path_buf(),
                name: c(name),
                id: id(up, name),
                depth: levels,
                place: TOP,
                parent: Parent::Below(Rc::clone(&base), steps(base_path, up)),
            }
            .open(kept)
        };
        assert!(pending_in(&b0, &mut kept).is_ok());
        assert!(!kept.0.is_empty(), "the folders it reopened are kept");
        assert!(pending().open(&mut kept).is_ok(), "another branch");

        // A folder on the way down becomes a symbolic link to itself.
        let swapped = &chain[HELD + 2];
        let aside = root.join("aside");
        fs::rename(swapped, &aside).unwrap();
        symlink(&aside, swapped).unwrap();
        let Err((path, _, e)) = pending().open(&mut Reopened::default()) else {
            panic!("the reopen followed a symbolic link");
        };
        assert_eq!(&path, swapped, "the folder that changed");
        assert_eq!(
            e.to_string(),
            "changed during the walk: it is a symbolic link now"
        );
        fs::remove_file(swapped).unwrap();
        fs::rename(&aside, swapped).unwrap();
        assert!(
            pending().open(&mut Reopened::default()).is_ok(),
            "back as it was"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_worktree_that_cannot_be_read_is_one_unreadable_entry() {
        let root = scratch("root");
        let w = root.join("w");
        fs::create_dir_all(w.join("a")).unwrap();
        fs::set_permissions(&w, fs::Permissions::from_mode(0o311)).unwrap();
        let usage = measure(&w, &[], &|_| false, &|_| false, &[]);
        fs::set_permissions(&w, fs::Permissions::from_mode(0o755)).unwrap();
        let usage = usage.unwrap();
        // SAFETY: geteuid only returns this process's effective user.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(usage.entries, 1, "the worktree itself");
            assert_eq!(usage.unreadable, 1);
            let error = usage.first_error.unwrap();
            assert!(error.starts_with(&format!("{}: ", w.display())), "{error}");
            assert!(usage.newest_mtime.is_some());
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// An environment file strictly inside an ignored entry is found, except
    /// in a package folder or a `.git` entry, when a rule names it itself,
    /// or when it is the entry, which the rules judge alone.
    #[test]
    fn an_environment_file_inside_an_ignored_folder_is_found() {
        let root = scratch("env");
        let w = root.join("w");
        let files = [
            "build/sub/.env.local",
            "build/sub/.env.production",
            "dist/.env.example",
            "node_modules/pkg/.env",
            "out/.venv/lib/.env",
            "named/.env",
            "copy/.git/.env",
            ".env",
        ];
        for file in files {
            let path = w.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "SECRET=1").unwrap();
        }
        let buckets: Vec<Bucket> = [
            "build",
            "dist",
            "node_modules",
            "out",
            "named",
            "copy",
            ".env",
        ]
        .iter()
        .map(|rel| Bucket {
            rel: PathBuf::from(rel),
            expand: false,
        })
        .collect();
        let usage = measure(&w, &buckets, &|_| true, &|rel| rel == "named/.env", &[]).unwrap();
        let found: Vec<Option<&str>> = usage
            .buckets
            .iter()
            .map(|b| b.env_file.as_deref())
            .collect();
        let first = found[0].unwrap_or_default();
        assert!(
            first == "build/sub/.env.local" || first == "build/sub/.env.production",
            "{found:?}"
        );
        assert_eq!(
            found[1..],
            [None, None, None, None, None, None],
            "{found:?}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// Git converts each name on its own and keeps one the conversion
    /// refuses as stored, so a path can mix both. Here a stand-in for
    /// Apple's converter composes `e` and U+0301 and refuses any name with
    /// a four-byte character, as that converter refuses U+1F680.
    #[test]
    fn each_name_is_spelled_on_its_own_and_a_refused_one_is_kept() {
        let calls = std::cell::Cell::new(0);
        let convert = |name: &[u8]| {
            calls.set(calls.get() + 1);
            if name.iter().any(|&b| b >= 0xf0) {
                return None;
            }
            let text = std::str::from_utf8(name).ok()?;
            Some(text.replace("e\u{301}", "\u{e9}").into_bytes())
        };
        let spell = |rel: &str| spell_names(rel.as_bytes(), convert).map(String::from_utf8);
        assert_eq!(
            spell("cafe\u{301}/\u{1f680}/build"),
            Some(Ok("caf\u{e9}/\u{1f680}/build".to_string())),
            "the composed name beside a refused one"
        );
        assert_eq!(
            spell("\u{1f680}/cafe\u{301}/build"),
            Some(Ok("\u{1f680}/caf\u{e9}/build".to_string())),
            "in the other order"
        );
        assert_eq!(
            spell("cafe\u{301}\u{1f680}/build"),
            None,
            "one refused name stays whole"
        );
        assert_eq!(spell("\u{1f680}/build"), None, "nothing changes");
        assert_eq!(
            spell("caf\u{e9}/\u{65e5}"),
            None,
            "names the conversion leaves alone"
        );
        calls.set(0);
        assert_eq!(spell("build/out/a.o"), None);
        assert_eq!(calls.get(), 0, "an ASCII name is never converted");
        assert_eq!(
            spell_names(b"\xff/cafe\xcc\x81/build", convert),
            Some(b"\xff/caf\xc3\xa9/build".to_vec()),
            "a name that is not UTF-8 is kept, byte for byte"
        );
    }

    /// The same with Apple's converter, which Git calls: it refuses U+1F680
    /// and U+20000, so a path that holds one beside a decomposed name is
    /// spelled name by name, never not at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn git_spells_a_path_name_by_name_with_apples_converter() {
        let root = Path::new("/w");
        let spell = |rel: &str| git_spelling(&root.join(rel), root);
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            spell("cafe\u{301}/\u{1f680}/build"),
            some("caf\u{e9}/\u{1f680}/build")
        );
        assert_eq!(
            spell("\u{1f680}/cafe\u{301}/build"),
            some("\u{1f680}/caf\u{e9}/build")
        );
        assert_eq!(
            spell("cafe\u{301}/\u{20000}/build"),
            some("caf\u{e9}/\u{20000}/build")
        );
        assert_eq!(
            spell("cafe\u{301}/\u{65e5}/build"),
            some("caf\u{e9}/\u{65e5}/build")
        );
        assert_eq!(spell("caf\u{e9}/\u{1f680}/build"), None, "already Git's");
        assert_eq!(spell("\u{1f680}/build"), None);
        assert_eq!(spell("build"), None);
        assert_eq!(git_spelling(Path::new("/elsewhere/a"), root), None);
    }

    /// A folder below a decomposed name and an emoji name is the bucket of
    /// Git's spelling of it: the walk sizes it and finds the environment
    /// file inside.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bucket_in_gits_spelling_is_found_below_a_refused_name() {
        let root = scratch("spelling");
        let w = root.join("w");
        let layouts = [
            ("cafe\u{301}/\u{1f680}", "caf\u{e9}/\u{1f680}"),
            ("\u{1f680}/cafe\u{301}", "\u{1f680}/caf\u{e9}"),
        ];
        for (disk, _) in layouts {
            let build = w.join(disk).join("build");
            fs::create_dir_all(&build).unwrap();
            fs::write(build.join("out.o"), "o").unwrap();
            fs::write(build.join(".env"), "SECRET=1\n").unwrap();
        }
        let buckets: Vec<Bucket> = layouts
            .iter()
            .map(|(_, git)| Bucket {
                rel: Path::new(git).join("build"),
                expand: false,
            })
            .collect();
        let usage = measure(&w, &buckets, &|_| true, &|_| false, &[]).unwrap();
        for ((disk, _), b) in layouts.iter().zip(&usage.buckets) {
            assert!(b.seen, "{disk}/build is its bucket");
            assert_eq!((b.files, b.bytes), (2, 10), "{disk}/build");
            assert_eq!(
                b.env_file.as_deref(),
                Some(format!("{disk}/build/.env").as_str())
            );
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_walk_reports_its_longest_path_and_each_wanted_file_it_lists() {
        let root = scratch("reach");
        let w = root.join("w");
        let deep = w.join("a").join("bb").join("ccc");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("file"), "x").unwrap();
        fs::write(w.join("top"), "x").unwrap();
        fs::write(root.join("outside"), "x").unwrap();
        let inside = id(&deep, "file");
        let outside = id(&root, "outside");
        let mut wanted = vec![inside, outside];
        wanted.sort_unstable();
        let usage = measure(&w, &[], &|_| false, &|_| false, &wanted).unwrap();
        assert_eq!(
            usage.reach.longest,
            deep.join("file").as_os_str().len(),
            "the longest path is the deepest file's"
        );
        assert_eq!(usage.reach.found, vec![inside]);
        let none = measure(&w, &[], &|_| false, &|_| false, &[]).unwrap();
        assert!(none.reach.found.is_empty());
        assert_eq!(none.reach.longest, usage.reach.longest);
        fs::remove_dir_all(&root).unwrap();
    }
}
