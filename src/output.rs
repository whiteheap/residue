//! Rendering: terminal text and JSON documents.
//!
//! Every string that came from the filesystem, Git or another process is
//! passed through [`clean`] before it reaches a terminal, so a crafted file
//! or branch name cannot inject escape sequences.

use std::io::IsTerminal;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::activity::{Evidence, Snapshot};
use crate::fsinfo::{Space, Usage};
use crate::inspect::{LastChange, Report, Status, Verdict};
use crate::json::Json;
use crate::policy::{Policy, RuleSource};

pub const SCHEMA_SCAN: &str = "residue.scan/v1";
pub const SCHEMA_EXPLAIN: &str = "residue.explain/v1";
pub const SCHEMA_REMOVE: &str = "residue.remove/v1";
pub const SCHEMA_RESTORE: &str = "residue.restore/v1";
pub const SCHEMA_STATS: &str = "residue.stats/v1";

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
                .and_then(|d| std::fs::canonicalize(d).ok())
                // From `/` (where launchd and systemd start a job) a
                // relative path would only drop the leading slash.
                .filter(|d| d.parent().is_some()),
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

/// `1,204,331`: every digit, grouped in threes, for counts a reader compares.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
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

/// Local civil time and the zone's offset from UTC in seconds, or `None`
/// when the C library cannot convert the time.
fn local(seconds: i64) -> Option<(libc::tm, i64)> {
    // `time_t` is inferred from localtime_r's signature rather than named:
    // the libc crate deprecates the name on musl while its width changes.
    // It is i64 on every target residue ships, so the cast is the identity.
    let t = seconds as _;
    // SAFETY: localtime_r reads `t` and writes into the zeroed struct we
    // own; both outlive the call. It is the reentrant form, safe across the
    // threads residue runs.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    let offset = utc_offset(&tm, seconds);
    Some((tm, offset))
}

/// The zone's offset from UTC, in seconds, for the instant `tm` describes.
#[cfg(not(any(target_os = "illumos", target_os = "solaris")))]
fn utc_offset(tm: &libc::tm, _seconds: i64) -> i64 {
    // `c_long`, which is i64 on every 64-bit target residue builds for.
    tm.tm_gmtoff
}

/// illumos and Solaris have no `tm_gmtoff`. Read the local civil time as if
/// it were UTC (Howard Hinnant's days-from-civil); its distance from the real
/// instant is the offset.
#[cfg(any(target_os = "illumos", target_os = "solaris"))]
fn utc_offset(tm: &libc::tm, seconds: i64) -> i64 {
    let month = i64::from(tm.tm_mon) + 1;
    let year = i64::from(tm.tm_year) + 1_900 - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + i64::from(tm.tm_mday) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let local = days * 86_400
        + i64::from(tm.tm_hour) * 3_600
        + i64::from(tm.tm_min) * 60
        + i64::from(tm.tm_sec);
    local - seconds
}

/// `+02:00`, `-05:30`, `+00:00`.
fn offset(seconds: i64) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let s = seconds.abs();
    format!("{sign}{:02}:{:02}", s / 3_600, (s % 3_600) / 60)
}

/// `2026-11-07 00:23 +02:00`: local time with its UTC offset, the same
/// zone the header uses. UTC, and says so, when the local time is unknown.
pub fn date(seconds: i64) -> String {
    match local(seconds) {
        Some((tm, off)) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02} {}",
            i64::from(tm.tm_year) + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            offset(off)
        ),
        None => {
            let (year, month, day, rem) = civil(seconds);
            format!(
                "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
                rem / 3_600,
                (rem % 3_600) / 60
            )
        }
    }
}

