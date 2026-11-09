//! Which processes run `git worktree remove` of a path, and when a process
//! started.
//!
//! A removal record without a `hold` line comes from a release that takes
//! no hold, 0.0.10 and earlier. Its run, killed alone, can leave the `git
//! worktree remove` it started still deleting, and no hold shows that Git;
//! its arguments do, since they name the worktree ([`removing`]). They are
//! read as the kernel keeps them, never through `ps`, which escapes bytes
//! it cannot print on the BSDs and cuts them to 80 bytes on illumos.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The processes other than this one whose arguments hold `worktree`, then
/// `remove`, then one of `paths` (a trailing `/` aside): a `git worktree
/// remove` of one of them, or a program that runs it with those arguments.
/// A process whose arguments residue may not read (another user's, or one
/// that exited since the listing) is skipped. `Err` when residue cannot
/// list the processes, or reads the arguments of none, this one among
/// them, so that it cannot tell.
pub fn removing(paths: &[&Path]) -> Result<Vec<u32>, String> {
    let wanted: Vec<&[u8]> = paths
        .iter()
        .map(|p| trim(p.as_os_str().as_bytes()))
        .collect();
    let me = std::process::id();
    let mut reader = platform::Reader::new()?;
    let mut read = 0usize;
    let mut found = Vec::new();
    for pid in platform::pids()? {
        let Some(args) = reader.args(pid) else {
            continue;
        };
        read += 1;
        if pid != me && names_removal(&args, &wanted) {
            found.push(pid);
        }
    }
    if read == 0 {
        return Err("residue could read the arguments of no process".into());
    }
    found.sort_unstable();
    Ok(found)
}

/// When process `pid` started, in Unix seconds, where the kernel says so
/// cheaply: macOS, Linux, FreeBSD and illumos. `None` elsewhere, or when it
/// cannot be read.
pub fn started(pid: u32) -> Option<i64> {
    platform::started(pid)
}

/// `args` hold `worktree` then `remove`, and after them one of `paths`.
fn names_removal(args: &[Vec<u8>], paths: &[&[u8]]) -> bool {
    args.windows(2).enumerate().any(|(at, pair)| {
        pair[0] == b"worktree"
            && pair[1] == b"remove"
            && args[at + 2..].iter().any(|a| paths.contains(&trim(a)))
    })
}

/// A path's bytes without trailing `/`, unless it is `/` itself.
fn trim(path: &[u8]) -> &[u8] {
    let mut end = path.len();
    while end > 1 && path[end - 1] == b'/' {
        end -= 1;
    }
    &path[..end]
}

/// NUL-separated strings, the last NUL optional, as one vector each.
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    test
))]
fn split_nul(bytes: &[u8]) -> Vec<Vec<u8>> {
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes.split(|b| *b == 0).map(<[u8]>::to_vec).collect()
}

/// macOS's `KERN_PROCARGS2` layout: `argc` as a native int, the executable
/// path and its terminating NUL, NUL padding, then `argc` NUL-terminated
/// arguments and the environment (`sysctl_procargsx` in xnu's
/// `bsd/kern/kern_sysctl.c`). `None` when it does not hold `argc` strings.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<Vec<u8>>> {
    let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
    let rest = &buf[4..];
    let path_end = rest.iter().position(|b| *b == 0)?;
    let mut at = path_end;
    while rest.get(at) == Some(&0) {
        at += 1;
    }
    let mut args = Vec::with_capacity(argc.min(4096));
    for _ in 0..argc {
        let tail = rest.get(at..)?;
        let end = tail.iter().position(|b| *b == 0)?;
        args.push(tail[..end].to_vec());
        at += end + 1;
    }
    Some(args)
}

