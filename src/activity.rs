//! Is any process using this directory?
//!
//! One snapshot of process working directories and open files (and, where
//! the backend reads them, mapped files) is taken per command and matched
//! against each worktree. The answer has three states,
//! never two: a process was seen using the directory (`in_use`), none of
//! the processes the probe could inspect was (`not_observed`), or the probe
//! failed or reads too little to show that none was (`unknown`). A probe
//! that does not read open files (`Seen::limits`) can show a use, never its
//! absence, so where it finds none the answer is `unknown`, which keeps the
//! worktree. A file a process holds whose path the probe could not read
//! (`Seen::unnamed`) is looked for in the directory by its inode, and where
//! residue cannot look the answer is `unknown` too. `not_observed` is not
//! proof of inactivity: processes owned by other users, in containers or on
//! other machines are outside what a same-user probe can see, and a process
//! can open the directory a moment later.

use std::borrow::Cow;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::fsinfo::Reach;

#[derive(Clone)]
pub struct Unavailable {
    /// Stable reason code: `probe_missing`, `timeout`, `probe_failed`,
    /// `parse_error`, `unsupported`, `partial_probe` (the probe found no
    /// use but does not read open files, so it cannot show the directory
    /// is unused), `unmatchable_path` (the probe prints the paths it read
    /// escaped, and residue cannot be sure how it prints this one),
    /// `long_path` (a path in the directory is as long as the probe names
    /// whole, `PATH_LIMIT`, or longer), or `unnamed_file` (a process holds a
    /// file whose path the probe could not read, and residue could not look
    /// for it in the directory).
    pub reason: &'static str,
    pub detail: String,
}

pub struct Use {
    pub pid: u32,
    pub command: String,
    pub path: Vec<u8>,
}

/// The length in bytes of the longest path the backend names whole. A
/// longer one is cut or left out, so a directory that holds one is
/// `unknown` (`long_path`), never `not_observed`.
///
/// - macOS: lsof prints a working directory or open file of 1,024 bytes or
///   more cut to its trailing components (`MAXPATHLEN`, measured on macOS
///   26.3.1). The limit is 20 bytes lower, the length of
///   `/System/Volumes/Data`, so it holds whichever spelling of a firmlinked
///   path the kernel gives.
/// - Linux: a readlink of `/proc/PID/cwd` or `fd/N` fails for a path of
///   `PATH_MAX`, 4,096 bytes, or more (`d_path` into a page,
///   `do_proc_readlink` in `fs/proc/base.c`).
/// - FreeBSD, NetBSD, OpenBSD, DragonFly and illumos: the kernel names a
///   path into a buffer of `MAXPATHLEN`, 1,024 bytes, with its NUL.
#[cfg(target_os = "macos")]
pub const PATH_LIMIT: usize = 1024 - "/System/Volumes/Data".len();
#[cfg(target_os = "linux")]
pub const PATH_LIMIT: usize = 4096;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub const PATH_LIMIT: usize = 1024;

pub struct Seen {
    pub uses: Vec<Use>,
    pub inspected: usize,
    pub total: Option<usize>,
    /// What the backend reads of each process, when that is less than its
    /// working directory and open files. When set, a directory with a use
    /// is `in_use` and one without is `unknown` (`partial_probe`), never
    /// `not_observed`: a process can hold a file open inside it while its
    /// working directory is elsewhere.
    pub limits: Option<&'static str>,
    /// How the paths in `uses` are written. `None`: byte for byte as the
    /// kernel names them. `Some(spell)`: escaped, as the backend's tool
    /// prints them; `spell` writes a directory the same way, so a use is
    /// matched in that form, or gives `None` for a path whose form residue
    /// cannot be sure of, whose activity is then `unknown`
    /// (`unmatchable_path`), never `not_observed`.
    pub spelling: Option<Spell>,
    /// Files the backend saw a process hold but could not name. A
    /// directory holding one of them, found by its device and inode, is in
    /// use by that process; where residue cannot look, it is `unknown`
    /// (`unnamed_file`), never `not_observed`. The illumos and FreeBSD
    /// backends fill it.
    pub unnamed: Vec<Unnamed>,
}

/// A file a process holds whose path the backend could not read: on
/// illumos one below a folder renamed after the kernel cached its path, on
/// FreeBSD one the kernel's name cache does not hold.
pub struct Unnamed {
    pub pid: u32,
    pub command: String,
    /// The file's `st_dev` and `st_ino`; `None` when residue could not
    /// read them, so it may be anywhere.
    pub file: Option<(u64, u64)>,
}

/// Writes a path the way a backend's tool prints one (`lsof::spell`).
pub type Spell = fn(&[u8]) -> Option<Cow<'_, [u8]>>;

pub struct Snapshot {
    pub backend: &'static str,
    pub state: Result<Seen, Unavailable>,
}

#[derive(Clone)]
pub enum Evidence {
    InUse(Vec<(u32, String)>),
    NotObserved,
    Unknown(Unavailable),
}

impl Evidence {
    pub fn code(&self) -> &'static str {
        match self {
            Evidence::InUse(_) => "in_use",
            Evidence::NotObserved => "not_observed",
            Evidence::Unknown(_) => "unknown",
        }
    }
}

impl Snapshot {
    pub fn take() -> Snapshot {
        platform::take()
    }

    /// The `(device, inode)` of each file the snapshot holds unnamed, sorted
    /// and once each, for the walk to look for (`fsinfo::measure`).
    pub fn wanted(&self) -> Vec<(u64, u64)> {
        let Ok(seen) = &self.state else {
            return Vec::new();
        };
        let mut wanted: Vec<(u64, u64)> = seen.unnamed.iter().filter_map(|u| u.file).collect();
        wanted.sort_unstable();
        wanted.dedup();
        wanted
    }

    /// Whether a process uses `dir`. `reach` is what the walk that sized
    /// `dir` reached, when it ran: the unnamed files it found, and its
    /// longest path. Without it, a file the probe could not name is looked
    /// for by a walk of its own (`inodes_within`), and only `dir`'s own path
    /// is held against `PATH_LIMIT`.
    pub fn evidence(&self, dir: &Path, reach: Option<&Reach>) -> Evidence {
        let seen = match &self.state {
            Ok(seen) => seen,
            Err(unavailable) => return Evidence::Unknown(unavailable.clone()),
        };
        let raw = dir.as_os_str().as_bytes();
        // The directory as the backend writes the paths it read: a use is
        // inside it in that form exactly when it is inside it byte for byte.
        let spelled = match seen.spelling {
            None => Cow::Borrowed(raw),
            Some(spell) => match spell(raw) {
                Some(spelled) => spelled,
                None => {
                    return Evidence::Unknown(Unavailable {
                        reason: "unmatchable_path",
                        detail: format!(
                            "{} prints a byte of this path in a form residue has not confirmed, so it cannot match a process to it",
                            self.backend
                        ),
                    });
                }
            },
        };
        let me = std::process::id();
        let mut hits: Vec<(u32, String)> = Vec::new();
        for item in &seen.uses {
            if item.pid != me
                && within(&item.path, &spelled)
                && !hits.iter().any(|(p, _)| *p == item.pid)
            {
                hits.push((item.pid, item.command.clone()));
            }
        }
        if hits.is_empty() {
            match self.unnamed_within(seen, dir, reach, me) {
                Ok(holders) => hits = holders,
                Err(unavailable) => return Evidence::Unknown(unavailable),
            }
        }
        if !hits.is_empty() {
            hits.sort();
            return Evidence::InUse(hits);
        }
        if let Some(limits) = seen.limits {
            // Finding no use is evidence of no use only when the probe
            // reads open files too; this one does not, so it cannot show
            // that the directory is unused.
            return Evidence::Unknown(Unavailable {
                reason: "partial_probe",
                detail: format!(
                    "{} reads {limits}, so it cannot show that no process is using it",
                    self.backend
                ),
            });
        }
        let longest = reach.map_or(0, |r| r.longest).max(raw.len());
        if longest >= PATH_LIMIT {
            return Evidence::Unknown(Unavailable {
                reason: "long_path",
                detail: format!(
                    "{} names a path whole only when it is shorter than {PATH_LIMIT} bytes, and a path here is {longest}, so it cannot show that no process is using it",
                    self.backend
                ),
            });
        }
        Evidence::NotObserved
    }

    /// The other processes holding a file inside `dir` that the backend
    /// could not name, found by the file's device and inode in `dir`: among
    /// those the walk that sized `dir` listed (`reach`), or else by a walk
    /// of its own (`inodes_within`). Or why residue cannot tell: it could
    /// not read such a file's device and inode, `dir`'s, or all of `dir`.
    /// Nothing is inside a directory that does not exist. `dir` is read
    /// only when the snapshot holds such a file, and walked here only when
    /// one is on its filesystem and no `reach` is given.
    fn unnamed_within(
        &self,
        seen: &Seen,
        dir: &Path,
        reach: Option<&Reach>,
        me: u32,
    ) -> Result<Vec<(u32, String)>, Unavailable> {
        let others: Vec<&Unnamed> = seen.unnamed.iter().filter(|u| u.pid != me).collect();
        let Some(first) = others.first() else {
            return Ok(Vec::new());
        };
        let unknown = |file: &Unnamed, why: String| {
            let who = if file.command.is_empty() {
                format!("process {}", file.pid)
            } else {
                format!("process {} ({})", file.pid, file.command)
            };
            Unavailable {
                reason: "unnamed_file",
                detail: format!(
                    "{} cannot give the path of a file {who} holds, and residue {why}, so it cannot show that no process is using it",
                    self.backend
                ),
            }
        };
        let root = match std::fs::metadata(dir) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(unknown(first, format!("cannot read this directory ({e})"))),
        };
        if let Some(file) = others.iter().find(|u| u.file.is_none()) {
            return Err(unknown(file, "cannot read where that file is".into()));
        }
        let wanted: Vec<&Unnamed> = others
            .into_iter()
            .filter(|u| u.file.is_some_and(|(device, _)| device == root.dev()))
            .collect();
        let mut inodes: Vec<u64> = wanted
            .iter()
            .filter_map(|u| u.file)
            .map(|(_, i)| i)
            .collect();
        inodes.sort_unstable();
        inodes.dedup();
        let Some(first) = wanted.first() else {
            return Ok(Vec::new());
        };
        let walked = match reach {
            Some(reach) => match &reach.unread {
                Some(e) => Err(e.clone()),
                None => Ok(reach
                    .found
                    .iter()
                    .filter(|(device, _)| *device == root.dev())
                    .map(|(_, inode)| *inode)
                    .collect()),
            },
            None => inodes_within(dir, &root, &inodes),
        };
        let found = walked.map_err(|e| {
            unknown(
                first,
                format!("could not read all of this directory to look for it ({e})"),
            )
        })?;
        let mut holders: Vec<(u32, String)> = Vec::new();
        for file in wanted {
            let inside = file.file.is_some_and(|(_, inode)| found.contains(&inode));
            if inside && !holders.iter().any(|(pid, _)| *pid == file.pid) {
                holders.push((file.pid, file.command.clone()));
            }
        }
        Ok(holders)
    }

    /// What the snapshot covers, in words: the header of `scan` and
    /// `explain`, and every `not_observed` answer.
    pub fn scope(&self) -> String {
        match &self.state {
            Ok(seen) => {
                let mut text = match seen.total {
                    Some(total) => format!(
                        "{} inspected {} of {} running processes",
                        self.backend, seen.inspected, total
                    ),
                    None => format!("{} inspected {} processes", self.backend, seen.inspected),
                };
                if let Some(limits) = seen.limits {
                    text.push_str(", ");
                    text.push_str(limits);
                }
                text
            }
            Err(u) => format!("{} unavailable ({}): {}", self.backend, u.reason, u.detail),
        }
    }
}

/// True when `path` is `dir` or lies below it, by whole path components:
/// `/x/cache2` is not inside `/x/cache`.
fn within(path: &[u8], dir: &[u8]) -> bool {
    path.starts_with(dir) && (path.len() == dir.len() || path[dir.len()] == b'/')
}

