//! `watch`'s record of its last pass: `watch-pass.json` in residue's state
//! folder (`stats::dir`), beside the stats ledger (#181).
//!
//! A running `watch` writes it at the end of each pass (`--once` writes
//! none: a preview is not the service's pass): when, how many linked
//! worktrees the scan found, how many were candidates, changed within
//! `--grace`, or kept and by which checks, what the pass handed to a
//! removal, the free space of the fullest filesystem, and when the next
//! pass can come. When the scan failed, it holds why instead. Either way
//! it names each ROOT the pass skipped and why (#201).
//! `residue service status` shows it in two lines and `residue stats
//! --json` as `watch_pass`, so a watch that waits and one that is stuck do
//! not look the same.
//!
//! It decides nothing: `watch` never reads it back, so a record that cannot
//! be written or read changes no pass, no removal and no exit status. A
//! write that fails is logged, once until its error changes; a reader shows
//! the error. A record is written whole: to a staged file of its own beside
//! it, synced, then renamed over it, so a reader sees one record or the one
//! before, never part of one. The staged file is named for the writer's
//! PID, so two writers never write into one file: one `watch` per user
//! holds its lock, and the last rename wins should another `XDG_STATE_HOME`
//! on macOS give a second one a lock of its own.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::jsonread::{self, Value};
use crate::output;
use crate::stats;

/// The record's file name in the state folder.
pub const FILE: &str = "watch-pass.json";
/// The record's `v`. A record with another is one this release cannot read.
const FORMAT: i64 = 1;
/// A record is stale once its pass is this old. A running `watch` passes at
/// least every six hours (`watch::REDISCOVER`), so an older one means no
/// `watch` has finished a pass since.
pub const STALE_AFTER: i64 = 7 * 60 * 60;
/// `STALE_AFTER`'s six hours, for the text that explains it.
const PASSES_EVERY: &str = "6 hours";
/// A record is under 2 KB, plus two paths for each ROOT the pass skipped; a
/// reader stops at this, so nothing at the path can make it read without
/// end. One longer is an error `service status` shows, never a record.
const READ_MAX: u64 = 64 * 1024;

/// One pass of a running `watch`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
    /// When it ended, in Unix seconds.
    pub at: i64,
    pub pid: u32,
    pub dry_run: bool,
    /// `--grace`, `--idle` and `--fetch-every` as `watch` shows them
    /// (`1h`), and `--min-free`.
    pub grace: String,
    pub idle: String,
    pub min_free: f64,
    /// `None` with `--no-fetch`.
    pub fetch_every: Option<String>,
    /// What the scan found and the pass did, or why the scan failed.
    pub outcome: Result<Seen, String>,
    /// The ROOTs the pass left out of its scan (`watch::live_roots`), in
    /// the order `watch` was given them. Kept apart from `outcome`: a pass
    /// skips them before its scan, whether that scan then fails or not. A
    /// record written before this field reads as none.
    pub skipped_roots: Vec<Skipped>,
}

/// A ROOT a pass skipped, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    /// The ROOT as `watch` started with it, canonical then.
    pub root: PathBuf,
    pub why: Skip,
}

/// Why a pass skipped a ROOT: it no longer canonicalizes to itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Skip {
    /// A folder on its path became a symbolic link or was renamed: the
    /// ROOT's canonical path now.
    LeadsTo(PathBuf),
    /// It cannot be canonicalized, such as when it is gone: the error.
    Unreachable(String),
}

impl Skip {
    /// `it now leads to ~/Other`, or `it cannot be reached (WHY)`: the
    /// words the log and `service status` share.
    pub fn text(&self) -> String {
        match self {
            Skip::LeadsTo(other) => {
                format!("it now leads to {}", output::home(&other.to_string_lossy()))
            }
            Skip::Unreachable(e) => format!("it cannot be reached ({e})"),
        }
    }
}

