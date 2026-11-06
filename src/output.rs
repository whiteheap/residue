//! Rendering: terminal text and JSON documents.
//!
//! Every string that came from the filesystem, Git or another process is
//! passed through [`clean`] before it reaches a terminal, so a crafted file
//! or branch name cannot inject escape sequences.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::activity::{Evidence, Snapshot};
use crate::fsinfo::{Space, Usage};
use crate::inspect::{Report, Status, Verdict};
use crate::json::Json;
use crate::policy::Policy;

pub const SCHEMA_SCAN: &str = "residue.scan/v1";
pub const SCHEMA_EXPLAIN: &str = "residue.explain/v1";
pub const SCHEMA_REMOVE: &str = "residue.remove/v1";

pub struct Style {
    on: bool,
}

impl Style {
    pub fn stdout() -> Style {
        Style::detect(std::io::stdout().is_terminal())
    }

    pub fn stderr() -> Style {
        Style::detect(std::io::stderr().is_terminal())
    }

    fn detect(terminal: bool) -> Style {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let dumb = std::env::var_os("TERM").is_some_and(|t| t == "dumb");
        Style {
            on: terminal && !no_color && !dumb,
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn verdict(&self, verdict: Verdict, width: usize) -> String {
        let word = format!("{:<width$}", verdict.code());
        match verdict {
            Verdict::Candidate => self.paint("1;36", &word),
            Verdict::Keep => word,
            Verdict::Unknown => self.paint("33", &word),
            Verdict::Stale => self.paint("2", &word),
        }
    }

    pub fn status(&self, status: Status) -> String {
        let word = format!("{:<8}", status.code());
        match status {
            Status::Pass => self.paint("2", &word),
            Status::Block => self.paint("31", &word),
            Status::Unknown => self.paint("33", &word),
        }
    }
}

/// Replace control characters (C0, DEL, C1) with visible escapes.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

pub struct Places {
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl Places {
    pub fn new() -> Places {
        Places {
            cwd: std::env::current_dir()
                .ok()
                .and_then(|d| std::fs::canonicalize(d).ok()),
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .and_then(|h| std::fs::canonicalize(h).ok()),
        }
    }

    /// Relative to the current directory when below it, so the text can be
    /// pasted into `residue remove`; otherwise absolute, with `~` for home.
    pub fn show(&self, path: &Path) -> String {
        if let Some(cwd) = &self.cwd {
            if let Ok(rel) = path.strip_prefix(cwd) {
                if !rel.as_os_str().is_empty() {
                    return clean(&rel.to_string_lossy());
                }
            }
        }
        if let Some(home) = &self.home {
            if let Ok(rel) = path.strip_prefix(home) {
                return clean(&format!("~/{}", rel.to_string_lossy()));
            }
        }
        clean(&path.to_string_lossy())
    }
}

pub fn bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn count(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.0}K", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn age(seconds: i64) -> String {
    let s = seconds.max(0);
    match s {
        0..=119 => "just now".into(),
        120..=7_199 => format!("{} minutes ago", s / 60),
        7_200..=172_799 => format!("{} hours ago", s / 3_600),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// UTC civil date and seconds into the day, from Unix seconds (Howard
/// Hinnant's days-from-civil inverse).
fn civil(seconds: i64) -> (i64, i64, i64, i64) {
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, rem)
}

/// `2026-11-06 21:04 UTC`
pub fn date(seconds: i64) -> String {
    let (year, month, day, rem) = civil(seconds);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3_600,
        (rem % 3_600) / 60
    )
}

/// RFC 3339 in UTC: `2026-11-06T21:04:05Z`.
pub fn rfc3339(seconds: i64) -> String {
    let (year, month, day, rem) = civil(seconds);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// `HH:MM:SS` in the local time zone. If the C library cannot convert the
/// time, the clock is shown in UTC and says so.
pub fn clock(seconds: i64) -> String {
    let utc = || {
        let rem = seconds.rem_euclid(86_400);
        format!(
            "{:02}:{:02}:{:02} UTC",
            rem / 3_600,
            (rem % 3_600) / 60,
            rem % 60
        )
    };
    // `time_t` is inferred from localtime_r's signature rather than named:
    // the libc crate deprecates the name on musl while its width changes.
    // It is i64 on every target residue ships, so the cast is the identity.
    let t = seconds as _;
    // SAFETY: localtime_r reads `t` and writes into the zeroed struct we
    // own; both outlive the call. It is the reentrant form, safe across the
    // threads residue runs.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return utc();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// The facts that never block, one short phrase each, for the scan table.
pub fn fact_phrases(r: &Report) -> Vec<String> {
    let mut out = Vec::new();
    match r.entry.short_branch() {
        Some(_) => out.push("branch kept".to_string()),
        None => out.push("detached".to_string()),
    }
    match &r.local_commits {
        Ok(Some(0)) => out.push("all commits on a remote".into()),
        Ok(Some(n)) => out.push(format!(
            "{n} local-only commit{}",
            if *n == 1 { "" } else { "s" }
        )),
        Ok(None) => out.push("no remote-tracking refs".into()),
        Err(_) => out.push("local commits unknown".into()),
    }
    if let Ok(Some(up)) = &r.upstream {
        if up.gone {
            out.push("upstream gone".into());
        }
    }
    if let Some(m) = r.usage.as_ref().and_then(|u| u.newest_mtime) {
        out.push(format!("changed {}", age(now() - m)));
    }
    out
}

pub fn usage_phrase(u: &Usage) -> String {
    format!(
        "{} allocated, {} apparent, about {} reclaimable",
        bytes(u.allocated),
        bytes(u.apparent),
        bytes(u.reclaimable)
    )
}

pub fn space_phrase(s: &Space) -> String {
    let pct = if s.total > 0 {
        (s.free as f64 / s.total as f64) * 100.0
    } else {
        0.0
    };
    let mut text = format!("{} free of {} ({pct:.0}%)", bytes(s.free), bytes(s.total));
    if s.total_inodes > 0 {
        text.push_str(&format!(
            ", {} of {} inodes free",
            count(s.free_inodes),
            count(s.total_inodes)
        ));
    }
    text
}

// ---------------------------------------------------------------- JSON

pub fn activity_json(s: &Snapshot) -> Json {
    match &s.state {
        Ok(seen) => Json::Obj(vec![
            ("backend", Json::str(s.backend)),
            ("available", Json::Bool(true)),
            ("inspected_processes", Json::UInt(seen.inspected as u64)),
            (
                "running_processes",
                Json::opt(seen.total, |t| Json::UInt(t as u64)),
            ),
            ("reason", Json::Null),
            ("detail", Json::Null),
        ]),
        Err(u) => Json::Obj(vec![
            ("backend", Json::str(s.backend)),
            ("available", Json::Bool(false)),
            ("inspected_processes", Json::Null),
            ("running_processes", Json::Null),
            ("reason", Json::str(u.reason)),
            ("detail", Json::str(u.detail.clone())),
        ]),
    }
}

fn evidence_json(e: &Evidence) -> Json {
    let processes = match e {
        Evidence::InUse(procs) => Json::Arr(
            procs
                .iter()
                .map(|(pid, cmd)| {
                    Json::Obj(vec![
                        ("pid", Json::UInt(u64::from(*pid))),
                        ("command", Json::str(cmd.clone())),
                    ])
                })
                .collect(),
        ),
        _ => Json::Arr(Vec::new()),
    };
    let (reason, detail) = match e {
        Evidence::Unknown(u) => (Json::str(u.reason), Json::str(u.detail.clone())),
        _ => (Json::Null, Json::Null),
    };
    Json::Obj(vec![
        ("state", Json::str(e.code())),
        ("processes", processes),
        ("reason", reason),
        ("detail", detail),
    ])
}

fn usage_json(u: &Usage) -> Json {
    Json::Obj(vec![
        ("apparent_bytes", Json::UInt(u.apparent)),
        ("allocated_bytes", Json::UInt(u.allocated)),
        ("reclaimable_bytes", Json::UInt(u.reclaimable)),
        ("entries", Json::UInt(u.entries)),
        ("unreadable_entries", Json::UInt(u.unreadable)),
        ("newest_mtime", Json::opt(u.newest_mtime, Json::Int)),
    ])
}

pub fn space_json(s: &Space) -> Json {
    Json::Obj(vec![
        ("free_bytes", Json::UInt(s.free)),
        ("total_bytes", Json::UInt(s.total)),
        ("free_inodes", Json::UInt(s.free_inodes)),
        ("total_inodes", Json::UInt(s.total_inodes)),
    ])
}

pub fn report_json(r: &Report) -> Json {
    let e = &r.entry;
    let (local_commits, local_commits_error) = match &r.local_commits {
        Ok(n) => (Json::opt(*n, Json::UInt), Json::Null),
        Err(err) => (Json::Null, Json::str(err.clone())),
    };
    let (upstream, upstream_gone, upstream_error) = match &r.upstream {
        Ok(Some(u)) => (Json::str(u.name.clone()), Json::Bool(u.gone), Json::Null),
        Ok(None) => (Json::Null, Json::Null, Json::Null),
        Err(err) => (Json::Null, Json::Null, Json::str(err.clone())),
    };
    Json::Obj(vec![
        ("path", Json::path(&e.path)),
        (
            "canonical_path",
            Json::opt(r.canonical.as_deref(), Json::path),
        ),
        ("verdict", Json::str(r.verdict.code())),
        ("stale_reason", Json::opt(r.stale_reason.clone(), Json::Str)),
        ("head", Json::opt(e.head.clone(), Json::Str)),
        ("branch", Json::opt(e.branch.clone(), Json::Str)),
        ("detached", Json::Bool(e.detached)),
        ("locked", Json::Bool(e.locked.is_some())),
        (
            "lock_reason",
            Json::opt(e.locked.clone().filter(|l| !l.is_empty()), Json::Str),
        ),
        (
            "checks",
            Json::Arr(
                r.checks
                    .iter()
                    .map(|c| {
                        Json::Obj(vec![
                            ("id", Json::str(c.id)),
                            ("status", Json::str(c.status.code())),
                            ("detail", Json::str(c.detail.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("activity", Json::opt(r.activity.as_ref(), evidence_json)),
        (
            "ignored",
            Json::Arr(
                r.ignored
                    .iter()
                    .map(|i| {
                        Json::Obj(vec![
                            ("path", Json::str(i.path.clone())),
                            ("regenerable", Json::Bool(i.regenerable())),
                            ("files", Json::opt(i.files, Json::UInt)),
                            ("bytes", Json::opt(i.bytes, Json::UInt)),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("local_only_commits", local_commits),
        ("local_only_commits_error", local_commits_error),
        ("upstream", upstream),
        ("upstream_gone", upstream_gone),
        ("upstream_error", upstream_error),
        ("size", Json::opt(r.usage.as_ref(), usage_json)),
    ])
}

// ---------------------------------------------------------------- text

/// The two-line scan entry for one worktree.
pub fn scan_entry(r: &Report, places: &Places, style: &Style) -> String {
    let path = places.show(r.canonical.as_deref().unwrap_or(&r.entry.path));
    let size = r
        .usage
        .as_ref()
        .map_or(String::new(), |u| bytes(u.allocated));
    let head = match (r.entry.short_branch(), r.entry.short_head()) {
        (Some(b), _) => format!("[{}]", clean(b)),
        (None, Some(h)) => format!("(detached {h})"),
        (None, None) => String::new(),
    };
    let notes = match r.verdict {
        Verdict::Stale => format!(
            "{}; only Git metadata remains, `git worktree prune` clears it",
            clean(r.stale_reason.as_deref().unwrap_or("stale"))
        ),
        Verdict::Candidate => fact_phrases(r).join(" · "),
        Verdict::Keep | Verdict::Unknown => {
            let reasons: Vec<String> = r.blocking().map(|c| clean(&c.short)).collect();
            reasons.join(" · ")
        }
    };
    format!(
        "  {} {:>8}  {}  {}\n  {:9} {:>8}  {}\n",
        style.verdict(r.verdict, 9),
        size,
        style.bold(&path),
        style.dim(&head),
        "",
        "",
        style.dim(&notes)
    )
}

/// `ignored_environment_rules`: the `residue.regenerable` values set through
/// the environment, which residue ignores. Empty when there are none; null
/// when Git could not read that environment.
pub fn ignored_environment_json(policy: &Policy) -> Json {
    match &policy.ignored_environment {
        Ok(values) => Json::Arr(values.iter().map(|v| Json::str(v.clone())).collect()),
        Err(_) => Json::Null,
    }
}
