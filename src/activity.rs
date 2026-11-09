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
//! worktree. `not_observed` is not proof of
//! inactivity: processes owned by other users, in containers or on other
//! machines are outside what a same-user probe can see, and a process can
//! open the directory a moment later.

use std::borrow::Cow;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Clone)]
pub struct Unavailable {
    /// Stable reason code: `probe_missing`, `timeout`, `probe_failed`,
    /// `parse_error`, `unsupported`, `partial_probe` (the probe found no
    /// use but does not read open files, so it cannot show the directory
    /// is unused), or `unmatchable_path` (the probe prints the paths it
    /// read escaped, and residue cannot be sure how it prints this one).
    pub reason: &'static str,
    pub detail: String,
}

pub struct Use {
    pub pid: u32,
    pub command: String,
    pub path: Vec<u8>,
}

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

    pub fn evidence(&self, dir: &Path) -> Evidence {
        let seen = match &self.state {
            Ok(seen) => seen,
            Err(unavailable) => return Evidence::Unknown(unavailable.clone()),
        };
        let dir = dir.as_os_str().as_bytes();
        // The directory as the backend writes the paths it read: a use is
        // inside it in that form exactly when it is inside it byte for byte.
        let dir = match seen.spelling {
            None => Cow::Borrowed(dir),
            Some(spell) => match spell(dir) {
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
                && within(&item.path, &dir)
                && !hits.iter().any(|(p, _)| *p == item.pid)
            {
                hits.push((item.pid, item.command.clone()));
            }
        }
        if !hits.is_empty() {
            hits.sort();
            return Evidence::InUse(hits);
        }
        match seen.limits {
            None => Evidence::NotObserved,
            // Finding no use is evidence of no use only when the probe
            // reads open files too; this one does not, so it cannot show
            // that the directory is unused.
            Some(limits) => Evidence::Unknown(Unavailable {
                reason: "partial_probe",
                detail: format!(
                    "{} reads {limits}, so it cannot show that no process is using it",
                    self.backend
                ),
            }),
        }
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
/// process's working directory, executable and open files from the kernel,
/// by the sysctl procstat(1) itself reads (`kern.proc.filedesc.<pid>`,
/// kinfo_getfile(3)), which names each path byte for byte. procstat is not
/// run: it prints a name through libxo, which in the C locale rewrites a
/// character that is not ASCII and leaves out a name that is not UTF-8. A
/// packaged lsof is not used either: on FreeBSD 14 it listed processes
/// without resolving their working directories.
#[cfg(target_os = "freebsd")]
mod platform {
    use super::{Seen, Snapshot, Use, kinfo, unavailable};
    use crate::run::{self, RunError};
    use std::mem::offset_of;
    use std::process::Command;
    use std::time::Duration;

    const BACKEND: &str = "sysctl";

    // `kinfo` reads records by these offsets and codes; the build fails if
    // they ever differ from `<sys/user.h>` as the libc crate declares it.
    const _: () = {
        assert!(offset_of!(libc::kinfo_file, kf_structsize) == kinfo::STRUCTSIZE);
        assert!(offset_of!(libc::kinfo_file, kf_type) == kinfo::TYPE);
        assert!(offset_of!(libc::kinfo_file, kf_fd) == kinfo::FD);
        assert!(offset_of!(libc::kinfo_file, kf_path) == kinfo::PATH);
        assert!(size_of::<libc::c_int>() == 4);
        assert!(libc::KF_TYPE_VNODE == kinfo::TYPE_VNODE);
        assert!(libc::KF_TYPE_FIFO == kinfo::TYPE_FIFO);
        assert!(libc::KF_FD_TYPE_CWD == kinfo::FD_CWD);
        // `KINFO_FILE_SIZE` in `<sys/user.h>`, which `RECORD` stands for.
        #[cfg(target_arch = "x86_64")]
        assert!(size_of::<libc::kinfo_file>() == 1392);
    };

    /// The most residue reads of one process's files: a record is 368
    /// bytes and its path, so this is more than 100,000 open files.
    const MOST: usize = 64 << 20;

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
        // Reused for every process, and grown when one has more files.
        let mut buf = vec![0u8; 64 << 10];
        for line in out.stdout.split(|b| *b == b'\n') {
            let line = line.trim_ascii();
            if line.is_empty() {
                continue;
            }
            let split = line
                .iter()
                .position(u8::is_ascii_whitespace)
                .unwrap_or(line.len());
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
            let len = match read(mib_pid, &mut buf) {
                Read::Got(len) => len,
                // Exited since ps listed it, or the user may not inspect it.
                Read::Skip => continue,
                Read::TooLarge => {
                    return unavailable(
                        BACKEND,
                        "probe_failed",
                        format!(
                            "process {pid} has more open files than residue reads ({} MiB of them)",
                            MOST >> 20
                        ),
                    );
                }
            };
            let command = String::from_utf8_lossy(line[split..].trim_ascii()).into_owned();
            // Inspected means the kernel named the process's working
            // directory; one it cannot name (a path not in its name cache)
            // comes back empty.
            let mut cwd = false;
            for file in kinfo::Files::new(&buf[..len]) {
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
                let named = matches!(file.kind, kinfo::TYPE_VNODE | kinfo::TYPE_FIFO)
                    && file.path.first() == Some(&b'/');
                if !named {
                    continue;
                }
                cwd |= file.fd == kinfo::FD_CWD;
                uses.push(Use {
                    pid,
                    command: command.clone(),
                    path: file.path.to_vec(),
                });
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
        Skip,
        TooLarge,
    }

    /// The longest record the kernel writes: a whole `kinfo_file`, 1392
    /// bytes, rounded up to 8 (`export_vnode_to_kinfo`, unpacked).
    const RECORD: usize = size_of::<libc::kinfo_file>().next_multiple_of(8);

    /// `kern.proc.filedesc.<pid>` into `buf`, grown and asked again while
    /// the records may not have fit. FreeBSD 13 and later answer ENOMEM
    /// then. FreeBSD 12 stops at the first record that does not fit and
    /// answers success (`export_kinfo_to_sb` in its `kern_descrip.c`), so
    /// an answer that ends less than one record short of the buffer may
    /// have been cut, and is asked again too.
    fn read(pid: libc::c_int, buf: &mut Vec<u8>) -> Read {
        let mib = [
            libc::CTL_KERN,
            libc::KERN_PROC,
            libc::KERN_PROC_FILEDESC,
            pid,
        ];
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
                if buf.len() - len >= RECORD {
                    return Read::Got(len);
                }
            } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
                // Any other error is the process's: ESRCH, it exited; EPERM,
                // the user may not inspect it.
                return Read::Skip;
            }
            if buf.len() >= MOST {
                return Read::TooLarge;
            }
            let grown = (buf.len() * 2).min(MOST);
            buf.resize(grown, 0);
        }
    }
}

