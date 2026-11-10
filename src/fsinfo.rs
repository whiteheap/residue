//! Disk usage of one worktree, and free space on its filesystem.
//!
//! The walk never follows symbolic links and never crosses into another
//! filesystem or mount ([`Mounts`]). It opens each folder relative to its parent's open
//! descriptor with `O_NOFOLLOW`, so a folder that becomes a symbolic link,
//! or another folder, before the walk opens it is reported as changed and
//! never read. A folder already open is read through its descriptor, as
//! any walk reads it: renamed away or mounted over after that, it is still
//! the folder the listing saw. It records what would make a removal
//! surprising: nested repositories, mount points and entries it could not
//! read. The same walk attributes files and bytes to each ignored entry (a
//! bucket), and judges the contents of a collapsed ignored directory that
//! a path rule reaches into, so the worktree is read once.

use std::collections::{HashMap, HashSet};
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
    /// outside the worktree (a pnpm store, for example) and, on APFS, the
    /// data blocks a file shares with a clone outside it (`Stat::unshared`,
    /// `Clones`). It can still overstate: snapshots, and clones on other
    /// systems (Btrfs and XFS reflinks, ZFS block cloning), are not
    /// detected. And it can understate: a block two files share counts for
    /// neither, unless they are a set of full clones the worktree holds
    /// whole, though removing both frees it, as when they are in two
    /// worktrees removed together.
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
    /// Each entry on another mount than the worktree's folder ([`Mounts`]).
    pub mount_points: Vec<PathBuf>,
    /// Why the walk could not tell a mount point from a folder: it read
    /// no mount table where it needed one ([`Mounts`]).
    pub mounts_unknown: Option<String>,
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

/// A hard-linked inode the walk found: how many of its links, and what
/// deleting it frees once every link is gone.
struct Shared {
    seen: u64,
    links: u64,
    frees: u64,
    /// The set of full clones it is one of, if any.
    set: Option<Member>,
}

/// A file that is one of a set of APFS full clones ([`Clones`]), by its
/// device and the set's clone ID.
#[derive(Clone, Copy)]
struct Member {
    key: (u64, u64),
    clones: Clones,
}

/// The sets of full clones the walk found files of, each file found whole:
/// with every hard link of it inside the worktree.
#[derive(Default)]
struct Sets {
    /// Sets the walk found one file of, by key alone: most, as pnpm and Bun
    /// clone each file of a node_modules from a store outside it. A set
    /// has two files or more, so none of these is whole inside.
    one: HashSet<(u64, u64)>,
    /// Sets it found more than one file of.
    more: HashMap<(u64, u64), Set>,
}

/// A set of full clones the walk found more than one file of.
struct Set {
    /// Its files the walk found whole.
    inside: u64,
    /// How many files the set has.
    count: u64,
    /// The data bytes they share.
    shared: u64,
}

impl Sets {
    /// The blocks that sets whose every file is inside share: removing the
    /// worktree frees them once, as it frees a hard-linked inode once.
    fn whole(&self) -> u64 {
        self.more
            .values()
            .filter(|set| set.inside >= set.count)
            .map(|set| set.shared)
            .sum()
    }
}

impl Member {
    /// Counts the file, found whole, toward its set. Files of one set agree
    /// on its count and size, and a set's are those of its files after the
    /// first; where two do not, as when one changed during the walk, the
    /// larger count and the smaller size count nothing twice.
    fn join(self, sets: &mut Sets) {
        if let Some(set) = sets.more.get_mut(&self.key) {
            set.inside += 1;
            set.count = set.count.max(self.clones.count);
            set.shared = set.shared.min(self.clones.shared);
        } else if !sets.one.insert(self.key) {
            sets.more.insert(
                self.key,
                Set {
                    inside: 2,
                    count: self.clones.count,
                    shared: self.clones.shared,
                },
            );
        }
    }
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
    measure_with(
        root,
        &|top| Mounts::of(root, top),
        buckets,
        matches,
        named,
        wanted,
    )
}

/// How the walk tells that an entry is on another mount than the worktree's
/// folder: a mount point, which Git's remove would delete through. An entry
/// on another device is one: any folder, and on every system but Linux any
/// entry (FreeBSD's nullfs mounts a file over a file; on Linux a file on
/// overlayfs can show the device of the layer it comes from, and the mount
/// ID tells there). A mount of a folder from the same filesystem keeps the
/// device: a bind mount on Linux, and a loopback mount (lofs) on illumos,
/// which takes the device and fsid of the filesystem it shows (`lo_getattr`,
/// `lo_mount`). FreeBSD's nullfs and unionfs, NetBSD's layered mounts, and
/// macOS's devfs and nullfs give each mount its own (`null_getattr`,
/// `vfs_getnewfsid`); OpenBSD has no such mount; and one disk mounts once,
/// so there the device tells every mount. DragonFly's null mount keeps the
/// device, and nothing there is a candidate (`activity`).
enum Mounts {
    /// By device alone: macOS and the BSDs, and where the mount table could
    /// not be read (`Usage::mounts_unknown`).
    Device,
    /// By the worktree folder's mount ID: Linux 5.8 and later (statx(2)'s
    /// `STATX_MNT_ID`, [`statx`]).
    Id(u64),
    /// By the mount table ([`table`]): where statx gives no mount ID (Linux
    /// before 5.8, a seccomp filter that refuses statx, qemu-user before
    /// 11), and on illumos.
    #[cfg(any(
        test,
        target_os = "linux",
        target_os = "android",
        target_os = "illumos",
        target_os = "solaris"
    ))]
    Table(table::Table),
}

impl Mounts {
    /// How to tell a mount under the worktree at `root`, whose own lstat is
    /// `top`. An `Err` is a mount table this system needs and residue could
    /// not read.
    fn of(root: &Path, top: &Stat) -> io::Result<Mounts> {
        if let Some(id) = top.mount {
            return Ok(Mounts::Id(id));
        }
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "illumos",
            target_os = "solaris"
        ))]
        return table::read(root, top.dev()).map(Mounts::Table);
        #[cfg(not(any(
            target_os = "linux",
            target_os = "android",
            target_os = "illumos",
            target_os = "solaris"
        )))]
        {
            let _ = root;
            Ok(Mounts::Device)
        }
    }

    /// Whether the entry at `path`, as its listing shows it, is on another
    /// mount than the worktree's folder, by more than its device, which
    /// the walk compares itself.
    #[cfg_attr(
        not(any(
            test,
            target_os = "linux",
            target_os = "android",
            target_os = "illumos",
            target_os = "solaris"
        )),
        allow(unused_variables)
    )]
    fn other(&self, path: &Path, meta: &Stat) -> bool {
        // statx says so itself (Linux 5.8 and later): the root of a mount.
        if meta.mount_root {
            return true;
        }
        match self {
            Mounts::Device => false,
            // An entry without a mount ID where the worktree's folder had
            // one cannot be told apart, so it counts as a mount point.
            Mounts::Id(id) => meta.mount != Some(*id),
            #[cfg(any(
                test,
                target_os = "linux",
                target_os = "android",
                target_os = "illumos",
                target_os = "solaris"
            ))]
            Mounts::Table(table) => table.lists(path, (meta.dev(), meta.ino())),
        }
    }
}

/// On Linux a file on overlayfs can show the device of the layer it comes
/// from, so there only a folder on another device counts by its device.
const ANY_ENTRY_BY_DEVICE: bool = !cfg!(any(target_os = "linux", target_os = "android"));