/// Which of `inodes`, all on `root`'s filesystem, `dir` or an entry below
/// it is: a walk that follows no symbolic link, enters each directory
/// once, and does not enter another filesystem, where none of them can
/// be. An entry removed during the walk is skipped; any other error ends
/// it, since what it did not read may hold one, and names where. It stops
/// once it has found them all.
fn inodes_within(dir: &Path, root: &std::fs::Metadata, inodes: &[u64]) -> Result<Vec<u64>, String> {
    use std::io::ErrorKind::NotFound;
    let mut found: Vec<u64> = Vec::new();
    if inodes.contains(&root.ino()) {
        found.push(root.ino());
    }
    let mut entered = std::collections::HashSet::from([root.ino()]);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        if found.len() == inodes.len() {
            break;
        }
        let failed = |e: std::io::Error| format!("{}: {e}", next.display());
        let entries = match std::fs::read_dir(&next) {
            Ok(entries) => entries,
            Err(e) if e.kind() == NotFound => continue,
            Err(e) => return Err(failed(e)),
        };
        for entry in entries {
            let (meta, path) = match entry.and_then(|entry| Ok((entry.metadata()?, entry.path()))) {
                Ok(read) => read,
                Err(e) if e.kind() == NotFound => continue,
                Err(e) => return Err(failed(e)),
            };
            if meta.dev() != root.dev() {
                continue;
            }
            if inodes.contains(&meta.ino()) && !found.contains(&meta.ino()) {
                found.push(meta.ino());
            }
            if meta.is_dir() && entered.insert(meta.ino()) {
                stack.push(path);
            }
        }
    }
    Ok(found)
}

fn unavailable(backend: &'static str, reason: &'static str, detail: String) -> Snapshot {
    Snapshot {
        backend,
        state: Err(Unavailable { reason, detail }),
    }
}

/// How macOS's lsof prints a name under `LC_ALL=C`, byte by byte:
/// `safestrprt` and `safepup` in its `misc.c` (Apple's `lsof-76`, lsof
/// 4.91), which print every name, working directories and open files
/// alike. In the C locale `MB_CUR_MAX` is 1 and `isprint` holds for 0x20
/// to 0x7e only, so each byte is printed on its own:
///
/// ```text
/// 0x20-0x7e but `\`   itself          `\`        \\
/// 0x08 0x09 0x0a      \b \t \n        0x0c 0x0d  \f \r
/// other 0x01-0x1f     ^ and the byte plus 0x40: 0x01 is ^A, 0x1b is ^[
/// 0x7f-0xfe           \x and two lowercase hex digits: é (c3 a9) is \xc3\xa9
/// ```
///
/// Each class was also checked against `LC_ALL=C lsof -F n` on macOS 26.3.1
/// for the working directories of processes in folders named with every
/// byte the disk accepts. A `/` is printed as itself, so a path inside a
/// directory is printed inside that directory's form. The form is not
/// unique: a literal `^A` and the byte 0x01 both print as `^A`, so a match
/// can name a process in a sibling with such a name, never miss one.
#[cfg(any(target_os = "macos", test))]
mod lsof {
    use std::borrow::Cow;

    /// `dir` as lsof prints it, or `None` when it holds a byte whose form
    /// was not confirmed: 0xff, which `safepup` prints as `\xff` or `^?`
    /// depending on whether the compiler's `char` is signed, and which no
    /// APFS or HFS+ name can hold, or a NUL, which no path can.
    pub fn spell(dir: &[u8]) -> Option<Cow<'_, [u8]>> {
        if dir
            .iter()
            .all(|&b| (0x20..=0x7e).contains(&b) && b != b'\\')
        {
            return Some(Cow::Borrowed(dir));
        }
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = Vec::with_capacity(dir.len() * 2);
        for &b in dir {
            match b {
                b'\\' => out.extend_from_slice(b"\\\\"),
                0x20..=0x7e => out.push(b),
                0x08 => out.extend_from_slice(b"\\b"),
                b'\t' => out.extend_from_slice(b"\\t"),
                b'\n' => out.extend_from_slice(b"\\n"),
                0x0c => out.extend_from_slice(b"\\f"),
                b'\r' => out.extend_from_slice(b"\\r"),
                0x01..=0x1f => out.extend_from_slice(&[b'^', b + 0x40]),
                0x7f..=0xfe => out.extend_from_slice(&[
                    b'\\',
                    b'x',
                    HEX[usize::from(b >> 4)],
                    HEX[usize::from(b & 0x0f)],
                ]),
                0x00 | 0xff => return None,
            }
        }
        Some(Cow::Owned(out))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Seen, Snapshot, Use, unavailable};
    use crate::run::{self, RunError};
    use std::process::Command;
    use std::time::Duration;

    const BACKEND: &str = "lsof";

    pub fn take() -> Snapshot {
        let mut cmd = Command::new("/usr/sbin/lsof");
        // -n -P: no DNS or port lookups. -F pcn: machine-readable fields for
        // pid, command and name; lsof adds the fd field on its own. lsof
        // lists itself too, so it runs in `/`: in residue's directory, its
        // own row would mark the worktree residue runs in as in use by lsof.
        // LC_ALL=C: it prints each byte of a name by itself, in the form
        // `lsof::spell` writes a directory in.
        cmd.args(["-n", "-P", "-F", "pcn"])
            .env("LC_ALL", "C")
            .current_dir("/");
        let out = match run::run(&mut cmd, Duration::from_secs(30), 512 << 20) {
            Ok(out) => out,
            Err(RunError::NotFound) => {
                return unavailable(BACKEND, "probe_missing", "/usr/sbin/lsof not found".into());
            }
            Err(RunError::Timeout(d)) => {
                return unavailable(
                    BACKEND,
                    "timeout",
                    format!("lsof ran longer than {}s", d.as_secs()),
                );
            }
            Err(e) => return unavailable(BACKEND, "probe_failed", format!("lsof: {e}")),
        };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first = stderr.lines().next().unwrap_or("no error output");
            return unavailable(
                BACKEND,
                "probe_failed",
                format!("lsof exited with {}: {first}", out.status),
            );
        }
        let mut uses = Vec::new();
        let mut pid: Option<u32> = None;
        let mut command = String::new();
        let mut inspected = 0;
        // Whether lsof could read everything of the current process.
        let mut whole = false;
        for line in out.stdout.split(|b| *b == b'\n') {
            let Some((&field, value)) = line.split_first() else {
                continue;
            };
            match field {
                b'p' => {
                    let parsed = std::str::from_utf8(value)
                        .ok()
                        .and_then(|v| v.parse::<u32>().ok());
                    let Some(p) = parsed else {
                        return unavailable(
                            BACKEND,
                            "parse_error",
                            "lsof printed a process id residue cannot read".into(),
                        );
                    };
                    if pid.is_some() && whole {
                        inspected += 1;
                    }
                    pid = Some(p);
                    command.clear();
                    whole = true;
                }
                b'c' => command = String::from_utf8_lossy(value).into_owned(),
                // Kept as lsof prints it, escaped (`lsof::spell`): reading
                // it back is ambiguous, writing a directory that way is not.
                b'n' if value.first() == Some(&b'/') => {
                    let Some(p) = pid else {
                        return unavailable(
                            BACKEND,
                            "parse_error",
                            "lsof printed a file before its process".into(),
                        );
                    };
                    uses.push(Use {
                        pid: p,
                        command: command.clone(),
                        path: value.to_vec(),
                    });
                }
                b'n' if unread(value) => whole = false,
                _ => {}
            }
        }
        if pid.is_some() && whole {
            inspected += 1;
        }
        Snapshot {
            backend: BACKEND,
            state: Ok(Seen {
                uses,
                inspected,
                total: count_processes(),
                limits: None,
                spelling: Some(super::lsof::spell),
                unnamed: Vec::new(),
            }),
        }
    }

    /// True for a name lsof prints in place of something it could not read,
    /// which could be a path inside a worktree. Apple's lsof (`lsof-76`,
    /// `dialects/darwin/libproc/dproc.c`, `dfile.c`, `dsock.c`) prints them
    /// in the name column: `cwd|rtd info error: `, `FD info error: `,
    /// `FILEPORT info error: `, `region info error: ` and `thread info
    /// error: ` followed by the error, `no more information` for a file it
    /// may not name, and `PREFIX: ERROR` (`err2nm`) for a descriptor it could
    /// not read. A descriptor closed between listing and reading
    /// (`: FD unavailable`) is gone, so it is not a gap.
    fn unread(name: &[u8]) -> bool {
        const ERR2NM: [&[u8]; 6] = [
            b"vnode: ",
            b"socket: ",
            b"pipe: ",
            b"kqueue: ",
            b"semaphore: ",
            b"POSIX shared memory: ",
        ];
        if name.windows(13).any(|w| w == b" info error: ") || name == b"no more information" {
            return true;
        }
        ERR2NM.iter().any(|prefix| name.starts_with(prefix)) && !name.ends_with(b": FD unavailable")
    }

    /// The number of processes, to state how much lsof could see: every pid
    /// the kernel lists, zombies and `kernel_task` (pid 0) included. That is
    /// the number `ps -A` printed before, which lists itself and not pid 0.
    /// `proc_listallpids` writes at most the number of processes plus 20
    /// (`proc_listpids` in xnu's `bsd/kern/proc_info.c`) and returns how
    /// many pids it wrote (`libsyscall/wrappers/libproc/libproc.c`); a full
    /// buffer may have been cut short, so it is asked again, larger.
    fn count_processes() -> Option<usize> {
        // SAFETY: a null buffer of size 0 asks only for the number of pids
        // to make room for; nothing is written.
        let mut room = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        for _ in 0..4 {
            let len = usize::try_from(room).ok()?.checked_add(64)?;
            let mut pids: Vec<libc::c_int> = vec![0; len];
            let bytes = libc::c_int::try_from(len * std::mem::size_of::<libc::c_int>()).ok()?;
            // SAFETY: `pids` is writable for `bytes` bytes, and the kernel
            // writes at most `bytes`.
            let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            let n = usize::try_from(n).ok()?;
            if n < len {
                return Some(n);
            }
            room = libc::c_int::try_from(len * 2).ok()?;
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::{count_processes, unread};

        #[test]
        fn a_name_lsof_could_not_read_is_a_gap_and_a_closed_descriptor_is_not() {
            for gap in [
                &b"cwd|rtd info error: Operation not permitted"[..],
                b"FD info error: Operation not permitted",
                b"region info error: Bad address",
                b"no more information",
                b"vnode: process unavailable",
                b"socket: Operation not permitted",
            ] {
                assert!(unread(gap), "{}", String::from_utf8_lossy(gap));
            }
            for name in [
                &b"vnode: FD unavailable"[..],
                b"->0x5a1c2f0e3d4b6a1f",
                b"*:5432",
                b"no address",
                b"count=0, state=0x8",
                b"(revoked)",
            ] {
                assert!(!unread(name), "{}", String::from_utf8_lossy(name));
            }
        }

        #[test]
        fn the_process_count_includes_this_process() {
            assert!(count_processes().is_some_and(|n| n >= 1));
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{Seen, Snapshot, Use, maps, unavailable};
    use std::fs;
    use std::os::unix::ffi::OsStringExt;

    const BACKEND: &str = "procfs";

    pub fn take() -> Snapshot {
        let listing = match fs::read_dir("/proc") {
            Ok(listing) => listing,
            Err(e) => return unavailable(BACKEND, "probe_missing", format!("/proc: {e}")),
        };
        let mut uses = Vec::new();
        let mut total = 0;
        let mut inspected = 0;
        let mut paths: Vec<Vec<u8>> = Vec::new();
        for item in listing {
            let item = match item {
                Ok(item) => item,
                Err(e) => return unavailable(BACKEND, "probe_failed", format!("/proc: {e}")),
            };
            let Some(pid) = item
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            total += 1;
            let base = item.path();
            // comm is display-only; a process that exits mid-scan has none.
            let command = fs::read_to_string(base.join("comm"))
                .map(|c| c.trim_end().to_string())
                .unwrap_or_default();
            paths.clear();
            // A kernel thread has no executable, and a process that exits
            // mid-scan loses its working directory: neither link is required.
            for link in ["cwd", "exe"] {
                if let Ok(target) = fs::read_link(base.join(link)) {
                    paths.push(target.into_os_string().into_vec());
                }
            }
            let fds_read = match fs::read_dir(base.join("fd")) {
                Ok(fds) => {
                    for fd in fds.flatten() {
                        // An fd closed between listing and readlink is gone; skip it.
                        if let Ok(target) = fs::read_link(fd.path()) {
                            paths.push(target.into_os_string().into_vec());
                        }
                    }
                    true
                }
                Err(_) => false,
            };
            // Mapped files: a shared library or native module loaded from a
            // worktree, or a file a process maps and then closes (Git's
            // index), holds no descriptor and no working directory there.
            let maps_read = match maps::read(&base.join("maps"), &mut paths) {
                Ok(()) => true,
                Err(maps::Error::Unreadable) => false,
                Err(maps::Error::TooLarge) => {
                    return unavailable(
                        BACKEND,
                        "probe_failed",
                        format!(
                            "/proc/{pid}/maps is larger than {} MiB, so residue cannot read every file process {pid} maps",
                            maps::CAP >> 20
                        ),
                    );
                }
            };
            paths.retain(|p| p.first() == Some(&b'/'));
            paths.sort_unstable();
            paths.dedup();
            uses.extend(paths.drain(..).map(|path| Use {
                pid,
                command: command.clone(),
                path,
            }));
            // Inspected means residue read the process's open files and its
            // mapped files. One it cannot read (another user's, or one that
            // exited) is not counted; what was read of it still counts as use.
            if fds_read && maps_read {
                inspected += 1;
            }
        }
        Snapshot {
            backend: BACKEND,
            state: Ok(Seen {
                uses,
                inspected,
                total: Some(total),
                limits: None,
                spelling: None,
                unnamed: Vec::new(),
            }),
        }
    }
}

/// The path column of `/proc/PID/maps`: the files a process has mapped into
/// memory. Its format is in the kernel's `Documentation/filesystems/proc.rst`
/// and proc_pid_maps(5), and is printed by `show_map_vma` in
/// `fs/proc/task_mmu.c`:
///
/// ```text
/// address           perms offset  dev   inode      pathname
/// 7f2c3e400000-7f2c3e428000 r--p 00000000 08:01 1835234    /usr/lib/libc.so.6
/// ```
///
/// Five fields, each followed by one space, then spaces to a fixed column,
/// then the name to the end of the line. A file's name is the path `d_path`
/// gives, which begins with `/`. Other names begin with `[` (`[heap]`,
/// `[stack]`, `[vdso]`, `[anon:NAME]`) or are empty (an anonymous mapping),
/// and name no file. The kernel escapes only a newline in a path, as `\012`
/// (`seq_path(m, path, "\n")`), and appends ` (deleted)` to a file that was
/// unlinked. Both are ambiguous, as proc_pid_maps(5) says: a path can contain
/// the four bytes `\012` or end in ` (deleted)` itself.
#[cfg(any(target_os = "linux", test))]
mod maps {
    use std::io::{BufRead, BufReader, Read};
    use std::path::Path;

    /// The most residue reads of one process's maps. A line is about 75
    /// bytes plus its path, and Linux allows 65,530 mappings per process by
    /// default (`vm.max_map_count`), so a real process stays well under it.
    /// The file is read through a 64 KiB buffer, so this bounds time, not
    /// memory.
    pub const CAP: u64 = 64 << 20;

    pub enum Error {
        /// Not opened, or a read failed: another user's process, or one that
        /// exited.
        Unreadable,
        /// Longer than `CAP`: what is past it was not read.
        TooLarge,
    }

    /// Adds the path of every file `file` maps to `paths`.
    pub fn read(file: &Path, paths: &mut Vec<Vec<u8>>) -> Result<(), Error> {
        let file = std::fs::File::open(file).map_err(|_| Error::Unreadable)?;
        let mut reader = BufReader::with_capacity(64 << 10, file.take(CAP + 1));
        let mut line = Vec::new();
        let mut read: u64 = 0;
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => return Ok(()),
                Ok(n) => read += n as u64,
                Err(_) => return Err(Error::Unreadable),
            }
            if read > CAP {
                return Err(Error::TooLarge);
            }
            let Some(path) = path(&line) else {
                continue;
            };
            // A library maps four or five regions in a row.
            if paths.last().is_some_and(|last| last.as_slice() == path) {
                continue;
            }
            if let Some(unescaped) = unescape(path) {
                paths.push(unescaped);
            }
            paths.push(path.to_vec());
        }
    }

    /// The path a maps line names, or `None` for a mapping of no file.
    /// ` (deleted)` is kept: it follows the path, so a deleted file inside a
    /// directory still lies inside it, as an open one does in `fd`.
    pub fn path(line: &[u8]) -> Option<&[u8]> {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let mut fields = line.splitn(6, |b| *b == b' ');
        for _ in 0..5 {
            fields.next()?;
        }
        let name = fields.next()?;
        let start = name.iter().position(|b| *b != b' ')?;
        let name = &name[start..];
        (name.first() == Some(&b'/')).then_some(name)
    }

    /// `path` with each `\012` turned back into the newline the kernel
    /// escaped, when it has one. The caller keeps both readings, since the
    /// path may have held those four bytes: a use read either way is a use.
    pub fn unescape(path: &[u8]) -> Option<Vec<u8>> {
        const ESCAPED: &[u8] = b"\\012";
        path.windows(ESCAPED.len()).position(|w| w == ESCAPED)?;
        let mut out = Vec::with_capacity(path.len());
        let mut rest = path;
        while !rest.is_empty() {
            if rest.starts_with(ESCAPED) {
                out.push(b'\n');
                rest = &rest[ESCAPED.len()..];
            } else {
                out.push(rest[0]);
                rest = &rest[1..];
            }
        }
        Some(out)
    }
}