/// `2026-11-07`: the local day of [`date`].
pub fn day(seconds: i64) -> String {
    date(seconds).chars().take(10).collect()
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

/// Unix seconds from a [`stamp`], or `None` when `text` is not one.
pub fn unstamp(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 16
        || b[8] != b'T'
        || b[15] != b'Z'
        || !b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
    {
        return None;
    }
    let num = |from: usize, to: usize| -> i64 {
        b[from..to]
            .iter()
            .fold(0, |n, d| n * 10 + i64::from(d - b'0'))
    };
    let (year, month, day) = (num(0, 4), num(4, 6), num(6, 8));
    let (hour, minute, second) = (num(9, 11), num(11, 13), num(13, 15));
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    // Howard Hinnant's days-from-civil, the inverse of `civil`.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// `20261107T040000Z`: UTC, for names that sort by time.
pub fn stamp(seconds: i64) -> String {
    let (year, month, day, rem) = civil(seconds);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Why a stale entry is stale, and what to do about it. A directory that
/// is gone may only be moved, or on a disk that is not mounted, so those
/// come first. residue names no line that clears Git's record of it, as
/// no removal record keeps what that drops (`gc_note`).
pub fn stale_note(r: &Report, main: &Path, common_dir: &Path) -> String {
    let why = clean(r.stale_reason.as_deref().unwrap_or("stale"));
    let git_dir = crate::removed::git_dir_of(common_dir, &[&r.entry.path]);
    let gc = gc_note(main, r.entry.locked.as_deref(), git_dir.as_deref());
    match std::fs::symlink_metadata(&r.entry.path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            format!("{why}; {}; {gc}", missing_note(main))
        }
        // Git marks it prunable also when its directory is there without
        // the `.git` file that names its Git directory.
        _ => match &git_dir {
            Some(dir) => format!(
                "{why}; its directory is still there, and a `.git` file in it that reads `gitdir: {}` reconnects it; {gc}",
                clean(&dir.to_string_lossy())
            ),
            None => format!("{why}; its directory is still there; {gc}"),
        },
    }
}

/// What to try first for a worktree Git lists whose directory is gone.
pub fn missing_note(main: &Path) -> String {
    format!(
        "if you moved it, `git -C {} worktree repair NEWPATH` reconnects it, and if its disk is not mounted, mounting it brings it back",
        shell(main)
    )
}

/// What becomes of Git's record of a worktree that no removal record names
/// and whose directory is gone, or that Git can no longer use. residue
/// names no line that clears it: that drops its HEAD, its reflog, its
/// index and its private refs, and only a removal record keeps those.
/// `git gc` clears it on its own (`git worktree prune --expire`), judged
/// by when its index last changed, and never while it is `locked`.
/// `git_dir` is its Git directory, `COMMON/worktrees/ID`, when residue
/// found the one that names it.
pub fn gc_note(main: &Path, locked: Option<&str>, git_dir: Option<&Path>) -> String {
    if let Some(reason) = locked {
        let reason = if reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", clean(reason))
        };
        return format!(
            "it is locked{reason}, so Git keeps its record, with its HEAD, reflog and index"
        );
    }
    let id = git_dir
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    let reflog = format!("worktrees/{}/HEAD", id.as_deref().unwrap_or("ID"));
    let which = if id.is_some() {
        ""
    } else {
        ", ID being its folder in Git's worktrees directory,"
    };
    format!(
        "`git gc` drops Git's record of it, with its HEAD, reflog and index, 3 months by default (`gc.worktreePruneExpire`) after its index last changed, never while it is locked; `git -C {} reflog show {}`{which} lists the commits that may go with it",
        shell(main),
        word(&reflog)
    )
}

/// What a file that is not a regular file is, as a phrase.
pub fn file_kind(file_type: &std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt;
    if file_type.is_symlink() {
        "a symbolic link"
    } else if file_type.is_dir() {
        "a directory"
    } else if file_type.is_fifo() {
        "a FIFO"
    } else if file_type.is_socket() {
        "a socket"
    } else if file_type.is_block_device() || file_type.is_char_device() {
        "a device"
    } else if file_type.is_file() {
        "a file"
    } else {
        "of a kind Git does not write"
    }
}

/// `test ! -e PATH && test ! -L PATH && `, to lead a line that moves a
/// worktree to PATH by hand: `git worktree move` into a directory that
/// is there moves the worktree inside it, so the line stops unless
/// nothing is at PATH, a dangling symbolic link included.
pub fn vacant(path: &Path) -> String {
    let path = shell(path);
    format!("test ! -e {path} && test ! -L {path} && ")
}

/// A path as a shell word ([`word_bytes`]), byte for byte.
pub fn shell(path: &Path) -> String {
    word_bytes(path.as_os_str().as_bytes())
}

/// Text as a shell word ([`word_bytes`]).
pub fn word(text: &str) -> String {
    word_bytes(text.as_bytes())
}

/// Bytes as a shell word that names them exactly: unchanged when they are
/// only characters no shell treats specially; in single quotes when they
/// are text with no control character; otherwise in `$'...'` (bash,
/// zsh, ksh and POSIX.1-2024 sh), with each control character and each
/// byte that is not UTF-8 as an octal escape, so a newline in a path
/// stays a newline and nothing in the word reaches the terminal raw.
pub fn word_bytes(bytes: &[u8]) -> String {
    let plain = !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"@%_+=:,./-".contains(b));
    if plain {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        if !text.chars().any(char::is_control) {
            return format!("'{}'", text.replace('\'', "'\\''"));
        }
    }
    let mut out = String::from("$'");
    let octal = |out: &mut String, b: u8| out.push_str(&format!("\\{b:03o}"));
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                c if c.is_control() => {
                    let mut buf = [0u8; 4];
                    for &b in c.encode_utf8(&mut buf).as_bytes() {
                        octal(&mut out, b);
                    }
                }
                c => out.push(c),
            }
        }
        for &b in chunk.invalid() {
            octal(&mut out, b);
        }
    }
    out.push('\'');
    out
}