fn measure_with(
    root: &Path,
    how: &dyn Fn(&Stat) -> io::Result<Mounts>,
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
    let mounts = how(&top).unwrap_or_else(|e| {
        usage.mounts_unknown = Some(e.to_string());
        Mounts::Device
    });
    usage.reach.note(root, &top, wanted);
    let lookup: HashMap<PathBuf, usize> = buckets
        .iter()
        .enumerate()
        .map(|(i, b)| (root.join(&b.rel), i))
        .collect();
    let mut walked = vec![0u64; buckets.len()];
    let mut shared: HashMap<(u64, u64), Shared> = HashMap::new();
    let mut sets = Sets::default();
    account(&mut usage, &mut shared, &mut sets, &top, root);
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
    let mut lister = Lister::default();
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
        let listing = list(fd.as_fd(), &dir, &mut lister);
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
            let mount = ((meta.is_dir() || ANY_ENTRY_BY_DEVICE) && meta.dev() != device)
                || mounts.other(&path, &meta);
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
                        } else if !meta.is_dir() || mount {
                            // A file no rule matches, or a mount point whose
                            // contents the walk cannot judge.
                            *state = Expansion::Unmatched(display(&path, root));
                        }
                    }
                }
            }
            if mount {
                if let Some(i) = here.bucket {
                    usage.buckets[i].incomplete = true;
                }
                usage.mount_points.push(path);
                continue;
            }
            let first = account(&mut usage, &mut shared, &mut sets, &meta, &path);
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
            usage.reclaimable += inode.frees;
            if let Some(member) = inode.set {
                member.join(&mut sets);
            }
        }
    }
    usage.reclaimable += sets.whole();
    // A mount the table lists inside the worktree counts even where the
    // walk did not list it by that path.
    #[cfg(any(
        test,
        target_os = "linux",
        target_os = "android",
        target_os = "illumos",
        target_os = "solaris"
    ))]
    if let Mounts::Table(table) = mounts {
        // Read again: a mount made inside the worktree during the walk may
        // be one the walk read through.
        match how(&top) {
            Ok(Mounts::Table(after)) => {
                if let Some(new) = after.inside.iter().find(|p| !table.inside.contains(p)) {
                    usage.mounts_unknown.get_or_insert_with(|| {
                        format!(
                            "a mount appeared at {} while residue read it",
                            display(new, root)
                        )
                    });
                } else if after.roots.iter().any(|id| !table.roots.contains(id)) {
                    usage.mounts_unknown.get_or_insert_with(|| {
                        "a lofs mount on its device appeared while residue read it".to_string()
                    });
                }
            }
            Ok(_) => {}
            Err(e) => {
                usage.mounts_unknown.get_or_insert_with(|| e.to_string());
            }
        }
        usage.mount_points.extend(table.inside);
        usage.mount_points.sort();
        usage.mount_points.dedup();
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
    let Some((at, name)) = holder(root)? else {
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
    // As its listing would show it, with its mount where the system gives one.
    let top = lstat_at(at.as_fd(), &name).map_err(|e| at_path(root, e))?;
    if top.link {
        return Err(at_path(root, changed("it is a symbolic link now")));
    }
    if !top.is_dir() {
        return Err(at_path(root, changed("it is not a folder now")));
    }
    Ok((top, open_listed(at.as_fd(), &name, (top.dev(), top.ino()))))
}

fn at_path(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// The folder that holds the last name of `path`, opened from `/` one name
/// at a time, each relative to the folder before it and never through a
/// symbolic link, with that name; `None` for `/` itself. `path` must be
/// absolute and canonical.
fn holder(path: &Path) -> io::Result<Option<(OwnedFd, CString)>> {
    let mut names = Vec::new();
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => names.push(name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a canonical path", path.display()),
                ));
            }
        }
    }
    if !path.has_root() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not an absolute path", path.display()),
        ));
    }
    let Some((last, above)) = names.split_last() else {
        return Ok(None);
    };
    // SAFETY: open reads the NUL-terminated literal; the descriptor it
    // returns is ours.
    let fd = unsafe { libc::open(c"/".as_ptr(), ABOVE) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor is open and nothing else owns it.
    let mut at = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut reached = PathBuf::from("/");
    for name in above {
        reached.push(name);
        at = leaf(&reached)
            .and_then(|c| open_dir(at.as_fd(), &c, ABOVE))
            .map_err(|e| at_path(&reached, e))?;
    }
    reached.push(last);
    let name = leaf(&reached).map_err(|e| at_path(&reached, e))?;
    Ok(Some((at, name)))
}

/// The lstat of the entry at `path`, looked up as the walk opens a
/// worktree: from `/`, one name at a time, never through a symbolic link,
/// the last name included. A symbolic link anywhere on it is an error.
#[cfg(any(
    test,
    target_os = "linux",
    target_os = "android",
    target_os = "illumos",
    target_os = "solaris"
))]
fn lookup(path: &Path) -> io::Result<Stat> {
    let Some((at, name)) = holder(path)? else {
        // SAFETY: as in `holder`.
        let fd = unsafe { libc::open(c"/".as_ptr(), DIR) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the descriptor is open and nothing else owns it.
        return fstat(unsafe { OwnedFd::from_raw_fd(fd) }.as_fd());
    };
    let stat = lstat_at(at.as_fd(), &name).map_err(|e| at_path(path, e))?;
    if stat.link {
        return Err(at_path(
            path,
            io::Error::other("a symbolic link, which residue does not follow"),
        ));
    }
    Ok(stat)
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
    sets: &mut Sets,
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
    // What deleting it frees: its blocks, less those it shares with a
    // clone, where the system says so. Every link of an inode reports the
    // same, so a hard-linked one is counted once, as its blocks are.
    let frees = meta.unshared.map_or(blocks, |b| b.min(blocks));
    let set = meta.clones.map(|clones| Member {
        key: (meta.dev(), clones.id),
        clones,
    });
    if !meta.is_dir() && meta.nlink() > 1 {
        let inode = shared.entry((meta.dev(), meta.ino())).or_insert(Shared {
            seen: 0,
            links: meta.nlink(),
            frees,
            set,
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
    usage.reclaimable += frees;
    if let Some(member) = set {
        member.join(sets);
    }
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
    link: bool,
    /// The ID of the mount it is on, where the system gives one: Linux 5.8
    /// and later ([`statx`]).
    mount: Option<u64>,
    /// It is the root of a mount, where the system says so: Linux 5.8 and
    /// later (`STATX_ATTR_MOUNT_ROOT`).
    mount_root: bool,
    dev: u64,
    ino: u64,
    nlink: u64,
    /// 512-byte blocks allocated, as `st_blocks`.
    blocks: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
    /// The bytes of its allocation that no other file shares, which
    /// deleting it frees: on APFS, for a file that may share blocks with a
    /// clone (macOS's listing, `bulk`). `None` where the system does not
    /// say, and then deleting it frees all of `blocks`.
    unshared: Option<u64>,
    /// The set of full clones it is one of, when it has others: on APFS.
    clones: Option<Clones>,
}

/// A set of files, on APFS, that are full clones of each other: each
/// shares every data block with the rest. Deleting all of them frees those
/// blocks once, as deleting every link of an inode frees its blocks once.
/// APFS gives each set an ID and counts its files (`ATTR_CMNEXT_CLONEID`
/// and `ATTR_CMNEXT_CLONE_REFCNT`, getattrlist(2)): cargo's copy of a
/// built binary into `target/debug/` is one, beside the file in `deps/`.
#[derive(Clone, Copy)]
// Only macOS's listing (`bulk`) makes one.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Clones {
    /// The set's clone ID, the same for each file in it.
    id: u64,
    /// How many files (inodes) the set has, this one among them.
    count: u64,
    /// The data bytes each file shares with the rest.
    shared: u64,
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
            link: st.st_mode & libc::S_IFMT == libc::S_IFLNK,
            mount: None,
            mount_root: false,
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
            nlink: st.st_nlink as u64,
            blocks: st.st_blocks as u64,
            len: st.st_size as u64,
            mtime: st.st_mtime as i64,
            mtime_nsec: mtime_nsec as i64,
            unshared: None,
            clones: None,
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

/// `lstat` of one entry, relative to its folder: on Linux through statx(2),
/// which also gives its mount.
fn lstat_at(at: BorrowedFd, name: &CStr) -> io::Result<Stat> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Some(stat) = statx::lstat(at, name) {
        return stat;
    }
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

/// statx(2), called directly: the C libraries residue builds with differ in
/// whether they have a wrapper. It gives what `fstatat` gives, the device
/// as `makedev` writes it, as std's metadata does, and the mount the entry
/// is on, which tells a bind mount of a folder from the same filesystem.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod statx {
    use super::Stat;
    use std::ffi::CStr;
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd};
    use std::sync::OnceLock;

    /// What residue knows of statx in this process, settled once by a probe
    /// ([`state`]) so that every walk in it, on any thread, lists the same
    /// way.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        /// The probe has not run.
        Untried,
        /// statx is there: an error from it is that entry's.
        Works,
        /// statx is not there: the probe failed, with `ENOSYS` (Linux before
        /// 4.11), `EPERM` (a seccomp filter that refuses it) or any other
        /// error. The walk uses fstatat and the mount table.
        Missing,
    }

    /// How one call ended.
    #[derive(Clone, Copy, Debug)]
    pub enum Outcome {
        Done,
        NoSys,
        Perm,
        /// Any other error.
        Other,
    }

    /// The state after a call that ended with `outcome`. Only the probe,
    /// from `Untried`, decides; after it, every error is the entry's,
    /// `ENOSYS` and `EPERM` too (a FUSE filesystem with no getattr answers
    /// `ENOSYS`), and leaves that entry unreadable.
    pub fn next(state: State, outcome: Outcome) -> State {
        match (state, outcome) {
            (State::Untried, Outcome::Done) => State::Works,
            // A statx of `/` has no other way to fail, and fstatat is
            // always right.
            (State::Untried, Outcome::NoSys | Outcome::Perm | Outcome::Other) => State::Missing,
            (State::Works, Outcome::Done | Outcome::NoSys | Outcome::Perm | Outcome::Other) => {
                State::Works
            }
            (State::Missing, Outcome::Done | Outcome::NoSys | Outcome::Perm | Outcome::Other) => {
                State::Missing
            }
        }
    }

    fn outcome(done: libc::c_long) -> Outcome {
        if done == 0 {
            return Outcome::Done;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::ENOSYS) => Outcome::NoSys,
            Some(libc::EPERM) => Outcome::Perm,
            _ => Outcome::Other,
        }
    }

    /// The process's state: the first caller runs the probe, a statx of `/`,
    /// while any other waits for it.
    fn state() -> State {
        static STATE: OnceLock<State> = OnceLock::new();
        *STATE.get_or_init(|| {
            let mut stx: libc::statx = unsafe { std::mem::zeroed() };
            // SAFETY: as in `lstat`, with a NUL-terminated literal.
            let done = unsafe {
                libc::syscall(
                    libc::SYS_statx,
                    libc::AT_FDCWD,
                    c"/".as_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
                    MASK,
                    &raw mut stx,
                )
            };
            next(State::Untried, outcome(done))
        })
    }

    /// What the walk reads: the type, links, inode, size, blocks and
    /// modification time, and the mount. The device always comes.
    const MASK: libc::c_uint = libc::STATX_TYPE
        | libc::STATX_NLINK
        | libc::STATX_INO
        | libc::STATX_SIZE
        | libc::STATX_BLOCKS
        | libc::STATX_MTIME
        | libc::STATX_MNT_ID;

    /// `lstat` of `name` in `at`, or `None` where there is no statx. The
    /// mount is `None` where the kernel gives none (before Linux 5.8) or
    /// gives 0 (qemu-user before 11).
    pub fn lstat(at: BorrowedFd, name: &CStr) -> Option<io::Result<Stat>> {
        if state() == State::Missing {
            return None;
        }
        // SAFETY: statx reads the NUL-terminated name, which outlives the
        // call, and writes into the zeroed struct we own. AT_NO_AUTOMOUNT,
        // as fstatat implies it: an automount point is not mounted by it.
        let mut stx: libc::statx = unsafe { std::mem::zeroed() };
        let done = unsafe {
            libc::syscall(
                libc::SYS_statx,
                at.as_raw_fd(),
                name.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
                MASK,
                &raw mut stx,
            )
        };
        if done != 0 {
            return Some(Err(io::Error::last_os_error()));
        }
        Some(Ok(from(&stx)))
    }

    pub fn from(stx: &libc::statx) -> Stat {
        let kind = libc::mode_t::from(stx.stx_mode) & libc::S_IFMT;
        // dev_t is u64 on most Linux targets; `as` widens it where not.
        #[allow(clippy::unnecessary_cast)]
        let dev = libc::makedev(stx.stx_dev_major, stx.stx_dev_minor) as u64;
        let root = libc::STATX_ATTR_MOUNT_ROOT as u64;
        Stat {
            dir: kind == libc::S_IFDIR,
            file: kind == libc::S_IFREG || kind == libc::S_IFLNK,
            link: kind == libc::S_IFLNK,
            mount: (stx.stx_mask & libc::STATX_MNT_ID != 0 && stx.stx_mnt_id != 0)
                .then_some(stx.stx_mnt_id),
            mount_root: stx.stx_attributes_mask & root != 0 && stx.stx_attributes & root != 0,
            dev,
            ino: stx.stx_ino,
            nlink: u64::from(stx.stx_nlink),
            blocks: stx.stx_blocks,
            len: stx.stx_size,
            mtime: stx.stx_mtime.tv_sec,
            mtime_nsec: i64::from(stx.stx_mtime.tv_nsec),
            unshared: None,
            clones: None,
        }
    }
}

