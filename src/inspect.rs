//! The checks that decide whether one linked worktree may be removed.
//!
//! Each check passes, blocks, or is unknown. A worktree is a removal
//! candidate only when every check passes; one block keeps it, and one
//! unknown keeps it too. Facts that never block (commits only on a local
//! branch, a gone upstream, age) are reported separately so a reader sees
//! each one instead of a single "safe" label.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::activity::{Evidence, Snapshot};
use crate::fsinfo::{self, Bucket, Expansion, Usage};
use crate::git::{self, Entry};
use crate::policy::{GIT_CONFIG_KEY, Policy, RuleSource, Rules};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Block,
    Unknown,
}

impl Status {
    pub fn code(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Block => "block",
            Status::Unknown => "unknown",
        }
    }
}

pub struct Check {
    /// Stable identifier, also used in JSON output.
    pub id: &'static str,
    pub status: Status,
    /// A few words for the scan table.
    pub short: String,
    /// A full sentence for `explain`.
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Candidate,
    Keep,
    Unknown,
    /// Git still has metadata for a worktree whose directory is gone.
    Stale,
}

impl Verdict {
    pub fn code(self) -> &'static str {
        match self {
            Verdict::Candidate => "candidate",
            Verdict::Keep => "keep",
            Verdict::Unknown => "unknown",
            Verdict::Stale => "stale",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Judgement {
    Regenerable,
    Kept,
    /// residue could not tell (part of a collapsed directory was unreadable).
    Unknown,
}

pub struct Ignored {
    pub path: String,
    pub judgement: Judgement,
    /// Non-directory entries inside it, each hard-linked inode once, and
    /// their apparent bytes. `None` when the walk did not measure it.
    pub files: Option<u64>,
    pub bytes: Option<u64>,
    /// How a collapsed directory was judged entry by entry, when it was.
    pub note: Option<String>,
}

impl Ignored {
    pub fn regenerable(&self) -> bool {
        self.judgement == Judgement::Regenerable
    }

    /// `node_modules/ (1204 files, 31 MB)`, or the bare path when unmeasured.
    pub fn sized(&self) -> String {
        match (self.files, self.bytes) {
            (Some(files), Some(bytes)) => format!(
                "{} ({}, {})",
                self.path,
                plural(files as usize, "file", "files"),
                crate::output::bytes(bytes)
            ),
            _ => self.path.clone(),
        }
    }

    /// [`Ignored::sized`] plus the note, for `explain`.
    fn described(&self) -> String {
        match (&self.note, self.files, self.bytes) {
            (Some(note), Some(files), Some(bytes)) => format!(
                "{} ({}, {}; {note})",
                self.path,
                plural(files as usize, "file", "files"),
                crate::output::bytes(bytes)
            ),
            (Some(note), _, _) => format!("{} ({note})", self.path),
            (None, _, _) => self.sized(),
        }
    }
}

/// Up to `limit` entries; past it, how many more and what all of them hold.
fn ignored_examples(items: &[&Ignored], limit: usize, show: fn(&Ignored) -> String) -> String {
    let shown: Vec<String> = items.iter().take(limit).map(|i| show(i)).collect();
    let more = items.len().saturating_sub(limit);
    if more == 0 {
        return shown.join(", ");
    }
    let files: u64 = items.iter().filter_map(|i| i.files).sum();
    let bytes: u64 = items.iter().filter_map(|i| i.bytes).sum();
    let partial = if items.iter().any(|i| i.files.is_none()) {
        "at least "
    } else {
        ""
    };
    format!(
        "{} (+{more} more; {} entries hold {partial}{}, {})",
        shown.join(", "),
        items.len(),
        plural(files as usize, "file", "files"),
        crate::output::bytes(bytes)
    )
}

/// Which of the three signals set a worktree's last change.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChangeSource {
    /// An entry in the checkout: a file, directory or symbolic link,
    /// ignored ones included.
    Files,
    /// A file directly in the worktree's Git directory: `index`, `HEAD`,
    /// `ORIG_HEAD`, `FETCH_HEAD`, `COMMIT_EDITMSG`.
    GitDir,
    /// The newest entry of the HEAD reflog.
    Reflog,
}

impl ChangeSource {
    pub fn code(self) -> &'static str {
        match self {
            ChangeSource::Files => "files",
            ChangeSource::GitDir => "git_dir",
            ChangeSource::Reflog => "reflog",
        }
    }
}

