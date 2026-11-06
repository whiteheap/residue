//! Find the repositories at or under the paths the operator names.
//!
//! residue looks only where it is told. A path inside a repository selects
//! that repository; any other directory is searched a few levels deep for
//! checkouts, without following symbolic links or crossing filesystems.
//! Linked worktrees are then taken from Git's own list, wherever they live.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::git::{self, Entry};

const MAX_DEPTH: usize = 4;
const SKIP: &[&str] = &["node_modules", "target", "Library"];

pub struct Repo {
    /// Canonical Git common directory: the repository's identity.
    pub common_dir: PathBuf,
    /// The main worktree, or the bare repository directory.
    pub main: Entry,
    pub linked: Vec<Entry>,
}

/// A checkout whose `.git` file points at a Git directory that no longer
/// exists: its repository is gone or Git pruned it. Reported, never removed.
pub struct Orphan {
    pub path: PathBuf,
    pub gitdir: PathBuf,
}

#[derive(Default)]
pub struct Found {
    pub repos: Vec<Repo>,
    pub orphans: Vec<Orphan>,
    pub errors: Vec<String>,
}

pub fn find(roots: &[PathBuf]) -> Found {
    let mut found = Found::default();
    let mut dirs: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for root in roots {
        let root = match git::canonical(root) {
            Ok(r) => r,
            Err(e) => {
                found.errors.push(e);
                continue;
            }
        };
        if let Ok(common) = git::common_dir(&root) {
            dirs.entry(common).or_insert(root);
            continue;
        }
        let device = match fs::symlink_metadata(&root) {
            Ok(m) if m.is_dir() => m.dev(),
            Ok(_) => {
                found
                    .errors
                    .push(format!("{}: not a directory", root.display()));
                continue;
            }
            Err(e) => {
                found.errors.push(format!("{}: {e}", root.display()));
                continue;
            }
        };
        walk(&root, device, 0, &mut dirs, &mut found);
    }
    for (common_dir, dir) in dirs {
        match git::worktrees(&dir) {
            Ok(mut entries) => {
                let main = entries.remove(0);
                found.repos.push(Repo {
                    common_dir,
                    main,
                    linked: entries,
                });
            }
            Err(e) => found.errors.push(format!("{}: {e}", dir.display())),
        }
    }
    found
}

fn walk(
    dir: &Path,
    device: u64,
    depth: usize,
    dirs: &mut BTreeMap<PathBuf, PathBuf>,
    found: &mut Found,
) {
    match fs::symlink_metadata(dir.join(".git")) {
        Ok(meta) => {
            if meta.is_file() {
                if let Some(orphan) = orphan(dir) {
                    found.orphans.push(orphan);
                    return;
                }
            }
            match git::common_dir(dir) {
                Ok(common) => {
                    dirs.entry(common).or_insert_with(|| dir.to_path_buf());
                }
                Err(e) => found.errors.push(format!("{}: {e}", dir.display())),
            }
            return;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            found.errors.push(format!("{}: {e}", dir.display()));
            return;
        }
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) => {
            // Unreadable directories below the root are common (other
            // users' homes, system folders); note them without failing.
            if depth == 0 {
                found.errors.push(format!("{}: {e}", dir.display()));
            }
            return;
        }
    };
    let mut children: Vec<PathBuf> = Vec::new();
    for item in listing.flatten() {
        let name = item.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP.contains(&name.as_ref()) {
            continue;
        }
        match item.metadata() {
            Ok(meta) if meta.is_dir() && meta.dev() == device => children.push(item.path()),
            _ => {}
        }
    }
    children.sort();
    for child in children {
        walk(&child, device, depth + 1, dirs, found);
    }
}

fn orphan(dir: &Path) -> Option<Orphan> {
    let text = fs::read_to_string(dir.join(".git")).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = dir.join(target);
    match fs::symlink_metadata(&gitdir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(Orphan {
            path: dir.to_path_buf(),
            gitdir,
        }),
        _ => None,
    }
}