/// FreeBSD: the process list from the base system's `ps`, then each
/// process's working directory, executable and open files, and the files
/// it maps, from the kernel, by the sysctls procstat(1) itself reads
/// (`kern.proc.filedesc.<pid>`, kinfo_getfile(3), and
/// `kern.proc.vmmap.<pid>`, kinfo_getvmmap(3)), which name each path byte
/// for byte. procstat is not run: it prints a name through libxo, which in
/// the C locale rewrites a character that is not ASCII and leaves out a
/// name that is not UTF-8. A packaged lsof is not used either: on FreeBSD
/// 14 it listed processes without resolving their working directories.
///
/// The kernel names a file from its name cache. A file it cannot name
/// comes back with no path but with its filesystem and inode, and is kept
/// in `Seen::unnamed` for the walk of each worktree to place.
///
/// Both sysctls answer only for a process the user may debug (`p_candebug`
/// in `sys/kern/kern_prot.c`). One that runs as another user is outside
/// what a same-user probe reads, as on every platform. One that runs as
/// this user and is refused anyway, with `security.bsd.unprivileged_proc_debug`
/// at 0, or a process that denies tracing, makes the probe fail: residue
/// cannot see the files of a process that could be using a worktree.
#[cfg(target_os = "freebsd")]
mod platform {
    use super::{Seen, Snapshot, Unnamed, Use, kinfo, unavailable, vmentry};
    use crate::run::{self, RunError};
    use std::mem::offset_of;
    use std::process::Command;
    use std::time::Duration;

    const BACKEND: &str = "sysctl";

    // `kinfo` and `vmentry` read records by these offsets and codes; the
    // build fails if they ever differ from `<sys/user.h>` as the libc crate
    // declares it.
    const _: () = {
        assert!(offset_of!(libc::kinfo_file, kf_structsize) == 0);
        assert!(offset_of!(libc::kinfo_file, kf_type) == kinfo::TYPE);
        assert!(offset_of!(libc::kinfo_file, kf_fd) == kinfo::FD);
        // The libc crate leaves the union `kf_un` opaque. It starts after
        // `kf_offset` and ends at `kf_status`, and its `kf_file` member
        // holds `FILE_TYPE`, `FSID` and `FILEID` at the offsets
        // `<sys/user.h>` gives them.
        assert!(offset_of!(libc::kinfo_file, kf_offset) + 8 == kinfo::FILE_TYPE);
        assert!(kinfo::FILEID + 8 <= kinfo::STATUS);
        assert!(offset_of!(libc::kinfo_file, kf_status) == kinfo::STATUS);
        assert!(offset_of!(libc::kinfo_file, kf_path) == kinfo::PATH);
        assert!(size_of::<libc::c_int>() == 4);
        assert!(libc::KF_TYPE_VNODE == kinfo::TYPE_VNODE);
        assert!(libc::KF_TYPE_FIFO == kinfo::TYPE_FIFO);
        assert!(libc::KF_FD_TYPE_CWD == kinfo::FD_CWD);
        assert!(libc::KF_ATTR_VALID == kinfo::ATTR_VALID as libc::c_int);
        assert!(libc::KF_VTYPE_VNON == kinfo::VTYPE_NON);
        assert!(libc::KF_VTYPE_VBAD == kinfo::VTYPE_BAD);
        assert!(offset_of!(libc::kinfo_vmentry, kve_structsize) == 0);
        assert!(offset_of!(libc::kinfo_vmentry, kve_vn_fileid) == vmentry::FILEID);
        assert!(offset_of!(libc::kinfo_vmentry, kve_vn_type) == vmentry::VTYPE);
        assert!(offset_of!(libc::kinfo_vmentry, kve_status) == vmentry::STATUS);
        assert!(offset_of!(libc::kinfo_vmentry, kve_vn_fsid) == vmentry::FSID);
        assert!(offset_of!(libc::kinfo_vmentry, kve_path) == vmentry::PATH);
        // `KINFO_FILE_SIZE` and `KINFO_VMENTRY_SIZE` in `<sys/user.h>`.
        #[cfg(target_arch = "x86_64")]
        assert!(size_of::<libc::kinfo_file>() == 1392);
        assert!(size_of::<libc::kinfo_vmentry>() == 1160);
    };

    /// The most residue reads of one process's files, or of its mappings:
    /// a file's record is 368 bytes and its path, a mapping's 136 and its
    /// path, so this is more than 100,000 of either.
    const MOST: usize = 64 << 20;

