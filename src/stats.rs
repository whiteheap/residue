//! The stats ledger: how much disk residue has freed on this machine, and
//! `residue stats`.
//!
//! One file per user, `ledger.jsonl`, in residue's state folder:
//! `~/Library/Application Support/residue/` on macOS, where a terminal, the
//! launchd agent and the Mac app find the same file whatever their
//! environment, and `$XDG_STATE_HOME/residue/` (default
//! `~/.local/state/residue/`) elsewhere. Each line is one JSON object, one
//! event:
//!
//! - `freed`: a deletion that frees space: `remove --no-trash`, `watch`'s
//!   child, a sweep, or the trash's purge. Written once `git worktree
//!   remove` succeeded.
//! - `trashed`: a move into the trash. It frees nothing on the same
//!   filesystem until the purge, which writes its own `freed` line.
//! - `restored`: `residue restore` put a worktree back.
//!
//! `bytes` is the worktree's reclaimable size as the plan measured it
//! (`fsinfo::Usage::reclaimable`), the number its removal record keeps: an
//! estimate. APFS snapshots, clones and hard links can keep some of it.
//!
//! The ledger never affects a removal. A line is written after the deletion,
//! a failure to read or write the ledger is a warning, and nothing reads it
//! to decide what to remove. Nothing in it leaves the machine.
//!
//! A writer appends each line with one `write(2)` to a file opened with
//! `O_APPEND`, for which POSIX allows no other change to the file between
//! finding its end and the write, so lines from runs at the same time (a
//! `watch` child and a terminal) never overwrite each other on a local
//! filesystem; NFS does not keep that promise. Each line stays under 512
//! bytes, the smallest `PIPE_BUF` of residue's platforms; POSIX makes that
//! a whole write for pipes only, so on a file it only keeps lines short. A
//! reader skips and counts a line it cannot read, and counts each event
//! once by its repository and entry: a removal its own run recorded and a
//! later run seeded from its removal record is one removal.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::cli::Options;
use crate::commands::{VERSION, fail, header};
use crate::discover::Repo;
use crate::json::Json;
use crate::jsonread::{self, Value};
use crate::output::{self, Places, Style, clean};
use crate::removed;
use crate::trash;

pub const FILE: &str = "ledger.jsonl";
/// The `v` of every line; a line with another is skipped and counted.
const FORMAT: i64 = 1;
/// A line, newline included, stays under this when it can.
const LINE_MAX: usize = 512;
const DAY: i64 = 86_400;
/// 9999-12-31T23:59:59Z. A line's `at` is kept between the epoch and
/// this, so a damaged or hand-edited time never prints a negative or a
/// five-digit year.
const LATEST: i64 = 253_402_300_799;
/// `residue stats --json` `weeks`: this many 7-day windows.
const WEEKS: i64 = 12;
/// The repositories `residue stats` lists in text; JSON lists every one.
const TOP: usize = 10;
/// `residue stats --json` `estimate_note`, unchanged for the JSON contract.
pub const ESTIMATE: &str = "Sizes are estimates, measured like \"reclaimable\" before each removal: APFS snapshots, clones and hard links can keep some of the space.";
/// The same caveat in `residue stats` text, in one short line.
const ESTIMATE_LINE: &str =
    "Sizes are estimates: APFS snapshots, clones and hard links can keep some space.";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Event {
    Freed,
    Trashed,
    Restored,
}

impl Event {
    pub fn code(self) -> &'static str {
        match self {
            Event::Freed => "freed",
            Event::Trashed => "trashed",
            Event::Restored => "restored",
        }
    }

    fn parse(code: &str) -> Option<Event> {
        match code {
            "freed" => Some(Event::Freed),
            "trashed" => Some(Event::Trashed),
            "restored" => Some(Event::Restored),
            _ => None,
        }
    }
}

/// One event in the ledger.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Line {
    /// Unix seconds.
    pub at: i64,
    pub event: Event,
    /// The command: [`removed::By::code`], or a code a later version wrote.
    /// `None` when unknown: a record written before residue named the
    /// command, a trash entry seeded from its record, a restore.
    pub by: Option<String>,
    /// [`removed::Reason::code`] of a removal or a move into the trash.
    pub reason: Option<String>,
    /// A restore's source: `trash`, `removed` or `interrupted`.
    pub from: Option<String>,
    pub bytes: Option<u64>,
    /// The repository's canonical Git common directory.
    pub repo: String,
    /// The worktree's directory name, for a person reading the file; left
    /// out when the line would be too long.
    pub worktree: Option<String>,
    /// The trash entry or removal record, `NAME-STAMP`, unique in its
    /// repository across both kinds.
    pub entry: String,
    /// Taken in from a removal record or a trash entry by a later run, not
    /// written by the run that did it.
    pub seeded: bool,
}

impl Line {
    fn key(&self) -> (Event, String, String) {
        (self.event, self.repo.clone(), self.entry.clone())
    }

    fn json(&self, worktree: bool) -> Json {
        let mut fields = vec![
            ("v", Json::Int(FORMAT)),
            ("at", Json::Int(self.at)),
            ("event", Json::str(self.event.code())),
        ];
        let optional = [
            ("by", &self.by),
            ("reason", &self.reason),
            ("from", &self.from),
        ];
        for (key, value) in optional {
            if let Some(v) = value {
                fields.push((key, Json::str(v.clone())));
            }
        }
        if let Some(b) = self.bytes {
            fields.push(("bytes", Json::UInt(b)));
        }
        fields.push(("repo", Json::str(self.repo.clone())));
        if let Some(w) = self.worktree.as_ref().filter(|_| worktree) {
            fields.push(("worktree", Json::str(w.clone())));
        }
        fields.push(("entry", Json::str(self.entry.clone())));
        if self.seeded {
            fields.push(("seeded", Json::Bool(true)));
        }
        Json::Obj(fields)
    }

    /// The line as written, with its newline: without `worktree` when that
    /// keeps it under [`LINE_MAX`]. A line still longer (a repository path
    /// of several hundred bytes) is written all the same: local filesystems
    /// keep one `O_APPEND` write whole, and the 512-byte bound only keeps
    /// lines short.
    pub fn text(&self) -> String {
        let mut text = self.json(true).render();
        if text.len() >= LINE_MAX {
            text = self.json(false).render();
        }
        text.push('\n');
        text
    }

