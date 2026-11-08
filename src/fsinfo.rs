//! Disk usage of one worktree, and free space on its filesystem.
//!
//! The walk never follows symbolic links and never crosses into another
//! filesystem. It records what would make a removal surprising: nested
//! repositories, mount points and entries it could not read. The same walk
//! attributes files and bytes to each ignored entry (a bucket), and judges
//! the contents of a collapsed ignored directory that a path rule reaches
//! into, so the worktree is read once.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The most entries residue judges one by one under a collapsed ignored
/// directory. Past it the directory is not called regenerable.
pub const EXPAND_LIMIT: u64 = 100_000;

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
}

pub fn measure(
    root: &Path,
    buckets: &[Bucket],
    matches: &dyn Fn(&str) -> bool,
) -> io::Result<Usage> {
    let top = Stat::from(&fs::symlink_metadata(root)?);
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
    let mut stack = vec![(
        root.to_path_buf(),
        Place {
            bucket: None,
            covered: false,
        },
    )];
    let mut buf = Vec::new();
    while let Some((dir, place)) = stack.pop() {
        let listing = match list(&dir, &mut buf) {
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
                if !meta.is_dir() && first {
                    usage.buckets[i].files += 1;
                    usage.buckets[i].bytes += meta.len();
                }
            }
            if meta.is_dir() {
                stack.push((path, here));
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

impl From<&Metadata> for Stat {
    fn from(meta: &Metadata) -> Stat {
        Stat {
            dir: meta.is_dir(),
            dev: meta.dev(),
            ino: meta.ino(),
            nlink: meta.nlink(),
            blocks: meta.blocks(),
            len: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
        }
    }
}

/// One entry of a directory listing.
struct Listed {
    path: PathBuf,
    /// The entry is named `.git`.
    git: bool,
    stat: io::Result<Stat>,
}

/// The entries of one directory, in the order the file system lists them.
/// An `Err` item is a listing error, after which the listing ends, as
/// `fs::read_dir` does; an entry whose `stat` failed carries that error.
type Listing = Vec<io::Result<Listed>>;

/// The listing through the standard library: `readdir`, then one `lstat`
/// per entry (relative to the directory where the platform allows).
fn list_std(dir: &Path) -> io::Result<Listing> {
    let mut out = Vec::new();
    for item in fs::read_dir(dir)? {
        out.push(item.map(|item| Listed {
            git: item.file_name() == ".git",
            path: item.path(),
            stat: item.metadata().map(|meta| Stat::from(&meta)),
        }));
    }
    Ok(out)
}

#[cfg(not(target_os = "macos"))]
fn list(dir: &Path, _buf: &mut Vec<u8>) -> io::Result<Listing> {
    list_std(dir)
}

/// On macOS, one `getattrlistbulk` call returns a batch of entries with
/// their attributes, where `readdir` plus `fstatat` costs one system call
/// per entry: `du` lists this way through fts. A regular file's fields come
/// from the batch. Any other entry, or one the call reports an error for,
/// is read with `fstatat`, exactly as the standard library reads it. A
/// directory the call refuses gets the standard listing.
#[cfg(target_os = "macos")]
fn list(dir: &Path, buf: &mut Vec<u8>) -> io::Result<Listing> {
    bulk::list(dir, buf)
}

#[cfg(target_os = "macos")]
mod bulk {
    use super::{Listed, Listing, Stat, list_std};
    use std::ffi::{CStr, CString, OsStr};
    use std::io;
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

    /// Closes the directory's descriptor when the listing is done.
    struct Fd(libc::c_int);

    impl Drop for Fd {
        fn drop(&mut self) {
            // SAFETY: the descriptor is ours, open, and closed only here.
            unsafe { libc::close(self.0) };
        }
    }

    pub fn list(dir: &Path, buf: &mut Vec<u8>) -> io::Result<Listing> {
        let c_dir = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        // The flags opendir(3) uses.
        // SAFETY: open reads the NUL-terminated path, which outlives the call.
        let fd = unsafe {
            libc::open(
                c_dir.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = Fd(fd);
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
                    fd.0,
                    (&raw mut attrs).cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    0,
                )
            };
            if count < 0 {
                // A file system without the call, or a directory it refuses
                // (one without search permission, say), gets the standard
                // listing, so each entry reports what `read_dir` reports.
                if first {
                    return list_std(dir);
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
                // std's read_dir skips these two, and the walk must never descend them.
                if entry.name == b"." || entry.name == b".." {
                    continue;
                }
                let path = dir.join(OsStr::from_bytes(entry.name));
                let stat = match entry.stat() {
                    Some(stat) => Ok(stat),
                    None => fstatat(fd.0, entry.name),
                };
                out.push(Ok(Listed {
                    git: entry.name == b".git",
                    path,
                    stat,
                }));
            }
        }
        Ok(out)
    }

    /// One entry of a batch: the attributes it returned.
    struct Entry<'a> {
        name: &'a [u8],
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
            name: &[],
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
        entry.name = CStr::from_bytes_until_nul(name).ok()?.to_bytes();
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

    /// `lstat` of one entry, relative to its directory, as std's
    /// `DirEntry::metadata` does on macOS.
    fn fstatat(dirfd: libc::c_int, name: &[u8]) -> io::Result<Stat> {
        let c_name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains a NUL byte"))?;
        // SAFETY: fstatat reads the NUL-terminated name, which outlives the
        // call, and writes into the zeroed struct we own.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatat(dirfd, c_name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Stat {
            dir: st.st_mode & libc::S_IFMT == libc::S_IFDIR,
            // As std's MetadataExt widens each field.
            dev: st.st_dev as u64,
            ino: st.st_ino,
            nlink: u64::from(st.st_nlink),
            blocks: st.st_blocks as u64,
            len: st.st_size as u64,
            mtime: st.st_mtime,
            mtime_nsec: st.st_mtime_nsec,
        })
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