/// The mount table, which the walk reads where the stat of an entry cannot
/// tell its mount: wherever statx gives no mount ID on Linux, and on illumos
/// ([`Mounts`]).
#[cfg(any(
    test,
    target_os = "linux",
    target_os = "android",
    target_os = "illumos",
    target_os = "solaris"
))]
mod table {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};

    /// One line of a mount table.
    pub struct Line {
        /// Where the mount is, as the table says: the path it had when it
        /// was mounted. A folder above it renamed since (`git worktree
        /// move` renames one) leaves it there (illumos `vfs.c`).
        pub point: Vec<u8>,
        /// A loopback mount (illumos lofs), which keeps the device of what
        /// it shows: what it shows (the table's first field), and the
        /// device it has, `None` where the table cannot give it.
        pub lofs: Option<(Vec<u8>, Option<u64>)>,
    }

    /// What the walk takes from the table.
    #[derive(Clone, Default, PartialEq, Eq, Debug)]
    pub struct Table {
        /// Each mount point strictly inside the worktree, sorted.
        pub inside: Vec<PathBuf>,
        /// The device and inode of the root of each lofs mount on the
        /// worktree's device, sorted. lofs gives the attributes of the folder
        /// or file it shows, so they are its source's, wherever the mount is
        /// now: a mount the table lists by an old path, and one made on a
        /// folder that another lofs mount shows, which is seen through that
        /// one (`lo_lookup` calls `traverse`) at a path the table does not
        /// list. An entry with one of these is such a root, or the source
        /// itself, which keeps its worktree for nothing.
        pub roots: Vec<(u64, u64)>,
    }

    impl Table {
        /// Whether the entry at `path`, with device and inode `id`, is a
        /// mount point the table gives.
        pub fn lists(&self, path: &Path, id: (u64, u64)) -> bool {
            self.inside
                .binary_search_by(|p| p.as_path().cmp(path))
                .is_ok()
                || self.roots.binary_search(&id).is_ok()
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    const TABLE: &str = "/proc/self/mountinfo";
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const LINE: fn(&[u8]) -> Option<Line> = mountinfo_line;
    #[cfg(any(target_os = "illumos", target_os = "solaris"))]
    const TABLE: &str = "/etc/mnttab";
    #[cfg(any(target_os = "illumos", target_os = "solaris"))]
    const LINE: fn(&[u8]) -> Option<Line> = mnttab_line;

    /// The table for the worktree at `root`, on `device`.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "illumos",
        target_os = "solaris"
    ))]
    pub fn read(root: &Path, device: u64) -> std::io::Result<Table> {
        use std::io;
        let at = |e: io::Error| io::Error::new(e.kind(), format!("{TABLE}: {e}"));
        // Read until two reads agree: before Linux 5.8, a read of the
        // table while a mount comes or goes elsewhere can skip a line.
        let mut last = std::fs::read(TABLE).map_err(at)?;
        for _ in 0..4 {
            let now = std::fs::read(TABLE).map_err(at)?;
            if now == last {
                let lines = lines(&now, LINE).ok_or_else(|| {
                    io::Error::other(format!("{TABLE}: a line residue cannot read"))
                })?;
                let look = |path: &Path| super::lookup(path).map(|s| (s.dev(), s.ino()));
                return table(lines, root, device, &look)
                    .map_err(|e| io::Error::other(format!("{TABLE}: {e}")));
            }
            last = now;
        }
        Err(io::Error::other(format!(
            "{TABLE}: it kept changing while residue read it"
        )))
    }

    /// Each line of `text`, a table of one mount a line. `None` for a line
    /// `line` cannot read: a mount the walk cannot see would be missed.
    pub fn lines(text: &[u8], line: fn(&[u8]) -> Option<Line>) -> Option<Vec<Line>> {
        text.split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .map(line)
            .collect()
    }

    /// What `lines` give the walk of the worktree at `root`, on `device`.
    /// `look` gives the device and inode of a path, looked up without
    /// following a symbolic link (`fsinfo::lookup`). Only the source of a
    /// lofs mount on `device`, or one whose device the table does not give,
    /// is looked up, and its mount point only where the source does not lead
    /// to `device`: a lofs mount on another device is a mount point by its
    /// device, and a lookup of a path on another filesystem could wait on a
    /// network server or mount an automounted folder. A mount on `device`
    /// that neither leads to is an `Err`: it could be inside the worktree.
    pub fn table(
        lines: Vec<Line>,
        root: &Path,
        device: u64,
        look: &dyn Fn(&Path) -> std::io::Result<(u64, u64)>,
    ) -> Result<Table, String> {
        let mut table = Table::default();
        for line in lines {
            let at = PathBuf::from(OsString::from_vec(line.point));
            if let Some((source, dev)) = line.lofs {
                if dev.is_none_or(|d| d == device) {
                    let source = PathBuf::from(OsString::from_vec(source));
                    // The source, then the mount point, which shows it: in a
                    // zone, a source in the global zone is not there to
                    // look up.
                    match look(&source) {
                        Ok(id) if id.0 == device => table.roots.push(id),
                        first => match look(&at) {
                            Ok(id) if id.0 == device => table.roots.push(id),
                            // A mount the table gives no device for, and
                            // that is on another one.
                            second if dev.is_none() && (first.is_ok() || second.is_ok()) => {}
                            second => {
                                let why = |r: std::io::Result<(u64, u64)>| match r {
                                    Ok(_) => "on another device".to_string(),
                                    Err(e) => e.to_string(),
                                };
                                return Err(format!(
                                    "cannot tell what the lofs mount at {} shows: looking up {}: {}; looking up {}: {}",
                                    at.display(),
                                    source.display(),
                                    why(first),
                                    at.display(),
                                    why(second)
                                ));
                            }
                        },
                    }
                }
            }
            if at != root && at.starts_with(root) {
                table.inside.push(at);
            }
        }
        table.inside.sort();
        table.inside.dedup();
        table.roots.sort_unstable();
        table.roots.dedup();
        Ok(table)
    }

    /// A line of Linux's `mountinfo`: the mount point is its fifth field,
    /// fields separated by spaces, with a space, tab, newline or backslash
    /// in it written `\ooo` (proc_pid_mountinfo(5)).
    #[cfg(any(test, target_os = "linux", target_os = "android"))]
    pub fn mountinfo_line(line: &[u8]) -> Option<Line> {
        let field = line.split(|&b| b == b' ').nth(4)?;
        let mut out = Vec::with_capacity(field.len());
        let mut i = 0;
        while let Some(&b) = field.get(i) {
            if b == b'\\' {
                let digits = field.get(i + 1..i + 4)?;
                let mut value = 0u32;
                for &d in digits {
                    if !(b'0'..=b'7').contains(&d) {
                        return None;
                    }
                    value = value * 8 + u32::from(d - b'0');
                }
                out.push(u8::try_from(value).ok()?);
                i += 4;
            } else {
                out.push(b);
                i += 1;
            }
        }
        (out.first() == Some(&b'/')).then_some(Line {
            point: out,
            lofs: None,
        })
    }

    /// A line of illumos's `/etc/mnttab`: exactly five fields separated by
    /// tabs, what is mounted, the mount point, the type, the options and
    /// the time (mnttab(5)). mntfs writes them as they are, so a tab inside
    /// one makes the line, and the table, one residue cannot read. The
    /// option `dev=` is the device in 32 bits (`cmpldev`), or `ffffffff`,
    /// `NODEV32`, where it does not fit (mntfs's comment says `7fffffff`,
    /// which is taken as no device too).
    #[cfg(any(test, target_os = "illumos", target_os = "solaris"))]
    pub fn mnttab_line(line: &[u8]) -> Option<Line> {
        let fields: Vec<&[u8]> = line.split(|&b| b == b'\t').collect();
        let [special, point, kind, options, _] = fields[..] else {
            return None;
        };
        if point.first() != Some(&b'/') {
            return None;
        }
        let lofs = (kind == b"lofs").then(|| {
            let dev = options
                .split(|&b| b == b',')
                .find_map(|o| o.strip_prefix(b"dev="))
                .and_then(|hex| u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok())
                .filter(|&d| d != 0xffff_ffff && d != 0x7fff_ffff)
                // The 64-bit device: major above 32 bits, minor below.
                .map(|d| (u64::from(d >> 18) << 32) | u64::from(d & 0x3_ffff));
            (special.to_vec(), dev)
        });
        Some(Line {
            point: point.to_vec(),
            lofs,
        })
    }
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