    /// `None` for anything but a whole line of the current format.
    fn parse(text: &str) -> Option<Line> {
        let doc = jsonread::parse(text).ok()?;
        if doc.get("v")?.i64()? != FORMAT {
            return None;
        }
        // Absent and null are `None`; any other type is a damaged line.
        let text_of = |key: &str| -> Option<Option<String>> {
            match doc.get(key) {
                None | Some(Value::Null) => Some(None),
                Some(v) => v.str().map(|s| Some(s.to_string())),
            }
        };
        let bytes = match doc.get("bytes") {
            None | Some(Value::Null) => None,
            Some(v) => Some(u64::try_from(v.i64()?).ok()?),
        };
        let seeded = match doc.get("seeded") {
            None | Some(Value::Null) => false,
            Some(v) => v.bool()?,
        };
        let repo = doc.get("repo")?.str()?.to_string();
        let entry = doc.get("entry")?.str()?.to_string();
        if repo.is_empty() || entry.is_empty() {
            return None;
        }
        Some(Line {
            at: doc.get("at")?.i64()?,
            event: Event::parse(doc.get("event")?.str()?)?,
            by: text_of("by")?,
            reason: text_of("reason")?,
            from: text_of("from")?,
            bytes,
            repo,
            worktree: text_of("worktree")?,
            entry,
            seeded,
        })
    }
}

/// The ledger as read: each event once, in the order written.
pub struct Ledger {
    pub path: PathBuf,
    pub lines: Vec<Line>,
    /// Lines that could not be read: damaged, cut short, or of a later
    /// format.
    pub skipped: u64,
    seen: HashSet<(Event, String, String)>,
}

impl Ledger {
    fn new(path: PathBuf) -> Ledger {
        Ledger {
            path,
            lines: Vec::new(),
            skipped: 0,
            seen: HashSet::new(),
        }
    }

    /// Take a line, unless the ledger has its event already. Its time is
    /// kept between the epoch and [`LATEST`].
    fn take(&mut self, mut line: Line) {
        line.at = line.at.clamp(0, LATEST);
        if self.seen.insert(line.key()) {
            self.lines.push(line);
        }
    }

    fn has(&self, line: &Line) -> bool {
        self.seen.contains(&line.key())
    }

    fn parse(path: PathBuf, bytes: &[u8]) -> Ledger {
        let mut ledger = Ledger::new(path);
        for raw in bytes.split(|b| *b == b'\n') {
            if raw.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match std::str::from_utf8(raw).ok().and_then(Line::parse) {
                Some(line) => ledger.take(line),
                None => ledger.skipped += 1,
            }
        }
        ledger
    }
}

/// Where the ledger is.
pub fn path() -> Result<PathBuf, String> {
    let home = || {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set, so residue has no stats ledger".to_string())
    };
    #[cfg(target_os = "macos")]
    let dir = home()?.join("Library/Application Support/residue");
    // A relative XDG_STATE_HOME is ignored, as the XDG spec says: it would
    // name a different file from each working directory.
    #[cfg(not(target_os = "macos"))]
    let dir = match std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
    {
        Some(d) => d.join("residue"),
        None => home()?.join(".local/state/residue"),
    };
    Ok(dir.join(FILE))
}

/// The ledger; an empty one when there is no file yet.
pub fn read() -> Result<Ledger, String> {
    read_at(path()?)
}

fn read_at(path: PathBuf) -> Result<Ledger, String> {
    let at = |e: io::Error| format!("{}: {e}", path.display());
    let mut file = match open(&path, OpenOptions::new().read(true)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Ledger::new(path)),
        Err(e) => return Err(at(e)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(at)?;
    Ok(Ledger::parse(path, &bytes))
}

/// Open the ledger without waiting, and only when it is a regular file
/// (or a symbolic link to one). A FIFO there would make the open or the
/// read wait for a writer forever, after `remove`'s confirmation too, and
/// a device such as `/dev/zero` would never end; the caller reports it as
/// any ledger it cannot use, a warning. `O_NONBLOCK` changes nothing for a
/// regular file.
fn open(path: &Path, options: &mut OpenOptions) -> io::Result<fs::File> {
    let file = options.custom_flags(libc::O_NONBLOCK).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(
            "not a regular file, so residue neither reads nor writes it",
        ));
    }
    Ok(file)
}