    pub fn take() -> Snapshot {
        let mut cmd = Command::new("/bin/ps");
        // -A: every process. -ww: never cut a line to the terminal width.
        // uid, ruid, svuid: the effective, real and saved user, all three
        // of which the kernel compares to let this user read a process.
        cmd.args([
            "-A", "-ww", "-o", "pid=", "-o", "uid=", "-o", "ruid=", "-o", "svuid=", "-o", "comm=",
        ])
        .env("LC_ALL", "C");
        let out = match run::run(&mut cmd, Duration::from_secs(30), 64 << 20) {
            Ok(out) => out,
            Err(RunError::NotFound) => {
                return unavailable(BACKEND, "probe_missing", "/bin/ps not found".into());
            }
            Err(RunError::Timeout(d)) => {
                return unavailable(
                    BACKEND,
                    "timeout",
                    format!("ps ran longer than {}s", d.as_secs()),
                );
            }
            Err(e) => return unavailable(BACKEND, "probe_failed", format!("ps: {e}")),
        };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first = stderr.lines().next().unwrap_or("no error output");
            return unavailable(
                BACKEND,
                "probe_failed",
                format!("ps exited with {}: {first}", out.status),
            );
        }
        // SAFETY: geteuid only returns this process's effective user.
        let me = unsafe { libc::geteuid() };
        let mut uses = Vec::new();
        let mut unnamed = Vec::new();
        let mut total = 0;
        let mut inspected = 0;
        // Reused for every process, and grown when one has more.
        let mut files = vec![0u8; 64 << 10];
        let mut maps = vec![0u8; 64 << 10];
        for line in out.stdout.split(|b| *b == b'\n') {
            if line.trim_ascii().is_empty() {
                continue;
            }
            let Some(listed) = super::ps_line(line) else {
                return unavailable(
                    BACKEND,
                    "parse_error",
                    format!(
                        "ps printed a line residue cannot read: {}",
                        String::from_utf8_lossy(line.trim_ascii())
                    ),
                );
            };
            let pid = listed.pid;
            let Ok(mib_pid) = libc::c_int::try_from(pid) else {
                return unavailable(
                    BACKEND,
                    "parse_error",
                    format!("ps printed process {pid}, which no sysctl can name"),
                );
            };
            total += 1;
            let ours = listed.uids == [me; 3];
            let mut len = [0; 2];
            for (n, (kind, buf, record)) in [
                (libc::KERN_PROC_FILEDESC, &mut files, FILE_RECORD),
                (libc::KERN_PROC_VMMAP, &mut maps, vmentry::RECORD),
            ]
            .into_iter()
            .enumerate()
            {
                len[n] = match ask(kind, mib_pid, buf, record) {
                    Read::Got(len) => len,
                    // Exited since ps listed it.
                    Read::Refused(libc::ESRCH) => 0,
                    // Another user's process, which this user may not
                    // inspect: outside what the probe reads, as `scope`
                    // says.
                    Read::Refused(libc::EPERM) if !ours => 0,
                    // Your own process (EPERM), or one still in the middle
                    // of an exec after `ask` asked again (EBUSY).
                    Read::Refused(errno) => {
                        let why = std::io::Error::from_raw_os_error(errno);
                        let name = String::from_utf8_lossy(listed.command);
                        let detail = if errno == libc::EPERM {
                            format!(
                                "the kernel would not show the files of {name}[{pid}], which runs as your user ({why}): security.bsd.unprivileged_proc_debug is 0, or the process denies tracing"
                            )
                        } else {
                            format!("the kernel would not show the files of {name}[{pid}] ({why})")
                        };
                        return unavailable(BACKEND, "probe_failed", detail);
                    }
                    Read::TooLarge => {
                        return unavailable(
                            BACKEND,
                            "probe_failed",
                            format!(
                                "process {pid} has more open or mapped files than residue reads ({} MiB of them)",
                                MOST >> 20
                            ),
                        );
                    }
                };
                if len[n] == 0 {
                    break;
                }
            }
            let command = String::from_utf8_lossy(listed.command).into_owned();
            // Inspected means the kernel named the process's working
            // directory; one it cannot name (a path not in its name cache)
            // comes back empty.
            let mut cwd = false;
            for file in kinfo::Files::new(&files[..len[0]]) {
                let file = match file {
                    Ok(file) => file,
                    Err(what) => {
                        return unavailable(
                            BACKEND,
                            "parse_error",
                            format!("kern.proc.filedesc for process {pid}: {what}"),
                        );
                    }
                };
                if !matches!(file.kind, kinfo::TYPE_VNODE | kinfo::TYPE_FIFO) {
                    continue;
                }
                if file.path.first() == Some(&b'/') {
                    cwd |= file.fd == kinfo::FD_CWD;
                    uses.push(Use {
                        pid,
                        command: command.clone(),
                        path: file.path.to_vec(),
                    });
                } else if !matches!(file.vtype, kinfo::VTYPE_NON | kinfo::VTYPE_BAD) {
                    unnamed.push(Unnamed {
                        pid,
                        command: command.clone(),
                        file: file.id,
                    });
                }
            }
            let mut last: &[u8] = &[];
            for map in vmentry::Mappings::new(&maps[..len[1]]) {
                let map = match map {
                    Ok(map) => map,
                    Err(what) => {
                        return unavailable(
                            BACKEND,
                            "parse_error",
                            format!("kern.proc.vmmap for process {pid}: {what}"),
                        );
                    }
                };
                // A mapping with no file (anonymous memory, a device, shared
                // memory), or a file the kernel let go of.
                if matches!(map.vtype, kinfo::VTYPE_NON | kinfo::VTYPE_BAD) {
                    continue;
                }
                if map.path.first() == Some(&b'/') {
                    // A file is mapped in several pieces, one after another.
                    if map.path != last {
                        uses.push(Use {
                            pid,
                            command: command.clone(),
                            path: map.path.to_vec(),
                        });
                        last = map.path;
                    }
                } else {
                    unnamed.push(Unnamed {
                        pid,
                        command: command.clone(),
                        file: map.id,
                    });
                }
            }
            if cwd {
                inspected += 1;
            }
        }
        // residue itself is always one of the processes it may inspect, so
        // none at all means the sysctl does not work here.
        if inspected == 0 {
            return unavailable(
                BACKEND,
                "probe_failed",
                format!("the kernel named the working directory of none of {total} processes"),
            );
        }
        Snapshot {
            backend: BACKEND,
            state: Ok(Seen {
                uses,
                unnamed,
                inspected,
                total: Some(total),
                limits: None,
                spelling: None,
            }),
        }
    }

    enum Read {
        /// The kernel wrote this many bytes of records into the buffer.
        Got(usize),
        /// The kernel refused, with this error.
        Refused(i32),
        TooLarge,
    }

    /// The longest record `kern.proc.filedesc` writes: a whole
    /// `kinfo_file`, 1392 bytes, rounded up to 8 (`export_vnode_to_kinfo`,
    /// unpacked).
    const FILE_RECORD: usize = size_of::<libc::kinfo_file>().next_multiple_of(8);

    /// [`read`], asked once more 10 ms later when the process is in the
    /// middle of an exec, the one refusal (EBUSY) that passes.
    fn ask(kind: libc::c_int, pid: libc::c_int, buf: &mut Vec<u8>, record: usize) -> Read {
        match read(kind, pid, buf, record) {
            Read::Refused(libc::EBUSY) => {
                std::thread::sleep(Duration::from_millis(10));
                read(kind, pid, buf, record)
            }
            other => other,
        }
    }

    /// `kern.proc.<kind>.<pid>` into `buf`, grown and asked again while the
    /// records may not have fit. FreeBSD 13 and later answer ENOMEM then.
    /// FreeBSD 12's `kern.proc.filedesc` stops at the first record that does
    /// not fit and answers success (`export_kinfo_to_sb` in its
    /// `kern_descrip.c`), so an answer that ends less than one `record`
    /// short of the buffer may have been cut, and is asked again too.
    fn read(kind: libc::c_int, pid: libc::c_int, buf: &mut Vec<u8>, record: usize) -> Read {
        let mib = [libc::CTL_KERN, libc::KERN_PROC, kind, pid];
        loop {
            let mut len: libc::size_t = buf.len();
            // SAFETY: `mib` holds 4 ints; `buf` is writable for `len` bytes
            // and the kernel writes at most `len`; no new value is set.
            let rc = unsafe {
                libc::sysctl(
                    mib.as_ptr(),
                    4,
                    buf.as_mut_ptr().cast(),
                    &mut len,
                    std::ptr::null(),
                    0,
                )
            };
            if rc == 0 {
                let len = len.min(buf.len());
                if buf.len() - len >= record {
                    return Read::Got(len);
                }
            } else {
                let errno = std::io::Error::last_os_error().raw_os_error();
                if errno != Some(libc::ENOMEM) {
                    return Read::Refused(errno.unwrap_or(libc::EINVAL));
                }
            }
            if buf.len() >= MOST {
                return Read::TooLarge;
            }
            let grown = (buf.len() * 2).min(MOST);
            buf.resize(grown, 0);
        }
    }
}

/// A line of `ps -o pid= -o uid= -o ruid= -o svuid= -o comm=`: four
/// numbers, then the command, which may hold spaces.
#[cfg(any(target_os = "freebsd", test))]
struct Listed<'a> {
    pid: u32,
    /// Effective, real and saved user.
    uids: [u32; 3],
    command: &'a [u8],
}

#[cfg(any(target_os = "freebsd", test))]
fn ps_line(line: &[u8]) -> Option<Listed<'_>> {
    let mut rest = line.trim_ascii();
    let mut field = || {
        let end = rest
            .iter()
            .position(u8::is_ascii_whitespace)
            .unwrap_or(rest.len());
        let number = std::str::from_utf8(&rest[..end]).ok()?.parse::<u32>().ok();
        rest = rest[end..].trim_ascii_start();
        number
    };
    let pid = field()?;
    let uids = [field()?, field()?, field()?];
    Some(Listed {
        pid,
        uids,
        command: rest,
    })
}

/// The packed records both FreeBSD sysctls return: each record ends after
/// its path's NUL, rounded up to 8 bytes, and its first int, its
/// `structsize`, gives its length (`pack_kinfo` in
/// `sys/kern/kern_descrip.c`, `kern_proc_vmmap_out` in
/// `sys/kern/kern_proc.c`). A record of size 0 ends the list, as in
/// libutil's kinfo_getfile(3), but only where nothing but zero bytes
/// follows it: the kernel writes no such record, so one with more of the
/// answer after it means the answer is not what residue reads.
///
/// Each item is a whole record and its path, without the NUL, or what is
/// wrong with the answer, which ends the walk.
#[cfg(any(target_os = "freebsd", test))]
type Record<'a> = Result<(&'a [u8], &'a [u8]), &'static str>;

#[cfg(any(target_os = "freebsd", test))]
fn next_record<'a>(rest: &mut &'a [u8], path_at: usize) -> Option<Record<'a>> {
    let all = *rest;
    if all.is_empty() {
        return None;
    }
    let size = int(all, 0).map(usize::try_from);
    let size = match size {
        Some(Ok(0)) => {
            *rest = &[];
            if all.iter().all(|b| *b == 0) {
                return None;
            }
            return Some(Err("a record of size 0 is followed by more of the answer"));
        }
        Some(Ok(size)) if size > path_at && size <= all.len() => size,
        _ => {
            *rest = &[];
            return Some(Err("a record's size does not fit the answer"));
        }
    };
    *rest = &all[size..];
    let name = &all[path_at..size];
    let Some(end) = name.iter().position(|b| *b == 0) else {
        *rest = &[];
        return Some(Err("a record's path has no end"));
    };
    Some(Ok((&all[..size], &name[..end])))
}

#[cfg(any(target_os = "freebsd", test))]
fn int(record: &[u8], at: usize) -> Option<i32> {
    let bytes = record.get(at..at + 4)?;
    Some(i32::from_ne_bytes(bytes.try_into().ok()?))
}

#[cfg(any(target_os = "freebsd", test))]
fn u64_at(record: &[u8], at: usize) -> Option<u64> {
    let bytes = record.get(at..at + 8)?;
    Some(u64::from_ne_bytes(bytes.try_into().ok()?))
}

#[cfg(any(target_os = "freebsd", test))]
fn u16_at(record: &[u8], at: usize) -> Option<u16> {
    let bytes = record.get(at..at + 2)?;
    Some(u16::from_ne_bytes(bytes.try_into().ok()?))
}

/// The records `kern.proc.filedesc.<pid>` returns on FreeBSD: one packed
/// `struct kinfo_file` (`<sys/user.h>`) per file, the process's executable,
/// controlling terminal, working directory, root and jail directories
/// first, then each descriptor ([`next_record`]).
#[cfg(any(target_os = "freebsd", test))]
mod kinfo {

    pub const TYPE: usize = 4;
    pub const FD: usize = 8;
    /// `kf_un.kf_file`: the vnode's type, filesystem and inode.
    pub const FILE_TYPE: usize = 32;
    pub const FSID: usize = 288;
    pub const FILEID: usize = 304;
    pub const STATUS: usize = 336;
    pub const PATH: usize = 368;
    pub const TYPE_VNODE: i32 = 1;
    pub const TYPE_FIFO: i32 = 4;
    pub const FD_CWD: i32 = -1;
    /// `kf_status` (and `kve_status`): the kernel read the file's
    /// attributes, so its filesystem and inode are set.
    pub const ATTR_VALID: u16 = 1;
    /// `KF_VTYPE_VNON`: no vnode. `KF_VTYPE_VBAD`: a vnode the kernel let
    /// go of, as a forced unmount does, which no file is behind.
    pub const VTYPE_NON: i32 = 0;
    pub const VTYPE_BAD: i32 = 8;