/// What the listings of one walk share: nothing, but on macOS.
#[cfg(not(target_os = "macos"))]
#[derive(Default)]
struct Lister {}

#[cfg(not(target_os = "macos"))]
fn list(fd: BorrowedFd, dir: &Path, _: &mut Lister) -> io::Result<Listing> {
    list_readdir(fd, dir)
}

#[cfg(target_os = "macos")]
use bulk::Lister;

/// On macOS, one `getattrlistbulk` call returns a batch of entries with
/// their attributes, where `readdir` plus `fstatat` costs one system call
/// per entry: `du` lists this way through fts. A regular file's fields come
/// from the batch, with what APFS says deleting it frees (`Stat::unshared`,
/// `Stat::clones`). Any other entry, or one the call reports an error for,
/// is read with `fstatat`, exactly as the standard library reads it. A
/// directory the call refuses gets the readdir listing.
#[cfg(target_os = "macos")]
fn list(fd: BorrowedFd, dir: &Path, lister: &mut Lister) -> io::Result<Listing> {
    bulk::list(fd, dir, lister)
}

#[cfg(target_os = "macos")]
mod bulk {
    use super::{Clones, Listed, Listing, Stat, list_readdir, lstat_at};
    use std::ffi::{CStr, OsStr};
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::OnceLock;

    // The libc crate does not have these. <sys/attr.h>:
    const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;
    const ATTR_CMNEXT_CLONE_REFCNT: libc::attrgroup_t = 0x0000_1000;
    // <sys/vnode.h>:
    const VREG: u32 = 1;
    // <sys/stat.h>, "file may share blocks with another file":
    const EF_MAY_SHARE_BLOCKS: u64 = 0x0000_0001;
    /// Bytes per call: room for several hundred entries.
    const BUF_LEN: usize = 128 * 1024;