/// `HH:MM:SS +02:00` in the local time zone, with its UTC offset so a log
/// read elsewhere is unambiguous. If the C library cannot convert the
/// time, the clock is shown in UTC and says so.
pub fn clock(seconds: i64) -> String {
    match local(seconds) {
        Some((tm, off)) => format!(
            "{:02}:{:02}:{:02} {}",
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            offset(off)
        ),
        None => {
            let rem = seconds.rem_euclid(86_400);
            format!(
                "{:02}:{:02}:{:02} UTC",
                rem / 3_600,
                (rem % 3_600) / 60,
                rem % 60
            )
        }
    }
}

/// A path with the home directory shown as `~`, for messages.
pub fn home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && h != "/" => match path.strip_prefix(h.trim_end_matches('/')) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_string(),
        },
        _ => path.to_string(),
    }
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
    if let Ok(Some(c)) = &r.last_change {
        out.push(format!("changed {}", age(now() - c.time)));
    }
    match &r.restored {
        Ok(Some(m)) => out.push(match m.at {
            Some(at) => format!("restored {}", age(now() - at)),
            None => "restored".into(),
        }),
        Ok(None) => {}
        Err(_) => out.push("restore mark unreadable".into()),
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
        // A decimal string, not a number: dev_t can exceed 2^53, which a
        // JavaScript reader would round.
        ("device", Json::str(s.device.to_string())),
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
    let (upstream, upstream_ref, upstream_gone, upstream_error) = match &r.upstream {
        Ok(Some(u)) => (
            Json::str(u.name.clone()),
            Json::str(u.full.clone()),
            Json::Bool(u.gone),
            Json::Null,
        ),
        Ok(None) => (Json::Null, Json::Null, Json::Null, Json::Null),
        Err(err) => (Json::Null, Json::Null, Json::Null, Json::str(err.clone())),
    };
    let (restored, restored_error) = crate::removed::mark_json(&r.restored);
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
        ("upstream_ref", upstream_ref),
        ("upstream_gone", upstream_gone),
        ("upstream_error", upstream_error),
        ("size", Json::opt(r.usage.as_ref(), usage_json)),
        ("last_change", last_change_json(&r.last_change)),
        (
            "last_change_error",
            match &r.last_change {
                Err(e) => Json::str(e.clone()),
                Ok(_) => Json::Null,
            },
        ),
        (
            "rule_sources",
            Json::Arr(r.rule_sources.iter().map(rule_source_json).collect()),
        ),
        ("restored", restored),
        ("restored_error", restored_error),
    ])
}

// ---------------------------------------------------------------- text

/// The two-line scan entry for one worktree.
pub fn scan_entry(
    r: &Report,
    main: &Path,
    common_dir: &Path,
    places: &Places,
    style: &Style,
) -> String {
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
        Verdict::Stale => stale_note(r, main, common_dir),
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

/// `{"unix": 1794003900, "at": "2026-11-06T21:05:00Z", "source": "git_dir",
/// "what": "index"}`, or null when residue did not or could not measure it.
fn last_change_json(c: &Result<Option<LastChange>, String>) -> Json {
    match c {
        Ok(Some(c)) => Json::Obj(vec![
            ("unix", Json::Int(c.time)),
            ("at", Json::str(rfc3339(c.time))),
            ("source", Json::str(c.source.code())),
            ("what", Json::str(c.what.clone())),
        ]),
        _ => Json::Null,
    }
}

pub fn rule_source_json(s: &RuleSource) -> Json {
    Json::Obj(vec![
        ("rule", Json::str(s.rule.clone())),
        ("scope", Json::str(s.scope.clone())),
        ("origin", Json::opt(s.origin.clone(), Json::Str)),
    ])
}

/// `idle`: the `--idle` threshold, or null when the run has none.
pub fn idle_json(policy: &Policy) -> Json {
    match &policy.idle {
        Some(idle) => Json::Obj(vec![
            ("threshold", Json::str(idle.text.clone())),
            ("threshold_seconds", Json::Int(idle.seconds)),
        ]),
        None => Json::Null,
    }
}

/// `rule_sources` at the top of a document: the rules beyond the built-in
/// list that apply to every repository in this run, from `--regenerable`
/// and the residue config file. Each worktree's own `rule_sources` is its
/// effective list: these, then its repository's Git config rules.
pub fn policy_sources_json(policy: &Policy) -> Json {
    Json::Arr(policy.sources.iter().map(rule_source_json).collect())
}
