//! A run's hold on a trash entry or a removal record: an exclusive
//! flock(2) on the entry's `info` file.
//!
//! The run that deletes a worktree holds the entry that names it, and so
//! does the `git worktree remove` it starts, which inherits the open file
//! ([`pass_to`]). The kernel drops the hold only when every process with
//! that open file has exited, so a residue killed alone, by `kill PID`, a
//! Force Quit or watch's deadline, leaves its Git child holding it while it
//! deletes. A restore takes the hold before it moves anything, and a purge
//! before it checks: neither acts while another run, or a Git it started,
//! may still delete or move the worktree. PIDs are reused; a hold is not.
//!
//! `info` is opened for writing as well as reading when residue may write
//! it, because NFS on Linux emulates flock(2) with fcntl(2) locks, and an
//! exclusive one needs a file open for writing (`take`). Nothing is written
//! to it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// A hold this process has. Dropping it closes the file, which lets go of
/// the hold once no child that inherited it still runs.
pub struct Hold {
    file: File,
}

/// What [`take`] found.
pub enum Taken {
    Held(Hold),
    /// Another run, or a Git child it started, holds it.
    Busy,
    /// The entry has no `info`: another run discarded it, or it never had
    /// one.
    Gone,
}

/// Take the hold on the entry whose `info` is at `info`, without waiting.
/// An `info` residue may not write (mode 0444) is opened for reading:
/// flock(2) holds a file open either way on the local filesystems of macOS,
/// Linux, the BSDs and illumos, whose `F_FLOCK` checks no open mode. Where
/// it needs one open for writing (NFS on Linux, which emulates it with
/// fcntl(2) locks), that lock fails, and the error names the open that was
/// refused. Every error is an error, never a free hold.
pub fn take(info: &Path) -> Result<Taken, String> {
    let (file, refused) = match OpenOptions::new().read(true).write(true).open(info) {
        Ok(f) => (f, None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Taken::Gone),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            match OpenOptions::new().read(true).open(info) {
                Ok(f) => (f, Some(e)),
                Err(again) if again.kind() == io::ErrorKind::NotFound => return Ok(Taken::Gone),
                Err(again) => return Err(format!("cannot open {}: {again}", info.display())),
            }
        }
        Err(e) => return Err(format!("cannot open {}: {e}", info.display())),
    };
    let locked = lock(&file).map_err(|e| match &refused {
        Some(w) => format!(
            "cannot lock {} open for reading ({e}), and cannot open it for writing ({w})",
            info.display()
        ),
        None => format!("cannot lock {}: {e}", info.display()),
    })?;
    if !locked {
        return Ok(Taken::Busy);
    }
    // A run that discarded the entry between the open and the lock left
    // this file unlinked, and a new entry under the same name has another
    // file: either way the hold is not on the entry's `info`.
    let held = file
        .metadata()
        .map_err(|e| format!("cannot read {}: {e}", info.display()))?;
    match fs::metadata(info) {
        Ok(now) if now.dev() == held.dev() && now.ino() == held.ino() => {
            Ok(Taken::Held(Hold { file }))
        }
        Ok(_) => Ok(Taken::Gone),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Taken::Gone),
        Err(e) => Err(format!("cannot read {}: {e}", info.display())),
    }
}

/// Whether a run holds the entry whose `info` is at `info` now. It takes
/// the hold and lets go of it at once, so for that instant a restore of
/// the same entry would find it held.
pub fn held(info: &Path) -> Result<bool, String> {
    match take(info)? {
        Taken::Busy => Ok(true),
        Taken::Held(_) | Taken::Gone => Ok(false),
    }
}

/// Write a new file at `path` with `bytes`, synced, and hold it before
/// anyone can open it: a record's `info.new`, held before it is renamed to
/// `info`, so no restore finds the new record free.
pub fn write_new(path: &Path, bytes: &[u8]) -> io::Result<Hold> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    // No other process has the file yet, so the lock cannot be busy.
    if !lock(&file)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "another process holds the file residue just made",
        ));
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(Hold { file })
}

/// Let the child `cmd` starts inherit `holds`, so they stay held while it
/// runs, after this process has exited too. Each is open close-on-exec in
/// this process, and only the child clears that flag.
pub fn pass_to(cmd: &mut Command, holds: &[&Hold]) {
    let fds: Vec<RawFd> = holds.iter().map(|h| h.file.as_raw_fd()).collect();
    if fds.is_empty() {
        return;
    }
    // SAFETY: the closure runs in the child between fork and exec. It reads
    // the vector the parent filled before the fork, allocates nothing, and
    // calls only fcntl(2), which is async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            for &fd in &fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// An exclusive, non-blocking flock(2): `false` when another open file
/// holds it.
fn lock(file: &File) -> io::Result<bool> {
    loop {
        // SAFETY: flock on a file descriptor this process owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => return Ok(false),
            _ => return Err(e),
        }
    }
}
