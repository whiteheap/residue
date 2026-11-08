//! Disk usage of one worktree, and free space on its filesystem.
//!
//! The walk never follows symbolic links and never crosses into another
//! filesystem. It opens each folder relative to its parent's open
//! descriptor with `O_NOFOLLOW`, so a folder that becomes a symbolic link,
//! or another folder, while the walk runs is reported as changed and never
//! read. It records what would make a removal surprising: nested
//! repositories, mount points and entries it could not read. The same walk
//! attributes files and bytes to each ignored entry (a bucket), and judges
//! the contents of a collapsed ignored directory that a path rule reaches
//! into, so the worktree is read once.

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
/// deeper than that is closed once listed, and its subfolders are opened
/// from the deepest folder still open, one name at a time, each checked
/// against the device and inode its listing showed (`Pending::open`). So a
/// walk holds at most `HELD + 2` descriptors however deep the tree, and
/// `scan`'s 8 workers at most 144: launchd starts a login service with a
/// soft limit of 256, and Git's pipes need some of the rest. The limit is
/// not raised, because nothing needs more. The deepest folder in five large
/// local worktrees and checkouts (41,650 to 112,595 entries, 2026-11-08)
/// was 14 levels down, so in practice no folder is reopened.
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
    pub nested_repos: Vec<PathBuf>,
    pub mount_points: Vec<PathBuf>,
    /// One result per bucket passed to [`measure`], in the same order.
    pub buckets: Vec<BucketUsage>,
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
    /// False when the walk never reached the entry (it vanished, or lies
    /// behind an unreadable directory or a mount point).
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
}

