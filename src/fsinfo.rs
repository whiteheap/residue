//! Disk usage of one worktree, and free space on its filesystem.
//!
//! The walk never follows symbolic links and never crosses into another
//! filesystem. It records what would make a removal surprising: nested
//! repositories, mount points and entries it could not read.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

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
    pub unreadable: u64,
    pub first_error: Option<String>,
    pub nested_repos: Vec<PathBuf>,
    pub mount_points: Vec<PathBuf>,
}

struct Shared {
    seen: u64,
    links: u64,
    blocks: u64,
}

pub fn measure(root: &Path) -> io::Result<Usage> {
    let top = fs::symlink_metadata(root)?;
    let device = top.dev();
    let mut usage = Usage::default();
    let mut shared: HashMap<(u64, u64), Shared> = HashMap::new();
    account(&mut usage, &mut shared, &top);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let listing = match fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(e) => {
                usage.unreadable_entry(&dir, &e);
                continue;
            }
        };
        for item in listing {
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    usage.unreadable_entry(&dir, &e);
                    continue;
                }
            };
            let path = item.path();
            // DirEntry::metadata does not follow symbolic links on Unix.
            let meta = match item.metadata() {
                Ok(meta) => meta,
                Err(e) => {
                    usage.unreadable_entry(&path, &e);
                    continue;
                }
            };
            if item.file_name() == ".git" && dir != root {
                usage.nested_repos.push(dir.clone());
            }
            if meta.is_dir() && meta.dev() != device {
                usage.mount_points.push(path);
                continue;
            }
            account(&mut usage, &mut shared, &meta);
            if meta.is_dir() {
                stack.push(path);
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

fn account(usage: &mut Usage, shared: &mut HashMap<(u64, u64), Shared>, meta: &Metadata) {
    usage.entries += 1;
    let mtime = meta.mtime();
    usage.newest_mtime = Some(usage.newest_mtime.map_or(mtime, |m| m.max(mtime)));
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
        }
        return;
    }
    usage.apparent += meta.len();
    usage.allocated += blocks;
    usage.reclaimable += blocks;
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