/// What a pass found and did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    /// Linked worktrees the scan found.
    pub worktrees: u64,
    /// Of those, the candidates: every check passed, unchanged for `--grace`.
    pub candidates: u64,
    /// Changed within `--grace`, and no other check keeps them.
    pub changed: u64,
    /// The rest: a check other than `idle` keeps each.
    pub kept: u64,
    /// The checks that keep them, each with how many worktrees it keeps,
    /// most first, then by check id. A worktree two checks keep counts
    /// under both.
    pub kept_by: Vec<(String, u64)>,
    /// Worktrees the pass handed to a removal: in a dry run, those it would
    /// remove.
    pub tried: u64,
    /// Of those, the ones removed.
    pub removed: u64,
    /// The filesystems that hold the worktrees.
    pub filesystems: u64,
    /// Free and total bytes of the one with the least free space, if any
    /// could be read.
    pub fullest: Option<(u64, u64)>,
    /// When a worktree that waits passes `--grace` or `--idle`.
    pub next_deadline: Option<i64>,
    /// When the next repository is due to fetch.
    pub next_fetch: Option<i64>,
}

impl Pass {
    /// The summary the log shows, without the time: a pass is logged only
    /// when this differs from the last one logged. `None` for a failed
    /// scan, which `watch` logs on its own. It leaves out the free space
    /// itself, which moves on every pass, and keeps only which side of
    /// `--min-free` it is on.
    pub fn key(&self) -> Option<String> {
        let seen = self.outcome.as_ref().ok()?;
        Some(format!("{}; low: {}", self.counts(seen), self.low(seen)))
    }

    /// The log line for this pass.
    pub fn log_line(&self) -> Option<String> {
        let seen = self.outcome.as_ref().ok()?;
        Some(format!(
            "pass: {}; {}; logged again when this changes",
            self.counts(seen),
            self.disk(seen)
        ))
    }

    fn low(&self, seen: &Seen) -> bool {
        seen.fullest
            .is_some_and(|(free, total)| percent(free, total) < self.min_free)
    }

    /// `23 worktrees: 1 candidate, 2 changed within 1h, 20 kept (ignored
    /// 13, activity 7)`.
    fn counts(&self, seen: &Seen) -> String {
        if seen.worktrees == 0 {
            return "no linked worktrees".to_string();
        }
        let mut parts = Vec::new();
        if seen.candidates > 0 {
            parts.push(plural(seen.candidates, "candidate"));
        }
        if seen.changed > 0 {
            parts.push(format!("{} changed within {}", seen.changed, self.grace));
        }
        if seen.kept > 0 {
            let by: Vec<String> = seen
                .kept_by
                .iter()
                .map(|(check, n)| format!("{check} {n}"))
                .collect();
            parts.push(if by.is_empty() {
                format!("{} kept", seen.kept)
            } else {
                format!("{} kept ({})", seen.kept, by.join(", "))
            });
        }
        format!(
            "{}: {}",
            plural(seen.worktrees, "worktree"),
            parts.join(", ")
        )
    }

    /// `the disk is 39.2% free, removes below 10%`.
    fn disk(&self, seen: &Seen) -> String {
        let Some((free, total)) = seen.fullest else {
            return "free space unknown".to_string();
        };
        let pct = percent(free, total);
        let shown = crate::watch::pct_text(pct, self.min_free);
        if pct < self.min_free {
            format!("{shown}% free, below {}%", self.min_free)
        } else {
            format!(
                "{} {shown}% free, removes below {}%",
                if seen.filesystems > 1 {
                    "every disk is at least"
                } else {
                    "the disk is"
                },
                self.min_free
            )
        }
    }