    pub struct File<'a> {
        pub kind: i32,
        pub fd: i32,
        /// For a vnode or a FIFO, its type (`KF_VTYPE_*`).
        pub vtype: i32,
        /// For a vnode or a FIFO, `(kf_file_fsid, kf_file_fileid)`, which
        /// are `stat`'s `(st_dev, st_ino)` (`vn_stat`), when the kernel read
        /// its attributes.
        pub id: Option<(u64, u64)>,
        /// Empty when the kernel cannot name the file.
        pub path: &'a [u8],
    }

    pub struct Files<'a> {
        rest: &'a [u8],
    }

    impl<'a> Files<'a> {
        pub fn new(records: &'a [u8]) -> Self {
            Files { rest: records }
        }
    }

    impl<'a> Iterator for Files<'a> {
        /// A record, or what is wrong with the answer, which ends the walk.
        type Item = Result<File<'a>, &'static str>;

        fn next(&mut self) -> Option<Self::Item> {
            let (record, path) = match super::next_record(&mut self.rest, PATH)? {
                Ok(found) => found,
                Err(what) => return Some(Err(what)),
            };
            let fields = (
                super::int(record, TYPE),
                super::int(record, FD),
                super::int(record, FILE_TYPE),
                super::u64_at(record, FSID),
                super::u64_at(record, FILEID),
                super::u16_at(record, STATUS),
            );
            let (Some(kind), Some(fd), Some(vtype), Some(fsid), Some(fileid), Some(status)) =
                fields
            else {
                return Some(Err("a record is shorter than its fields"));
            };
            Some(Ok(File {
                kind,
                fd,
                vtype,
                id: (status & ATTR_VALID != 0).then_some((fsid, fileid)),
                path,
            }))
        }
    }
}

/// The records `kern.proc.vmmap.<pid>` returns on FreeBSD: one packed
/// `struct kinfo_vmentry` (`<sys/user.h>`) per mapping of the process's
/// memory ([`next_record`]). A mapping backed by a file carries its vnode's
/// type, and its path and attributes as for an open file
/// (`kern_proc_vmmap_out`).
#[cfg(any(target_os = "freebsd", test))]
mod vmentry {

    pub const FILEID: usize = 32;
    pub const VTYPE: usize = 68;
    pub const STATUS: usize = 86;
    pub const FSID: usize = 88;
    pub const PATH: usize = 136;
    /// `KINFO_VMENTRY_SIZE`: the longest record, unpacked.
    pub const RECORD: usize = 1160;

    pub struct Mapping<'a> {
        /// The vnode's type (`KF_VTYPE_*`); `KF_VTYPE_VNON` when no file
        /// backs the mapping.
        pub vtype: i32,
        /// `(kve_vn_fsid, kve_vn_fileid)` when the kernel read the file's
        /// attributes.
        pub id: Option<(u64, u64)>,
        /// Empty when the kernel cannot name the file.
        pub path: &'a [u8],
    }

    pub struct Mappings<'a> {
        rest: &'a [u8],
    }

    impl<'a> Mappings<'a> {
        pub fn new(records: &'a [u8]) -> Self {
            Mappings { rest: records }
        }
    }

    impl<'a> Iterator for Mappings<'a> {
        /// A record, or what is wrong with the answer, which ends the walk.
        type Item = Result<Mapping<'a>, &'static str>;

        fn next(&mut self) -> Option<Self::Item> {
            let (record, path) = match super::next_record(&mut self.rest, PATH)? {
                Ok(found) => found,
                Err(what) => return Some(Err(what)),
            };
            let fields = (
                super::int(record, VTYPE),
                super::u64_at(record, FSID),
                super::u64_at(record, FILEID),
                super::u16_at(record, STATUS),
            );
            let (Some(vtype), Some(fsid), Some(fileid), Some(status)) = fields else {
                return Some(Err("a record is shorter than its fields"));
            };
            Some(Ok(Mapping {
                vtype,
                id: (status & super::kinfo::ATTR_VALID != 0).then_some((fsid, fileid)),
                path,
            }))
        }
    }
}

/// NetBSD, OpenBSD and DragonFly: the process list from the base system's
/// `ps`, then each process's working directory, and its executable where
/// the kernel names one, from the kernel by sysctl. Their `fstat` cannot
/// stand in: on NetBSD and OpenBSD it names an open file only by mount
/// point and inode ("Normally the name cannot be determined", fstat(1)), so
/// it cannot say whether a process is inside a worktree. Open files are not
/// read, so `limits` is set: a worktree with no use found is `unknown`,
/// never `not_observed`, and nothing here is a removal candidate.
#[cfg(any(target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly"))]
mod platform {
    use super::{Seen, Snapshot, Use, unavailable};
    use crate::run::{self, RunError};
    use std::process::Command;
    use std::time::Duration;

    const BACKEND: &str = "sysctl";

    /// NetBSD's `<sys/sysctl.h>`: `KERN_PROC_CWD 6`, under
    /// `kern.proc_args.<pid>`. The libc crate does not define it.
    #[cfg(target_os = "netbsd")]
    const KERN_PROC_CWD: libc::c_int = 6;

    #[cfg(target_os = "openbsd")]
    const LIMITS: &str = "working directories only, not open files";
    #[cfg(not(target_os = "openbsd"))]
    const LIMITS: &str = "working directories and executables only, not open files";