/// The records `kern.proc.filedesc.<pid>` returns on FreeBSD: one packed
/// `struct kinfo_file` (`<sys/user.h>`) per file, the process's executable,
/// controlling terminal, working directory, root and jail directories
/// first, then each descriptor. Packed means each record ends after its
/// path's NUL, rounded up to 8 bytes, and `kf_structsize` gives its length
/// (`pack_kinfo` in `sys/kern/kern_descrip.c`). A record of size 0 ends the
/// list, as in libutil's kinfo_getfile(3).
#[cfg(any(target_os = "freebsd", test))]
mod kinfo {
    pub const STRUCTSIZE: usize = 0;
    pub const TYPE: usize = 4;
    pub const FD: usize = 8;
    pub const PATH: usize = 368;
    pub const TYPE_VNODE: i32 = 1;
    pub const TYPE_FIFO: i32 = 4;
    pub const FD_CWD: i32 = -1;

    pub struct File<'a> {
        pub kind: i32,
        pub fd: i32,
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
            let rest = self.rest;
            if rest.is_empty() {
                return None;
            }
            let size = int(rest, STRUCTSIZE).map(usize::try_from);
            let size = match size {
                Some(Ok(0)) => {
                    self.rest = &[];
                    return None;
                }
                Some(Ok(size)) if size > PATH && size <= rest.len() => size,
                _ => {
                    self.rest = &[];
                    return Some(Err("a record's size does not fit the answer"));
                }
            };
            self.rest = &rest[size..];
            let (Some(kind), Some(fd)) = (int(rest, TYPE), int(rest, FD)) else {
                return Some(Err("a record is shorter than its fields"));
            };
            let name = &rest[PATH..size];
            let Some(end) = name.iter().position(|b| *b == 0) else {
                self.rest = &[];
                return Some(Err("a record's path has no end"));
            };
            Some(Ok(File {
                kind,
                fd,
                path: &name[..end],
            }))
        }
    }

    fn int(record: &[u8], at: usize) -> Option<i32> {
        let bytes = record.get(at..at + 4)?;
        Some(i32::from_ne_bytes(bytes.try_into().ok()?))
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

/// illumos: `/proc/<pid>/path/`, which holds a link for the working
/// directory (`cwd`), for each open file descriptor and for each mapped
/// object (`a.out`, the executable), each naming the file's path when the
/// kernel knows it (proc(5)). Not `/proc/<pid>/cwd`: proc(5) says its
/// readlink yields a null string.
#[cfg(target_os = "illumos")]
mod platform {
    use super::{Seen, Snapshot, Use, unavailable};
    use std::fs;
    use std::os::unix::ffi::OsStringExt;

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
            let paths = base.join("path");
            // A process whose working directory has no path here (exited,
            // not the user's, or not nameable) does not count as inspected.
            let Some(cwd) = link(&paths.join("cwd")) else {
                continue;
            };
            inspected += 1;
            uses.push(Use {
                pid,
                command: command.clone(),
                path: cwd,
            });
            let Ok(entries) = fs::read_dir(&paths) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name == "cwd" || name == "root" {
                    continue;
                }
                // A descriptor closed since the listing, a socket or a pipe
                // has no path; skip it.
                if let Some(path) = link(&entry.path()) {
                    uses.push(Use {
                        pid,
                        command: command.clone(),
                        path,
                    });
                }
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
            }),
        }
    }

    fn link(path: &std::path::Path) -> Option<Vec<u8>> {
        let bytes = fs::read_link(path).ok()?.into_os_string().into_vec();
        (bytes.first() == Some(&b'/')).then_some(bytes)
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

    use super::{Evidence, Seen, Snapshot, Use};
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
            }),
        }
    }

    #[test]
    fn a_full_probe_finding_no_use_is_not_observed() {
        let e = snapshot(None).evidence(Path::new("/w/idle"));
        assert_eq!(e.code(), "not_observed");
    }

    #[test]
    fn a_partial_probe_finding_no_use_is_unknown() {
        let s = snapshot(Some("working directories only, not open files"));
        let Evidence::Unknown(u) = s.evidence(Path::new("/w/idle")) else {
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
            let Evidence::InUse(procs) = snapshot(limits).evidence(Path::new("/w/busy")) else {
                panic!("a use inside the directory must be in_use");
            };
            assert_eq!(procs, vec![(1, "sleep".to_string())]);
        }
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_a_use() {
        let s = snapshot(Some("working directories only, not open files"));
        assert_eq!(s.evidence(Path::new("/w/bus")).code(), "unknown");
        assert_eq!(
            snapshot(None).evidence(Path::new("/w/bus")).code(),
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
            }),
        };
        assert_eq!(s.evidence(Path::new("/w/gone")).code(), "in_use");
        assert_eq!(s.evidence(Path::new("/w/gon")).code(), "not_observed");
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
        assert_eq!(s.evidence(dir(b"/w/caf\xc3\xa9")).code(), "in_use");
        assert_eq!(s.evidence(dir(b"/w/back\\slash")).code(), "in_use");
        assert_eq!(s.evidence(dir(b"/w/caf")).code(), "not_observed");
        assert_eq!(s.evidence(dir(b"/w/back")).code(), "not_observed");
        assert_eq!(s.evidence(dir(b"/w/caf\xc3\xa9x")).code(), "not_observed");
    }

    #[test]
    fn a_directory_lsof_may_print_two_ways_is_unknown() {
        let s = lsof_snapshot(&[b"/w/busy"]);
        let Evidence::Unknown(u) = s.evidence(dir(b"/w/\xff")) else {
            panic!("a path whose printed form is not confirmed must be unknown");
        };
        assert_eq!(u.reason, "unmatchable_path");
    }

    /// Packed `kinfo_file` records as FreeBSD's kernel writes them: size,
    /// type and fd, then the path at offset 368, NUL-ended and padded to 8.
    fn kinfo_record(kind: i32, fd: i32, path: &[u8]) -> Vec<u8> {
        use super::kinfo::{FD, PATH, STRUCTSIZE, TYPE};
        let size = (PATH + path.len() + 1).next_multiple_of(8);
        let mut r = vec![0u8; size];
        let size = i32::try_from(size).unwrap();
        r[STRUCTSIZE..STRUCTSIZE + 4].copy_from_slice(&size.to_ne_bytes());
        r[TYPE..TYPE + 4].copy_from_slice(&kind.to_ne_bytes());
        r[FD..FD + 4].copy_from_slice(&fd.to_ne_bytes());
        r[PATH..PATH + path.len()].copy_from_slice(path);
        r
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
        // A record of size 0 ends the list, whatever follows it.
        buf.extend([0u8; 16]);
        buf.extend(kinfo_record(TYPE_VNODE, 5, b"/w/after"));
        assert_eq!(Files::new(&buf).count(), 4);
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
}