pub struct LastChange {
    /// Unix seconds.
    pub time: i64,
    pub source: ChangeSource,
    /// The entry relative to the worktree root (`.` for the root itself),
    /// the Git directory file name, or the reflog message.
    pub what: String,
}

impl LastChange {
    /// `index in its Git directory`, for messages.
    pub fn described(&self) -> String {
        match self.source {
            ChangeSource::Files if self.what == "." => "the worktree directory".to_string(),
            ChangeSource::Files => format!("{} in the worktree", self.what),
            ChangeSource::GitDir => format!("{} in its Git directory", self.what),
            ChangeSource::Reflog => format!("HEAD reflog: {}", self.what),
        }
    }
}

pub struct Upstream {
    pub name: String,
    pub gone: bool,
}

pub struct Report {
    pub entry: Entry,
    pub canonical: Option<PathBuf>,
    pub verdict: Verdict,
    pub stale_reason: Option<String>,
    pub checks: Vec<Check>,
    pub activity: Option<Evidence>,
    pub ignored: Vec<Ignored>,
    /// Commits reachable from HEAD but from no remote-tracking ref, as the
    /// local refs see it (residue never fetches). `Ok(None)` when the
    /// repository has no remote-tracking refs at all. Facts never block, so
    /// a failed query is kept as its error and shown, not hidden.
    pub local_commits: Result<Option<u64>, String>,
    pub upstream: Result<Option<Upstream>, String>,
    pub usage: Option<Usage>,
    /// The newest of the three change signals. `Ok(None)` when it was not
    /// measured (a stale or unidentified worktree).
    pub last_change: Result<Option<LastChange>, String>,
    /// This repository's Git config rules, with their source.
    pub rule_sources: Vec<RuleSource>,
}

impl Report {
    fn new(entry: &Entry) -> Report {
        Report {
            entry: entry.clone(),
            canonical: None,
            verdict: Verdict::Unknown,
            stale_reason: None,
            checks: Vec::new(),
            activity: None,
            ignored: Vec::new(),
            local_commits: Ok(None),
            upstream: Ok(None),
            usage: None,
            last_change: Ok(None),
            rule_sources: Vec::new(),
        }
    }

    fn push(
        &mut self,
        id: &'static str,
        status: Status,
        short: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.checks.push(Check {
            id,
            status,
            short: short.into(),
            detail: detail.into(),
        });
    }

    fn pass(&mut self, id: &'static str, detail: impl Into<String>) {
        self.push(id, Status::Pass, "", detail);
    }

    fn block(&mut self, id: &'static str, short: impl Into<String>, detail: impl Into<String>) {
        self.push(id, Status::Block, short, detail);
    }

    fn unknown(&mut self, id: &'static str, short: impl Into<String>, detail: impl Into<String>) {
        self.push(id, Status::Unknown, short, detail);
    }

    fn finish(mut self) -> Report {
        self.verdict = if self.stale_reason.is_some() {
            Verdict::Stale
        } else if self.checks.iter().any(|c| c.status == Status::Block) {
            Verdict::Keep
        } else if self.checks.iter().any(|c| c.status == Status::Unknown) || self.checks.is_empty()
        {
            Verdict::Unknown
        } else {
            Verdict::Candidate
        };
        self
    }

    pub fn blocking(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.status != Status::Pass)
    }
}

