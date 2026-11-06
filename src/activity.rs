//! Is any process using this directory?
//!
//! One snapshot of process working directories and open files is taken per
//! command and matched against each worktree. The answer has three states,
//! never two: a process was seen using the directory (`in_use`), none of
//! the processes the probe could inspect was (`not_observed`), or the probe
//! failed and nothing is known (`unknown`). `not_observed` is not proof of
//! inactivity: processes owned by other users, in containers or on other
//! machines are outside what a same-user probe can see, and a process can
//! open the directory a moment later.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Clone)]
pub struct Unavailable {
    /// Stable reason code: `probe_missing`, `timeout`, `probe_failed`,
    /// `parse_error` or `unsupported`.
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
        if hits.is_empty() {
            Evidence::NotObserved
        } else {
            hits.sort();
            Evidence::InUse(hits)
        }
    }

    /// What a `not_observed` answer covers, in words.
    pub fn scope(&self) -> String {
        match &self.state {
            Ok(seen) => match seen.total {
                Some(total) => format!(
                    "{} inspected {} of {} running processes",
                    self.backend, seen.inspected, total
                ),
                None => format!("{} inspected {} processes", self.backend, seen.inspected),
            },
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
        // pid, command and name; lsof adds the fd field on its own.
        cmd.args(["-n", "-P", "-F", "pcn"]).env("LC_ALL", "C");
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
                    pid = Some(p);
                    command.clear();
                    inspected += 1;
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
                _ => {}
            }
        }
        Snapshot {
            backend: BACKEND,
            state: Ok(Seen {
                uses,
                inspected,
                total: count_processes(),
            }),
        }
    }

    /// The number of running processes, to state how much lsof could see.
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
            }),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
