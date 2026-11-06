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
    let top = fs::symlink_metadata(root)?;
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
    while let Some((dir, place)) = stack.pop() {
        let listing = match fs::read_dir(&dir) {
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
            let path = item.path();
            // DirEntry::metadata does not follow symbolic links on Unix.
            let meta = match item.metadata() {
                Ok(meta) => meta,
                Err(e) => {
                    usage.unreadable_entry(&path, &e);
                    judge_unreadable(&mut usage, buckets, place, &path, root, &e);
                    continue;
                }
            };
            if item.file_name() == ".git" && dir != root {
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
    meta: &Metadata,
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