/// Append `lines` to the ledger at `path`, each with one `write(2)`.
fn append(path: &Path, lines: &[Line]) -> Result<(), String> {
    let at = |e: io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut file = open(
        path,
        OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600),
    )
    .map_err(at)?;
    // A crash or a short write can leave the last line without its
    // newline. The first line then starts with one, in the same write, so
    // it is not glued onto the cut line and skipped with it. Two writers
    // that both see the cut line leave one blank line, which a reader
    // ignores.
    let len = file.metadata().map_err(at)?.len();
    let cut = if len > 0 {
        let mut last = [0];
        file.read_exact_at(&mut last, len - 1).map_err(at)?;
        last[0] != b'\n'
    } else {
        false
    };
    for (i, line) in lines.iter().enumerate() {
        let mut text = line.text();
        if i == 0 && cut {
            text.insert(0, '\n');
        }
        let written = file.write(text.as_bytes()).map_err(at)?;
        if written != text.len() {
            // A short write leaves part of a line. End it, so the next line
            // starts on its own and only this one is skipped as damaged.
            let ended = match file.write(b"\n") {
                Ok(1) => String::new(),
                Ok(_) => "; it could not end the line".into(),
                Err(e) => format!("; it could not end the line: {e}"),
            };
            return Err(format!(
                "{}: wrote {written} of {} bytes{ended}",
                path.display(),
                text.len()
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ seed

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn name_of(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The `freed` line a removal record stands for. None for a record that
/// cannot be read or has no `info`, an interrupted removal (Git still lists
/// the worktree, so nothing is freed), a worktree that is back at its path
/// (whatever its removal freed is taken again), and the record of a run
/// still going: its deletion can yet fail and take the record back, and
/// that run writes its own line.
fn record_line(item: &removed::Item, now: i64) -> Option<Line> {
    match item.state() {
        removed::State::Removed => {}
        removed::State::Incomplete
        | removed::State::Unreadable
        | removed::State::Interrupted
        | removed::State::Back => return None,
    }
    let r = item.record.as_ref().ok()?;
    let running =
        r.pid.is_some_and(crate::remove::running) && now - r.removed < removed::INCOMPLETE_AFTER;
    if running {
        return None;
    }
    Some(Line {
        at: r.removed,
        event: Event::Freed,
        by: r.by.clone(),
        reason: Some(r.reason.clone()),
        from: None,
        bytes: r.reclaimable,
        repo: lossy(&item.common_dir),
        worktree: name_of(&r.original),
        entry: item.name.clone(),
        seeded: true,
    })
}

/// The `trashed` line a trash entry stands for; none for an entry whose
/// record cannot be read or whose worktree is not in it.
fn trash_line(item: &trash::Item) -> Option<Line> {
    let r = item.record.as_ref().ok()?;
    if item.is_empty_record() {
        return None;
    }
    Some(Line {
        at: r.trashed,
        event: Event::Trashed,
        by: None,
        reason: None,
        from: None,
        bytes: r.reclaimable,
        repo: lossy(&item.common_dir),
        worktree: name_of(&r.original),
        entry: item.name.clone(),
        seeded: true,
    })
}

/// Take into the ledger each removal record and trash entry it does not
/// have yet, and return it. A removal made while the ledger could not be
/// written, or by a build that had removal records and no ledger, counts
/// from here on. What has no record is not seeded: a record forgotten past
/// `--keep-removed` or taken back by a restore, and a removal by 0.0.6 or
/// earlier, which wrote none.
pub fn seed<'a>(
    records: impl IntoIterator<Item = &'a removed::Item>,
    trash: impl IntoIterator<Item = &'a trash::Item>,
) -> Result<Ledger, String> {
    let now = output::now();
    let mut ledger = read()?;
    let mut missing: Vec<Line> = Vec::new();
    let found = records
        .into_iter()
        .filter_map(|i| record_line(i, now))
        .chain(trash.into_iter().filter_map(trash_line));
    for line in found {
        if !ledger.has(&line) && !missing.iter().any(|m| m.key() == line.key()) {
            missing.push(line);
        }
    }
    if !missing.is_empty() {
        append(&ledger.path, &missing)?;
        for line in missing {
            ledger.take(line);
        }
    }
    Ok(ledger)
}

/// [`seed`] from the repositories a `remove` run covers. A record or entry
/// residue cannot list is left out here: the run's own listing reports it.
pub fn seed_repos(repos: &[Repo]) -> Result<Ledger, String> {
    let mut records = Vec::new();
    let mut entries = Vec::new();
    for repo in repos {
        records.extend(removed::of_repo(repo).unwrap_or_default());
        entries.extend(
            trash::list(&repo.common_dir, &repo.command_dir(), &repo.trash).unwrap_or_default(),
        );
    }
    seed(&records, &entries)
}

/// The line for a removal or a move into the trash this run made, once it
/// succeeded: `entry` is the removal record or trash entry it wrote,
/// `bytes` the plan's reclaimable size, `original` the worktree's path.
pub fn made(
    event: Event,
    by: removed::By,
    reason: removed::Reason,
    bytes: Option<u64>,
    common_dir: &Path,
    original: &Path,
    entry: &Path,
) -> Line {
    Line {
        at: output::now(),
        event,
        by: Some(by.code().to_string()),
        reason: Some(reason.code().to_string()),
        from: None,
        bytes,
        repo: lossy(common_dir),
        worktree: name_of(original),
        entry: name_of(entry).unwrap_or_else(|| lossy(entry)),
        seeded: false,
    }
}

/// The line for a worktree `residue restore` put back. `from` is `trash`,
/// `removed` or `interrupted`.
pub fn restored(
    from: &str,
    common_dir: &Path,
    entry: &str,
    original: Option<&Path>,
    bytes: Option<u64>,
) -> Line {
    Line {
        at: output::now(),
        event: Event::Restored,
        by: None,
        reason: None,
        from: Some(from.to_string()),
        bytes,
        repo: lossy(common_dir),
        worktree: original.and_then(name_of),
        entry: entry.to_string(),
        seeded: false,
    }
}

/// Record lines for events this run made, into `ledger` when it has one.
pub fn record(ledger: Option<&mut Ledger>, lines: Vec<Line>) -> Result<(), String> {
    if lines.is_empty() {
        return Ok(());
    }
    match ledger {
        Some(ledger) => {
            append(&ledger.path, &lines)?;
            for line in lines {
                ledger.take(line);
            }
            Ok(())
        }
        None => append(&path()?, &lines),
    }
}

// ----------------------------------------------------------- one run

/// What one `remove` run freed and moved, and the total after it.
pub struct Run {
    pub freed: Sum,
    pub trashed: Sum,
    /// Freed on this machine, all time, and when the ledger starts. `None`
    /// when the run freed and moved nothing (it says nothing about the
    /// ledger then), or when the ledger could not be read or written.
    pub total: Option<(Sum, i64)>,
    pub ledger: Option<PathBuf>,
    /// Why the ledger is not up to date: a warning, never an error.
    pub error: Option<String>,
}

impl Run {
    /// Record a `remove` run's `lines`. `seeded` is the ledger as the run
    /// seeded it before acting, `None` when it did not act.
    pub fn record(seeded: Option<Result<Ledger, String>>, lines: Vec<Line>) -> Run {
        let mut freed = Sum::default();
        let mut trashed = Sum::default();
        for line in &lines {
            match line.event {
                Event::Freed => freed.add(line.bytes),
                Event::Trashed => trashed.add(line.bytes),
                Event::Restored => {}
            }
        }
        let mut run = Run {
            freed,
            trashed,
            total: None,
            ledger: None,
            error: None,
        };
        let warn = |e: String| {
            format!(
                "the stats ledger is not up to date ({e}); this does not change what was removed"
            )
        };
        if lines.is_empty() {
            run.error = seeded.and_then(Result::err).map(warn);
            return run;
        }
        match seeded {
            Some(Ok(mut ledger)) => match record(Some(&mut ledger), lines) {
                Ok(()) => {
                    let all = tally(&ledger, output::now());
                    run.total = all.since.map(|since| (all.all_time.all, since));
                    run.ledger = Some(ledger.path);
                }
                Err(e) => {
                    run.ledger = Some(ledger.path);
                    run.error = Some(warn(e));
                }
            },
            other => {
                // The ledger could not be read or seeded: record the run
                // all the same, so its own removals count once it can be.
                let first = other.and_then(Result::err);
                let wrote = record(None, lines);
                run.ledger = path().ok();
                run.error = first.or(wrote.err()).map(warn);
            }
        }
        run
    }

    /// The line a `remove` run ends with, when it freed or moved anything.
    /// `keep` is its `--keep-trash`.
    pub fn text(&self, keep: &str) -> Option<String> {
        let mut parts = Vec::new();
        if self.freed.worktrees > 0 {
            parts.push(format!("freed about {}", output::bytes(self.freed.bytes)));
        }
        if self.trashed.worktrees > 0 {
            parts.push(format!(
                "moved about {} to the trash, freed when a remove run purges it after {keep}",
                output::bytes(self.trashed.bytes)
            ));
        }
        if parts.is_empty() {
            return None;
        }
        let total = match &self.total {
            Some((sum, _)) if sum.worktrees == 0 => {
                "nothing counted on this machine yet".to_string()
            }
            Some((sum, since)) => format!(
                "about {} on this machine since {}",
                output::bytes(sum.bytes),
                output::day(*since)
            ),
            None => "stats ledger not updated; see the warning".to_string(),
        };
        Some(format!("{} ({total})", parts.join(" and ")))
    }

    /// `remove --json` `stats`.
    pub fn json(&self) -> Json {
        Json::Obj(vec![
            ("freed_bytes", Json::UInt(self.freed.bytes)),
            ("freed_worktrees", Json::UInt(self.freed.worktrees)),
            ("trashed_bytes", Json::UInt(self.trashed.bytes)),
            ("trashed_worktrees", Json::UInt(self.trashed.worktrees)),
            (
                "total_freed_bytes",
                Json::opt(self.total.map(|(s, _)| s.bytes), Json::UInt),
            ),
            (
                "total_freed_worktrees",
                Json::opt(self.total.map(|(s, _)| s.worktrees), Json::UInt),
            ),
            (
                "since",
                Json::opt(self.total.map(|(_, since)| since), |s| {
                    Json::str(output::rfc3339(s))
                }),
            ),
            ("ledger", Json::opt(self.ledger.as_deref(), Json::path)),
            ("error", Json::opt(self.error.clone(), Json::Str)),
        ])
    }
}

// ------------------------------------------------------------ totals

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Sum {
    /// Known sizes only: a line without one counts as a worktree, 0 bytes.
    pub bytes: u64,
    pub worktrees: u64,
}

impl Sum {
    fn add(&mut self, bytes: Option<u64>) {
        self.bytes = self.bytes.saturating_add(bytes.unwrap_or(0));
        self.worktrees += 1;
    }

    fn json(&self) -> Json {
        Json::Obj(vec![
            ("bytes", Json::UInt(self.bytes)),
            ("worktrees", Json::UInt(self.worktrees)),
        ])
    }
}

/// The commands `freed` is split by: [`removed::By`], then `other` for a
/// record that names none and a code a later version wrote.
const BY: [&str; 4] = ["remove", "sweep", "watch", "other"];

fn by_slot(by: Option<&str>) -> usize {
    BY[..3].iter().position(|b| Some(*b) == by).unwrap_or(3)
}

/// What was freed in one span of time.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Freed {
    pub all: Sum,
    /// By [`BY`].
    pub by: [Sum; 4],
}

impl Freed {
    fn add(&mut self, line: &Line) {
        self.all.add(line.bytes);
        self.by[by_slot(line.by.as_deref())].add(line.bytes);
    }

    fn json(&self) -> Json {
        Json::Obj(self.fields())
    }

    /// `bytes`, `worktrees` and `by`: the shape every span, week and
    /// repository of `residue stats --json` shares.
    fn fields(&self) -> Vec<(&'static str, Json)> {
        vec![
            ("bytes", Json::UInt(self.all.bytes)),
            ("worktrees", Json::UInt(self.all.worktrees)),
            (
                "by",
                Json::Obj(
                    BY.iter()
                        .zip(self.by.iter())
                        .map(|(name, sum)| (*name, sum.json()))
                        .collect(),
                ),
            ),
        ]
    }
}

pub struct Repository {
    pub common_dir: String,
    pub freed: Freed,
    pub last: i64,
}

impl Repository {
    /// The folder a person knows the repository by: the main checkout for
    /// `PATH/.git`, the repository itself when it is bare.
    pub fn path(&self) -> &Path {
        let dir = Path::new(&self.common_dir);
        match dir.file_name() {
            Some(n) if n == ".git" => dir.parent().unwrap_or(dir),
            _ => dir,
        }
    }
}

/// Everything `residue stats` reports from the ledger.
pub struct Stats {
    /// When the ledger starts: its earliest line.
    pub since: Option<i64>,
    pub all_time: Freed,
    pub last_30_days: Freed,
    pub last_7_days: Freed,
    /// The start of each 7-day window and what it freed, oldest first; the
    /// last one ends now.
    pub weeks: Vec<(i64, Freed)>,
    /// Most freed first.
    pub repositories: Vec<Repository>,
    pub trashed: Sum,
    pub restored: u64,
    pub restored_from_trash: u64,
    pub restored_from_records: u64,
    /// The last `freed` line of all, then by [`BY`].
    pub last: Option<i64>,
    pub last_by: [Option<i64>; 4],
    pub skipped: u64,
}

pub fn tally(ledger: &Ledger, now: i64) -> Stats {
    let first_week = now - WEEKS * 7 * DAY;
    let mut stats = Stats {
        since: ledger.lines.iter().map(|l| l.at).min(),
        all_time: Freed::default(),
        last_30_days: Freed::default(),
        last_7_days: Freed::default(),
        weeks: (0..WEEKS)
            .map(|k| (first_week + k * 7 * DAY, Freed::default()))
            .collect(),
        repositories: Vec::new(),
        trashed: Sum::default(),
        restored: 0,
        restored_from_trash: 0,
        restored_from_records: 0,
        last: None,
        last_by: [None; 4],
        skipped: ledger.skipped,
    };
    let mut repos: BTreeMap<&str, (Freed, i64)> = BTreeMap::new();
    for line in &ledger.lines {
        match line.event {
            Event::Freed => {
                stats.all_time.add(line);
                if line.at >= now - 30 * DAY {
                    stats.last_30_days.add(line);
                }
                if line.at >= now - 7 * DAY {
                    stats.last_7_days.add(line);
                }
                if line.at >= first_week {
                    // A line from a clock ahead of this one counts in the
                    // last window.
                    let k = ((line.at - first_week) / (7 * DAY)).min(WEEKS - 1);
                    stats.weeks[k as usize].1.add(line);
                }
                let repo = repos
                    .entry(&line.repo)
                    .or_insert((Freed::default(), line.at));
                repo.0.add(line);
                repo.1 = repo.1.max(line.at);
                stats.last = stats.last.max(Some(line.at));
                let slot = &mut stats.last_by[by_slot(line.by.as_deref())];
                *slot = (*slot).max(Some(line.at));
            }
            Event::Trashed => stats.trashed.add(line.bytes),
            Event::Restored => {
                stats.restored += 1;
                match line.from.as_deref() {
                    Some("trash") => stats.restored_from_trash += 1,
                    Some("removed" | "interrupted") => stats.restored_from_records += 1,
                    _ => {}
                }
            }
        }
    }
    stats.repositories = repos
        .into_iter()
        .map(|(dir, (freed, last))| Repository {
            common_dir: dir.to_string(),
            freed,
            last,
        })
        .collect();
    stats.repositories.sort_by(|a, b| {
        b.freed
            .all
            .bytes
            .cmp(&a.freed.all.bytes)
            .then(b.freed.all.worktrees.cmp(&a.freed.all.worktrees))
            .then(a.common_dir.cmp(&b.common_dir))
    });
    stats
}

// -------------------------------------------------------------- lines

/// The line `residue scan` ends with once anything was freed.
pub fn scan_line(ledger: &Ledger, now: i64) -> Option<String> {
    let stats = tally(ledger, now);
    let all = stats.all_time.all;
    if all.worktrees == 0 {
        return None;
    }
    Some(format!(
        "freed about {} from {} worktree{} since {}; {} in the last 7 days · residue stats",
        output::bytes(all.bytes),
        all.worktrees,
        if all.worktrees == 1 { "" } else { "s" },
        output::day(stats.since.unwrap_or(now)),
        output::bytes(stats.last_7_days.all.bytes)
    ))
}

/// `service status`'s `freed:` line: what `watch` freed in the last 7 days
/// and when it last removed a worktree. `installed` is whether residue's
/// own service is; without it the line shows only once `watch` freed
/// something, as a Homebrew service's would.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn service_line(installed: bool) -> Option<String> {
    let now = output::now();
    let ledger = match read() {
        Ok(l) => l,
        Err(e) if installed => {
            return Some(format!(
                "freed: unknown, residue could not read its stats ledger ({e})"
            ));
        }
        Err(_) => return None,
    };
    let stats = tally(&ledger, now);
    let watch = BY.iter().position(|b| *b == "watch").unwrap_or(2);
    let week = stats.last_7_days.by[watch];
    let last = stats.last_by[watch];
    let when = |at: i64| format!("{} ({})", output::date(at), output::age(now - at));
    Some(match last {
        None if !installed => return None,
        None => "freed: nothing counted yet (residue counts from version 0.0.8 on)".to_string(),
        Some(at) if week.worktrees == 0 => format!(
            "freed: nothing in the last 7 days; last removal {}",
            when(at)
        ),
        Some(at) => format!(
            "freed: about {} from {} worktree{} in the last 7 days; last removal {}",
            output::bytes(week.bytes),
            week.worktrees,
            if week.worktrees == 1 { "" } else { "s" },
            when(at)
        ),
    })
}

// ----------------------------------------------------------- command

/// What is in the trash now, in the repositories the ledger names: entries
/// whose worktree is still there, and their reclaimable size.
#[derive(Default)]
struct TrashNow {
    entries: u64,
    bytes: u64,
    repositories: u64,
}

fn trash_now(ledger: &Ledger, errors: &mut Vec<String>) -> TrashNow {
    let repos: BTreeSet<&str> = ledger.lines.iter().map(|l| l.repo.as_str()).collect();
    let mut now = TrashNow::default();
    for repo in repos {
        let dir = Path::new(repo);
        // Only the entries are read, never Git: the command directory is
        // not used, and an entry whose worktree is there counts.
        let items = match trash::list(dir, dir, &[]) {
            Ok(items) => items,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let held: Vec<&trash::Item> = items.iter().filter(|i| i.holds_worktree()).collect();
        if held.is_empty() {
            continue;
        }
        now.repositories += 1;
        for item in held {
            now.entries += 1;
            let bytes = item.record.as_ref().ok().and_then(|r| r.reclaimable);
            now.bytes = now.bytes.saturating_add(bytes.unwrap_or(0));
        }
    }
    now
}

/// `residue stats [--json]`.
pub fn command(options: &Options) -> u8 {
    let started = output::now();
    let ledger = match read() {
        Ok(l) => l,
        Err(e) => return fail(&format!("cannot read the stats ledger: {e}")),
    };
    let stats = tally(&ledger, started);
    let mut errors = Vec::new();
    let trash = trash_now(&ledger, &mut errors);
    if options.json {
        println!(
            "{}",
            json(&ledger, &stats, &trash, &errors, started).render()
        );
    } else {
        print!("{}", text(&ledger, &stats, &trash, &errors, started));
    }
    0
}

fn json(ledger: &Ledger, stats: &Stats, trash: &TrashNow, errors: &[String], started: i64) -> Json {
    let time = |t: Option<i64>| Json::opt(t, |t| Json::str(output::rfc3339(t)));
    Json::Obj(vec![
        ("schema", Json::str(output::SCHEMA_STATS)),
        ("version", Json::str(VERSION)),
        ("generated_at", Json::str(output::rfc3339(started))),
        ("ledger", Json::path(&ledger.path)),
        ("since", time(stats.since)),
        // The sentence the text report ends with, for a client to show as
        // it is.
        ("estimate_note", Json::str(ESTIMATE)),
        (
            "freed",
            Json::Obj(vec![
                ("all_time", stats.all_time.json()),
                ("last_30_days", stats.last_30_days.json()),
                ("last_7_days", stats.last_7_days.json()),
            ]),
        ),
        (
            "weeks",
            Json::Arr(
                stats
                    .weeks
                    .iter()
                    .map(|(start, freed)| {
                        let mut fields = vec![
                            ("start", Json::str(output::rfc3339(*start))),
                            ("end", Json::str(output::rfc3339(start + 7 * DAY))),
                        ];
                        fields.extend(freed.fields());
                        Json::Obj(fields)
                    })
                    .collect(),
            ),
        ),
        (
            "repositories",
            Json::Arr(
                stats
                    .repositories
                    .iter()
                    .map(|r| {
                        let mut fields = vec![
                            ("path", Json::path(r.path())),
                            ("common_dir", Json::str(r.common_dir.clone())),
                        ];
                        fields.extend(r.freed.fields());
                        fields.push(("last_freed_at", Json::str(output::rfc3339(r.last))));
                        Json::Obj(fields)
                    })
                    .collect(),
            ),
        ),
        ("trashed", stats.trashed.json()),
        (
            "trash_now",
            Json::Obj(vec![
                ("entries", Json::UInt(trash.entries)),
                ("bytes", Json::UInt(trash.bytes)),
                ("repositories", Json::UInt(trash.repositories)),
            ]),
        ),
        (
            "restored",
            Json::Obj(vec![
                ("worktrees", Json::UInt(stats.restored)),
                ("from_trash", Json::UInt(stats.restored_from_trash)),
                (
                    "from_removal_records",
                    Json::UInt(stats.restored_from_records),
                ),
            ]),
        ),
        (
            "last_freed_at",
            Json::Obj(
                std::iter::once(("all", time(stats.last)))
                    .chain(BY.iter().zip(stats.last_by).map(|(b, t)| (*b, time(t))))
                    .collect(),
            ),
        ),
        ("skipped_lines", Json::UInt(stats.skipped)),
        (
            "errors",
            Json::Arr(errors.iter().map(|e| Json::str(e.clone())).collect()),
        ),
    ])
}

fn plural(n: u64, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// One row of `residue stats`: a label, what it freed, and a note.
struct Row {
    label: String,
    sum: Sum,
    note: Option<String>,
}

/// A titled block of rows, their labels padded to the longest.
fn section(out: &mut String, style: &Style, title: &str, rows: &[Row]) {
    if rows.is_empty() {
        return;
    }
    out.push_str(&format!("\n{}\n", style.bold(title)));
    let width = rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    for r in rows {
        let note = r
            .note
            .as_ref()
            .map(|n| style.dim(&format!(" · {n}")))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:<width$}  {:>8}  {}{note}\n",
            r.label,
            output::bytes(r.sum.bytes),
            plural(r.sum.worktrees, "worktree")
        ));
    }
}

fn text(
    ledger: &Ledger,
    stats: &Stats,
    trash: &TrashNow,
    errors: &[String],
    started: i64,
) -> String {
    let style = Style::stdout();
    let places = Places::new();
    let mut out = style.dim(&format!("{}\n", header(started)));
    let row = |label: &str, sum: Sum| Row {
        label: label.to_string(),
        sum,
        note: None,
    };
    match stats.since {
        Some(since) if stats.all_time.all.worktrees > 0 => {
            let title = format!("freed on this machine since {}", output::day(since));
            let spans = [
                row("last 7 days", stats.last_7_days.all),
                row("last 30 days", stats.last_30_days.all),
                row("all time", stats.all_time.all),
            ];
            section(&mut out, &style, &title, &spans);
            let labels = [
                "residue remove (by hand)",
                "residue remove --all (sweeps)",
                "residue watch (automatic cleanup)",
                "command not recorded",
            ];
            let commands: Vec<Row> = labels
                .iter()
                .enumerate()
                .filter(|(i, _)| stats.all_time.by[*i].worktrees > 0)
                .map(|(i, label)| Row {
                    note: stats.last_by[i].map(|at| format!("last {}", output::date(at))),
                    ..row(label, stats.all_time.by[i])
                })
                .collect();
            section(&mut out, &style, "by command, all time", &commands);
            let repos: Vec<Row> = stats
                .repositories
                .iter()
                .take(TOP)
                .map(|r| row(&places.show(r.path()), r.freed.all))
                .collect();
            section(&mut out, &style, "by repository, all time", &repos);
            if stats.repositories.len() > TOP {
                out.push_str(&style.dim(&format!(
                    "  and {} more: residue stats --json lists every one\n",
                    stats.repositories.len() - TOP
                )));
            }
        }
        _ => out.push_str(
            "\nresidue has not counted any freed space yet. It counts from version 0.0.8 on.\n",
        ),
    }
    let mut trash_rows = Vec::new();
    if stats.trashed.worktrees > 0 {
        trash_rows.push(row("moved there, all time", stats.trashed));
    }
    if trash.entries > 0 {
        trash_rows.push(Row {
            note: Some(match trash.repositories {
                1 => "in 1 repository".to_string(),
                n => format!("in {n} repositories"),
            }),
            ..row(
                "in it now",
                Sum {
                    bytes: trash.bytes,
                    worktrees: trash.entries,
                },
            )
        });
    }
    section(&mut out, &style, "trash", &trash_rows);
    if !trash_rows.is_empty() {
        out.push_str(
            &style.dim("  frees nothing until a remove run purges it, after --keep-trash\n"),
        );
    }
    if stats.restored > 0 {
        out.push_str(&format!(
            "\nrestored {}: {} from the trash, {} from removal records\n",
            plural(stats.restored, "worktree"),
            stats.restored_from_trash,
            stats.restored_from_records
        ));
    }
    if !errors.is_empty() {
        out.push_str("\ncould not read the trash of:\n");
        for e in errors {
            out.push_str(&format!("  {}\n", clean(e)));
        }
    }
    out.push('\n');
    out.push_str(&style.dim(&format!("{ESTIMATE_LINE}\n")));
    let mut where_ = format!("ledger: {}", places.show(&ledger.path));
    if !ledger.path.exists() {
        where_.push_str(" (not written yet)");
    }
    if stats.skipped > 0 {
        where_.push_str(&format!(
            " · skipped {} it could not read",
            plural(stats.skipped, "line")
        ));
    }
    out.push_str(&style.dim(&format!("{where_}\n")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(event: Event, at: i64, by: Option<&str>, bytes: Option<u64>, entry: &str) -> Line {
        Line {
            at,
            event,
            by: by.map(str::to_string),
            reason: by.map(|_| "remove".to_string()),
            from: None,
            bytes,
            repo: "/src/app/.git".into(),
            worktree: Some("fix".into()),
            entry: entry.into(),
            seeded: false,
        }
    }

    fn ledger_of(text: &str) -> Ledger {
        Ledger::parse(PathBuf::from("/l"), text.as_bytes())
    }

    #[test]
    fn a_time_out_of_range_is_kept_in_range() {
        let ledger = ledger_of(
            "{\"v\":1,\"at\":-99999999999,\"event\":\"freed\",\"repo\":\"/r\",\"entry\":\"a\"}\n\
             {\"v\":1,\"at\":99999999999999,\"event\":\"freed\",\"repo\":\"/r\",\"entry\":\"b\"}\n",
        );
        let at: Vec<i64> = ledger.lines.iter().map(|l| l.at).collect();
        assert_eq!(at, vec![0, LATEST]);
        let stats = tally(&ledger, 100 * DAY);
        assert_eq!(
            stats.since.map(output::rfc3339).as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert_eq!(output::rfc3339(LATEST), "9999-12-31T23:59:59Z");
    }

    #[test]
    fn a_line_reads_back_as_written() {
        let mut l = line(
            Event::Freed,
            1_794_000_000,
            Some("watch"),
            Some(2048),
            "fix-20261107T040000Z",
        );
        l.seeded = true;
        let text = l.text();
        assert!(text.ends_with('\n') && text.len() < LINE_MAX, "{text}");
        assert_eq!(Line::parse(text.trim_end()), Some(l.clone()));
        let restored = Line {
            from: Some("trash".into()),
            by: None,
            reason: None,
            bytes: None,
            worktree: None,
            event: Event::Restored,
            ..l
        };
        assert_eq!(Line::parse(restored.text().trim_end()), Some(restored));
    }

    #[test]
    fn a_long_line_drops_the_worktree_name_first() {
        let mut l = line(
            Event::Freed,
            1,
            Some("remove"),
            Some(1),
            "x-20261107T040000Z",
        );
        l.worktree = Some("w".repeat(400));
        let text = l.text();
        assert!(text.len() < LINE_MAX, "{}", text.len());
        assert_eq!(Line::parse(text.trim_end()).unwrap().worktree, None);
        // Too long even without it: written whole, and it still reads.
        l.repo = format!("/{}", "r".repeat(600));
        let text = l.text();
        assert!(text.len() > LINE_MAX);
        assert_eq!(Line::parse(text.trim_end()).unwrap().repo, l.repo);
    }

    #[test]
    fn damaged_lines_are_skipped_and_counted() {
        let good = line(Event::Freed, 10, Some("remove"), Some(5), "a-1").text();
        let text = format!(
            "{good}garbage\n{{\"v\":2,\"at\":1,\"event\":\"freed\",\"repo\":\"/r\",\"entry\":\"e\"}}\n\
             {{\"v\":1,\"at\":1,\"event\":\"vanished\",\"repo\":\"/r\",\"entry\":\"e\"}}\n\
             {{\"v\":1,\"at\":1,\"event\":\"freed\",\"repo\":\"/r\",\"entry\":\"e\",\"bytes\":-4}}\n\
             {{\"v\":1,\"at\":1,\"event\":\"freed\",\"repo\":\"/r\"}}\n\
             \n   \n{{\"v\":1,\"at\":1,\"ev"
        );
        let ledger = ledger_of(&text);
        assert_eq!(ledger.lines.len(), 1);
        assert_eq!(ledger.skipped, 6);
        let bad_utf8 = [good.as_bytes(), b"\xff\xfe\n"].concat();
        let ledger = Ledger::parse(PathBuf::from("/l"), &bad_utf8);
        assert_eq!((ledger.lines.len(), ledger.skipped), (1, 1));
    }

    #[test]
    fn an_event_counts_once() {
        let live = line(Event::Freed, 10, Some("watch"), Some(5), "a-1");
        let seeded = Line {
            seeded: true,
            ..live.clone()
        };
        let moved = line(Event::Trashed, 9, Some("remove"), Some(7), "a-1");
        let text = [live.text(), seeded.text(), moved.text()].concat();
        let ledger = ledger_of(&text);
        assert_eq!(ledger.lines, vec![live, moved]);
        let stats = tally(&ledger, 20);
        assert_eq!(
            stats.all_time.all,
            Sum {
                bytes: 5,
                worktrees: 1
            }
        );
        assert_eq!(
            stats.trashed,
            Sum {
                bytes: 7,
                worktrees: 1
            }
        );
    }

    #[test]
    fn totals_split_by_time_command_and_repository() {
        let now = 100 * DAY;
        let mut lines = vec![
            line(Event::Freed, now - DAY, Some("watch"), Some(100), "a-1"),
            line(
                Event::Freed,
                now - 10 * DAY,
                Some("remove"),
                Some(20),
                "b-1",
            ),
            line(Event::Freed, now - 40 * DAY, Some("sweep"), None, "c-1"),
            line(Event::Freed, now - 90 * DAY, None, Some(3), "d-1"),
            line(Event::Freed, now + DAY, Some("future"), Some(1), "e-1"),
            line(Event::Restored, now - DAY, None, Some(9), "b-1"),
        ];
        lines[5].from = Some("removed".into());
        lines[1].repo = "/srv/bare.git".into();
        let mut ledger = Ledger::new(PathBuf::from("/l"));
        for l in lines {
            ledger.take(l);
        }
        let stats = tally(&ledger, now);
        assert_eq!(stats.since, Some(now - 90 * DAY));
        assert_eq!(
            stats.all_time.all,
            Sum {
                bytes: 124,
                worktrees: 5
            }
        );
        assert_eq!(
            stats.last_30_days.all,
            Sum {
                bytes: 121,
                worktrees: 3
            }
        );
        assert_eq!(
            stats.last_7_days.all,
            Sum {
                bytes: 101,
                worktrees: 2
            }
        );
        assert_eq!(
            stats.all_time.by[0],
            Sum {
                bytes: 20,
                worktrees: 1
            }
        );
        assert_eq!(
            stats.all_time.by[1],
            Sum {
                bytes: 0,
                worktrees: 1
            }
        );
        assert_eq!(
            stats.all_time.by[2],
            Sum {
                bytes: 100,
                worktrees: 1
            }
        );
        assert_eq!(
            stats.all_time.by[3],
            Sum {
                bytes: 4,
                worktrees: 2
            }
        );
        assert_eq!(stats.last_by[2], Some(now - DAY));
        assert_eq!(stats.last, Some(now + DAY));
        // 12 windows, the last ending now; the future line lands in it.
        // Each window and each repository splits by command, as the spans
        // do: by hand against automatic cleanup, per week and repository.
        let sum = |bytes, worktrees| Sum { bytes, worktrees };
        let none = Sum::default();
        assert_eq!(stats.weeks.len(), 12);
        assert_eq!(stats.weeks[11].0 + 7 * DAY, now);
        assert_eq!(
            stats.weeks[11].1,
            Freed {
                all: sum(101, 2),
                by: [none, none, sum(100, 1), sum(1, 1)],
            }
        );
        assert_eq!(
            stats.weeks[10].1,
            Freed {
                all: sum(20, 1),
                by: [sum(20, 1), none, none, none],
            }
        );
        let in_weeks: u64 = stats.weeks.iter().map(|w| w.1.all.worktrees).sum();
        assert_eq!(
            in_weeks, 4,
            "the line 90 days old is before the first window"
        );
        assert_eq!(stats.repositories.len(), 2);
        assert_eq!(stats.repositories[0].path(), Path::new("/src/app"));
        assert_eq!(
            stats.repositories[0].freed,
            Freed {
                all: sum(104, 4),
                by: [none, sum(0, 1), sum(100, 1), sum(4, 2)],
            }
        );
        assert_eq!(stats.repositories[1].path(), Path::new("/srv/bare.git"));
        assert_eq!(
            stats.repositories[1].freed.by,
            [sum(20, 1), none, none, none]
        );
        // The JSON carries the same split, and the estimate sentence.
        let doc = json(&ledger, &stats, &TrashNow::default(), &[], now).render();
        let doc = jsonread::parse(&doc).unwrap();
        assert_eq!(
            doc.get("estimate_note").and_then(Value::str),
            Some(ESTIMATE)
        );
        let week = &doc.get("weeks").map(Value::items).unwrap()[11];
        let watch = week.get("by").and_then(|b| b.get("watch")).unwrap();
        assert_eq!(watch.get("bytes").and_then(Value::i64), Some(100));
        let repo = &doc.get("repositories").map(Value::items).unwrap()[1];
        let remove = repo.get("by").and_then(|b| b.get("remove")).unwrap();
        assert_eq!(remove.get("worktrees").and_then(Value::i64), Some(1));
        assert_eq!((stats.restored, stats.restored_from_records), (1, 1));
    }

    fn record(name: &str, removed_at: i64, pid: Option<u32>) -> removed::Item {
        removed::Item {
            name: name.into(),
            dir: PathBuf::from(format!("/src/app/.git/residue-removed/{name}")),
            common_dir: PathBuf::from("/src/app/.git"),
            command_dir: PathBuf::from("/src/app"),
            record: Ok(removed::Record {
                original: PathBuf::from("/src/app/.worktrees/fix"),
                aside: PathBuf::from("/src/app/.worktrees/.fix.residue-removing-1"),
                branch: Some("refs/heads/fix".into()),
                head: "0f607062a2d10dfb8bc578c32a96c976609da819".into(),
                upstream: None,
                reason: "watch_landed".into(),
                by: Some("watch".into()),
                removed: removed_at,
                keep: None,
                allocated: Some(4096),
                reclaimable: Some(2048),
                gone: Vec::new(),
                pid,
                version: Some("0.0.7".into()),
                kept: Vec::new(),
            }),
            incomplete: false,
            worktree_config: false,
            head_reflog: false,
            modified: Some(removed_at),
            registered: None,
            back: false,
        }
    }

    fn entry(path: &str) -> crate::git::Entry {
        crate::git::Entry {
            path: PathBuf::from(path),
            head: None,
            branch: None,
            bare: false,
            detached: false,
            locked: None,
            prunable: None,
        }
    }

    fn trashed(name: &str) -> trash::Item {
        trash::Item {
            name: name.into(),
            dir: PathBuf::from(format!("/src/app/.git/residue-trash/{name}")),
            common_dir: PathBuf::from("/src/app/.git"),
            command_dir: PathBuf::from("/src/app"),
            record: Ok(trash::Record {
                original: PathBuf::from("/src/app/.worktrees/old"),
                branch: None,
                head: None,
                trashed: 50,
                allocated: Some(10),
                reclaimable: Some(8),
            }),
            worktree: Some(PathBuf::from(format!(
                "/src/app/.git/residue-trash/{name}/old"
            ))),
            // Git lists it there, so the entry holds a worktree.
            registered: Some(entry(&format!("/src/app/.git/residue-trash/{name}/old"))),
            incomplete: false,
            bare: false,
            modified: Some(50),
        }
    }

    #[test]
    fn seeding_takes_what_the_ledger_lacks_and_nothing_twice() {
        let now = 10 * DAY;
        let mut live = line(Event::Freed, 100, Some("watch"), Some(2048), "fix-1");
        live.worktree = Some("fix".into());
        let mut ledger = Ledger::new(PathBuf::from("/l"));
        ledger.take(live);
        let mut interrupted = record("fix-3", 300, None);
        interrupted.registered = Some(entry("/src/app/.worktrees/.fix.residue-removing-1"));
        let mut unreadable = record("fix-4", 400, None);
        unreadable.record = Err("damaged".into());
        // This process is running: a record it wrote within the hour is a
        // removal still going, which writes its own line.
        let going = record("fix-5", now - 60, Some(std::process::id()));
        let old_pid = record(
            "fix-6",
            now - 2 * removed::INCOMPLETE_AFTER,
            Some(std::process::id()),
        );
        // Git lists its worktree at its path again: nothing is freed.
        let mut back = record("fix-7", 700, None);
        back.back = true;
        let records = [
            record("fix-1", 100, None),
            record("fix-2", 200, None),
            interrupted,
            unreadable,
            going,
            old_pid,
            back,
        ];
        let found: Vec<Line> = records
            .iter()
            .filter_map(|r| record_line(r, now))
            .chain(trash_line(&trashed("old-1")))
            .collect();
        let entries: Vec<&str> = found.iter().map(|l| l.entry.as_str()).collect();
        assert_eq!(entries, ["fix-1", "fix-2", "fix-6", "old-1"]);
        assert!(found.iter().all(|l| l.seeded));
        assert_eq!(found[1].by.as_deref(), Some("watch"));
        assert_eq!(found[1].bytes, Some(2048));
        assert_eq!(found[3].event, Event::Trashed);
        for l in found {
            ledger.take(l);
        }
        // fix-1 was recorded live: seeding it again adds nothing.
        let stats = tally(&ledger, now);
        assert_eq!(
            stats.all_time.all,
            Sum {
                bytes: 3 * 2048,
                worktrees: 3
            }
        );
        assert_eq!(
            stats.trashed,
            Sum {
                bytes: 8,
                worktrees: 1
            }
        );
        let mut empty = trashed("old-2");
        empty.registered = None;
        assert!(
            trash_line(&empty).is_none(),
            "an entry without its worktree"
        );
    }

    #[test]
    fn append_writes_whole_lines_that_read_back() {
        let dir = std::env::temp_dir().join(format!(
            "residue-stats-test-{}-{}",
            std::process::id(),
            output::now()
        ));
        let path = dir.join("state").join(FILE);
        let a = line(Event::Freed, 1, Some("remove"), Some(1), "a-1");
        let b = line(Event::Trashed, 2, Some("remove"), Some(2), "b-1");
        append(&path, std::slice::from_ref(&a)).unwrap();
        append(&path, &[b.clone(), a.clone()]).unwrap();
        let ledger = read_at(path.clone()).unwrap();
        assert_eq!(ledger.lines, vec![a.clone(), b.clone()]);
        assert_eq!(ledger.skipped, 0);
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // A last line cut short, as a crash leaves it: the next append
        // starts on a line of its own, so only the cut one is skipped.
        let mut cut = OpenOptions::new().append(true).open(&path).unwrap();
        cut.write_all(b"{\"v\":1,\"at\":3,\"ev").unwrap();
        drop(cut);
        let c = line(Event::Freed, 4, Some("watch"), Some(4), "c-1");
        append(&path, std::slice::from_ref(&c)).unwrap();
        let ledger = read_at(path.clone()).unwrap();
        assert_eq!(ledger.lines, vec![a.clone(), b, c]);
        assert_eq!(ledger.skipped, 1);
        // A FIFO: refused at once, never an open or a read that waits.
        let fifo = dir.join("fifo");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.unwrap().success());
        assert!(read_at(fifo.clone()).is_err());
        assert!(append(&fifo, std::slice::from_ref(&a)).is_err());
        // A ledger that is a directory: an error, never a panic.
        assert!(read_at(dir.join("state")).is_err());
        assert!(
            append(
                &dir.join("state"),
                &[line(Event::Freed, 3, None, None, "c")]
            )
            .is_err()
        );
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            read_at(dir.join(FILE)).map(|l| l.lines.len()).ok(),
            Some(0),
            "no file yet is an empty ledger"
        );
    }

    #[test]
    fn a_run_says_what_it_freed_and_the_total() {
        let mut ledger = Ledger::new(PathBuf::from("/nonexistent-dir-for-residue-test/l"));
        ledger.take(line(
            Event::Freed,
            86_400 * 400,
            Some("watch"),
            Some(1_000_000_000),
            "a-1",
        ));
        let run = Run {
            freed: Sum {
                bytes: 2_100_000_000,
                worktrees: 1,
            },
            trashed: Sum::default(),
            total: Some((
                Sum {
                    bytes: 48_300_000_000,
                    worktrees: 9,
                },
                86_400 * 400,
            )),
            ledger: None,
            error: None,
        };
        let text = run.text("7d").unwrap();
        assert!(
            text.starts_with("freed about 2.1 GB (about 48.3 GB on this machine since "),
            "{text}"
        );
        let none = Run {
            freed: Sum::default(),
            trashed: Sum::default(),
            total: None,
            ledger: None,
            error: None,
        };
        assert_eq!(none.text("7d"), None);
        // Nothing freed: no total, and nothing written.
        let quiet = Run::record(Some(Ok(ledger)), Vec::new());
        assert!(quiet.total.is_none() && quiet.error.is_none() && quiet.ledger.is_none());
    }
}