    pub fn take() -> Snapshot {
        let mut cmd = Command::new("/bin/ps");
        // -A: every process. -ww: never cut a line to the terminal width.
        cmd.args(["-A", "-ww", "-o", "pid=", "-o", "comm="])
            .env("LC_ALL", "C");
        let out = match run::run(&mut cmd, Duration::from_secs(30), 64 << 20) {
            Ok(out) => out,
            Err(RunError::NotFound) => {
                return unavailable(BACKEND, "probe_missing", "/bin/ps not found".into());
            }
            Err(RunError::Timeout(d)) => {
                return unavailable(
                    BACKEND,
                    "timeout",
                    format!("ps ran longer than {}s", d.as_secs()),
                );
            }
            Err(e) => return unavailable(BACKEND, "probe_failed", format!("ps: {e}")),
        };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first = stderr.lines().next().unwrap_or("no error output");
            return unavailable(
                BACKEND,
                "probe_failed",
                format!("ps exited with {}: {first}", out.status),
            );
        }
        let mut uses = Vec::new();
        let mut total = 0;
        let mut inspected = 0;
        for line in out.stdout.split(|b| *b == b'\n') {
            let line = line.trim_ascii();
            if line.is_empty() {
                continue;
            }
            let split = line
                .iter()
                .position(u8::is_ascii_whitespace)
                .unwrap_or(line.len());
            // DragonFly's ps lists kernel threads with pid -1. They have no
            // working directory and are not processes, so they are skipped;
            // any other pid that is not a number fails the probe.
            if &line[..split] == b"-1" {
                continue;
            }
            let pid = std::str::from_utf8(&line[..split])
                .ok()
                .and_then(|p| p.parse::<u32>().ok())
                .and_then(|p| libc::c_int::try_from(p).ok().map(|c| (p, c)));
            let Some((pid, mib_pid)) = pid else {
                return unavailable(
                    BACKEND,
                    "parse_error",
                    format!(
                        "ps printed a line residue cannot read: {}",
                        String::from_utf8_lossy(line)
                    ),
                );
            };
            total += 1;
            let command = String::from_utf8_lossy(line[split..].trim_ascii()).into_owned();
            // A process that exited since ps listed it, one the user may not
            // inspect, or one whose working directory the kernel cannot
            // name has no path here, and does not count as inspected.
            if let Some(path) = read(&cwd_mib(mib_pid)) {
                inspected += 1;
                uses.push(Use {
                    pid,
                    command: command.clone(),
                    path,
                });
            }
            if let Some(path) = exe_mib(mib_pid).and_then(|mib| read(&mib)) {
                uses.push(Use { pid, command, path });
            }
        }
        // residue itself is always one of the processes it may inspect, so
        // none at all means the sysctl does not work here.
        if inspected == 0 {
            return unavailable(
                BACKEND,
                "probe_failed",
                format!("the kernel named the working directory of none of {total} processes"),
            );
        }
        Snapshot {
            backend: BACKEND,
            state: Ok(Seen {
                uses,
                inspected,
                total: Some(total),
                limits: Some(LIMITS),
                spelling: None,
                unnamed: Vec::new(),
            }),
        }
    }

    #[cfg(target_os = "netbsd")]
    fn cwd_mib(pid: libc::c_int) -> Vec<libc::c_int> {
        vec![libc::CTL_KERN, libc::KERN_PROC_ARGS, pid, KERN_PROC_CWD]
    }

    #[cfg(target_os = "openbsd")]
    fn cwd_mib(pid: libc::c_int) -> Vec<libc::c_int> {
        vec![libc::CTL_KERN, libc::KERN_PROC_CWD, pid]
    }

    #[cfg(target_os = "dragonfly")]
    fn cwd_mib(pid: libc::c_int) -> Vec<libc::c_int> {
        vec![libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_CWD, pid]
    }

    #[cfg(target_os = "netbsd")]
    fn exe_mib(pid: libc::c_int) -> Option<Vec<libc::c_int>> {
        Some(vec![
            libc::CTL_KERN,
            libc::KERN_PROC_ARGS,
            pid,
            libc::KERN_PROC_PATHNAME,
        ])
    }

    /// OpenBSD's kernel does not keep the executable's path.
    #[cfg(target_os = "openbsd")]
    fn exe_mib(_pid: libc::c_int) -> Option<Vec<libc::c_int>> {
        None
    }

    #[cfg(target_os = "dragonfly")]
    fn exe_mib(pid: libc::c_int) -> Option<Vec<libc::c_int>> {
        Some(vec![
            libc::CTL_KERN,
            libc::KERN_PROC,
            libc::KERN_PROC_PATHNAME,
            pid,
        ])
    }

    /// One path-valued sysctl: the absolute path, or `None` when the kernel
    /// returned an error or no path. DragonFly answers a process the caller
    /// may not inspect with success and no bytes, so empty is `None` too.
    fn read(mib: &[libc::c_int]) -> Option<Vec<u8>> {
        // MAXPATHLEN * 4, the most OpenBSD's KERN_PROC_CWD returns.
        let mut buf = vec![0u8; 4 * 4096];
        let mut len: libc::size_t = buf.len();
        let namelen = libc::c_uint::try_from(mib.len()).ok()?;
        // SAFETY: `mib` holds `namelen` ints; `buf` is writable for `len`
        // bytes and the kernel writes at most `len`; no new value is set.
        let rc = unsafe {
            libc::sysctl(
                mib.as_ptr(),
                namelen,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(len.min(buf.len()));
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        buf.truncate(end);
        (buf.first() == Some(&b'/')).then_some(buf)
    }
}

/// What a `/proc/PID/path/NAME` link the illumos kernel could not give
/// holds. Its readlink is `vnodetopath` (`prreadlink_lookup` in
/// `fs/proc/prvnops.c`), which checks the path the vnode cached when it was
/// created or looked up (`v_path`). For a directory whose cached path is
/// stale it rebuilds one from `..` (`dirtopath`); for any other file it
/// fails (`vnodetopath_common` in `fs/lookup.c`). A path goes stale when a
/// folder above the file is renamed. ZFS gives the renamed folder its new
/// path (`vn_renamepath`, called only by ZFS and the NFS servers), and a
/// file below it takes the new path once looked up by it
/// (`vn_updatepath` in `fop_lookup`). tmpfs and UFS keep the folder's old
/// path, so a lookup changes nothing below it, and the file keeps the old
/// path until the folder's is rebuilt (`dirtopath`) and the file is looked
/// up again. A path of `MAXPATHLEN` bytes or more fails too. So a link
/// that fails can name a file inside any directory on its filesystem,
/// and residue looks for it there by device and inode (`Snapshot::evidence`).
///
/// Its device, inode and link count come from `fd/N`, `object/NAME` or the
/// opened `cwd` (`platform::attributes`). A file whose link count is 0 is
/// named by no directory, so it is inside no worktree: tmpfs
/// (`tdirdelete`), ZFS (`zfs_link_destroy`) and UFS (`ufs_dirremove`)
/// lower it as each name is removed, and their getattr reports it. A
/// socket, a pipe and a cloned device, which have no file behind them,
/// report 0 too (`socket_vop_getattr`, `fifo_getattr`, `spec_getattr`);
/// no descriptor names a filesystem's socket file, which open(2) refuses
/// (`vn_openat`). A door reports a link count but is on doorfs
/// (`door_getattr`), a device no worktree has. A regular file, a named
/// FIFO and a device file report their filesystem's. A directory open by
/// descriptor reports a link count of 1 (`prgetattr`), so a removed one
/// is looked for too, and found nowhere. `prgetattr` leaves the device
/// and inode the file's own.
#[cfg(any(target_os = "illumos", test))]
mod unnamed {
    /// What a process holds through a `path` entry whose link procfs could
    /// not read.
    #[derive(Debug, PartialEq)]
    pub enum Held {
        /// Nothing inside a directory: a file no directory names, or one
        /// closed or unmapped since the listing, or a process that exited.
        Nothing,
        /// A file with this `st_dev` and `st_ino`.
        File(u64, u64),
        /// A file whose device and inode residue could not read either.
        Unreadable,
    }

    /// Which `/proc/PID` entry gives the attributes of what the `path`
    /// entry NAME names: `cwd`, a descriptor number, or a mapped object
    /// (`a.out` among them), the names `pr_readdir_pathdir` lists.
    #[derive(Debug, PartialEq)]
    pub enum Stand {
        Cwd,
        Fd,
        Object,
    }

    pub fn stand_in(name: &[u8]) -> Stand {
        if name == b"cwd" {
            Stand::Cwd
        } else if !name.is_empty() && name.iter().all(u8::is_ascii_digit) {
            Stand::Fd
        } else {
            Stand::Object
        }
    }

    /// From the file's device, inode and link count, or the error reading
    /// them.
    pub fn held(read: std::io::Result<(u64, u64, u64)>) -> Held {
        match read {
            Ok((_, _, 0)) => Held::Nothing,
            Ok((device, inode, _)) => Held::File(device, inode),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Held::Nothing,
            Err(_) => Held::Unreadable,
        }
    }
}

/// illumos: `/proc/<pid>/path/`, which holds a link for the working
/// directory (`cwd`), for each open file descriptor and for each mapped
/// object (`a.out`, the executable), each naming the file's path when the
/// kernel knows it (proc(5)). Not `/proc/<pid>/cwd`: proc(5) says its
/// readlink yields a null string. A link the kernel cannot give is an
/// unnamed file on the filesystem it is on (`super::unnamed`).
#[cfg(target_os = "illumos")]
mod platform {
    use super::unnamed::{Held, Stand};
    use super::{Seen, Snapshot, Unnamed, Use, unavailable};
    use std::ffi::OsStr;
    use std::fs;
    use std::io::{self, ErrorKind};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    const BACKEND: &str = "procfs";

    /// Where `pr_pid` and `pr_fname` sit in a 64-bit `psinfo_t`
    /// (`<sys/procfs.h>`).
    const PSINFO_PID: usize = 8;
    const PSINFO_FNAME: usize = 136;
    const PRFNSZ: usize = 16;

    pub fn take() -> Snapshot {
        let listing = match fs::read_dir("/proc") {
            Ok(listing) => listing,
            Err(e) => return unavailable(BACKEND, "probe_missing", format!("/proc: {e}")),
        };
        let mut uses = Vec::new();
        let mut unnamed: Vec<Unnamed> = Vec::new();
        let mut total = 0;
        let mut inspected = 0;
        for item in listing {
            let item = match item {
                Ok(item) => item,
                Err(e) => return unavailable(BACKEND, "probe_failed", format!("/proc: {e}")),
            };
            let Some(pid) = item
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            total += 1;
            let base = item.path();
            let command = match command(&base.join("psinfo"), pid) {
                Ok(command) => command,
                Err(detail) => return unavailable(BACKEND, "parse_error", detail),
            };
            let first = unnamed.len();
            let mut note = |held: Held| {
                let file = match held {
                    Held::Nothing => return,
                    Held::File(device, inode) => Some((device, inode)),
                    Held::Unreadable => None,
                };
                if !unnamed[first..].iter().any(|u| u.file == file) {
                    unnamed.push(Unnamed {
                        pid,
                        command: command.clone(),
                        file,
                    });
                }
            };
            // Only the process's owner, or root, may list `path` (mode 0500,
            // `prgetnode`); a process that is not the user's or has exited
            // is skipped. Any other failure leaves its files unread.
            let entries = match fs::read_dir(base.join("path")) {
                Ok(entries) => entries,
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::PermissionDenied) => {
                    continue;
                }
                Err(_) => {
                    note(Held::Unreadable);
                    continue;
                }
            };
            let mut cwd = false;
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    // The process exited while residue read the list.
                    Err(e) if e.kind() == ErrorKind::NotFound => break,
                    Err(_) => {
                        note(Held::Unreadable);
                        break;
                    }
                };
                let name = entry.file_name();
                if name == "root" {
                    continue;
                }
                if let Some(path) = link(&entry.path()) {
                    cwd |= name == "cwd";
                    uses.push(Use {
                        pid,
                        command: command.clone(),
                        path,
                    });
                    continue;
                }
                note(super::unnamed::held(attributes(&base, &name)));
            }
            // A process whose working directory has no path here does not
            // count as inspected; what was read of it still counts.
            if cwd {
                inspected += 1;
            }
        }
        // residue itself is always one of the processes it may inspect.
        if inspected == 0 {
            return unavailable(
                BACKEND,
                "probe_failed",
                format!("/proc named the working directory of none of {total} processes"),
            );
        }
        Snapshot {
            backend: BACKEND,
            state: Ok(Seen {
                uses,
                inspected,
                total: Some(total),
                limits: None,
                spelling: None,
                unnamed,
            }),
        }
    }

    fn link(path: &Path) -> Option<Vec<u8>> {
        let bytes = fs::read_link(path).ok()?.into_os_string().into_vec();
        (bytes.first() == Some(&b'/')).then_some(bytes)
    }

    /// The device, inode and link count of the file the `path` entry NAME
    /// names,
    /// from the entry procfs gives that file's own attributes in: `fd/N`
    /// and `object/NAME` (`prgetattr` asks the file itself for PR_FD and
    /// PR_OBJECT), and for `cwd` the directory itself, opened through
    /// `/proc/PID/cwd` (`propen` hands back the real directory, while a
    /// stat of `cwd` gives procfs's own attributes). That open needs read
    /// access to the directory (`praccess`); without it the file is
    /// unreadable, which keeps every worktree.
    fn attributes(base: &Path, name: &OsStr) -> io::Result<(u64, u64, u64)> {
        let meta = match super::unnamed::stand_in(name.as_bytes()) {
            Stand::Cwd => fs::File::open(base.join("cwd"))?.metadata()?,
            Stand::Fd => fs::metadata(base.join("fd").join(name))?,
            Stand::Object => fs::metadata(base.join("object").join(name))?,
        };
        Ok((meta.dev(), meta.ino(), meta.nlink()))
    }

    /// The executable's name from `psinfo`, which every user may read. It
    /// is display-only, so a process that exited has an empty one; but a
    /// `psinfo` whose `pr_pid` is not this process means the layout above
    /// is wrong, and fails the probe.
    fn command(path: &std::path::Path, pid: u32) -> Result<String, String> {
        let Ok(info) = fs::read(path) else {
            return Ok(String::new());
        };
        let field = |at: usize, len: usize| info.get(at..at + len);
        let (Some(raw_pid), Some(fname)) = (field(PSINFO_PID, 4), field(PSINFO_FNAME, PRFNSZ))
        else {
            return Err(format!("{} is shorter than a psinfo_t", path.display()));
        };
        let mut word = [0u8; 4];
        word.copy_from_slice(raw_pid);
        if u32::from_ne_bytes(word) != pid {
            return Err(format!(
                "{} does not hold process {pid} where psinfo_t keeps pr_pid",
                path.display()
            ));
        }
        let end = fname.iter().position(|b| *b == 0).unwrap_or(fname.len());
        Ok(String::from_utf8_lossy(&fname[..end]).into_owned())
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
    target_os = "illumos"
)))]
mod platform {
    use super::{Snapshot, unavailable};

    pub fn take() -> Snapshot {
        unavailable(
            "none",
            "unsupported",
            "residue has no process probe for this operating system".into(),
        )
    }
}

#[cfg(test)]
mod tests {
    //! The activity decision for one directory, on synthetic snapshots: a
    //! probe that reads open files may answer `not_observed`; one that does
    //! not (`limits`) answers `in_use` or `unknown`, never `not_observed`.

    use super::{Evidence, Seen, Snapshot, Unnamed, Use};
    use std::path::Path;

    fn snapshot(limits: Option<&'static str>) -> Snapshot {
        Snapshot {
            backend: "test",
            state: Ok(Seen {
                uses: vec![Use {
                    pid: 1,
                    command: "sleep".into(),
                    path: b"/w/busy/src".to_vec(),
                }],
                inspected: 1,
                total: Some(1),
                limits,
                spelling: None,
                unnamed: Vec::new(),
            }),
        }
    }

    #[test]
    fn a_full_probe_finding_no_use_is_not_observed() {
        let e = snapshot(None).evidence(Path::new("/w/idle"), None);
        assert_eq!(e.code(), "not_observed");
    }

    #[test]
    fn a_partial_probe_finding_no_use_is_unknown() {
        let s = snapshot(Some("working directories only, not open files"));
        let Evidence::Unknown(u) = s.evidence(Path::new("/w/idle"), None) else {
            panic!("a partial probe with no use found must be unknown");
        };
        assert_eq!(u.reason, "partial_probe");
        assert_eq!(
            u.detail,
            "test reads working directories only, not open files, so it cannot show that no process is using it"
        );
    }

    #[test]
    fn a_use_is_in_use_with_either_probe() {
        for limits in [None, Some("working directories only, not open files")] {
            let Evidence::InUse(procs) = snapshot(limits).evidence(Path::new("/w/busy"), None)
            else {
                panic!("a use inside the directory must be in_use");
            };
            assert_eq!(procs, vec![(1, "sleep".to_string())]);
        }
    }

    /// A snapshot whose only process holds files it could not name: each
    /// its device and inode, or `None` where residue could not read them.
    fn unnamed_snapshot(pid: u32, files: &[Option<(u64, u64)>]) -> Snapshot {
        let mut s = snapshot(None);
        if let Ok(seen) = &mut s.state {
            for file in files {
                seen.unnamed.push(Unnamed {
                    pid,
                    command: "sleep".into(),
                    file: *file,
                });
            }
        }
        s
    }

    /// A folder of its own in the system's temporary directory, and in it
    /// the directory `w` holding `sub/file.txt`, beside `outside.txt`.
    fn scratch(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let top =
            std::env::temp_dir().join(format!("residue-activity-{name}-{}", std::process::id()));
        let dir = top.join("w");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/file.txt"), "x").unwrap();
        std::fs::write(top.join("outside.txt"), "x").unwrap();
        (top, dir)
    }