/// Measures the worktree at `root`, which must be canonical
/// (`git::canonical`): the walk refuses a symbolic link anywhere on it.
pub fn measure(
    root: &Path,
    buckets: &[Bucket],
    matches: &dyn Fn(&str) -> bool,
) -> io::Result<Usage> {
    let top_fd = Rc::new(open_root(root)?);
    let top = fstat(top_fd.as_fd())?;
    let device = top.dev();
    let mut usage = Usage {
        buckets: vec![BucketUsage::default(); buckets.len()],
        ..Usage::default()
    };
    let lookup: HashMap<PathBuf, usize> = buckets
        .iter()
        .enumerate()
        .map(|(i, b)| (root.join(&b.rel), i))
        .collect();
    let mut walked = vec![0u64; buckets.len()];
    let mut shared: HashMap<(u64, u64), Shared> = HashMap::new();
    account(&mut usage, &mut shared, &top, root);
    let mut next = Some(Folder {
        below: Parent::Open(Rc::clone(&top_fd)),
        fd: top_fd,
        path: root.to_path_buf(),
        depth: 0,
        place: Place {
            bucket: None,
            covered: false,
            in_git: false,
        },
    });
    let mut stack: Vec<Pending> = Vec::new();
    let mut buf = Vec::new();
    loop {
        let folder = match next.take() {
            Some(folder) => folder,
            None => {
                let Some(pending) = stack.pop() else {
                    break;
                };
                match pending.open() {
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
        // Only `below` keeps it open from here, and only above HELD.
        drop(fd);
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
            if item.git && dir != root {
                usage.nested_repos.push(dir.clone());
            }
            let mut here = place;
            if here.bucket.is_none() {
                if let Some(&i) = lookup.get(&path) {
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
                        if relative(&path, root).is_some_and(|rel| matches(&rel)) {
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
                }
                if !meta.is_dir() && first {
                    usage.buckets[i].files += 1;
                    usage.buckets[i].bytes += meta.len();
                }
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
    /// Its parent is deeper than [`HELD`] and was closed after its listing:
    /// the deepest folder still open above it, and the folders from there
    /// down to its parent, each as its own parent listed it.
    Below(Rc<OwnedFd>, Rc<[Step]>),
}

#[derive(Clone)]
struct Step {
    name: CString,
    id: (u64, u64),
}

impl Pending {
    /// Opens it, and returns where its subfolders will be opened from. A
    /// failure names the folder that failed, this one or a folder above it
    /// that had to be reopened, with the place to judge it in.
    fn open(self) -> Result<Folder, (PathBuf, Place, io::Error)> {
        let fd = match &self.parent {
            Parent::Open(at) => open_listed(at.as_fd(), &self.name, self.id),
            Parent::Below(base, steps) => {
                let mut held: Option<OwnedFd> = None;
                for (i, step) in steps.iter().enumerate() {
                    let at = held.as_ref().map_or(base.as_fd(), AsFd::as_fd);
                    match open_listed(at, &step.name, step.id) {
                        Ok(fd) => held = Some(fd),
                        Err(e) => {
                            // steps[i] is the folder `steps.len() - i` levels up.
                            let up = self.path.ancestors().nth(steps.len() - i);
                            let path = up.unwrap_or(&self.path).to_path_buf();
                            return Err((path, self.place, e));
                        }
                    }
                }
                let at = held.as_ref().map_or(base.as_fd(), AsFd::as_fd);
                open_listed(at, &self.name, self.id)
            }
        };
        let fd = match fd {
            Ok(fd) => Rc::new(fd),
            Err(e) => return Err((self.path, self.place, e)),
        };
        let below = if self.depth < HELD {
            Parent::Open(Rc::clone(&fd))
        } else {
            let step = Step {
                name: self.name,
                id: self.id,
            };
            match self.parent {
                Parent::Open(base) => Parent::Below(base, Rc::from([step])),
                Parent::Below(base, steps) => {
                    Parent::Below(base, steps.iter().cloned().chain([step]).collect())
                }
            }
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
/// canonical, so a symbolic link on it means the path changed since.
fn open_root(root: &Path) -> io::Result<OwnedFd> {
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
    let flags = if names.is_empty() { DIR } else { ABOVE };
    // SAFETY: open reads the NUL-terminated literal; the descriptor it
    // returns is ours.
    let fd = unsafe { libc::open(c"/".as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor is open and nothing else owns it.
    let mut at = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut reached = PathBuf::from("/");
    for (i, name) in names.iter().enumerate() {
        reached.push(name);
        let flags = if i + 1 == names.len() { DIR } else { ABOVE };
        at = leaf(&reached)
            .and_then(|c| open_dir(at.as_fd(), &c, flags))
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", reached.display())))?;
    }
    Ok(at)
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

impl From<&libc::stat> for Stat {
    // stat's field types differ by platform (st_dev is i32 on macOS and
    // u64 on Linux, st_nlink u16 to u64), so each is widened with `as`, as
    // std's MetadataExt widens it; on some platforms that is the same type.
    #[allow(clippy::unnecessary_cast)]
    fn from(st: &libc::stat) -> Stat {
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
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Stat::from(&st))
}

/// `lstat` of one entry, relative to its folder.
fn lstat_at(at: BorrowedFd, name: &CStr) -> io::Result<Stat> {
    lstat_raw(at, name).map(|st| Stat::from(&st))
}

fn lstat_raw(at: BorrowedFd, name: &CStr) -> io::Result<libc::stat> {
    // SAFETY: fstatat reads the NUL-terminated name, which outlives the
    // call, and writes into the zeroed struct we own.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
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
    /// The entry is named `.git`.
    git: bool,
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
        let entry = unsafe { libc::readdir(stream.0) };
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
            git: bytes == b".git",
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
                    git: name == b".git",
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
    //! parent was listed is reported as changed and never read, and a walk
    //! deeper than [`HELD`] reads every level and keeps the same checks.

    use super::{HELD, Parent, Pending, Place, Step, lstat_at, measure, open_listed, open_root};
    use std::ffi::CString;
    use std::fs;
    use std::os::fd::AsFd;
    use std::os::unix::fs::symlink;
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
    };

    /// The device and inode of `name` in `dir`, as the listing records it.
    fn id(dir: &Path, name: &str) -> (u64, u64) {
        let at = open_root(dir).unwrap();
        let st = lstat_at(at.as_fd(), &c(name)).unwrap();
        (st.dev(), st.ino())
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
        let parent = Rc::new(open_root(&w).unwrap());
        let listed = id(&w, "a");
        let pending = |parent: &Rc<_>| Pending {
            path: w.join("a"),
            name: c("a"),
            id: listed,
            depth: 1,
            place: TOP,
            parent: Parent::Open(Rc::clone(parent)),
        };
        assert!(pending(&parent).open().is_ok(), "the folder as listed");

        // Now `a` is a symbolic link to a folder outside the worktree.
        fs::rename(w.join("a"), root.join("a-before")).unwrap();
        symlink(&elsewhere, w.join("a")).unwrap();
        let e = open_listed(parent.as_fd(), &c("a"), listed).unwrap_err();
        assert_eq!(
            e.to_string(),
            "changed during the walk: it is a symbolic link now"
        );
        let Err((path, _, e)) = pending(&parent).open() else {
            panic!("the walk opened a symbolic link");
        };
        assert_eq!(path, w.join("a"));
        assert!(e.to_string().starts_with("changed during the walk"), "{e}");
        // A symbolic link to the very folder the listing saw is not it either.
        fs::remove_file(w.join("a")).unwrap();
        symlink(root.join("a-before"), w.join("a")).unwrap();
        assert!(
            pending(&parent).open().is_err(),
            "a link to the same folder"
        );

        // Another folder in its place: a new inode.
        fs::remove_file(w.join("a")).unwrap();
        fs::create_dir(w.join("a")).unwrap();
        let Err((_, _, e)) = pending(&parent).open() else {
            panic!("the walk read another folder as the one it listed");
        };
        assert_eq!(
            e.to_string(),
            "changed during the walk: another folder is in its place"
        );

        // A file in its place.
        fs::remove_dir(w.join("a")).unwrap();
        fs::write(w.join("a"), "z").unwrap();
        let Err((_, _, e)) = pending(&parent).open() else {
            panic!("the walk opened a file as a folder");
        };
        assert_eq!(
            e.to_string(),
            "changed during the walk: it is not a folder now"
        );

        // A worktree path with a symbolic link on it is refused whole.
        symlink(&w, root.join("link")).unwrap();
        let Err(e) = measure(&root.join("link"), &[], &|_| false) else {
            panic!("the walk opened a worktree through a symbolic link");
        };
        assert!(e.to_string().contains("symbolic link"), "{e}");

        // The walk itself reads the link as one entry and nothing behind it.
        fs::remove_file(w.join("a")).unwrap();
        symlink(&elsewhere, w.join("a")).unwrap();
        let usage = measure(&w, &[], &|_| false).unwrap();
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
        let usage = measure(&w, &[], &|_| false).unwrap();
        assert_eq!(usage.unreadable, 0, "{:?}", usage.first_error);
        // w, each level and its file, left, right and right/.git.
        assert_eq!(usage.entries, 1 + 2 * levels as u64 + 3);
        assert_eq!(usage.nested_repos, vec![dir.join("right")]);

        // A folder below HELD opens from the deepest folder held, one name
        // at a time, each checked against its listing.
        let base_path = &chain[HELD - 2];
        let steps: Vec<Step> = chain[HELD - 1..levels - 1]
            .iter()
            .map(|d| {
                let name = d.file_name().unwrap().to_str().unwrap();
                Step {
                    name: c(name),
                    id: id(d.parent().unwrap(), name),
                }
            })
            .collect();
        let base = Rc::new(open_root(base_path).unwrap());
        let last = id(&chain[levels - 2], &format!("d{levels}"));
        let pending = || Pending {
            path: dir.clone(),
            name: c(&format!("d{levels}")),
            id: last,
            depth: levels,
            place: TOP,
            parent: Parent::Below(Rc::clone(&base), Rc::from(steps.clone())),
        };
        let opened = pending().open();
        assert!(opened.is_ok(), "the folder as listed");

        // A folder on the way down becomes a symbolic link to itself.
        let swapped = &chain[HELD + 2];
        let aside = root.join("aside");
        fs::rename(swapped, &aside).unwrap();
        symlink(&aside, swapped).unwrap();
        let Err((path, _, e)) = pending().open() else {
            panic!("the reopen followed a symbolic link");
        };
        assert_eq!(&path, swapped, "the folder that changed");
        assert_eq!(
            e.to_string(),
            "changed during the walk: it is a symbolic link now"
        );
        fs::remove_file(swapped).unwrap();
        fs::rename(&aside, swapped).unwrap();
        assert!(pending().open().is_ok(), "back as it was");
        fs::remove_dir_all(&root).unwrap();
    }
}