fn examples(items: &[String], limit: usize) -> String {
    let shown: Vec<&str> = items.iter().take(limit).map(String::as_str).collect();
    let more = items.len().saturating_sub(limit);
    if more > 0 {
        format!("{} (+{more} more)", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Judge one linked worktree of the repository whose canonical common
/// directory is `common_dir`.
pub fn worktree(entry: &Entry, common_dir: &Path, snapshot: &Snapshot, policy: &Policy) -> Report {
    let mut r = Report::new(entry);
    if let Some(reason) = &entry.prunable {
        r.stale_reason = Some(if reason.is_empty() {
            "git marks it prunable".to_string()
        } else {
            format!("git marks it prunable: {reason}")
        });
        return r.finish();
    }
    match fs::symlink_metadata(&entry.path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            r.stale_reason = Some("the directory no longer exists".into());
            return r.finish();
        }
        Err(e) => {
            r.unknown(
                "identity",
                "unreadable",
                format!("cannot read the directory: {e}"),
            );
            return r.finish();
        }
        Ok(_) => {}
    }
    let canonical = match git::canonical(&entry.path) {
        Ok(p) => p,
        Err(e) => {
            r.unknown(
                "identity",
                "unreadable",
                format!("cannot resolve the path: {e}"),
            );
            return r.finish();
        }
    };
    r.canonical = Some(canonical.clone());
    let identity = match git::identity(&canonical) {
        Ok(id) => id,
        Err(e) => {
            r.unknown("identity", "git failed", e);
            return r.finish();
        }
    };
    if identity.common_dir != common_dir || identity.toplevel != canonical {
        r.unknown(
            "identity",
            "identity mismatch",
            format!(
                "git resolves this path to the checkout at {} (repository {}), not the listed worktree",
                identity.toplevel.display(),
                identity.common_dir.display()
            ),
        );
        return r.finish();
    }
    r.pass(
        "identity",
        "the directory is the linked worktree Git lists for this repository",
    );

    check_lock(&mut r, entry);
    check_activity(&mut r, &canonical, snapshot);
    // `git status` runs before the walk so the walk can size each ignored
    // entry and judge the collapsed directories a path rule reaches into.
    // The checks are still reported in their usual order.
    let status = git::status(&canonical);
    let repo = RepoRules::read(&canonical);
    let rules = policy.with(&repo.rules);
    let buckets: Vec<Bucket> = match &status {
        Ok(s) => s
            .ignored
            .iter()
            .map(|(text, rel)| Bucket {
                rel: rel.clone(),
                expand: text.ends_with('/') && !rules.matches(text) && rules.reaches_inside(text),
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    let usage = fsinfo::measure(&canonical, &buckets, &|rel| rules.matches(rel));
    r.last_change = last_change(&usage, &canonical, &identity.git_dir);
    check_status(&mut r, status, &repo, &rules, &buckets, &usage);
    check_index_flags(&mut r, &canonical);
    check_operation(&mut r, &identity.git_dir);
    check_private_refs(&mut r, &canonical);
    check_head(&mut r, entry, &canonical);
    check_layout(&mut r, &canonical);
    check_size(&mut r, &canonical, usage);
    if let Some(idle) = &policy.idle {
        check_idle(&mut r, idle);
    }
    r.rule_sources = repo.values;
    gather_facts(&mut r, entry, &canonical);
    r.finish()
}

/// When this worktree last changed, as the newest of three signals:
///
/// - every entry the walk reached in the checkout (files, directories and
///   symbolic links, ignored output included), so an edit, a new file, a
///   deletion (its directory's mtime) or a build writing `target/` counts;
/// - the files directly in the worktree's Git directory: `index` (add,
///   commit, checkout, reset, stash, and a `git status` that refreshes stale
///   stat data), `HEAD`, `ORIG_HEAD`, `FETCH_HEAD`, `COMMIT_EDITMSG`. Not
///   `logs/`: `git gc` in any worktree of the repository rewrites every
///   worktree's `logs/HEAD` without anyone using it (measured with Git
///   2.50.1), so its mtime would hold leaked worktrees forever;
/// - the timestamp written inside the newest HEAD reflog entry, which gc
///   keeps: commits, checkouts, resets, rebases, merges and pulls.
///
/// residue's own Git commands run with optional locks off, so a scan never
/// writes the index and never resets this clock. Reads (`git diff`, `git
/// log`, `cat`, an editor that only opens a file) leave no trace in any of
/// the three. mtime, not ctime: `touch -t`, `tar` and `rsync -a` set mtime
/// on purpose; atime is noise from backups and searches.
fn last_change(
    usage: &io::Result<Usage>,
    dir: &Path,
    git_dir: &Path,
) -> Result<Option<LastChange>, String> {
    let usage = usage
        .as_ref()
        .map_err(|e| format!("cannot walk the worktree: {e}"))?;
    let mut found: Vec<LastChange> = Vec::new();
    if let (Some(time), Some(path)) = (usage.newest_mtime, &usage.newest_path) {
        let what = match path.strip_prefix(dir) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
            Ok(rel) => rel.display().to_string(),
            Err(_) => path.display().to_string(),
        };
        found.push(LastChange {
            time,
            source: ChangeSource::Files,
            what,
        });
    }
    let unreadable = |e: io::Error| format!("cannot read {}: {e}", git_dir.display());
    for item in fs::read_dir(git_dir).map_err(unreadable)? {
        let item = item.map_err(unreadable)?;
        let meta = item
            .metadata()
            .map_err(|e| format!("cannot read {}: {e}", item.path().display()))?;
        if meta.is_file() {
            found.push(LastChange {
                time: std::os::unix::fs::MetadataExt::mtime(&meta),
                source: ChangeSource::GitDir,
                what: item.file_name().to_string_lossy().into_owned(),
            });
        }
    }
    if let Some((time, message)) = git::reflog_newest(dir)? {
        found.push(LastChange {
            time,
            source: ChangeSource::Reflog,
            what: message,
        });
    }
    // The first of equal times wins, so a tie names the checkout first.
    Ok(found
        .into_iter()
        .reduce(|a, b| if b.time > a.time { b } else { a }))
}

/// `--idle`: keep a worktree that changed within the threshold. A failed
/// check keeps it (`block`); a last change residue could not read is
/// `unknown`, which keeps it too. Reported right after `activity`, the
/// other check about whether someone is using the worktree.
fn check_idle(r: &mut Report, idle: &crate::cli::Idle) {
    let now = crate::output::now();
    let (status, short, detail) = match &r.last_change {
        Ok(Some(c)) => {
            let ago = crate::output::age(now - c.time);
            let when = format!("{ago} ({}, {})", crate::output::date(c.time), c.described());
            if now - c.time < idle.seconds {
                (
                    Status::Block,
                    format!("changed {ago} (--idle {})", idle.text),
                    format!(
                        "last changed {when}, within --idle {}; a session may still be using it",
                        idle.text
                    ),
                )
            } else {
                (
                    Status::Pass,
                    String::new(),
                    format!("last changed {when}, longer ago than --idle {}", idle.text),
                )
            }
        }
        Ok(None) => (
            Status::Unknown,
            "last change unknown".to_string(),
            "residue found no time for the last change".to_string(),
        ),
        Err(e) => (
            Status::Unknown,
            "last change unknown".to_string(),
            format!("last change unknown: {e}"),
        ),
    };
    let check = Check {
        id: "idle",
        status,
        short,
        detail,
    };
    let at = r
        .checks
        .iter()
        .position(|c| c.id == "activity")
        .map_or(r.checks.len(), |i| i + 1);
    r.checks.insert(at, check);
}

/// `global config (~/.gitconfig) adds ".env"; this repository's config
/// (/src/app/.git/config) adds "web/.tscache", ".local-dev/*.lock"`:
/// consecutive rules from one file are grouped, in the order Git read them.
pub fn describe_sources(sources: &[RuleSource]) -> String {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for s in sources {
        let name = source_name(s);
        let rule = format!("{:?}", s.rule);
        match groups.last_mut() {
            Some((last, rules)) if *last == name => rules.push(rule),
            _ => groups.push((name, vec![rule])),
        }
    }
    groups
        .into_iter()
        .map(|(name, rules)| format!("{name} adds {}", rules.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `global config (~/.gitconfig)`, `this repository's config (...)`.
pub fn source_name(s: &RuleSource) -> String {
    let label = match s.scope.as_str() {
        "local" => "this repository's config".to_string(),
        "worktree" => "this worktree's config".to_string(),
        "global" => "global config".to_string(),
        "system" => "system config".to_string(),
        "residue_config" => "the residue config file".to_string(),
        "command_line" => "--regenerable".to_string(),
        other => format!("{other} config"),
    };
    match &s.origin {
        Some(origin) => format!("{label} ({})", crate::output::home(origin)),
        None => label,
    }
}

/// The rules a repository adds through `git config residue.regenerable`.
struct RepoRules {
    rules: Rules,
    /// Every value as read, valid or not, with its scope and file, for
    /// `explain` and JSON `rule_sources`.
    values: Vec<RuleSource>,
    /// An invalid value or a failed read, as (short, detail). It makes the
    /// `ignored` check unknown; it never aborts the scan.
    problem: Option<(String, String)>,
}

impl RepoRules {
    fn read(dir: &Path) -> RepoRules {
        let mut repo = RepoRules {
            rules: Rules::default(),
            values: Vec::new(),
            problem: None,
        };
        match git::regenerable_config(dir) {
            Ok(values) => {
                for source in &values {
                    let value = &source.rule;
                    if let Err(e) = repo.rules.add(value) {
                        repo.problem.get_or_insert((
                            format!("invalid {GIT_CONFIG_KEY} {value:?}"),
                            format!(
                                "{GIT_CONFIG_KEY} in {} has an invalid value {value:?}: {e}",
                                source_name(source)
                            ),
                        ));
                    }
                }
                repo.values = values;
            }
            Err(e) => repo.problem = Some((format!("{GIT_CONFIG_KEY} unreadable"), e)),
        }
        repo
    }
}

fn check_lock(r: &mut Report, entry: &Entry) {
    match &entry.locked {
        Some(reason) if reason.is_empty() => r.block(
            "locked",
            "locked",
            "locked with `git worktree lock`; residue never unlocks or forces",
        ),
        Some(reason) => r.block(
            "locked",
            format!("locked: {reason}"),
            format!("locked with `git worktree lock` ({reason}); residue never unlocks or forces"),
        ),
        None => r.pass("locked", "not locked"),
    }
}

fn check_activity(r: &mut Report, dir: &Path, snapshot: &Snapshot) {
    let evidence = snapshot.evidence(dir);
    match &evidence {
        Evidence::InUse(procs) => {
            let names: Vec<String> = procs
                .iter()
                .map(|(pid, cmd)| format!("{cmd}[{pid}]"))
                .collect();
            r.block(
                "activity",
                format!("in use: {}", examples(&names, 3)),
                format!("in use by {}", examples(&names, 8)),
            );
        }
        Evidence::NotObserved => r.pass(
            "activity",
            format!(
                "no process observed using it ({}; processes it cannot see are not checked)",
                snapshot.scope()
            ),
        ),
        Evidence::Unknown(u) => r.unknown(
            "activity",
            format!("activity unknown ({})", u.reason),
            format!("activity unknown: {} ({})", u.detail, u.reason),
        ),
    }
    r.activity = Some(evidence);
}

fn check_status(
    r: &mut Report,
    status: Result<git::Status, String>,
    repo: &RepoRules,
    rules: &Rules,
    buckets: &[Bucket],
    usage: &std::io::Result<Usage>,
) {
    let status = match status {
        Ok(s) => s,
        Err(e) => {
            for id in ["tracked_changes", "untracked", "ignored"] {
                r.unknown(id, "git status failed", e.clone());
            }
            return;
        }
    };
    if status.tracked.is_empty() {
        r.pass("tracked_changes", "no staged or modified tracked files");
    } else {
        let n = status.tracked.len();
        r.block(
            "tracked_changes",
            plural(n, "change", "changes"),
            format!(
                "{} staged or modified: {}",
                plural(n, "tracked file", "tracked files"),
                examples(&status.tracked, 5)
            ),
        );
    }
    if status.untracked.is_empty() {
        r.pass("untracked", "no untracked files");
    } else {
        let n = status.untracked.len();
        r.block(
            "untracked",
            plural(n, "untracked", "untracked"),
            format!(
                "{}: {}",
                plural(n, "untracked entry", "untracked entries"),
                examples(&status.untracked, 5)
            ),
        );
    }
    r.ignored = status
        .ignored
        .into_iter()
        .enumerate()
        .map(|(i, (path, _))| judge(path, rules, &buckets[i], i, usage))
        .collect();
    let kept: Vec<&Ignored> = r
        .ignored
        .iter()
        .filter(|i| i.judgement == Judgement::Kept)
        .collect();
    let unsure: Vec<&Ignored> = r
        .ignored
        .iter()
        .filter(|i| i.judgement == Judgement::Unknown)
        .collect();
    let deletable: Vec<&Ignored> = r.ignored.iter().filter(|i| i.regenerable()).collect();
    let from_config = if repo.values.is_empty() {
        String::new()
    } else {
        format!(
            ". Git config {GIT_CONFIG_KEY}: {}",
            describe_sources(&repo.values)
        )
    };
    let (status, short, detail) = if let Some((short, problem)) = &repo.problem {
        (
            Status::Unknown,
            short.clone(),
            format!(
                "{problem}. Ignored entries cannot be judged until it is fixed (`git config --show-origin --get-all {GIT_CONFIG_KEY}` lists each value and its file){from_config}"
            ),
        )
    } else if !kept.is_empty() {
        (
            Status::Block,
            format!("ignored: {}", ignored_examples(&kept, 2, Ignored::sized)),
            format!(
                "ignored entries not known to be regenerable: {}. `git worktree remove` would delete them without asking. If they are regenerable, add their names or paths with `git config --add {GIT_CONFIG_KEY} PATH` in this repository, the residue config file, or --regenerable{from_config}",
                ignored_examples(&kept, 8, Ignored::described)
            ),
        )
    } else if !unsure.is_empty() {
        (
            Status::Unknown,
            format!(
                "ignored: {} not judged",
                ignored_examples(&unsure, 2, Ignored::sized)
            ),
            format!(
                "could not judge every ignored entry: {}{from_config}",
                ignored_examples(&unsure, 8, Ignored::described)
            ),
        )
    } else if deletable.is_empty() {
        (
            Status::Pass,
            String::new(),
            format!("no ignored files{from_config}"),
        )
    } else {
        (
            Status::Pass,
            String::new(),
            format!(
                "every ignored entry is regenerable: {}{from_config}",
                ignored_examples(&deletable, 8, Ignored::described)
            ),
        )
    };
    r.push("ignored", status, short, detail);
}

/// Decide one ignored entry: matched by a rule itself, or (for a collapsed
/// directory a path rule reaches into) by the walk's verdict on its contents.
fn judge(
    path: String,
    rules: &Rules,
    bucket: &Bucket,
    index: usize,
    usage: &std::io::Result<Usage>,
) -> Ignored {
    let measured = usage
        .as_ref()
        .ok()
        .and_then(|u| u.buckets.get(index))
        .filter(|b| b.seen);
    let mut item = Ignored {
        judgement: Judgement::Kept,
        files: measured.map(|b| b.files),
        bytes: measured.map(|b| b.bytes),
        note: None,
        path,
    };
    if rules.matches(&item.path) {
        item.judgement = Judgement::Regenerable;
        return item;
    }
    if !bucket.expand {
        return item;
    }
    let (judgement, note) = match (usage, measured) {
        (Err(e), _) => (
            Judgement::Unknown,
            format!("could not walk the worktree: {e}"),
        ),
        (Ok(_), None) => (Judgement::Unknown, "the walk did not reach it".to_string()),
        (Ok(_), Some(b)) => match &b.expansion {
            Expansion::Matched => (
                Judgement::Regenerable,
                "every file in it matches a rule".to_string(),
            ),
            Expansion::Unmatched(p) => (Judgement::Kept, format!("{p} matches no rule")),
            Expansion::OverLimit => (
                Judgement::Kept,
                format!(
                    "over {} entries, too many to judge one by one",
                    fsinfo::EXPAND_LIMIT
                ),
            ),
            Expansion::Unreadable(e) => (Judgement::Unknown, format!("could not read {e}")),
            Expansion::NotRequested => (
                Judgement::Unknown,
                "its contents were not judged".to_string(),
            ),
        },
    };
    item.judgement = judgement;
    item.note = Some(note);
    item
}

fn check_index_flags(r: &mut Report, dir: &Path) {
    match git::hidden_from_status(dir) {
        Ok(hidden) if hidden.is_empty() => r.pass(
            "index_flags",
            "no tracked file is marked assume-unchanged or skip-worktree",
        ),
        Ok(hidden) => r.block(
            "index_flags",
            "hidden edits possible",
            format!(
                "{} marked assume-unchanged or skip-worktree, so `git status` cannot see edits to them: {}",
                plural(hidden.len(), "tracked file is", "tracked files are"),
                examples(&hidden, 5)
            ),
        ),
        Err(e) => r.unknown("index_flags", "git failed", e),
    }
}

fn check_operation(r: &mut Report, git_dir: &Path) {
    const MARKERS: &[(&str, &str)] = &[
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase or am"),
        ("MERGE_HEAD", "merge"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
        ("BISECT_LOG", "bisect"),
        ("sequencer", "cherry-pick or revert sequence"),
    ];
    let mut found = Vec::new();
    for (marker, name) in MARKERS {
        match fs::symlink_metadata(git_dir.join(marker)) {
            Ok(_) => found.push(name.to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                r.unknown(
                    "operation",
                    "git dir unreadable",
                    format!("cannot read {}: {e}", git_dir.join(marker).display()),
                );
                return;
            }
        }
    }
    if found.is_empty() {
        r.pass(
            "operation",
            "no rebase, merge, cherry-pick, revert or bisect in progress",
        );
    } else {
        found.dedup();
        r.block(
            "operation",
            format!("{} in progress", found.join(", ")),
            format!("{} in progress", found.join(", ")),
        );
    }
}

fn check_private_refs(r: &mut Report, dir: &Path) {
    match git::git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/worktree/",
            "refs/bisect/",
            "refs/rewritten/",
        ],
    ) {
        Ok(out) => {
            let refs: Vec<String> = String::from_utf8_lossy(&out)
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            if refs.is_empty() {
                r.pass("private_refs", "no worktree-private refs");
            } else {
                r.block(
                    "private_refs",
                    "private refs",
                    format!(
                        "worktree-private refs, deleted with the worktree: {}",
                        examples(&refs, 5)
                    ),
                );
            }
        }
        Err(e) => r.unknown("private_refs", "git failed", e),
    }
}

fn check_head(r: &mut Report, entry: &Entry, dir: &Path) {
    let Some(head) = entry.short_head() else {
        r.unknown(
            "head",
            "no HEAD",
            "Git reports no HEAD commit for this worktree",
        );
        return;
    };
    if let Some(branch) = entry.short_branch() {
        r.pass(
            "head",
            format!("on branch {branch}; removal keeps the branch and its commits"),
        );
        return;
    }
    match git::git(
        dir,
        &[
            "for-each-ref",
            "--count=1",
            "--contains=HEAD",
            "--format=%(refname)",
            "refs/heads/",
            "refs/tags/",
            "refs/remotes/",
        ],
    ) {
        Ok(out) => {
            let found = String::from_utf8_lossy(&out).trim().to_string();
            if found.is_empty() {
                r.block(
                    "head",
                    "unreferenced detached HEAD",
                    format!(
                        "detached HEAD {head} is on no branch, tag or remote-tracking ref; removing the worktree would leave it reachable only from the reflog"
                    ),
                );
            } else {
                r.pass(
                    "head",
                    format!("detached HEAD {head} is contained in {found}"),
                );
            }
        }
        Err(e) => r.unknown("head", "git failed", e),
    }
}

fn check_layout(r: &mut Report, dir: &Path) {
    let mut problems = Vec::new();
    match fs::symlink_metadata(dir.join(".gitmodules")) {
        Ok(_) => problems.push((
            "submodules".to_string(),
            "has submodules (.gitmodules); Git removes these only with --force, which residue never uses".to_string(),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            r.unknown("layout", "unreadable", format!("cannot read .gitmodules: {e}"));
            return;
        }
    }
    match git::git_exit(
        dir,
        &["config", "--type=bool", "--get", "core.sparseCheckout"],
    ) {
        Ok(exit) if exit.success && String::from_utf8_lossy(&exit.stdout).trim() == "true" => {
            problems.push((
                "sparse checkout".to_string(),
                "sparse checkout; residue reports these but does not remove them".to_string(),
            ));
        }
        // Exit 1 means the key is unset.
        Ok(exit) if exit.success || exit.code == Some(1) => {}
        Ok(exit) => {
            r.unknown(
                "layout",
                "git config failed",
                format!("git config failed: {}", exit.stderr),
            );
            return;
        }
        Err(e) => {
            r.unknown("layout", "git failed", e);
            return;
        }
    }
    if problems.is_empty() {
        r.pass("layout", "no submodules and not a sparse checkout");
    } else {
        let short: Vec<String> = problems.iter().map(|(s, _)| s.clone()).collect();
        let detail: Vec<String> = problems.into_iter().map(|(_, d)| d).collect();
        r.block("layout", short.join(", "), detail.join("; "));
    }
}

fn check_size(r: &mut Report, dir: &Path, usage: std::io::Result<Usage>) {
    let usage = match usage {
        Ok(u) => u,
        Err(e) => {
            r.unknown(
                "contents",
                "unreadable",
                format!("cannot measure the directory: {e}"),
            );
            return;
        }
    };
    let show = |paths: &[PathBuf]| -> String {
        let names: Vec<String> = paths
            .iter()
            .map(|p| {
                p.strip_prefix(dir)
                    .map(|rel| rel.display().to_string())
                    .unwrap_or_else(|_| p.display().to_string())
            })
            .collect();
        examples(&names, 3)
    };
    let mut blocked = false;
    if !usage.nested_repos.is_empty() {
        blocked = true;
        r.block(
            "nested_repo",
            "nested repository",
            format!(
                "contains another repository or checkout at {}; removal would delete it",
                show(&usage.nested_repos)
            ),
        );
    }
    if !usage.mount_points.is_empty() {
        blocked = true;
        r.block(
            "mount_point",
            "mount point",
            format!(
                "contains a mount point at {}; removal could delete files on another filesystem",
                show(&usage.mount_points)
            ),
        );
    }
    if usage.unreadable > 0 {
        r.unknown(
            "contents",
            "unreadable entries",
            format!(
                "could not read {}: {}",
                plural(usage.unreadable as usize, "entry", "entries"),
                usage.first_error.clone().unwrap_or_default()
            ),
        );
    } else if !blocked {
        r.pass(
            "contents",
            "every entry is readable; no nested repository or mount point",
        );
    }
    r.usage = Some(usage);
}

fn gather_facts(r: &mut Report, entry: &Entry, dir: &Path) {
    r.local_commits = local_commits(dir);
    r.upstream = match &entry.branch {
        Some(branch) => upstream(dir, branch),
        None => Ok(None),
    };
}

fn local_commits(dir: &Path) -> Result<Option<u64>, String> {
    let remotes = git::git(
        dir,
        &[
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "refs/remotes/",
        ],
    )?;
    if remotes.trim_ascii().is_empty() {
        return Ok(None);
    }
    let out = git::git(dir, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?;
    let text = String::from_utf8_lossy(&out);
    text.trim()
        .parse::<u64>()
        .map(Some)
        .map_err(|_| format!("git rev-list --count printed {:?}", text.trim()))
}

fn upstream(dir: &Path, branch: &str) -> Result<Option<Upstream>, String> {
    let out = git::git(
        dir,
        &[
            "for-each-ref",
            "--format=%(upstream:short)%00%(upstream:track)",
            branch,
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let line = text.lines().next().unwrap_or("");
    match line.split_once('\0') {
        Some((name, track)) if !name.is_empty() => Ok(Some(Upstream {
            name: name.to_string(),
            gone: track.contains("gone"),
        })),
        _ => Ok(None),
    }
}

/// Judge many worktrees in parallel; results come back in job order.
/// Measuring disk usage dominates, and it is independent per worktree.
pub fn all(jobs: &[(&Entry, &Path)], snapshot: &Snapshot, policy: &Policy) -> Vec<Report> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 8)
        .min(jobs.len().max(1));
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let next = &next;
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((entry, common)) = jobs.get(i) else {
                        break;
                    };
                    // The receiver lives until every worker has finished.
                    drop(tx.send((i, worktree(entry, common, snapshot, policy))));
                }
            });
        }
    });
    drop(tx);
    let mut slots: Vec<Option<Report>> = jobs.iter().map(|_| None).collect();
    for (i, report) in rx {
        slots[i] = Some(report);
    }
    // A worker that panicked re-raises its panic when the scope ends, so
    // every slot is filled by the time we get here.
    slots.into_iter().flatten().collect()
}