    /// The device and inode of `path`, as the backend records a file.
    fn id(path: &Path) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).unwrap();
        Some((meta.dev(), meta.ino()))
    }

    #[test]
    fn an_unnamed_file_inside_the_directory_is_its_holders_use() {
        let (top, dir) = scratch("inside");
        let file = id(&dir.join("sub/file.txt"));
        let Evidence::InUse(procs) = unnamed_snapshot(2, &[file]).evidence(&dir, None) else {
            panic!("an unnamed file inside the directory must be in_use");
        };
        assert_eq!(procs, vec![(2, "sleep".to_string())]);
        // The directory itself, held open, and one process listed once.
        let both = unnamed_snapshot(2, &[id(&dir), id(&dir.join("sub"))]);
        let Evidence::InUse(procs) = both.evidence(&dir, None) else {
            panic!("the directory held open must be in_use");
        };
        assert_eq!(procs, vec![(2, "sleep".to_string())]);
        std::fs::remove_dir_all(&top).unwrap();
    }

    #[test]
    fn an_unnamed_file_elsewhere_is_no_use_until_a_name_of_it_is_inside() {
        let (top, dir) = scratch("outside");
        let outside = id(&top.join("outside.txt"));
        assert_eq!(
            unnamed_snapshot(2, &[outside]).evidence(&dir, None).code(),
            "not_observed"
        );
        // On another filesystem it cannot be inside the directory.
        let (device, inode) = id(&dir.join("sub/file.txt")).unwrap();
        let other = unnamed_snapshot(2, &[Some((device.wrapping_add(1), inode))]);
        assert_eq!(other.evidence(&dir, None).code(), "not_observed");
        // A second name inside the directory is inside it.
        std::fs::hard_link(top.join("outside.txt"), dir.join("sub/link.txt")).unwrap();
        assert_eq!(
            unnamed_snapshot(2, &[outside]).evidence(&dir, None).code(),
            "in_use"
        );
        std::fs::remove_dir_all(&top).unwrap();
    }

    #[test]
    fn an_unnamed_file_residue_cannot_place_is_unknown() {
        use std::os::unix::fs::PermissionsExt;
        let (top, dir) = scratch("unread");
        let Evidence::Unknown(u) = unnamed_snapshot(2, &[None]).evidence(&dir, None) else {
            panic!("an unnamed file residue cannot read must be unknown");
        };
        assert_eq!(u.reason, "unnamed_file");
        assert_eq!(
            u.detail,
            "test cannot give the path of a file process 2 (sleep) holds, and residue cannot read where that file is, so it cannot show that no process is using it"
        );
        // A folder residue cannot read may hold it; root reads any, so the
        // check is only where the folder is unreadable.
        let outside = id(&top.join("outside.txt"));
        let sub = dir.join("sub");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(&sub).is_err();
        let evidence = unnamed_snapshot(2, &[outside]).evidence(&dir, None);
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        if unreadable {
            let Evidence::Unknown(u) = evidence else {
                panic!("a directory residue cannot read all of must be unknown");
            };
            assert_eq!(u.reason, "unnamed_file");
            assert!(
                u.detail.contains("could not read all of this directory"),
                "{}",
                u.detail
            );
        }
        std::fs::remove_dir_all(&top).unwrap();
    }

    #[test]
    fn a_use_outranks_an_unnamed_file_and_residues_own_do_not_count() {
        let (top, dir) = scratch("own");
        let mut s = unnamed_snapshot(std::process::id(), &[None]);
        assert_eq!(s.evidence(&dir, None).code(), "not_observed");
        if let Ok(seen) = &mut s.state {
            seen.unnamed[0].pid = 2;
            seen.uses[0].path = dir.as_os_str().as_encoded_bytes().to_vec();
        }
        let Evidence::InUse(procs) = s.evidence(&dir, None) else {
            panic!("a use inside the directory must be in_use");
        };
        assert_eq!(procs, vec![(1, "sleep".to_string())]);
        std::fs::remove_dir_all(&top).unwrap();
    }

    #[test]
    fn a_directory_that_does_not_exist_holds_no_unnamed_file() {
        let (top, dir) = scratch("gone");
        let file = id(&dir.join("sub/file.txt"));
        std::fs::remove_dir_all(&top).unwrap();
        assert_eq!(
            unnamed_snapshot(2, &[file]).evidence(&dir, None).code(),
            "not_observed"
        );
        assert_eq!(
            unnamed_snapshot(2, &[None]).evidence(&dir, None).code(),
            "not_observed"
        );
    }

    #[test]
    fn a_path_entry_is_read_where_procfs_gives_its_files_attributes() {
        use super::unnamed::{Stand, stand_in};
        assert_eq!(stand_in(b"cwd"), Stand::Cwd);
        assert_eq!(stand_in(b"0"), Stand::Fd);
        assert_eq!(stand_in(b"255"), Stand::Fd);
        assert_eq!(stand_in(b"a.out"), Stand::Object);
        assert_eq!(stand_in(b"zfs.65538.4194"), Stand::Object);
        assert_eq!(stand_in(b""), Stand::Object);
    }

    #[test]
    fn an_unlinked_or_closed_file_holds_nothing_and_an_unread_one_may_hold_any() {
        use super::unnamed::{Held, held};
        use std::io::{Error, ErrorKind};
        assert_eq!(held(Ok((7, 9, 1))), Held::File(7, 9));
        assert_eq!(held(Ok((7, 9, 2))), Held::File(7, 9));
        assert_eq!(held(Ok((7, 9, 0))), Held::Nothing);
        assert_eq!(held(Err(Error::from(ErrorKind::NotFound))), Held::Nothing);
        for kind in [ErrorKind::PermissionDenied, ErrorKind::Other] {
            assert_eq!(held(Err(Error::from(kind))), Held::Unreadable);
        }
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_a_use() {
        let s = snapshot(Some("working directories only, not open files"));
        assert_eq!(s.evidence(Path::new("/w/bus"), None).code(), "unknown");
        assert_eq!(
            snapshot(None).evidence(Path::new("/w/bus"), None).code(),
            "not_observed"
        );
    }

    /// A maps file as Linux prints it (proc_pid_maps(5)): libraries over
    /// several regions, the pseudo-names, an anonymous mapping, a path with
    /// spaces, a deleted file and a path with an escaped newline.
    const MAPS: &str = "\
55d0c4a00000-55d0c4a02000 r--p 00000000 08:01 1835012                    /usr/bin/sleep
55d0c4a02000-55d0c4a06000 r-xp 00002000 08:01 1835012                    /usr/bin/sleep
55d0c5800000-55d0c5821000 rw-p 00000000 00:00 0                          [heap]
7f2c3e400000-7f2c3e428000 r--p 00000000 08:01 1835234                    /w/busy/node_modules/a b.so
7f2c3e428000-7f2c3e5bd000 r-xp 00028000 08:01 1835234                    /w/busy/node_modules/a b.so
7f2c3e5bd000-7f2c3e5c0000 rw-p 00000000 00:00 0 \n\
7f2c3e600000-7f2c3e601000 r--s 00000000 08:01 1835240                    /w/gone/.git/index (deleted)
7f2c3e700000-7f2c3e701000 r--p 00000000 08:01 1835241                    /w/line\\012break/x.so
7fff6b3a0000-7fff6b3c1000 rw-p 00000000 00:00 0                          [stack]
7fff6b3fc000-7fff6b3fe000 r-xp 00000000 00:00 0                          [vdso]
";

    #[test]
    fn maps_names_only_files_whole_and_in_both_readings() {
        let file = std::env::temp_dir().join(format!("residue-maps-test-{}", std::process::id()));
        std::fs::write(&file, MAPS).unwrap();
        let mut paths = Vec::new();
        let read = super::maps::read(&file, &mut paths);
        std::fs::remove_file(&file).unwrap();
        assert!(read.is_ok());
        let paths: Vec<&[u8]> = paths.iter().map(Vec::as_slice).collect();
        assert_eq!(
            paths,
            vec![
                &b"/usr/bin/sleep"[..],
                b"/w/busy/node_modules/a b.so",
                b"/w/gone/.git/index (deleted)",
                b"/w/line\nbreak/x.so",
                b"/w/line\\012break/x.so",
            ]
        );
    }

    #[test]
    fn a_mapped_file_is_a_use_of_its_directory_even_when_deleted() {
        let s = Snapshot {
            backend: "test",
            state: Ok(Seen {
                uses: vec![Use {
                    pid: 1,
                    command: "git".into(),
                    path: b"/w/gone/.git/index (deleted)".to_vec(),
                }],
                inspected: 1,
                total: Some(1),
                limits: None,
                spelling: None,
                unnamed: Vec::new(),
            }),
        };
        assert_eq!(s.evidence(Path::new("/w/gone"), None).code(), "in_use");
        assert_eq!(s.evidence(Path::new("/w/gon"), None).code(), "not_observed");
    }

    #[test]
    fn lsof_spells_each_byte_as_it_prints_it() {
        use super::lsof::spell;
        use std::borrow::Cow;
        assert!(matches!(spell(b"/w/plain dir"), Some(Cow::Borrowed(_))));
        let cases: [(&[u8], &[u8]); 6] = [
            (b"/w/caf\xc3\xa9", b"/w/caf\\xc3\\xa9"),
            (b"/w/back\\slash", b"/w/back\\\\slash"),
            (b"/w/\x08\t\n\x0c\r", b"/w/\\b\\t\\n\\f\\r"),
            (b"/w/\x01\x07\x1b\x1f", b"/w/^A^G^[^_"),
            (b"/w/\x7f\x80\xfe", b"/w/\\x7f\\x80\\xfe"),
            (b"/w/\xf0\x9f\x9a\x80", b"/w/\\xf0\\x9f\\x9a\\x80"),
        ];
        for (dir, printed) in cases {
            assert_eq!(spell(dir).as_deref(), Some(printed));
        }
        assert_eq!(spell(b"/w/\xff"), None);
        assert_eq!(spell(b"/w/\0"), None);
    }

    /// A directory named `A` and every byte from 0x01 to 0x7f but `/`, and its
    /// name as `LC_ALL=C lsof -F n` printed it on macOS 26.3.1 for a process
    /// working in it.
    #[test]
    fn lsof_spells_every_ascii_byte_as_lsof_printed_it() {
        let dir: Vec<u8> = b"/w/A"
            .iter()
            .copied()
            .chain((0x01..=0x7f).filter(|b| *b != b'/'))
            .collect();
        let printed: &[u8] = b"/w/A^A^B^C^D^E^F^G\\b\\t\\n^K\\f\\r^N^O^P^Q^R^S^T^U^V^W^X^Y^Z^[^\\^]^^^_ !\"#$%&'()*+,-.0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\\\]^_`abcdefghijklmnopqrstuvwxyz{|}~\\x7f";
        assert_eq!(super::lsof::spell(&dir).as_deref(), Some(printed));
    }

    fn lsof_snapshot(paths: &[&[u8]]) -> Snapshot {
        Snapshot {
            backend: "lsof",
            state: Ok(Seen {
                uses: paths
                    .iter()
                    .map(|p| Use {
                        pid: 7,
                        command: "sleep".into(),
                        path: p.to_vec(),
                    })
                    .collect(),
                inspected: 1,
                total: Some(1),
                limits: None,
                spelling: Some(super::lsof::spell),
                unnamed: Vec::new(),
            }),
        }
    }

    fn dir(bytes: &[u8]) -> &Path {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(bytes))
    }

    #[test]
    fn an_escaped_use_is_a_use_of_the_directory_it_names() {
        let s = lsof_snapshot(&[b"/w/caf\\xc3\\xa9/src", b"/w/back\\\\slash"]);
        assert_eq!(s.evidence(dir(b"/w/caf\xc3\xa9"), None).code(), "in_use");
        assert_eq!(s.evidence(dir(b"/w/back\\slash"), None).code(), "in_use");
        assert_eq!(s.evidence(dir(b"/w/caf"), None).code(), "not_observed");
        assert_eq!(s.evidence(dir(b"/w/back"), None).code(), "not_observed");
        assert_eq!(
            s.evidence(dir(b"/w/caf\xc3\xa9x"), None).code(),
            "not_observed"
        );
    }

    #[test]
    fn a_directory_lsof_may_print_two_ways_is_unknown() {
        let s = lsof_snapshot(&[b"/w/busy"]);
        let Evidence::Unknown(u) = s.evidence(dir(b"/w/\xff"), None) else {
            panic!("a path whose printed form is not confirmed must be unknown");
        };
        assert_eq!(u.reason, "unmatchable_path");
    }

    /// A parsed record: its vnode's type, filesystem and inode, and path.
    type Parsed = (i32, Option<(u64, u64)>, Vec<u8>);

    /// A packed record as FreeBSD's kernel writes one: its size first, the
    /// `fields` at their offsets, then the path at `path_at`, NUL-ended
    /// and padded to 8.
    fn packed(path_at: usize, fields: &[(usize, &[u8])], path: &[u8]) -> Vec<u8> {
        let size = (path_at + path.len() + 1).next_multiple_of(8);
        let mut r = vec![0u8; size];
        r[..4].copy_from_slice(&i32::try_from(size).unwrap().to_ne_bytes());
        for (at, bytes) in fields {
            r[*at..*at + bytes.len()].copy_from_slice(bytes);
        }
        r[path_at..path_at + path.len()].copy_from_slice(path);
        r
    }

    /// A `kinfo_file` record for a regular file, with no attributes.
    fn kinfo_record(kind: i32, fd: i32, path: &[u8]) -> Vec<u8> {
        kinfo_file(kind, fd, 1, None, path)
    }

    /// A `kinfo_file` record: type, fd, the vnode's type and, when `id`
    /// is set, its filesystem and inode with `KF_ATTR_VALID`.
    fn kinfo_file(kind: i32, fd: i32, vtype: i32, id: Option<(u64, u64)>, path: &[u8]) -> Vec<u8> {
        use super::kinfo::{ATTR_VALID, FD, FILE_TYPE, FILEID, FSID, PATH, STATUS, TYPE};
        let (fsid, fileid) = id.unwrap_or((7, 7));
        let status = if id.is_some() { ATTR_VALID } else { 0 };
        packed(
            PATH,
            &[
                (TYPE, &kind.to_ne_bytes()),
                (FD, &fd.to_ne_bytes()),
                (FILE_TYPE, &vtype.to_ne_bytes()),
                (FSID, &fsid.to_ne_bytes()),
                (FILEID, &fileid.to_ne_bytes()),
                (STATUS, &status.to_ne_bytes()),
            ],
            path,
        )
    }

    /// A packed `kinfo_vmentry` record for a mapping.
    fn vmentry_record(vtype: i32, id: Option<(u64, u64)>, path: &[u8]) -> Vec<u8> {
        use super::vmentry::{FILEID, FSID, PATH, STATUS, VTYPE};
        let (fsid, fileid) = id.unwrap_or((7, 7));
        let status = u16::from(id.is_some());
        packed(
            PATH,
            &[
                (VTYPE, &vtype.to_ne_bytes()),
                (FSID, &fsid.to_ne_bytes()),
                (FILEID, &fileid.to_ne_bytes()),
                (STATUS, &status.to_ne_bytes()),
            ],
            path,
        )
    }

    #[test]
    fn kinfo_records_give_each_file_by_filesystem_and_inode() {
        use super::kinfo::{Files, TYPE_VNODE, VTYPE_BAD};
        let mut buf = kinfo_file(TYPE_VNODE, 3, 1, Some((0x1_0000_0002, 42)), b"/w/a");
        buf.extend(kinfo_file(TYPE_VNODE, 4, 1, Some((9, u64::MAX)), b""));
        buf.extend(kinfo_file(TYPE_VNODE, 5, VTYPE_BAD, None, b""));
        let files: Vec<Parsed> = Files::new(&buf)
            .map(|f| f.map(|f| (f.vtype, f.id, f.path.to_vec())))
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            files,
            vec![
                (1, Some((0x1_0000_0002, 42)), b"/w/a".to_vec()),
                (1, Some((9, u64::MAX)), Vec::new()),
                (VTYPE_BAD, None, Vec::new()),
            ]
        );
    }

    #[test]
    fn vmentry_records_give_each_mapping_and_its_file() {
        use super::kinfo::VTYPE_NON;
        use super::vmentry::{Mappings, RECORD};
        // The longest path the kernel writes, 1,023 bytes, fills a whole record.
        assert_eq!(vmentry_record(1, None, &[b'a'; 1023]).len(), RECORD);
        let mut buf = vmentry_record(VTYPE_NON, None, b"");
        buf.extend(vmentry_record(1, Some((5, 6)), b"/w/lib/x.so"));
        buf.extend(vmentry_record(1, Some((5, 7)), b""));
        buf.extend(vmentry_record(1, None, b"/w/caf\xc3\xa9 \xff"));
        let maps: Vec<Parsed> = Mappings::new(&buf)
            .map(|m| m.map(|m| (m.vtype, m.id, m.path.to_vec())))
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            maps,
            vec![
                (VTYPE_NON, None, Vec::new()),
                (1, Some((5, 6)), b"/w/lib/x.so".to_vec()),
                (1, Some((5, 7)), Vec::new()),
                (1, None, b"/w/caf\xc3\xa9 \xff".to_vec()),
            ]
        );
        // Cut short, or a size past the answer: an error, never a skip.
        let last = |buf: &[u8]| Mappings::new(buf).last().map(|m| m.is_err());
        assert_eq!(last(&buf[..buf.len() - 8]), Some(true));
        let mut no_end = vmentry_record(1, None, b"/w/a");
        no_end[super::vmentry::PATH..].fill(b'a');
        assert_eq!(last(&no_end), Some(true));
    }

    #[test]
    fn a_ps_line_gives_the_pid_the_three_users_and_the_command() {
        let listed = super::ps_line(b"  812  1001  1001     0 my tool \n").unwrap();
        assert_eq!(listed.pid, 812);
        assert_eq!(listed.uids, [1001, 1001, 0]);
        assert_eq!(listed.command, b"my tool");
        let listed = super::ps_line(b"1 0 0 0 init").unwrap();
        assert_eq!(
            (listed.pid, listed.uids, listed.command),
            (1, [0; 3], &b"init"[..])
        );
        assert!(super::ps_line(b"  812  1001  1001").is_none());
        assert!(super::ps_line(b"  812  root  1001  1001 sh").is_none());
        assert!(super::ps_line(b"  -1  0  0  0 sh").is_none());
    }

    #[test]
    fn kinfo_records_give_each_path_byte_for_byte() {
        use super::kinfo::{FD_CWD, Files, TYPE_FIFO, TYPE_VNODE};
        let mut buf = kinfo_record(TYPE_VNODE, -5, b"/bin/sleep");
        buf.extend(kinfo_record(TYPE_VNODE, FD_CWD, b"/w/caf\xc3\xa9 \xff\\x"));
        buf.extend(kinfo_record(TYPE_FIFO, 3, b"/w/pipe"));
        buf.extend(kinfo_record(TYPE_VNODE, 4, b""));
        let files: Vec<(i32, i32, Vec<u8>)> = Files::new(&buf)
            .map(|f| f.map(|f| (f.kind, f.fd, f.path.to_vec())))
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            files,
            vec![
                (TYPE_VNODE, -5, b"/bin/sleep".to_vec()),
                (TYPE_VNODE, FD_CWD, b"/w/caf\xc3\xa9 \xff\\x".to_vec()),
                (TYPE_FIFO, 3, b"/w/pipe".to_vec()),
                (TYPE_VNODE, 4, Vec::new()),
            ]
        );
        // A record of size 0 ends the list when only zero bytes follow it,
        // and is an error when a record does: the walk never skips one.
        let mut ended = buf.clone();
        ended.extend([0u8; 16]);
        assert_eq!(Files::new(&ended).count(), 4);
        assert!(Files::new(&ended).all(|f| f.is_ok()));
        ended.extend(kinfo_record(TYPE_VNODE, 5, b"/w/after"));
        let last = Files::new(&ended).last().map(|f| f.is_err());
        assert_eq!(last, Some(true));
        assert_eq!(Files::new(&ended).count(), 5);
    }

    #[test]
    fn a_kinfo_answer_cut_short_is_an_error() {
        use super::kinfo::{Files, TYPE_VNODE};
        let whole = kinfo_record(TYPE_VNODE, 0, b"/w/a");
        let last = |buf: &[u8]| Files::new(buf).last().map(|f| f.is_err());
        assert_eq!(last(&whole), Some(false));
        assert_eq!(last(&whole[..whole.len() - 8]), Some(true));
        assert_eq!(last(&whole[..2]), Some(true));
        let mut no_end = whole.clone();
        no_end[super::kinfo::PATH..].fill(b'a');
        assert_eq!(last(&no_end), Some(true));
    }

    #[test]
    fn a_maps_line_without_a_file_names_none() {
        use super::maps::path;
        assert_eq!(path(b"7f00-7f01 rw-p 00000000 00:00 0 \n"), None);
        assert_eq!(path(b"7f00-7f01 rw-p 00000000 00:00 0"), None);
        assert_eq!(
            path(b"7f00-7f01 rw-p 00000000 00:00 0      [anon:x /y]\n"),
            None
        );
        assert_eq!(
            path(b"7f00-7f01 r--p 00000000 08:01 7      /a  b \n"),
            Some(&b"/a  b "[..])
        );
    }

    #[test]
    fn a_path_the_probe_cannot_name_whole_is_unknown() {
        use super::{PATH_LIMIT, Reach};
        let s = snapshot(None);
        let reach = |longest| Reach {
            longest,
            ..Reach::default()
        };
        let idle = Path::new("/w/idle");
        let at = |longest| s.evidence(idle, Some(&reach(longest)));
        assert_eq!(at(PATH_LIMIT - 1).code(), "not_observed");
        let Evidence::Unknown(u) = at(PATH_LIMIT) else {
            panic!("a path of PATH_LIMIT bytes must be unknown");
        };
        assert_eq!(u.reason, "long_path");
        assert_eq!(
            u.detail,
            format!(
                "test names a path whole only when it is shorter than {PATH_LIMIT} bytes, and a path here is {PATH_LIMIT}, so it cannot show that no process is using it"
            )
        );
        // The directory's own path counts without a walk.
        let long = format!("/{}", "a".repeat(PATH_LIMIT));
        let e = s.evidence(Path::new(&long), None);
        assert_eq!(e.code(), "unknown");
        // A use the probe did name is still a use.
        let e = s.evidence(Path::new("/w/busy"), Some(&reach(PATH_LIMIT * 2)));
        assert_eq!(e.code(), "in_use");
    }

    /// With the walk that sized the directory, an unnamed file is placed by
    /// what that walk listed, and no second walk runs: inode 42 is not on
    /// disk here, so only the walk's list can put it inside.
    #[test]
    fn an_unnamed_file_is_placed_by_the_walk_that_sized_the_directory() {
        use super::Reach;
        use std::os::unix::fs::MetadataExt;
        let (top, dir) = scratch("walked");
        let here = std::fs::metadata(&dir).unwrap().dev();
        let walked = |found: Vec<(u64, u64)>, unread: Option<&str>| Reach {
            longest: 1,
            found,
            unread: unread.map(String::from),
        };
        let code = |files: &[Option<(u64, u64)>], reach: &Reach| {
            unnamed_snapshot(2, files)
                .evidence(&dir, Some(reach))
                .code()
        };
        let Evidence::InUse(procs) = unnamed_snapshot(2, &[Some((here, 42))])
            .evidence(&dir, Some(&walked(vec![(here, 42)], None)))
        else {
            panic!("an unnamed file the walk listed must be in_use");
        };
        assert_eq!(procs, vec![(2, "sleep".to_string())]);
        // On this filesystem but not in the walk's list: outside.
        assert_eq!(
            code(&[Some((here, 42))], &walked(Vec::new(), None)),
            "not_observed"
        );
        // On another filesystem it cannot be inside.
        assert_eq!(
            code(
                &[Some((here ^ 1, 42))],
                &walked(Vec::new(), Some("x: denied"))
            ),
            "not_observed"
        );
        // A walk that could not read all of the directory cannot show it
        // outside.
        let Evidence::Unknown(u) = unnamed_snapshot(2, &[Some((here, 42))])
            .evidence(&dir, Some(&walked(Vec::new(), Some("x: denied"))))
        else {
            panic!("an unnamed file behind what the walk could not read must be unknown");
        };
        assert_eq!(u.reason, "unnamed_file");
        assert_eq!(
            u.detail,
            "test cannot give the path of a file process 2 (sleep) holds, and residue could not read all of this directory to look for it (x: denied), so it cannot show that no process is using it"
        );
        // One with no device and inode may be anywhere.
        let Evidence::Unknown(u) =
            unnamed_snapshot(2, &[None]).evidence(&dir, Some(&walked(vec![(here, 42)], None)))
        else {
            panic!("an unnamed file residue cannot place must be unknown");
        };
        assert_eq!(u.reason, "unnamed_file");
        std::fs::remove_dir_all(&top).unwrap();
    }

    #[test]
    fn the_walk_looks_for_each_unnamed_file_once() {
        let s = unnamed_snapshot(2, &[Some((2, 9)), None, Some((1, 5)), Some((2, 9))]);
        assert_eq!(s.wanted(), vec![(1, 5), (2, 9)]);
        let failed = super::unavailable("test", "timeout", String::new());
        assert!(failed.wanted().is_empty());
    }
}
