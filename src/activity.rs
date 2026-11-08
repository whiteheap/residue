//! Is any process using this directory?
//!
//! One snapshot of process working directories and open files is taken per
//! command and matched against each worktree. The answer has three states,
//! never two: a process was seen using the directory (`in_use`), none of
//! the processes the probe could inspect was (`not_observed`), or the probe
//! failed or reads too little to show that none was (`unknown`). A probe
//! that does not read open files (`Seen::limits`) can show a use, never its
//! absence, so where it finds none the answer is `unknown`, which keeps the
//! worktree. `not_observed` is not proof of
//! inactivity: processes owned by other users, in containers or on other
//! machines are outside what a same-user probe can see, and a process can
//! open the directory a moment later.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Clone)]
pub struct Unavailable {
    /// Stable reason code: `probe_missing`, `timeout`, `probe_failed`,
    /// `parse_error`, `unsupported`, or `partial_probe` (the probe found no
    /// use but does not read open files, so it cannot show the directory
    /// is unused).
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
}

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
        let me = std::process::id();
        let mut hits: Vec<(u32, String)> = Vec::new();
        for item in &seen.uses {
            if item.pid != me
                && within(&item.path, dir)
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
    use super::{Seen, Snapshot, Use, unavailable};
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
            let mut readable = false;
            let mut push = |target: std::path::PathBuf| {
                let bytes = target.into_os_string().into_vec();
                if bytes.first() == Some(&b'/') {
                    uses.push(Use {
                        pid,
                        command: command.clone(),
                        path: bytes,
                    });
                }
            };
            for link in ["cwd", "exe"] {
                if let Ok(target) = fs::read_link(base.join(link)) {
                    readable = true;
                    push(target);
                }
            }
            if let Ok(fds) = fs::read_dir(base.join("fd")) {
                readable = true;
                for fd in fds.flatten() {
                    // An fd closed between listing and readlink is gone; skip it.
                    if let Ok(target) = fs::read_link(fd.path()) {
                        push(target);
                    }
                }
            }
            if readable {
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
            }),
        }
    }
}

/// FreeBSD: one `procstat -f` snapshot from the base system. procstat is
/// built with the running kernel, unlike a packaged lsof, which on FreeBSD
/// can list a process without resolving its working directory.
#[cfg(target_os = "freebsd")]
mod platform {
    use super::{Seen, Snapshot, Use, unavailable};
    use crate::run::{self, RunError};
    use std::collections::HashSet;
    use std::process::Command;
    use std::time::Duration;

    const BACKEND: &str = "procstat";