    /// The lines `residue service status` shows: what the pass found and
    /// did, one line for each ROOT it skipped, then what the next pass
    /// waits for. A failed scan has no last line.
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    pub fn status_lines(&self, now: i64) -> Vec<String> {
        let skipped = self.skipped_roots.iter().map(|s| {
            format!(
                "skipped {}: {}",
                output::home(&s.root.to_string_lossy()),
                s.why.text()
            )
        });
        let when = format!(
            "{} ({}{})",
            output::date(self.at),
            output::age(now - self.at),
            if self.stale(now) {
                format!(", stale: a running watch passes at least every {PASSES_EVERY}")
            } else {
                String::new()
            }
        );
        let seen = match &self.outcome {
            Ok(seen) => seen,
            Err(message) => {
                let mut lines = vec![format!("last pass: {when}: the scan failed: {message}")];
                lines.extend(skipped);
                return lines;
            }
        };
        let acted = match (seen.tried, self.dry_run) {
            (0, false) => String::new(),
            (0, true) => "; dry run".to_string(),
            (n, true) => format!("; would remove {n} (dry run)"),
            (n, false) if seen.removed == n => format!("; removed {n}"),
            (n, false) => format!(
                "; removed {} of the {n} it tried, the log says why",
                seen.removed
            ),
        };
        let mut waits = vec![format!(
            "a branch lands ({})",
            match (&self.fetch_every, seen.next_fetch) {
                (Some(_), Some(at)) => format!("next fetch {}", output::date(at)),
                (Some(every), None) => format!("fetches every {every}"),
                (None, _) => "after your own git fetch --prune; --no-fetch".to_string(),
            }
        )];
        if !self.low(seen) {
            waits.push(format!("the disk drops below {}%", self.min_free));
        }
        if let Some(at) = seen.next_deadline {
            waits.push(format!(
                "{}, when a worktree passes --grace or --idle",
                output::date(at)
            ));
        }
        let last = waits.pop().unwrap_or_default();
        let waits = if waits.is_empty() {
            last
        } else {
            format!("{}, or {last}", waits.join(", "))
        };
        let mut lines = vec![format!(
            "last pass: {when}, {}; {}{acted}",
            self.counts(seen),
            self.disk(seen)
        )];
        lines.extend(skipped);
        lines.push(format!("next pass: when {waits}"));
        lines
    }

    pub fn stale(&self, now: i64) -> bool {
        now - self.at > STALE_AFTER
    }