    const COMMON: libc::attrgroup_t = libc::ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_ERROR
        | libc::ATTR_CMN_NAME
        | libc::ATTR_CMN_DEVID
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_MODTIME
        | libc::ATTR_CMN_FLAGS
        | libc::ATTR_CMN_FILEID;
    const FILE: libc::attrgroup_t = libc::ATTR_FILE_LINKCOUNT
        | libc::ATTR_FILE_ALLOCSIZE
        | libc::ATTR_FILE_DATALENGTH
        | libc::ATTR_FILE_DATAALLOCSIZE;
    /// The common extended attributes residue asks for, in `forkattr` with
    /// `FSOPT_ATTR_CMN_EXTENDED` (getattrlist(2)): whether a file may share
    /// blocks (`flags`), and for one that may, what deleting it frees and
    /// the full clones it is one of (`private`). A file system without them
    /// returns none, and each file counts all its blocks, as before.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Extended {
        pub flags: libc::attrgroup_t,
        pub private: libc::attrgroup_t,
    }

    impl Extended {
        /// Those this kernel knows: getattrlist(2) refuses a bit it does
        /// not know with EINVAL, and xnu's `ATTR_CMNEXT_VALIDMASK` gains
        /// `ATTR_CMNEXT_CLONEID` and `ATTR_CMNEXT_EXT_FLAGS` in xnu-7195
        /// (Darwin 20, macOS 11) and `ATTR_CMNEXT_CLONE_REFCNT` in
        /// xnu-10002 (Darwin 23, macOS 14). Before Darwin 20, and where the
        /// release cannot be read, residue asks for none, as before.
        pub(super) fn known() -> Extended {
            static KNOWN: OnceLock<Extended> = OnceLock::new();
            *KNOWN.get_or_init(|| Extended::of(darwin()))
        }

        /// Whether the kernel counts a file's full clones, which residue
        /// needs to count a set of them inside the worktree once.
        #[cfg(test)]
        pub(super) fn sets(self) -> bool {
            self.private & ATTR_CMNEXT_CLONE_REFCNT != 0
        }

        pub(super) fn of(darwin: Option<u32>) -> Extended {
            match darwin {
                Some(major) if major >= 20 => Extended {
                    flags: libc::ATTR_CMNEXT_EXT_FLAGS,
                    private: libc::ATTR_CMNEXT_PRIVATESIZE
                        | libc::ATTR_CMNEXT_CLONEID
                        | if major >= 23 {
                            ATTR_CMNEXT_CLONE_REFCNT
                        } else {
                            0
                        },
                },
                _ => Extended {
                    flags: 0,
                    private: 0,
                },
            }
        }
    }

    /// The Darwin release's major number, from uname(3): 25 on macOS 26.
    fn darwin() -> Option<u32> {
        // SAFETY: utsname is plain bytes, for which all zeros is valid.
        let mut name: libc::utsname = unsafe { std::mem::zeroed() };
        // SAFETY: uname writes only into `name`, which is ours.
        if unsafe { libc::uname(&mut name) } != 0 {
            return None;
        }
        let release: Vec<u8> = name.release.iter().map(|&c| c as u8).collect();
        let release = CStr::from_bytes_until_nul(&release).ok()?;
        release.to_str().ok()?.split('.').next()?.parse().ok()
    }

    /// What the listings of one walk share: the batch buffer, and whether
    /// the folder listed last held a file that may share blocks.
    ///
    /// Asking for the private size made the listings of a worktree of
    /// 27,000 files that share no blocks take 2.2 times the system time,
    /// measured on macOS 26.3.1 (#193), where `ATTR_CMNEXT_EXT_FLAGS`,
    /// `ATTR_CMN_FLAGS` and `ATTR_FILE_DATAALLOCSIZE` together cost 0 to 15
    /// percent, as much as two runs differ. So a folder's listing asks for
    /// it only after a folder that held such a file, as the folders of a
    /// node_modules that pnpm or Bun cloned do. Any other listing asks each
    /// such file on its own.
    #[derive(Default)]
    pub struct Lister {
        buf: Vec<u8>,
        clones: bool,
    }

    pub fn list(fd: BorrowedFd, dir: &Path, lister: &mut Lister) -> io::Result<Listing> {
        let known = Extended::known();
        let batched = std::mem::take(&mut lister.clones);
        let forkattr = known.flags | if batched { known.private } else { 0 };
        let mut attrs = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: COMMON,
            volattr: 0,
            dirattr: 0,
            fileattr: FILE,
            forkattr,
        };
        let options = if forkattr == 0 {
            0
        } else {
            u64::from(libc::FSOPT_ATTR_CMN_EXTENDED)
        };
        let buf = &mut lister.buf;
        buf.resize(BUF_LEN, 0);
        let mut out = Vec::new();
        let mut first = true;
        loop {
            // SAFETY: the call reads `attrs` and writes at most `buf.len()`
            // bytes into `buf`; both are ours and outlive it. It reads the
            // folder through the descriptor the walk holds, as before.
            let count = unsafe {
                libc::getattrlistbulk(
                    fd.as_raw_fd(),
                    (&raw mut attrs).cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    options,
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
                let Some((mut entry, len)) = buf.get(at..).and_then(parse) else {
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
                if entry.may_share() {
                    lister.clones = true;
                    if !batched {
                        entry.ask_private(fd);
                    }
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
    #[derive(Default)]
    struct Entry<'a> {
        name: &'a CStr,
        error: u32,
        objtype: Option<u32>,
        dev: Option<i32>,
        mtime: Option<(i64, i64)>,
        /// `st_flags`.
        flags: Option<u32>,
        ino: Option<u64>,
        nlink: Option<u32>,
        alloc: Option<i64>,
        len: Option<i64>,
        /// The data fork's allocation.
        data_alloc: Option<i64>,
        /// The data bytes "not trapped inside a clone or snapshot, and which
        /// would be freed immediately if the file were deleted".
        private: Option<i64>,
        clone_id: Option<u64>,
        ext_flags: Option<u64>,
        clone_count: Option<u32>,
    }

    impl Entry<'_> {
        /// The fields of a regular file, when the batch returned all of
        /// them without an error.
        fn stat(&self) -> Option<Stat> {
            if self.error != 0 || self.objtype? != VREG {
                return None;
            }
            let (mtime, mtime_nsec) = self.mtime?;
            let alloc = u64::try_from(self.alloc?).ok()?;
            Some(Stat {
                dir: false,
                file: true,
                link: false,
                mount: None,
                mount_root: false,
                // As std's MetadataExt::dev widens st_dev.
                dev: self.dev? as u64,
                ino: self.ino?,
                nlink: u64::from(self.nlink?),
                // Allocation is in whole file system blocks, so this is
                // exact; st_blocks counts the same allocation.
                blocks: alloc.div_ceil(512),
                len: u64::try_from(self.len?).ok()?,
                mtime,
                mtime_nsec,
                unshared: self.shared().map(|shared| alloc.saturating_sub(shared)),
                clones: self.clones(),
            })
        }

        /// A regular file that may share blocks with another
        /// (`EF_MAY_SHARE_BLOCKS`, which a clone and its source carry) and
        /// is not compressed (`UF_COMPRESSED`): the only files whose
        /// private size residue reads ([`Entry::shared`]).
        fn may_share(&self) -> bool {
            self.error == 0
                && self.objtype == Some(VREG)
                && self.ext_flags.is_some_and(|f| f & EF_MAY_SHARE_BLOCKS != 0)
                && self.flags.is_some_and(|f| f & libc::UF_COMPRESSED == 0)
        }

        /// The data bytes the file shares with a clone, where APFS says:
        /// its data fork's allocation less its private size. Measured on
        /// macOS 26.3.1 (#193), the private size counts the data fork alone
        /// (a resource fork is left out), is 0 for a compressed file, which
        /// keeps its data elsewhere, and is the same for every hard link of
        /// an inode. getattrlist(2) says it also leaves out blocks a
        /// snapshot holds, which deleting the file frees once the snapshot
        /// goes. So it is read only for a file that may share blocks
        /// ([`Entry::may_share`]); any other file, or one an attribute is
        /// missing for, frees all of its blocks.
        fn shared(&self) -> Option<u64> {
            if !self.may_share() {
                return None;
            }
            let data = u64::try_from(self.data_alloc?).ok()?;
            let private = u64::try_from(self.private?).ok()?;
            Some(data.saturating_sub(private))
        }

        /// Asks for the file's private size and clone set on its own,
        /// through the folder's descriptor the walk holds and without
        /// following a link, and keeps them only when the name still names
        /// the inode the batch listed. When the call fails, as when the
        /// file was deleted meanwhile, the attributes stay missing and the
        /// file counts all its blocks: the estimate as before, never a
        /// guess.
        fn ask_private(&mut self, fd: BorrowedFd) {
            let mut attrs = libc::attrlist {
                bitmapcount: libc::ATTR_BIT_MAP_COUNT,
                reserved: 0,
                commonattr: libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_FILEID,
                volattr: 0,
                dirattr: 0,
                fileattr: 0,
                forkattr: Extended::known().private,
            };
            // The length, the returned set, FILEID and the three: 52 bytes.
            let mut buf = [0u8; 64];
            // SAFETY: the call reads `attrs` and the NUL-terminated name, a
            // single component the batch returned, and writes at most
            // `buf.len()` bytes into `buf`; all are ours and outlive it.
            let failed = unsafe {
                libc::getattrlistat(
                    fd.as_raw_fd(),
                    self.name.as_ptr(),
                    (&raw mut attrs).cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    libc::c_ulong::from(libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED),
                )
            } != 0;
            if failed {
                return;
            }
            if let Some(private) = parse_private(&buf).filter(|p| Some(p.ino) == self.ino) {
                self.private = private.size;
                self.clone_id = private.clone_id;
                self.clone_count = private.clone_count;
            }
        }

        /// The set of full clones the file is one of, when it has others.
        fn clones(&self) -> Option<Clones> {
            let count = u64::from(self.clone_count?);
            if count < 2 {
                return None;
            }
            Some(Clones {
                id: self.clone_id?,
                count,
                shared: self.shared()?,
            })
        }
    }

    /// What one file's own call returned ([`Entry::ask_private`]).
    struct Private {
        ino: u64,
        size: Option<i64>,
        clone_id: Option<u64>,
        clone_count: Option<u32>,
    }

    /// Reads [`Entry::ask_private`]'s reply: the length, the returned
    /// attribute set, FILEID, then the returned ones of
    /// `Extended::private` in bit order, as getattrlist(2) lays them out.
    fn parse_private(buf: &[u8]) -> Option<Private> {
        let len = u32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
        let rec = buf.get(..len)?;
        let mut at = 4;
        let common = u32::from_ne_bytes(take(rec, &mut at)?);
        let _vol = take::<4>(rec, &mut at)?;
        let _dir = take::<4>(rec, &mut at)?;
        let _file = take::<4>(rec, &mut at)?;
        let extended = u32::from_ne_bytes(take(rec, &mut at)?);
        if common & libc::ATTR_CMN_FILEID == 0 {
            return None;
        }
        let ino = u64::from_ne_bytes(take(rec, &mut at)?);
        let mut private = Private {
            ino,
            size: None,
            clone_id: None,
            clone_count: None,
        };
        if extended & libc::ATTR_CMNEXT_PRIVATESIZE != 0 {
            private.size = Some(i64::from_ne_bytes(take(rec, &mut at)?));
        }
        if extended & libc::ATTR_CMNEXT_CLONEID != 0 {
            private.clone_id = Some(u64::from_ne_bytes(take(rec, &mut at)?));
        }
        if extended & ATTR_CMNEXT_CLONE_REFCNT != 0 {
            private.clone_count = Some(u32::from_ne_bytes(take(rec, &mut at)?));
        }
        Some(private)
    }

    fn take<const N: usize>(rec: &[u8], at: &mut usize) -> Option<[u8; N]> {
        let bytes = rec.get(*at..at.checked_add(N)?)?;
        *at += N;
        bytes.try_into().ok()
    }

    /// Reads the entry at the start of `buf`, and returns it with its
    /// length. The layout is getattrlist(2)'s: the length, the returned
    /// attribute set, the error code, then each returned attribute in bit
    /// order, common, directory, file, then the common extended ones in
    /// the fork set, every one a multiple of 4 bytes and 4-byte aligned.
    fn parse(buf: &[u8]) -> Option<(Entry<'_>, usize)> {
        let len = u32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
        let rec = buf.get(..len)?;
        let mut at = 4;
        let common = u32::from_ne_bytes(take(rec, &mut at)?);
        let _vol = take::<4>(rec, &mut at)?;
        let _dir = take::<4>(rec, &mut at)?;
        let file = u32::from_ne_bytes(take(rec, &mut at)?);
        let extended = u32::from_ne_bytes(take(rec, &mut at)?);
        let mut entry = Entry::default();
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
        if common & libc::ATTR_CMN_FLAGS != 0 {
            entry.flags = Some(u32::from_ne_bytes(take(rec, &mut at)?));
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
        if file & libc::ATTR_FILE_DATAALLOCSIZE != 0 {
            entry.data_alloc = Some(i64::from_ne_bytes(take(rec, &mut at)?));
        }
        if extended & libc::ATTR_CMNEXT_PRIVATESIZE != 0 {
            entry.private = Some(i64::from_ne_bytes(take(rec, &mut at)?));
        }
        if extended & libc::ATTR_CMNEXT_CLONEID != 0 {
            entry.clone_id = Some(u64::from_ne_bytes(take(rec, &mut at)?));
        }
        if extended & libc::ATTR_CMNEXT_EXT_FLAGS != 0 {
            entry.ext_flags = Some(u64::from_ne_bytes(take(rec, &mut at)?));
        }
        if extended & ATTR_CMNEXT_CLONE_REFCNT != 0 {
            entry.clone_count = Some(u32::from_ne_bytes(take(rec, &mut at)?));
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

/// What [`read_in`] found.
pub enum Read {
    /// No folder at the path: a name on it is missing or not a folder.
    NoFolder,
    /// The folder holds no entry by that name.
    NoFile,
    /// The file's bytes, and the folder's `(device, inode)`.
    File(Vec<u8>, (u64, u64)),
}

/// Reads the regular file `name` in the folder `dir`, at most `limit`
/// bytes, where `dir` comes from a file a cloned repository can hold (a
/// worktree's Cargo config, `target_dir`). `dir` must be absolute, with no
/// `.` or `..`. It opens `dir` from `/` one name at a time, never through a
/// symbolic link, as the walk opens a worktree, and looks a name up only in
/// a folder on one of `devices`, the filesystems the worktree's own path is
/// on: a path from that file never makes residue look into another
/// filesystem, which could wait on a network server or mount an
/// automounted folder. `name` is opened only once lstat shows a regular
/// file, so never a FIFO or a device.
pub fn read_in(dir: &Path, name: &str, devices: &[u64], limit: usize) -> io::Result<Read> {
    let mut names = Vec::new();
    for part in dir.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(n) => names.push(n),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a canonical path", dir.display()),
                ));
            }
        }
    }
    if !dir.has_root() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not an absolute path", dir.display()),
        ));
    }
    // SAFETY: open reads the NUL-terminated literal; the descriptor it
    // returns is ours.
    let fd = unsafe { libc::open(c"/".as_ptr(), ABOVE) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor is open and nothing else owns it.
    let mut at = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut here = fstat(at.as_fd())?;
    let mut reached = PathBuf::from("/");
    let elsewhere = |path: &Path| {
        at_path(
            path,
            io::Error::other(
                "on another filesystem than the worktree's, which residue does not look into",
            ),
        )
    };
    for part in names {
        if !devices.contains(&here.dev()) {
            return Err(elsewhere(&reached));
        }
        reached.push(part);
        let c = leaf(&reached).map_err(|e| at_path(&reached, e))?;
        let next = match lstat_at(at.as_fd(), &c) {
            Ok(next) => next,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Read::NoFolder),
            Err(e) => return Err(at_path(&reached, e)),
        };
        if next.link {
            return Err(at_path(
                &reached,
                io::Error::other("a symbolic link, which residue does not follow"),
            ));
        }
        if !next.is_dir() {
            return Ok(Read::NoFolder);
        }
        at = open_dir(at.as_fd(), &c, ABOVE).map_err(|e| at_path(&reached, e))?;
        here = fstat(at.as_fd()).map_err(|e| at_path(&reached, e))?;
        if (here.dev(), here.ino()) != (next.dev(), next.ino()) {
            return Err(at_path(&reached, changed("another folder is in its place")));
        }
    }
    if !devices.contains(&here.dev()) {
        return Err(elsewhere(&reached));
    }
    let folder = (here.dev(), here.ino());
    let path = reached.join(name);
    let c = leaf(&path).map_err(|e| at_path(&path, e))?;
    let meta = match lstat_at(at.as_fd(), &c) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Read::NoFile),
        Err(e) => return Err(at_path(&path, e)),
    };
    if meta.link {
        return Err(at_path(
            &path,
            io::Error::other("a symbolic link, which residue does not follow"),
        ));
    }
    // A regular file: `file` counts a symbolic link too, refused above.
    if !meta.is_file() {
        return Err(at_path(&path, io::Error::other("not a regular file")));
    }
    // SAFETY: openat reads the NUL-terminated name, which outlives the
    // call; the descriptor it returns is ours.
    let fd = unsafe {
        libc::openat(
            at.as_raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(at_path(&path, io::Error::last_os_error()));
    }
    // SAFETY: the descriptor is open and nothing else owns it.
    let file = unsafe { OwnedFd::from_raw_fd(fd) };
    let now = fstat(file.as_fd()).map_err(|e| at_path(&path, e))?;
    if (now.dev(), now.ino()) != (meta.dev(), meta.ino()) {
        return Err(at_path(&path, changed("another file is in its place")));
    }
    let mut bytes = Vec::new();
    io::Read::read_to_end(
        &mut io::Read::take(fs::File::from(file), limit as u64 + 1),
        &mut bytes,
    )
    .map_err(|e| at_path(&path, e))?;
    if bytes.len() > limit {
        return Err(at_path(
            &path,
            io::Error::other(format!("larger than {limit} bytes")),
        ));
    }
    Ok(Read::File(bytes, folder))
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
    use super::bulk::Extended;
    #[cfg(target_os = "macos")]
    use super::git_spelling;
    use super::{
        Bucket, HELD, Mounts, Parent, Pending, Place, REOPENED, Reopened, Step, lstat_at, measure,
        measure_with, open_listed, open_root, spell_names, table,
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

    const MIB: usize = 1024 * 1024;

    /// A file of `len` bytes of its own, written to disk, so its blocks are
    /// allocated when the walk reads it.
    fn write_synced(path: &Path, len: usize, seed: u8) {
        use std::io::Write;
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8 ^ seed).collect();
        let mut file = fs::File::create(path).unwrap();
        file.write_all(&data).unwrap();
        file.sync_all().unwrap();
    }

    /// Allocated bytes, and the bytes of them that removal does not free.
    fn kept(w: &Path) -> (u64, u64) {
        let usage = measure(w, &[], &|_| false, &|_| false, &[]).unwrap();
        (usage.allocated, usage.allocated - usage.reclaimable)
    }

    /// A hard-linked file counts once, and frees nothing while a link of it
    /// is outside the worktree.
    #[test]
    fn a_hard_link_outside_keeps_the_file_and_one_inside_counts_once() {
        let root = scratch("links");
        let (w, outside) = (root.join("w"), root.join("outside"));
        fs::create_dir_all(&w).unwrap();
        fs::create_dir_all(&outside).unwrap();
        write_synced(&w.join("pair"), MIB, 1);
        fs::hard_link(w.join("pair"), w.join("pair2")).unwrap();
        write_synced(&w.join("kept"), MIB, 2);
        fs::hard_link(w.join("kept"), outside.join("kept")).unwrap();
        let blocks = |name: &str| {
            use std::os::unix::fs::MetadataExt;
            fs::symlink_metadata(w.join(name)).unwrap().blocks() * 512
        };
        assert_eq!(
            kept(&w),
            (blocks("pair") + blocks("kept") + blocks(""), blocks("kept"))
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// An APFS clone of `from` at `to`, as `cp -c` makes one, or false where
    /// the file system makes none.
    #[cfg(target_os = "macos")]
    fn clone_file(from: &Path, to: &Path) -> bool {
        let from = CString::new(from.as_os_str().as_bytes()).unwrap();
        let to = CString::new(to.as_os_str().as_bytes()).unwrap();
        // SAFETY: both are NUL-terminated paths that outlive the call,
        // which only reads them.
        if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            return true;
        }
        let error = std::io::Error::last_os_error();
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ENOTSUP),
            "clonefile: {error}"
        );
        false
    }

    /// On APFS, the blocks a file shares with a clone outside the worktree
    /// are not freed by removing it, and a file and its full clone, both
    /// inside, free their blocks once. Allocated size counts every file's
    /// blocks, as before (#193). Each case runs twice: in the worktree's
    /// top folder, which the walk lists first, so each file is asked on its
    /// own, and in a folder listed after one that held a clone, so its
    /// listing call asks for every file's private size.
    #[cfg(target_os = "macos")]
    #[test]
    fn blocks_shared_with_a_clone_count_only_where_removal_frees_them() {
        let root = scratch("clones");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        for (seed, name) in [(2, "primer"), (3, "a"), (4, "b"), (5, "c")] {
            write_synced(&outside.join(name), MIB, seed);
        }
        if !clone_file(&outside.join("a"), &root.join("probe")) {
            eprintln!("skip: {} makes no clones", root.display());
            fs::remove_dir_all(&root).unwrap();
            return;
        }
        let mib = MIB as u64;
        for batched in [false, true] {
            // The worktree, and the folder the case goes in. Batched, the
            // walk lists `w/primer/` alone after `w/`, and it holds a clone
            // of a file kept elsewhere, so the case's folder, the only one
            // in it, is listed next with every private size. The primer is
            // 1 MiB allocated that removal does not free.
            let worktree = |name: &str| {
                let w = root.join(format!("{name}-{batched}"));
                if !batched {
                    fs::create_dir_all(&w).unwrap();
                    return (w.clone(), w);
                }
                let case = w.join("primer").join("case");
                fs::create_dir_all(&case).unwrap();
                let primer = w.join("primer").join("primer");
                assert!(clone_file(&outside.join("primer"), &primer));
                (w, case)
            };
            let primed = if batched { mib } else { 0 };
            let at = |what: &str| format!("{what}, batched: {batched}");

            // A clone of a file kept elsewhere frees nothing; a file of its
            // own frees its blocks.
            let (w, case) = worktree("full");
            assert!(clone_file(&outside.join("a"), &case.join("a")));
            write_synced(&case.join("own"), MIB, 6);
            let want = (2 * mib + primed, mib + primed);
            assert_eq!(kept(&w), want, "{}", at("a clone of a file kept"));

            // With its first quarter rewritten, it frees that quarter.
            let (w, case) = worktree("partial");
            assert!(clone_file(&outside.join("b"), &case.join("b")));
            {
                use std::io::Write;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .open(case.join("b"))
                    .unwrap();
                file.write_all(&vec![0xa5; MIB / 4]).unwrap();
                file.sync_all().unwrap();
            }
            let want = (mib + primed, 3 * mib / 4 + primed);
            assert_eq!(kept(&w), want, "{}", at("a partial clone"));

            // Two links of one clone of a file kept elsewhere: the inode
            // once, and it frees nothing.
            let (w, case) = worktree("linked");
            assert!(clone_file(&outside.join("c"), &case.join("c")));
            fs::hard_link(case.join("c"), case.join("c2")).unwrap();
            let want = (mib + primed, mib + primed);
            assert_eq!(kept(&w), want, "{}", at("a hard-linked clone"));

            // A file and its clone, both inside, the clone with a second
            // link: removing them frees the blocks they share, once. Before
            // macOS 14 the kernel does not count a file's clones, so they
            // count for neither.
            let (w, case) = worktree("pair");
            write_synced(&case.join("src"), MIB, 7);
            assert!(clone_file(&case.join("src"), &case.join("dst")));
            fs::hard_link(case.join("dst"), case.join("dst2")).unwrap();
            let once = if Extended::known().sets() {
                mib
            } else {
                2 * mib
            };
            let want = (2 * mib + primed, once + primed);
            assert_eq!(kept(&w), want, "{}", at("a file and its clone"));
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// The listing asks only for the extended attributes the kernel knows,
    /// by xnu's `ATTR_CMNEXT_VALIDMASK` in each release, and none where it
    /// cannot read the release.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_listing_asks_only_for_attributes_the_kernel_knows() {
        let none = Extended::of(None);
        assert_eq!((none.flags, none.private), (0, 0));
        assert_eq!(Extended::of(Some(19)), none, "macOS 10.15");
        let eleven = Extended::of(Some(20));
        assert_eq!(eleven.flags, libc::ATTR_CMNEXT_EXT_FLAGS);
        assert_eq!(
            eleven.private,
            libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_CLONEID
        );
        assert!(!eleven.sets());
        assert_eq!(Extended::of(Some(22)), eleven, "macOS 13");
        let fourteen = Extended::of(Some(23));
        assert_eq!(fourteen.private, eleven.private | 0x0000_1000);
        assert!(fourteen.sets());
        assert_eq!(Extended::of(Some(25)), fourteen, "macOS 26");
        let mut uname = std::process::Command::new("/usr/bin/uname");
        uname.arg("-r");
        let release = crate::run::run(&mut uname, std::time::Duration::from_secs(30), 4096);
        let major = String::from_utf8(release.unwrap().stdout).unwrap();
        let major = major.split('.').next().unwrap().parse().unwrap();
        assert_eq!(Extended::known(), Extended::of(Some(major)), "uname -r");
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

    /// The mount points strictly inside the worktree that a table gives.
    fn inside(
        text: &[u8],
        root: &Path,
        line: fn(&[u8]) -> Option<table::Line>,
    ) -> Option<Vec<PathBuf>> {
        let none =
            |_: &Path| -> std::io::Result<(u64, u64)> { panic!("no lofs source is looked up") };
        table::lines(text, line).map(|lines| table::table(lines, root, 0, &none).unwrap().inside)
    }

    /// Both tables' lines, as Linux's mountinfo and illumos's mnttab write
    /// them: only a mount point strictly inside the worktree counts, and a
    /// line the parser cannot read makes the whole table unreadable. A
    /// mnttab line has exactly five fields.
    #[test]
    fn the_mount_table_gives_each_mount_strictly_inside_the_worktree() {
        let root = Path::new("/srv/rc/wt");
        let mountinfo = b"22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
36 22 8:1 /srv/shared /srv/rc/wt/build rw,relatime shared:1 - ext4 /dev/sda1 rw
37 22 0:52 / /srv/rc/wt/my\\040out\\011put rw - tmpfs tmpfs rw
38 22 8:1 /srv/x /srv/rc/wtx/build rw - ext4 /dev/sda1 rw
39 22 8:1 /srv/y /srv/rc/wt rw - ext4 /dev/sda1 rw
40 36 8:1 /srv/z /srv/rc/wt/build/deep\\134er rw - ext4 /dev/sda1 rw
";
        let found = inside(mountinfo, root, table::mountinfo_line).unwrap();
        assert_eq!(
            found,
            [
                root.join("build"),
                root.join("build/deep\\er"),
                root.join("my out\tput"),
            ]
        );
        assert_eq!(inside(b"", root, table::mountinfo_line), Some(vec![]));
        for bad in [
            &b"36 22 8:1 /srv/shared\n"[..],
            b"36 22 8:1 /a /srv/rc/wt/x\\08 rw - ext4 /dev/sda1 rw",
            b"36 22 8:1 /a /srv/rc/wt/x\\40 rw - ext4 /dev/sda1 rw",
            b"36 22 8:1 /a relative rw - ext4 /dev/sda1 rw",
        ] {
            assert_eq!(
                inside(bad, root, table::mountinfo_line),
                None,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }

        let mnttab = b"rpool/ROOT/omnios\t/\tzfs\tdev=4010002\t0
/srv/shared\t/srv/rc/wt/build\tlofs\tdev=4010002\t1762700000
swap\t/srv/rc/wt/a b\ttmpfs\txattr,dev=5000001\t1762700000
/srv/x\t/srv/rc/wtx/build\tlofs\tdev=4010002\t1762700000
";
        let found = inside(mnttab, root, table::mnttab_line).unwrap();
        assert_eq!(found, [root.join("a b"), root.join("build")]);
        for bad in [
            &b"swap\n"[..],
            // Four fields, and six: a tab inside a mount point.
            b"swap\t/srv/rc/wt/a\ttmpfs\txattr",
            b"swap\t/srv/rc/wt/a\tb\ttmpfs\txattr,dev=5000001\t1762700000",
            b"swap\trelative\ttmpfs\txattr\t1762700000",
        ] {
            assert_eq!(
                inside(bad, root, table::mnttab_line),
                None,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    /// The `dev=` option of a mnttab line is the 32-bit device (`cmpldev`):
    /// the walk turns it into the 64-bit one stat gives, major above 32
    /// bits. `7fffffff`, a device that does not fit, and a line without the
    /// option give none.
    #[test]
    fn a_mnttab_line_gives_its_lofs_source_and_device() {
        let line =
            table::mnttab_line(b"/srv/shared\t/srv/rc/wt/build\tlofs\trw,dev=4010002\t1762700000")
                .unwrap();
        assert_eq!(line.point, b"/srv/rc/wt/build");
        assert_eq!(
            line.lofs,
            Some((b"/srv/shared".to_vec(), Some((0x100 << 32) | 0x10002)))
        );
        for (options, dev) in [
            ("dev=ffffffff", None),
            ("dev=7fffffff", None),
            ("rw,nosub", None),
            ("dev=zz", None),
            ("dev=5", Some(5)),
        ] {
            let text = format!("/a\t/b\tlofs\t{options}\t0");
            let line = table::mnttab_line(text.as_bytes()).unwrap();
            assert_eq!(line.lofs.unwrap().1, dev, "{options}");
        }
        let line = table::mnttab_line(b"swap\t/tmp\ttmpfs\tdev=5\t0").unwrap();
        assert!(line.lofs.is_none(), "only lofs keeps the device");
    }

    /// Only the source of a lofs mount on the worktree's device, or of one
    /// whose device the table does not give, is looked up, and its device
    /// and inode are a mount's root wherever the mount point is now. A
    /// source on the device that cannot be looked up, or that is on another
    /// device now, makes the table one residue cannot use. A source the
    /// table gives no device for that is on another device is left out.
    #[test]
    fn a_lofs_mount_on_the_worktrees_device_counts_by_its_source() {
        use std::cell::RefCell;
        let dev = |d: u64| format!("{:x}", ((d >> 32) << 18) | (d & 0x3_ffff));
        let device = (7 << 32) | 3;
        let other = (7 << 32) | 4;
        let text = format!(
            "/src/here\t/old/wt/build\tlofs\tdev={}\t0\n/src/far\t/x\tlofs\tdev={}\t0\n/src/unsaid\t/y\tlofs\tdev=ffffffff\t0\n/src/unsaid-far\t/z\tlofs\trw\t0\nswap\t/srv/rc/wt/t\ttmpfs\tdev={}\t0\n",
            dev(device),
            dev(other),
            dev(other)
        );
        let looked = RefCell::new(Vec::new());
        let look = |path: &Path| -> std::io::Result<(u64, u64)> {
            looked.borrow_mut().push(path.to_path_buf());
            match path.to_str().unwrap() {
                "/src/here" => Ok((device, 11)),
                "/src/unsaid" => Ok((device, 12)),
                "/src/unsaid-far" => Ok((other, 13)),
                "/z" => Ok((other, 14)),
                _ => panic!("looked up {}", path.display()),
            }
        };
        let lines = table::lines(text.as_bytes(), table::mnttab_line).unwrap();
        let t = table::table(lines, Path::new("/srv/rc/wt"), device, &look).unwrap();
        assert_eq!(
            *looked.borrow(),
            [
                PathBuf::from("/src/here"),
                PathBuf::from("/src/unsaid"),
                PathBuf::from("/src/unsaid-far"),
                PathBuf::from("/z")
            ]
        );
        assert_eq!(t.roots, [(device, 11), (device, 12)]);
        assert_eq!(t.inside, [PathBuf::from("/srv/rc/wt/t")]);
        // The mount point the table names is stale; the walk finds the root
        // at the folder's new path by its device and inode.
        assert!(t.lists(Path::new("/new/wt/build"), (device, 11)));
        assert!(!t.lists(Path::new("/new/wt/build"), (device, 14)));

        // Where the source does not lead to the device, the mount point,
        // which shows it, is looked up instead.
        let text = format!("/src/here\t/old/wt/build\tlofs\tdev={}\t0\n", dev(device));
        let gone = || Err(std::io::Error::other("No such file or directory"));
        for (source, point, want) in [
            (gone(), Ok((device, 21)), Ok(vec![(device, 21)])),
            (Ok((other, 1)), Ok((device, 22)), Ok(vec![(device, 22)])),
            (
                gone(),
                gone(),
                Err(
                    "cannot tell what the lofs mount at /old/wt/build shows: looking up /src/here: No such file or directory; looking up /old/wt/build: No such file or directory",
                ),
            ),
            (
                Ok((other, 1)),
                Ok((other, 2)),
                Err(
                    "cannot tell what the lofs mount at /old/wt/build shows: looking up /src/here: on another device; looking up /old/wt/build: on another device",
                ),
            ),
        ] {
            let answers = RefCell::new(vec![point, source]);
            let look = |_: &Path| answers.borrow_mut().pop().unwrap();
            let lines = table::lines(text.as_bytes(), table::mnttab_line).unwrap();
            let got = table::table(lines, Path::new("/srv/rc/wt"), device, &look).map(|t| t.roots);
            assert_eq!(got, want.map_err(str::to_string));
        }
        // With no device in the table, a mount whose source and mount point
        // lead elsewhere is on another device, and left out.
        let answers = RefCell::new(vec![gone(), Ok((other, 1))]);
        let look = |_: &Path| answers.borrow_mut().pop().unwrap();
        let lines = table::lines(b"/src/x\t/y\tlofs\trw\t0\n", table::mnttab_line).unwrap();
        assert_eq!(
            table::table(lines, Path::new("/srv/rc/wt"), device, &look)
                .unwrap()
                .roots,
            []
        );
    }

    /// The lookup of a lofs source goes from `/` one name at a time and
    /// never through a symbolic link, the last name included, and gives a
    /// file as well as a folder.
    #[test]
    fn a_lofs_source_is_looked_up_without_following_links() {
        use std::os::unix::fs::MetadataExt;
        let root = scratch("source");
        fs::create_dir_all(root.join("d")).unwrap();
        fs::write(root.join("d").join("f"), "x").unwrap();
        symlink(root.join("d"), root.join("l")).unwrap();
        let file = super::lookup(&root.join("d").join("f")).unwrap();
        let meta = fs::symlink_metadata(root.join("d").join("f")).unwrap();
        assert_eq!((file.dev(), file.ino()), (meta.dev(), meta.ino()));
        assert!(super::lookup(&root.join("d")).unwrap().is_dir());
        for through in [root.join("l"), root.join("l").join("f")] {
            let Err(e) = super::lookup(&through) else {
                panic!("{through:?} was looked up through a symbolic link");
            };
            assert!(e.to_string().contains("symbolic link"), "{through:?}: {e}");
        }
        assert!(super::lookup(Path::new("relative")).is_err());
        assert!(super::lookup(&root.join("d").join("..").join("d")).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    /// A mount the table lists is a mount point: the walk records it, marks
    /// the ignored entry it is in incomplete, and never reads it. One the
    /// walk does not list by that path counts too, and so does a folder
    /// with a lofs mount's root. A table the walk needed and could not
    /// read, at the start or after the walk, or a mount inside the worktree
    /// that the second read finds and the first did not, leaves the mounts
    /// unknown.
    #[test]
    fn a_mount_the_table_lists_is_a_mount_point_and_never_read() {
        use std::cell::Cell;
        use std::os::unix::fs::MetadataExt;
        let root = scratch("table");
        let w = root.join("w");
        fs::create_dir_all(w.join("build").join("deep")).unwrap();
        fs::write(w.join("build").join("deep").join("f"), "outside").unwrap();
        fs::create_dir_all(w.join("lofs").join("in")).unwrap();
        fs::write(w.join("lofs").join("in").join("g"), "outside").unwrap();
        fs::write(w.join("keep"), "x").unwrap();
        let buckets = [Bucket {
            rel: PathBuf::from("build"),
            expand: false,
        }];
        let lofs = fs::metadata(w.join("lofs").join("in")).unwrap();
        let listed = table::Table {
            inside: vec![w.join("build").join("deep"), w.join("gone").join("x")],
            roots: vec![(lofs.dev(), lofs.ino())],
        };
        let walk = |how: &dyn Fn(&super::Stat) -> std::io::Result<Mounts>| {
            measure_with(&w, how, &buckets, &|_| true, &|_| false, &[]).unwrap()
        };
        let usage = walk(&|_| Ok(Mounts::Table(listed.clone())));
        assert_eq!(
            usage.mount_points,
            [
                w.join("build").join("deep"),
                w.join("gone").join("x"),
                w.join("lofs").join("in")
            ]
        );
        assert!(usage.buckets[0].seen);
        assert!(usage.buckets[0].incomplete);
        assert_eq!(usage.buckets[0].files, 0, "nothing below the mount is read");
        assert_eq!(usage.entries, 4, "w, build, lofs and keep");
        assert!(usage.mounts_unknown.is_none());

        let usage = walk(&|_| Err(std::io::Error::other("/proc/self/mountinfo: gone")));
        assert_eq!(
            usage.mounts_unknown.as_deref(),
            Some("/proc/self/mountinfo: gone")
        );
        assert!(usage.mount_points.is_empty());

        // The second read finds a mount inside the worktree the first did
        // not, or fails.
        let reads = Cell::new(0);
        let usage = walk(&|_| {
            reads.set(reads.get() + 1);
            let mut t = listed.clone();
            if reads.get() > 1 {
                t.inside.push(w.join("keep"));
                t.inside.sort();
            }
            Ok(Mounts::Table(t))
        });
        assert_eq!(reads.get(), 2);
        assert_eq!(
            usage.mounts_unknown.as_deref(),
            Some("a mount appeared at keep while residue read it")
        );
        let reads = Cell::new(0);
        let usage = walk(&|_| {
            reads.set(reads.get() + 1);
            if reads.get() > 1 {
                return Err(std::io::Error::other("/etc/mnttab: gone"));
            }
            Ok(Mounts::Table(listed.clone()))
        });
        assert_eq!(usage.mounts_unknown.as_deref(), Some("/etc/mnttab: gone"));
        // A lofs mount on its device that the second read finds.
        let reads = Cell::new(0);
        let usage = walk(&|_| {
            reads.set(reads.get() + 1);
            let mut t = listed.clone();
            if reads.get() > 1 {
                t.roots.push((u64::MAX, u64::MAX));
            }
            Ok(Mounts::Table(t))
        });
        assert_eq!(
            usage.mounts_unknown.as_deref(),
            Some("a lofs mount on its device appeared while residue read it")
        );
        // One that is gone by the second read is not new.
        let reads = Cell::new(0);
        let usage = walk(&|_| {
            reads.set(reads.get() + 1);
            let mut t = listed.clone();
            if reads.get() > 1 {
                t.inside.clear();
            }
            Ok(Mounts::Table(t))
        });
        assert!(usage.mounts_unknown.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    /// statx is missing when the probe, a statx of `/`, fails at all:
    /// ENOSYS, EPERM or anything else. After a probe that worked, any error
    /// is that entry's, ENOSYS and EPERM included (a FUSE filesystem with
    /// no getattr answers ENOSYS), and nothing moves the state again.
    #[cfg(target_os = "linux")]
    #[test]
    fn statx_is_missing_only_when_its_probe_is_refused() {
        use super::statx::{Outcome, State, next};
        let all = [Outcome::Done, Outcome::NoSys, Outcome::Perm, Outcome::Other];
        assert_eq!(next(State::Untried, Outcome::Done), State::Works);
        assert_eq!(next(State::Untried, Outcome::Other), State::Missing);
        assert_eq!(next(State::Untried, Outcome::NoSys), State::Missing);
        assert_eq!(next(State::Untried, Outcome::Perm), State::Missing);
        assert_eq!(next(State::Works, Outcome::NoSys), State::Works);
        assert_eq!(next(State::Works, Outcome::Perm), State::Works);
        for outcome in all {
            assert_eq!(next(State::Works, outcome), State::Works);
            assert_eq!(next(State::Missing, outcome), State::Missing);
        }
        for first in all {
            for then in all {
                let settled = next(State::Untried, first);
                assert_eq!(next(settled, then), settled, "{first:?} then {then:?}");
            }
        }
    }

    /// On Linux the listing is statx(2): every field the walk and the
    /// fingerprint read is the one fstatat gives, for a folder, a symbolic
    /// link and a regular file, and an entry on another mount than the
    /// worktree's folder is a mount point that the walk never reads,
    /// whatever its device.
    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_the_listing_reads_each_entrys_mount() {
        use super::{Stat, lstat_raw};
        use std::os::fd::AsFd;
        let root = scratch("statx");
        let w = root.join("w");
        fs::create_dir_all(w.join("d").join("e")).unwrap();
        fs::write(w.join("d").join("f"), "12345").unwrap();
        symlink("d", w.join("l")).unwrap();
        let fields = |s: &Stat| {
            (
                s.dir,
                s.file,
                s.link,
                s.dev,
                s.ino,
                s.nlink,
                s.blocks,
                s.len,
                s.mtime,
                s.mtime_nsec,
            )
        };
        for (at, name) in [(&w, "d"), (&w, "l"), (&w.join("d"), "f")] {
            let dir = open(at);
            let new = lstat_at(dir.as_fd(), &c(name)).unwrap();
            let old = Stat::from(&lstat_raw(dir.as_fd(), &c(name)).unwrap());
            assert_eq!(fields(&new), fields(&old), "{name}");
            assert!(!new.mount_root, "{name}");
        }
        let top = open_root(&w).unwrap().0;
        // Linux 5.8 and later; CI's kernels are.
        let Some(mount) = top.mount else {
            eprintln!("this kernel gives no mount ID; the walk reads the mount table");
            fs::remove_dir_all(&root).unwrap();
            return;
        };
        let usage = measure(&w, &[], &|_| false, &|_| false, &[]).unwrap();
        assert!(usage.mount_points.is_empty());
        assert!(usage.mounts_unknown.is_none());
        assert_eq!(usage.entries, 5, "w, d, d/e, d/f and l");
        let usage = measure_with(
            &w,
            &|_| Ok(Mounts::Id(mount + 1)),
            &[],
            &|_| false,
            &|_| false,
            &[],
        )
        .unwrap();
        let mut found = usage.mount_points.clone();
        found.sort();
        assert_eq!(found, [w.join("d"), w.join("l")]);
        assert_eq!(usage.entries, 1, "only w itself");
        fs::remove_dir_all(&root).unwrap();
    }

    /// /proc is a mount of its own on Linux: statx says that its folder is
    /// the root of a mount (Linux 5.8 and later), which counts by itself
    /// where mount IDs are 0 (qemu-user before 11).
    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_a_mount_root_counts_without_a_mount_id() {
        use std::os::fd::AsFd;
        let slash = open(Path::new("/"));
        let proc = lstat_at(slash.as_fd(), &c("proc")).unwrap();
        let etc = lstat_at(slash.as_fd(), &c("etc")).unwrap();
        if proc.mount.is_none() && !proc.mount_root {
            eprintln!("this kernel says neither; the walk reads the mount table");
            return;
        }
        assert!(proc.mount_root);
        assert!(!etc.mount_root);
        let none = super::Mounts::Device;
        assert!(none.other(Path::new("/proc"), &proc));
        assert!(!none.other(Path::new("/etc"), &etc));
    }
}