/// The process IDs `/proc` lists.
#[cfg(any(target_os = "linux", target_os = "illumos"))]
fn proc_pids() -> Result<Vec<u32>, String> {
    let listing = std::fs::read_dir("/proc").map_err(|e| format!("/proc: {e}"))?;
    let mut pids = Vec::new();
    for item in listing {
        let item = item.map_err(|e| format!("/proc: {e}"))?;
        if let Some(pid) = item.file_name().to_str().and_then(|n| n.parse().ok()) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// The process IDs the base system's `ps` lists. DragonFly lists kernel
/// threads with PID -1; they are not processes and are skipped.
#[cfg(any(
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn ps_pids() -> Result<Vec<u32>, String> {
    use crate::run::{self, RunError};
    use std::process::Command;
    use std::time::Duration;

    let mut cmd = Command::new("/bin/ps");
    cmd.args(["-A", "-o", "pid="]).env("LC_ALL", "C");
    let out = match run::run(&mut cmd, Duration::from_secs(30), 16 << 20) {
        Ok(out) => out,
        Err(RunError::NotFound) => return Err("/bin/ps not found".into()),
        Err(e) => return Err(format!("ps: {e}")),
    };
    if !out.status.success() {
        return Err(format!("ps exited with {}", out.status));
    }
    let mut pids = Vec::new();
    for line in out.stdout.split(|b| *b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() || line == b"-1" {
            continue;
        }
        let pid = std::str::from_utf8(line)
            .ok()
            .and_then(|p| p.parse().ok())
            .ok_or_else(|| {
                format!(
                    "ps printed a line residue cannot read: {}",
                    String::from_utf8_lossy(line)
                )
            })?;
        pids.push(pid);
    }
    Ok(pids)
}

/// One sysctl read into `buf`, grown on ENOMEM up to 64 MiB: the bytes the
/// kernel wrote, or `None` when it refused (no such process, or one the
/// caller may not inspect).
#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn sysctl<'a>(mib: &[libc::c_int], buf: &'a mut Vec<u8>) -> Option<&'a [u8]> {
    let namelen = libc::c_uint::try_from(mib.len()).ok()?;
    loop {
        let mut len: libc::size_t = buf.len();
        // SAFETY: `mib` holds `namelen` ints; `buf` is writable for `len`
        // bytes and the kernel writes at most `len`; no new value is set.
        let rc = unsafe {
            libc::sysctl(
                // macOS declares the name mutable; no kernel writes it.
                mib.as_ptr().cast_mut(),
                namelen,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            return buf.get(..len);
        }
        let grow = std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOMEM)
            && buf.len() < 64 << 20;
        if !grow {
            return None;
        }
        let larger = buf.len() * 2;
        buf.resize(larger, 0);
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{parse_procargs2, sysctl};

    pub struct Reader {
        buf: Vec<u8>,
    }

    impl Reader {
        /// A buffer of `kern.argmax` bytes, the most `KERN_PROCARGS2`
        /// returns.
        pub fn new() -> Result<Reader, String> {
            let mut buf = vec![0u8; 4];
            let max = sysctl(&[libc::CTL_KERN, libc::KERN_ARGMAX], &mut buf)
                .and_then(|b| b.try_into().ok())
                .map(i32::from_ne_bytes)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or("the kernel did not say how long a process's arguments may be")?;
            Ok(Reader {
                buf: vec![0u8; max],
            })
        }

        pub fn args(&mut self, pid: u32) -> Option<Vec<Vec<u8>>> {
            let pid = libc::c_int::try_from(pid).ok()?;
            parse_procargs2(sysctl(
                &[libc::CTL_KERN, libc::KERN_PROCARGS2, pid],
                &mut self.buf,
            )?)
        }
    }

    /// Every PID, from `proc_listallpids`, which writes at most the number
    /// of processes plus 20 and returns how many it wrote; a full buffer
    /// may have been cut short, so it is asked again, larger.
    pub fn pids() -> Result<Vec<u32>, String> {
        // SAFETY: a null buffer of size 0 asks only for the number of pids
        // to make room for; nothing is written.
        let mut room = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        for _ in 0..4 {
            let len = usize::try_from(room)
                .ok()
                .and_then(|r| r.checked_add(64))
                .ok_or("proc_listallpids failed")?;
            let mut pids: Vec<libc::c_int> = vec![0; len];
            let bytes = libc::c_int::try_from(len * std::mem::size_of::<libc::c_int>())
                .map_err(|_| "too many processes to list")?;
            // SAFETY: `pids` is writable for `bytes` bytes, and the kernel
            // writes at most `bytes`.
            let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            let n = usize::try_from(n).map_err(|_| "proc_listallpids failed")?;
            if n < len {
                pids.truncate(n);
                return Ok(pids
                    .into_iter()
                    .filter_map(|p| u32::try_from(p).ok())
                    .collect());
            }
            room = libc::c_int::try_from(len * 2).map_err(|_| "too many processes to list")?;
        }
        Err("the process list kept growing while residue read it".into())
    }

    pub fn started(pid: u32) -> Option<i64> {
        let id = libc::c_int::try_from(pid).ok()?;
        // SAFETY: a plain C struct of integers and byte arrays, for which
        // all zeros is a valid value.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `info` is writable for `size` bytes, and the kernel
        // writes at most `size`.
        let n = unsafe {
            libc::proc_pidinfo(id, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        if n != size || info.pbi_pid != pid {
            return None;
        }
        i64::try_from(info.pbi_start_tvsec).ok()
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::split_nul;
    use std::fs;

    pub struct Reader;

    impl Reader {
        pub fn new() -> Result<Reader, String> {
            Ok(Reader)
        }

        /// `/proc/PID/cmdline`, which any user may read.
        pub fn args(&mut self, pid: u32) -> Option<Vec<Vec<u8>>> {
            fs::read(format!("/proc/{pid}/cmdline"))
                .ok()
                .map(|b| split_nul(&b))
        }
    }

    pub fn pids() -> Result<Vec<u32>, String> {
        super::proc_pids()
    }

    /// `starttime`, the 22nd field of `/proc/PID/stat`, in clock ticks
    /// after boot, plus `btime` from `/proc/stat` (proc(5)). The command
    /// in the second field is in parentheses and may hold spaces or
    /// parentheses, so the fields are counted after the last `)`.
    pub fn started(pid: u32) -> Option<i64> {
        let stat = fs::read(format!("/proc/{pid}/stat")).ok()?;
        let close = stat.iter().rposition(|b| *b == b')')?;
        let ticks: u64 = stat[close + 1..]
            .split(u8::is_ascii_whitespace)
            .filter(|f| !f.is_empty())
            .nth(19)
            .and_then(|f| std::str::from_utf8(f).ok())
            .and_then(|f| f.parse().ok())?;
        // SAFETY: sysconf only reads a system constant.
        let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
        let boot: i64 = fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("btime "))
            .and_then(|b| b.trim().parse().ok())?;
        boot.checked_add(i64::try_from(ticks.checked_div(hz)?).ok()?)
    }
}

/// FreeBSD and DragonFly: `kern.proc.args.PID`, the arguments NUL-separated
/// (sysctl(3)).
#[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
mod platform {
    use super::{split_nul, sysctl};

    pub struct Reader {
        buf: Vec<u8>,
    }

    impl Reader {
        pub fn new() -> Result<Reader, String> {
            Ok(Reader {
                buf: vec![0u8; 256 << 10],
            })
        }

        pub fn args(&mut self, pid: u32) -> Option<Vec<Vec<u8>>> {
            let pid = libc::c_int::try_from(pid).ok()?;
            let mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ARGS, pid];
            sysctl(&mib, &mut self.buf).map(split_nul)
        }
    }

    pub fn pids() -> Result<Vec<u32>, String> {
        super::ps_pids()
    }

    /// `ki_start` of the process's `kinfo_proc` (`kern.proc.pid.PID`).
    /// The kernel answers ENOMEM, and this `None`, when its `kinfo_proc`
    /// is larger than the one residue was built with.
    #[cfg(target_os = "freebsd")]
    pub fn started(pid: u32) -> Option<i64> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
        // SAFETY: a plain C struct of integers, pointers and byte arrays,
        // for which all zeros is a valid value.
        let mut info: libc::kinfo_proc = unsafe { std::mem::zeroed() };
        let mut len: libc::size_t = std::mem::size_of::<libc::kinfo_proc>();
        // SAFETY: `info` is writable for `len` bytes and the kernel writes
        // at most `len`; no new value is set.
        let rc = unsafe {
            libc::sysctl(
                mib.as_ptr(),
                4,
                (&raw mut info).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len != std::mem::size_of::<libc::kinfo_proc>() || info.ki_pid != pid {
            return None;
        }
        Some(info.ki_start.tv_sec)
    }

    #[cfg(target_os = "dragonfly")]
    pub fn started(_pid: u32) -> Option<i64> {
        None
    }
}

/// NetBSD: `kern.proc_args.PID.argv`, the arguments NUL-separated
/// (sysctl(7)).
#[cfg(target_os = "netbsd")]
mod platform {
    use super::{split_nul, sysctl};

    pub struct Reader {
        buf: Vec<u8>,
    }

    impl Reader {
        pub fn new() -> Result<Reader, String> {
            Ok(Reader {
                buf: vec![0u8; 256 << 10],
            })
        }

        pub fn args(&mut self, pid: u32) -> Option<Vec<Vec<u8>>> {
            let pid = libc::c_int::try_from(pid).ok()?;
            let mib = [
                libc::CTL_KERN,
                libc::KERN_PROC_ARGS,
                pid,
                libc::KERN_PROC_ARGV,
            ];
            sysctl(&mib, &mut self.buf).map(split_nul)
        }
    }

    pub fn pids() -> Result<Vec<u32>, String> {
        super::ps_pids()
    }

    pub fn started(_pid: u32) -> Option<i64> {
        None
    }
}

/// OpenBSD: `kern.proc_args.PID.argv`, which writes a NULL-terminated array
/// of pointers into the caller's own buffer, then the strings they point at
/// (`sysctl_proc_args` in `sys/kern/kern_sysctl.c`).
#[cfg(target_os = "openbsd")]
mod platform {
    use super::sysctl;

    pub struct Reader {
        buf: Vec<u8>,
    }

    impl Reader {
        pub fn new() -> Result<Reader, String> {
            Ok(Reader {
                buf: vec![0u8; 256 << 10],
            })
        }

        pub fn args(&mut self, pid: u32) -> Option<Vec<Vec<u8>>> {
            let pid = libc::c_int::try_from(pid).ok()?;
            let mib = [
                libc::CTL_KERN,
                libc::KERN_PROC_ARGS,
                pid,
                libc::KERN_PROC_ARGV,
            ];
            let bytes = sysctl(&mib, &mut self.buf)?;
            // The pointers name the buffer of the read that succeeded, the
            // one `bytes` is, even when it grew and moved on the way.
            let base = bytes.as_ptr() as usize;
            let width = std::mem::size_of::<usize>();
            let mut args = Vec::new();
            for slot in bytes.chunks_exact(width) {
                let mut word = [0u8; std::mem::size_of::<usize>()];
                word.copy_from_slice(slot);
                let at = usize::from_ne_bytes(word);
                if at == 0 {
                    return Some(args);
                }
                let tail = bytes.get(at.checked_sub(base)?..)?;
                let end = tail.iter().position(|b| *b == 0)?;
                args.push(tail[..end].to_vec());
            }
            None
        }
    }

    pub fn pids() -> Result<Vec<u32>, String> {
        super::ps_pids()
    }

    pub fn started(_pid: u32) -> Option<i64> {
        None
    }
}

/// illumos: `pr_argc` and `pr_argv` from `/proc/PID/psinfo`, then the
/// argument vector and its strings from the process's address space,
/// `/proc/PID/as`, as pargs(1) reads them (proc(5)). `pr_psargs` holds
/// only the first 80 bytes.
#[cfg(target_os = "illumos")]
mod platform {
    use std::fs::{self, File};
    use std::os::unix::fs::FileExt;

    /// Where these sit in a 64-bit `psinfo_t` (`<sys/procfs.h>`).
    const PSINFO_PID: usize = 8;
    const PSINFO_START: usize = 88;
    const PSINFO_ARGC: usize = 236;
    const PSINFO_ARGV: usize = 240;
    const PSINFO_DMODEL: usize = 256;
    const PR_MODEL_ILP32: u8 = 1;
    const PR_MODEL_LP64: u8 = 2;
    /// More than any process's arguments may hold (`ARG_MAX` is 2 MiB).
    const MOST: usize = 4 << 20;

    pub struct Reader {
        page: u64,
    }

    impl Reader {
        pub fn new() -> Result<Reader, String> {
            // SAFETY: sysconf only reads a system constant.
            let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
                .ok()
                .filter(|p| *p > 0)
                .ok_or("the system did not say its page size")?;
            Ok(Reader { page })
        }

        pub fn args(&mut self, pid: u32) -> Option<Vec<Vec<u8>>> {
            let info = psinfo(pid)?;
            let argc = usize::try_from(i32::from_ne_bytes(
                info.get(PSINFO_ARGC..PSINFO_ARGC + 4)?.try_into().ok()?,
            ))
            .ok()?;
            let argv = u64::from_ne_bytes(info.get(PSINFO_ARGV..PSINFO_ARGV + 8)?.try_into().ok()?);
            let width = match *info.get(PSINFO_DMODEL)? {
                PR_MODEL_ILP32 => 4,
                PR_MODEL_LP64 => 8,
                _ => return None,
            };
            if argc.checked_mul(width)? > MOST {
                return None;
            }
            let space = File::open(format!("/proc/{pid}/as")).ok()?;
            let mut vector = vec![0u8; argc * width];
            space.read_exact_at(&mut vector, argv).ok()?;
            let mut total = 0;
            let mut args = Vec::with_capacity(argc);
            for slot in vector.chunks_exact(width) {
                let at = if width == 4 {
                    u64::from(u32::from_ne_bytes(slot.try_into().ok()?))
                } else {
                    u64::from_ne_bytes(slot.try_into().ok()?)
                };
                let arg = self.string(&space, at, MOST - total)?;
                total += arg.len() + 1;
                args.push(arg);
            }
            Some(args)
        }

        /// The NUL-terminated string at `at`, read a page at a time, since
        /// a read that crosses into an unmapped page fails.
        fn string(&self, space: &File, mut at: u64, most: usize) -> Option<Vec<u8>> {
            let mut out = Vec::new();
            let mut chunk = vec![0u8; usize::try_from(self.page).ok()?];
            loop {
                let room = usize::try_from(self.page - at % self.page).ok()?;
                let n = space.read_at(&mut chunk[..room], at).ok()?;
                if n == 0 {
                    return None;
                }
                if let Some(end) = chunk[..n].iter().position(|b| *b == 0) {
                    out.extend_from_slice(&chunk[..end]);
                    return Some(out);
                }
                out.extend_from_slice(&chunk[..n]);
                if out.len() > most {
                    return None;
                }
                at = at.checked_add(u64::try_from(n).ok()?)?;
            }
        }
    }

    /// The process's `psinfo_t`, which any user may read, once its
    /// `pr_pid` is this process: one that is not means the layout above
    /// is wrong.
    fn psinfo(pid: u32) -> Option<Vec<u8>> {
        let info = fs::read(format!("/proc/{pid}/psinfo")).ok()?;
        let at = u32::from_ne_bytes(info.get(PSINFO_PID..PSINFO_PID + 4)?.try_into().ok()?);
        (at == pid).then_some(info)
    }

    pub fn pids() -> Result<Vec<u32>, String> {
        super::proc_pids()
    }

    /// `pr_start.tv_sec`.
    pub fn started(pid: u32) -> Option<i64> {
        let info = psinfo(pid)?;
        Some(i64::from_ne_bytes(
            info.get(PSINFO_START..PSINFO_START + 8)?.try_into().ok()?,
        ))
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
    pub struct Reader;

    impl Reader {
        pub fn new() -> Result<Reader, String> {
            Err("residue cannot read the arguments of processes on this operating system".into())
        }

        pub fn args(&mut self, _pid: u32) -> Option<Vec<Vec<u8>>> {
            None
        }
    }

    pub fn pids() -> Result<Vec<u32>, String> {
        Err("residue cannot list processes on this operating system".into())
    }

    pub fn started(_pid: u32) -> Option<i64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<Vec<u8>> {
        list.iter().map(|a| a.as_bytes().to_vec()).collect()
    }

    #[test]
    fn a_git_worktree_remove_names_its_path_after_the_subcommand() {
        let paths: &[&[u8]] = &[b"/r/.w.residue-removing-7"];
        let git = args(&[
            "git",
            "-C",
            "/r/main",
            "-c",
            "core.hooksPath=/dev/null",
            "worktree",
            "remove",
            "--force",
            "/r/.w.residue-removing-7/",
        ]);
        assert!(names_removal(&git, paths));
        // A script that runs Git with the same arguments counts too.
        let mut wrapped = args(&["/bin/sh", "/x/bin/git"]);
        wrapped.extend(git[1..].iter().cloned());
        assert!(names_removal(&wrapped, paths));
        // The path before the subcommand, another path after it, or no
        // `remove` does not.
        assert!(!names_removal(
            &args(&[
                "git",
                "-C",
                "/r/.w.residue-removing-7",
                "worktree",
                "remove",
                "/r/x"
            ]),
            paths
        ));
        assert!(!names_removal(
            &args(&["git", "worktree", "list", "/r/.w.residue-removing-7"]),
            paths
        ));
        assert!(!names_removal(
            &args(&["git", "worktree", "remove", "/r/.w.residue-removing-70"]),
            paths
        ));
    }

    #[test]
    fn nul_separated_arguments_keep_empty_ones_and_drop_the_last_nul() {
        assert_eq!(
            split_nul(b"git\0worktree\0\0x\0"),
            args(&["git", "worktree", "", "x"])
        );
        assert_eq!(split_nul(b"git\0x"), args(&["git", "x"]));
        assert!(split_nul(b"").is_empty());
    }

    #[test]
    fn procargs2_skips_the_executable_and_its_padding() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/bin/git\0\0\0\0git\0worktree\0remove\0HOME=/h\0");
        assert_eq!(
            parse_procargs2(&buf),
            Some(args(&["git", "worktree", "remove"]))
        );
        // Fewer strings than argc says: not a vector residue can trust.
        let mut short = 5i32.to_ne_bytes().to_vec();
        short.extend_from_slice(b"/usr/bin/git\0git\0x\0");
        assert_eq!(parse_procargs2(&short), None);
    }

    #[test]
    fn this_process_has_readable_arguments() {
        let mut reader = platform::Reader::new().expect("a reader");
        let mine = reader
            .args(std::process::id())
            .expect("this process's arguments");
        assert!(!mine.is_empty());
        assert!(removing(&[Path::new("/nonexistent/residue-test")]).is_ok_and(|p| p.is_empty()));
    }
}