    /// The record's fields, as the file holds them and `residue stats
    /// --json` shows them.
    pub fn fields(&self) -> Vec<(&'static str, Json)> {
        let time = |t: Option<i64>| Json::opt(t, |t| Json::str(output::rfc3339(t)));
        let seen = self.outcome.as_ref().ok();
        let count = |f: fn(&Seen) -> u64| Json::opt(seen, |s| Json::UInt(f(s)));
        let fullest = seen.and_then(|s| s.fullest);
        vec![
            ("v", Json::Int(FORMAT)),
            ("at", Json::str(output::rfc3339(self.at))),
            ("pid", Json::UInt(u64::from(self.pid))),
            ("dry_run", Json::Bool(self.dry_run)),
            ("grace", Json::str(self.grace.clone())),
            ("idle", Json::str(self.idle.clone())),
            ("min_free_percent", Json::Num(self.min_free)),
            (
                "fetch_every",
                Json::opt(self.fetch_every.clone(), Json::str),
            ),
            (
                "scan_error",
                Json::opt(self.outcome.as_ref().err().cloned(), Json::str),
            ),
            ("worktrees", count(|s| s.worktrees)),
            ("candidates", count(|s| s.candidates)),
            ("changed_within_grace", count(|s| s.changed)),
            ("kept", count(|s| s.kept)),
            (
                "kept_by",
                Json::opt(seen, |s| {
                    Json::Arr(
                        s.kept_by
                            .iter()
                            .map(|(check, n)| {
                                Json::Obj(vec![
                                    ("check", Json::str(check.clone())),
                                    ("worktrees", Json::UInt(*n)),
                                ])
                            })
                            .collect(),
                    )
                }),
            ),
            ("tried", count(|s| s.tried)),
            ("removed", count(|s| s.removed)),
            ("filesystems", count(|s| s.filesystems)),
            (
                "free_bytes",
                Json::opt(fullest, |(free, _)| Json::UInt(free)),
            ),
            (
                "total_bytes",
                Json::opt(fullest, |(_, total)| Json::UInt(total)),
            ),
            (
                "free_percent",
                Json::opt(fullest, |(free, total)| Json::Num(percent(free, total))),
            ),
            ("next_deadline_at", time(seen.and_then(|s| s.next_deadline))),
            ("next_fetch_at", time(seen.and_then(|s| s.next_fetch))),
            (
                "skipped_roots",
                Json::Arr(
                    self.skipped_roots
                        .iter()
                        .map(|s| {
                            let (leads_to, error) = match &s.why {
                                Skip::LeadsTo(other) => (Json::path(other), Json::Null),
                                Skip::Unreachable(e) => (Json::Null, Json::str(e.clone())),
                            };
                            Json::Obj(vec![
                                ("root", Json::path(&s.root)),
                                ("leads_to", leads_to),
                                ("error", error),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]
    }

    /// A record from the file's text.
    fn parse(text: &str) -> Result<Pass, String> {
        let doc = jsonread::parse(text)?;
        match doc.get("v").and_then(Value::i64) {
            Some(FORMAT) => {}
            Some(v) => return Err(format!("format {v}, which this release cannot read")),
            None => return Err("no format".to_string()),
        }
        let missing = |key: &str| format!("no valid {key}");
        let str_of = |key: &str| doc.get(key).and_then(Value::str).map(str::to_string);
        let time = |key: &str| -> Result<Option<i64>, String> {
            match doc.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v
                    .str()
                    .and_then(output::parse_rfc3339)
                    .map(Some)
                    .ok_or_else(|| missing(key)),
            }
        };
        let count = |key: &str| -> Result<u64, String> {
            doc.get(key)
                .and_then(Value::i64)
                .and_then(|n| u64::try_from(n).ok())
                .ok_or_else(|| missing(key))
        };
        let at = time("at")?.ok_or_else(|| missing("at"))?;
        // Absent from a record written before it (#201): none skipped.
        let mut skipped_roots = Vec::new();
        for item in doc.get("skipped_roots").map(Value::items).unwrap_or(&[]) {
            let text = |key: &str| item.get(key).and_then(Value::str);
            let null = |key: &str| matches!(item.get(key), Some(Value::Null));
            let why = match (text("leads_to"), text("error")) {
                (Some(other), None) if null("error") => Skip::LeadsTo(PathBuf::from(other)),
                (None, Some(e)) if null("leads_to") => Skip::Unreachable(e.to_string()),
                _ => return Err(missing("skipped_roots")),
            };
            match text("root") {
                Some(root) => skipped_roots.push(Skipped {
                    root: PathBuf::from(root),
                    why,
                }),
                None => return Err(missing("skipped_roots")),
            }
        }
        let outcome = match str_of("scan_error") {
            Some(message) => Err(message),
            None => {
                let mut kept_by = Vec::new();
                for item in doc.get("kept_by").map(Value::items).unwrap_or(&[]) {
                    let check = item.get("check").and_then(Value::str);
                    let n = item
                        .get("worktrees")
                        .and_then(Value::i64)
                        .and_then(|n| u64::try_from(n).ok());
                    match (check, n) {
                        (Some(check), Some(n)) => kept_by.push((check.to_string(), n)),
                        _ => return Err(missing("kept_by")),
                    }
                }
                let fullest = match (doc.get("free_bytes"), doc.get("total_bytes")) {
                    (Some(Value::Null) | None, _) => None,
                    _ => Some((count("free_bytes")?, count("total_bytes")?)),
                };
                Ok(Seen {
                    worktrees: count("worktrees")?,
                    candidates: count("candidates")?,
                    changed: count("changed_within_grace")?,
                    kept: count("kept")?,
                    kept_by,
                    tried: count("tried")?,
                    removed: count("removed")?,
                    filesystems: count("filesystems")?,
                    fullest: fullest.filter(|(_, total)| *total > 0),
                    next_deadline: time("next_deadline_at")?,
                    next_fetch: time("next_fetch_at")?,
                })
            }
        };
        Ok(Pass {
            at,
            pid: u32::try_from(count("pid")?).map_err(|_| missing("pid"))?,
            dry_run: doc
                .get("dry_run")
                .and_then(Value::bool)
                .ok_or_else(|| missing("dry_run"))?,
            grace: str_of("grace").ok_or_else(|| missing("grace"))?,
            idle: str_of("idle").ok_or_else(|| missing("idle"))?,
            min_free: doc
                .get("min_free_percent")
                .and_then(Value::f64)
                .ok_or_else(|| missing("min_free_percent"))?,
            fetch_every: str_of("fetch_every"),
            outcome,
            skipped_roots,
        })
    }
}

fn percent(free: u64, total: u64) -> f64 {
    crate::watch::percent(free, total)
}

fn plural(n: u64, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// Where the record is.
pub fn path() -> Result<PathBuf, String> {
    Ok(stats::dir()?.join(FILE))
}

/// Write `pass` as the record, whole.
pub fn write(pass: &Pass) -> Result<(), String> {
    write_at(&path()?, pass)
}

fn write_at(path: &Path, pass: &Pass) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    // As the ledger's: the folder only its user can read.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let staged = dir.join(format!(".{FILE}.{}.new", std::process::id()));
    let at = |e: io::Error| format!("{}: {e}", staged.display());
    // One a killed write of this PID left. `remove_file` takes a symbolic
    // link itself, never what it names, and the create below refuses one.
    match fs::remove_file(&staged) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(at(e)),
        _ => {}
    }
    let mut bytes = Json::Obj(pass.fields()).render();
    bytes.push('\n');
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(bytes.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&staged, path));
    if let Err(e) = written {
        // Why-safe: the staged file is this PID's own, and a later write
        // removes it first; the error that matters is the one returned.
        if let Err(cleanup) = fs::remove_file(&staged) {
            if cleanup.kind() != io::ErrorKind::NotFound {
                return Err(format!("{}; and cannot remove it: {cleanup}", at(e)));
            }
        }
        return Err(at(e));
    }
    Ok(())
}

/// The record, or `None` when no `watch` has written one.
pub fn read() -> Result<Option<Pass>, String> {
    read_at(&path()?)
}

fn read_at(path: &Path) -> Result<Option<Pass>, String> {
    let at = |e: String| format!("{}: {e}", output::home(&path.to_string_lossy()));
    // Opened as the ledger is: without waiting, and only a regular file.
    let file = match stats::open(path, OpenOptions::new().read(true)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(at(e.to_string())),
    };
    let mut text = String::new();
    file.take(READ_MAX)
        .read_to_string(&mut text)
        .map_err(|e| at(e.to_string()))?;
    Pass::parse(&text).map(Some).map_err(at)
}

/// The lines `residue service status` shows for the last pass. With
/// `installed`, residue's own service is: then a missing record says so.
/// Without it, only a record some `watch` wrote shows.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn status_lines(installed: bool, now: i64) -> Vec<String> {
    match read() {
        Ok(Some(pass)) => pass.status_lines(now),
        Ok(None) if installed => vec![
            "last pass: none recorded yet; watch records each pass once it ends, and a watch started by residue 0.0.13 or earlier records none".to_string(),
        ],
        Ok(None) => Vec::new(),
        Err(e) => vec![format!(
            "last pass: unknown, residue could not read its record ({e})"
        )],
    }
}

/// `residue stats --json` `watch_pass`: null when no `watch` has written a
/// record; else its `path`, `stale`, `error` (why it could not be read, or
/// null) and, when it could be, the record's fields.
pub fn json(now: i64) -> Json {
    let path = match path() {
        Ok(path) => path,
        Err(e) => {
            return Json::Obj(vec![
                ("path", Json::Null),
                ("stale", Json::Null),
                ("error", Json::str(e)),
            ]);
        }
    };
    match read_at(&path) {
        Ok(None) => Json::Null,
        Ok(Some(pass)) => {
            let mut fields = vec![
                ("path", Json::path(&path)),
                ("stale", Json::Bool(pass.stale(now))),
                ("error", Json::Null),
            ];
            fields.extend(pass.fields());
            Json::Obj(fields)
        }
        Err(e) => Json::Obj(vec![
            ("path", Json::path(&path)),
            ("stale", Json::Null),
            ("error", Json::str(e)),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "residue-pass-{test}-{}-{}",
            std::process::id(),
            output::now()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pass(seen: Seen) -> Pass {
        Pass {
            at: 1_762_800_000,
            pid: 4242,
            dry_run: false,
            grace: "1h".into(),
            idle: "2d".into(),
            min_free: 10.0,
            fetch_every: Some("30m".into()),
            outcome: Ok(seen),
            skipped_roots: Vec::new(),
        }
    }

    fn skipped() -> Vec<Skipped> {
        vec![
            Skipped {
                root: PathBuf::from("/src/B"),
                why: Skip::LeadsTo(PathBuf::from("/Volumes/Backup/B")),
            },
            Skipped {
                root: PathBuf::from("/src/C"),
                why: Skip::Unreachable("No such file or directory (os error 2)".into()),
            },
        ]
    }

    fn seen() -> Seen {
        Seen {
            worktrees: 23,
            candidates: 1,
            changed: 0,
            kept: 22,
            kept_by: vec![
                ("ignored".into(), 13),
                ("activity".into(), 7),
                ("untracked".into(), 2),
            ],
            tried: 0,
            removed: 0,
            filesystems: 1,
            fullest: Some((39 * G, 100 * G)),
            next_deadline: None,
            next_fetch: Some(1_762_801_800),
        }
    }

    /// What `write` writes, `read` reads back, a failed scan included; a
    /// second write replaces the first and leaves no staged file.
    #[test]
    fn a_record_reads_back_as_written() {
        let dir = scratch("roundtrip");
        let path = dir.join("state").join(FILE);
        assert_eq!(read_at(&path).unwrap(), None, "no file is no record");
        let mut first = pass(seen());
        first.dry_run = true;
        first.min_free = 99.9;
        first.fetch_every = None;
        if let Ok(s) = first.outcome.as_mut() {
            s.next_deadline = Some(1_762_803_600);
            s.fullest = None;
        }
        write_at(&path, &first).unwrap();
        assert_eq!(read_at(&path).unwrap(), Some(first));
        let mut failed = pass(seen());
        failed.outcome = Err("residue scan: timed out".into());
        failed.skipped_roots = skipped();
        // A folder name may hold any byte but NUL and `/`.
        failed.skipped_roots.push(Skipped {
            root: PathBuf::from("/src/a\nlog: \"b\"\t\u{1b}[31m"),
            why: Skip::LeadsTo(PathBuf::from("/x\\y")),
        });
        write_at(&path, &failed).unwrap();
        assert_eq!(read_at(&path).unwrap(), Some(failed));
        let mut skipping = pass(seen());
        skipping.skipped_roots = skipped();
        write_at(&path, &skipping).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.ends_with(
                ",\"skipped_roots\":[{\"root\":\"/src/B\",\"leads_to\":\"/Volumes/Backup/B\",\"error\":null},{\"root\":\"/src/C\",\"leads_to\":null,\"error\":\"No such file or directory (os error 2)\"}]}\n"
            ),
            "skipped_roots is not the last field: {text}"
        );
        assert_eq!(read_at(&path).unwrap(), Some(skipping));
        let names: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![FILE.to_string()], "a staged file was left");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The record as the release before `skipped_roots` (#197) wrote it:
    /// still read, with no ROOT skipped.
    const BEFORE_SKIPPED_ROOTS: &str = "{\"v\":1,\"at\":\"2026-11-10T18:40:00Z\",\"pid\":4242,\"dry_run\":false,\"grace\":\"1h\",\"idle\":\"2d\",\"min_free_percent\":10,\"fetch_every\":\"30m\",\"scan_error\":null,\"worktrees\":23,\"candidates\":1,\"changed_within_grace\":0,\"kept\":22,\"kept_by\":[{\"check\":\"ignored\",\"worktrees\":13},{\"check\":\"activity\",\"worktrees\":7},{\"check\":\"untracked\",\"worktrees\":2}],\"tried\":0,\"removed\":0,\"filesystems\":1,\"free_bytes\":41875931136,\"total_bytes\":107374182400,\"free_percent\":39,\"next_deadline_at\":null,\"next_fetch_at\":\"2026-11-10T19:10:00Z\"}\n";

    #[test]
    fn a_record_without_skipped_roots_reads_as_none_skipped() {
        let dir = scratch("before-skipped-roots");
        let path = dir.join(FILE);
        fs::write(&path, BEFORE_SKIPPED_ROOTS).unwrap();
        let read = read_at(&path).unwrap().unwrap();
        assert_eq!(read.skipped_roots, Vec::new());
        assert_eq!(read.outcome.as_ref().map(|s| s.worktrees), Ok(23));
        assert_eq!(read.status_lines(read.at).len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A record that cannot be read is an error to show, never a record:
    /// a damaged file, a newer format, a folder or a FIFO at its path.
    #[test]
    fn a_record_that_cannot_be_read_is_an_error() {
        let dir = scratch("unreadable");
        let path = dir.join(FILE);
        let damaged = |entry: &str| {
            BEFORE_SKIPPED_ROOTS.replace("}\n", &format!(",\"skipped_roots\":[{entry}]}}\n"))
        };
        for text in [
            "".to_string(),
            "{\"v\":1".to_string(),
            "{\"v\":2,\"at\":\"2026-11-10T00:00:00Z\"}".to_string(),
            "{\"v\":1}".to_string(),
            damaged("{\"root\":\"/a\",\"leads_to\":null,\"error\":null}"),
            damaged("{\"root\":\"/a\",\"leads_to\":\"/b\",\"error\":\"x\"}"),
            damaged("{\"leads_to\":\"/b\",\"error\":null}"),
            damaged("{\"root\":\"/a\",\"leads_to\":\"/b\"}"),
        ] {
            fs::write(&path, &text).unwrap();
            assert!(read_at(&path).is_err(), "read {text:?} as a record");
        }
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read_at(&path).is_err(), "read a folder as a record");
        fs::remove_dir(&path).unwrap();
        let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: mkfifo reads the C string we own for the call's length.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(read_at(&path).is_err(), "a FIFO is no record");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A record that cannot be written is an error, and the record before
    /// it stays whole.
    #[test]
    fn a_failed_write_leaves_the_last_record() {
        let dir = scratch("unwritable");
        let path = dir.join(FILE);
        let first = pass(seen());
        write_at(&path, &first).unwrap();
        // A folder where the record goes: the rename fails.
        let blocked = dir.join("blocked");
        fs::create_dir_all(blocked.join(FILE).join("inside")).unwrap();
        assert!(write_at(&blocked.join(FILE), &first).is_err());
        assert_eq!(
            fs::read_dir(&blocked).unwrap().count(),
            1,
            "the staged file was left"
        );
        assert_eq!(read_at(&path).unwrap(), Some(first));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The log's key moves with the counts and with crossing --min-free,
    /// never with free space alone, which changes on every pass.
    #[test]
    fn the_log_key_ignores_free_space_on_one_side_of_min_free() {
        let base = pass(seen());
        let mut more_free = pass(seen());
        if let Ok(s) = more_free.outcome.as_mut() {
            s.fullest = Some((41 * G, 100 * G));
            s.next_fetch = Some(1);
        }
        assert_eq!(base.key(), more_free.key());
        let mut low = pass(seen());
        if let Ok(s) = low.outcome.as_mut() {
            s.fullest = Some((9 * G, 100 * G));
        }
        assert_ne!(base.key(), low.key());
        let mut kept_more = pass(seen());
        if let Ok(s) = kept_more.outcome.as_mut() {
            s.kept_by[0].1 = 14;
        }
        assert_ne!(base.key(), kept_more.key());
        let mut failed = pass(seen());
        failed.outcome = Err("x".into());
        assert_eq!(failed.key(), None);
        assert_eq!(
            base.log_line().unwrap(),
            "pass: 23 worktrees: 1 candidate, 22 kept (ignored 13, activity 7, untracked 2); the disk is 39.0% free, removes below 10%; logged again when this changes"
        );
    }

    #[test]
    fn status_says_what_waits_and_when_it_is_stale() {
        let p = pass(seen());
        let lines = p.status_lines(p.at + 180);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].ends_with(
                "(3 minutes ago), 23 worktrees: 1 candidate, 22 kept (ignored 13, activity 7, untracked 2); the disk is 39.0% free, removes below 10%"
            ),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("next pass: when a branch lands (next fetch ")
                && lines[1].ends_with("), or the disk drops below 10%"),
            "{}",
            lines[1]
        );
        assert!(!lines[0].contains("stale"));
        let old = p.status_lines(p.at + STALE_AFTER + 1);
        assert!(
            old[0].contains("hours ago, stale: a running watch passes at least every 6 hours)"),
            "{}",
            old[0]
        );
        let mut failed = pass(seen());
        failed.outcome = Err("residue scan: timed out".into());
        let lines = failed.status_lines(failed.at);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with(": the scan failed: residue scan: timed out"));
        let mut dry = pass(seen());
        dry.dry_run = true;
        if let Ok(s) = dry.outcome.as_mut() {
            s.tried = 1;
        }
        assert!(dry.status_lines(dry.at)[0].ends_with("; would remove 1 (dry run)"));
        let mut none = pass(Seen::default());
        none.fetch_every = None;
        let lines = none.status_lines(none.at);
        assert!(
            lines[0].ends_with(", no linked worktrees; free space unknown"),
            "{}",
            lines[0]
        );
        assert_eq!(
            lines[1],
            "next pass: when a branch lands (after your own git fetch --prune; --no-fetch), or the disk drops below 10%"
        );
    }

    /// Each ROOT a pass skipped is one line under `last pass:`, before
    /// `next pass:`, also after a failed scan.
    #[test]
    fn status_names_each_skipped_root() {
        let mut p = pass(seen());
        p.skipped_roots = skipped();
        let lines = p.status_lines(p.at);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].starts_with("last pass: "), "{}", lines[0]);
        assert_eq!(
            lines[1],
            "skipped /src/B: it now leads to /Volumes/Backup/B"
        );
        assert_eq!(
            lines[2],
            "skipped /src/C: it cannot be reached (No such file or directory (os error 2))"
        );
        assert!(lines[3].starts_with("next pass: "), "{}", lines[3]);
        p.outcome = Err("residue scan: timed out".into());
        let lines = p.status_lines(p.at);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].ends_with(": the scan failed: residue scan: timed out"));
        assert!(lines[1].starts_with("skipped /src/B: "));
        assert!(lines[2].starts_with("skipped /src/C: "));
    }
}