    pub fn take() -> Snapshot {
        let mut cmd = Command::new("/usr/bin/procstat");
        // -h: no header. -a: every process. -f: its files, working directory
        // (fd `cwd`) and executable (fd `text`).
        cmd.args(["-h", "-a", "-f"]).env("LC_ALL", "C");
        let out = match run::run(&mut cmd, Duration::from_secs(30), 512 << 20) {
            Ok(out) => out,
            Err(RunError::NotFound) => {
                return unavailable(
                    BACKEND,
                    "probe_missing",
                    "/usr/bin/procstat not found".into(),
                );
            }
            Err(RunError::Timeout(d)) => {
                return unavailable(
                    BACKEND,
                    "timeout",
                    format!("procstat ran longer than {}s", d.as_secs()),
                );
            }
            Err(e) => return unavailable(BACKEND, "probe_failed", format!("procstat: {e}")),
        };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first = stderr.lines().next().unwrap_or("no error output");
            return unavailable(
                BACKEND,
                "probe_failed",
                format!("procstat exited with {}: {first}", out.status),
            );
        }
        let mut uses = Vec::new();
        // Processes whose working directory came back as a path. procstat
        // prints nothing for a process the user may not inspect, and `-` for
        // a path the kernel cannot name; neither counts as inspected.
        let mut inspected: HashSet<u32> = HashSet::new();
        for line in out.stdout.split(|b| *b == b'\n') {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let Some(row) = parse(line) else {
                return unavailable(
                    BACKEND,
                    "parse_error",
                    format!(
                        "procstat printed a line residue cannot read: {}",
                        String::from_utf8_lossy(line).trim()
                    ),
                );
            };
            if row.kind != b"v" || row.path.first() != Some(&b'/') {
                continue;
            }
            if row.fd == b"cwd" {
                inspected.insert(row.pid);
            }
            uses.push(Use {
                pid: row.pid,
                command: row.command,
                path: row.path.to_vec(),
            });
        }
        Snapshot {
            backend: BACKEND,
            state: Ok(Seen {
                uses,
                inspected: inspected.len(),
                total: count_processes(),
                limits: None,
            }),
        }
    }

    struct Row<'a> {
        pid: u32,
        command: String,
        fd: &'a [u8],
        kind: &'a [u8],
        path: &'a [u8],
    }

    /// One line of `procstat -h -f`: PID COMM FD T V FLAGS REF OFFSET PRO
    /// NAME. COMM can contain spaces, so the FD column is the first one
    /// followed by the shape of T, V, FLAGS, REF and OFFSET. `None` when no
    /// column has that shape, so an unexpected format fails the whole probe
    /// instead of hiding a process.
    fn parse(line: &[u8]) -> Option<Row<'_>> {
        let cols = columns(line);
        let col = |i: usize| &line[cols[i].0..cols[i].1];
        let pid = std::str::from_utf8(col(0)).ok()?.parse::<u32>().ok()?;
        for i in 2..cols.len().saturating_sub(5) {
            let fd = col(i);
            let fd_ok = (!fd.is_empty() && fd.iter().all(u8::is_ascii_digit))
                || matches!(
                    fd,
                    b"cwd" | b"text" | b"ctty" | b"root" | b"jail" | b"trace"
                );
            let flags = col(i + 3);
            let shaped = fd_ok
                && col(i + 1).len() == 1
                && col(i + 2).len() == 1
                && flags.len() == 8
                && flags.iter().all(|c| b"rwasfndl-".contains(c))
                && dash_or_number(col(i + 4))
                && dash_or_number(col(i + 5));
            if !shaped {
                continue;
            }
            let kind = col(i + 1);
            // For a vnode the PRO column is `-` and NAME is the rest of the
            // line, padded on the right.
            let path = if kind == b"v" && cols.len() > i + 7 && col(i + 6) == b"-" {
                line[cols[i + 7].0..].trim_ascii_end()
            } else {
                &[]
            };
            return Some(Row {
                pid,
                command: String::from_utf8_lossy(&line[cols[1].0..cols[i - 1].1]).into_owned(),
                fd,
                kind,
                path,
            });
        }
        None
    }

    fn dash_or_number(s: &[u8]) -> bool {
        s == b"-" || (!s.is_empty() && s.iter().all(u8::is_ascii_digit))
    }

    /// The byte ranges of the whitespace-separated columns of `line`.
    fn columns(line: &[u8]) -> Vec<(usize, usize)> {
        let mut cols = Vec::new();
        let mut start = None;
        for (i, b) in line.iter().enumerate() {
            match (b.is_ascii_whitespace(), start) {
                (false, None) => start = Some(i),
                (true, Some(s)) => {
                    cols.push((s, i));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            cols.push((s, line.len()));
        }
        cols
    }

    /// The number of running processes, to state how much procstat could see.
    fn count_processes() -> Option<usize> {
        let mut cmd = Command::new("/bin/ps");
        cmd.args(["-A", "-o", "pid="]);
        let out = run::run(&mut cmd, Duration::from_secs(10), 16 << 20).ok()?;
        out.status.success().then(|| {
            out.stdout
                .split(|b| *b == b'\n')
                .filter(|l| l.iter().any(u8::is_ascii_digit))
                .count()
        })
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
}
