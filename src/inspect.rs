//! The checks that decide whether one linked worktree may be removed.
//!
//! Each check passes, blocks, or is unknown. A worktree is a removal
//! candidate only when every check passes; one block keeps it, and one
//! unknown keeps it too. Facts that never block (commits only on a local
//! branch, a gone upstream, age) are reported separately so a reader sees
//! each one instead of a single "safe" label.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::activity::{Evidence, Snapshot};
use crate::fsinfo::{self, Bucket, BucketUsage, Expansion, Reach, Usage};
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
    /// The environment file inside it that keeps it, when a rule matches it
    /// but names no environment file inside (`policy::env_file`).
    pub env: Option<String>,
}

impl Ignored {
    pub fn regenerable(&self) -> bool {
        self.judgement == Judgement::Regenerable
    }

    /// The environment file that keeps it, `build/.env`, or else
    /// [`Ignored::sized`]: what the one-line summary names.
    fn brief(&self) -> String {
        self.env.clone().unwrap_or_else(|| self.sized())
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
    /// As Git abbreviates it, such as `origin/fix`.
    pub name: String,
    /// The full refname, such as `refs/remotes/origin/fix`, or
    /// `refs/heads/main` for a local upstream.
    pub full: String,
    pub gone: bool,
}

impl Upstream {
    /// A remote-tracking ref, so `gone` means the branch was deleted on a
    /// remote. A local upstream that is gone is a deleted local branch.
    pub fn remote(&self) -> bool {
        self.full.starts_with("refs/remotes/")
    }
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
    /// local refs see it (as of the last fetch). `Ok(None)` when the
    /// repository has no remote-tracking refs at all. Facts never block, so
    /// a failed query is kept as its error and shown, not hidden.
    pub local_commits: Result<Option<u64>, String>,
    pub upstream: Result<Option<Upstream>, String>,
    pub usage: Option<Usage>,
    /// The newest of the three change signals. `Ok(None)` when it was not
    /// measured (a stale or unidentified worktree).
    pub last_change: Result<Option<LastChange>, String>,
    /// Every rule beyond the built-in list that applies to this worktree,
    /// with its source: `--regenerable`, the residue config file, then Git
    /// config in the order Git read it.
    pub rule_sources: Vec<RuleSource>,
    /// What `remove` compares right before it acts. An error when residue
    /// could not record it, which keeps the worktree.
    pub fingerprint: Result<Fingerprint, String>,
    /// `residue restore` put it back (`removed::MARKER` in its Git
    /// directory), so `watch`'s landed-work rule never removes it again.
    /// An error when residue could not read the mark, which counts as
    /// marked for that rule.
    pub restored: Result<Option<crate::removed::Mark>, String>,
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
            fingerprint: Err("not measured".into()),
            restored: Ok(None),
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

pub(crate) fn examples(items: &[String], limit: usize) -> String {
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

/// What a repository keeps the way it keeps its main checkout. A bare
/// repository has no main checkout: every checkout is a linked worktree,
/// and the one on the branch its HEAD names is the one a person works in,
/// so residue keeps it (the `head` check). Read once per repository from
/// Git's list of its worktrees, whose first entry is the main one. A trash
/// purge's plan and its re-check read it too, so an entry on that branch
/// stays in the trash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// A repository with a main checkout, a bare one whose HEAD is
    /// detached.
    Nothing,
    /// A bare repository whose HEAD names this branch (`refs/heads/main`).
    Branch(String),
    /// A bare repository whose HEAD residue could not read: any branch may
    /// be the kept one.
    Unreadable(String),
}

impl Kept {
    /// `main` is the first entry of Git's list for the repository whose
    /// common directory is `common_dir`. Runs Git only for a bare one.
    pub fn of(main: &Entry, common_dir: &Path) -> Kept {
        if !main.bare {
            return Kept::Nothing;
        }
        match git::head_branch(common_dir) {
            Ok(Some(branch)) => Kept::Branch(branch),
            Ok(None) => Kept::Nothing,
            Err(e) => Kept::Unreadable(e),
        }
    }
}

/// Judge one linked worktree of the repository whose canonical common
/// directory is `common_dir`, which keeps what `kept` names.
pub fn worktree(
    entry: &Entry,
    common_dir: &Path,
    kept: &Kept,
    snapshot: &Snapshot,
    policy: &Policy,
) -> Report {
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
    r.restored = crate::removed::marked(&identity.git_dir);

    check_lock(&mut r, entry);
    // `git status` runs before the walk so the walk can size each ignored
    // entry and judge the collapsed directories a path rule reaches into,
    // and the activity check follows the walk, which tells it how long the
    // worktree's paths are and which files the probe could not name lie
    // inside it. The checks are still reported in their usual order.
    let mut status = git::status(&canonical);
    let hidden = status.as_mut().ok().and_then(|s| s.hidden.take());
    let repo = RepoRules::read(&canonical);
    let rules = policy.with(&repo.rules);
    let (buckets, usage) = walk(&canonical, &mut status, &rules, &snapshot.wanted());
    check_activity(
        &mut r,
        &canonical,
        snapshot,
        usage.as_ref().ok().map(|u| &u.reach),
        crate::remove::deleted_at(&canonical, &identity.common_dir),
    );
    r.last_change = last_change(&usage, &canonical, &identity.git_dir);
    r.fingerprint = match &status {
        Ok(s) => Fingerprint::of(entry.head.clone(), &identity.git_dir, s, &usage),
        Err(e) => Err(e.clone()),
    };
    check_status(&mut r, status, &repo, &rules, &buckets, &usage);
    check_index_flags(&mut r, &canonical, hidden);
    check_operation(&mut r, &identity.git_dir);
    check_private_refs(&mut r, &canonical);
    check_head(&mut r, entry, kept, &canonical);
    check_layout(&mut r, &canonical);
    check_size(&mut r, &canonical, usage);
    if let Some(idle) = &policy.idle {
        check_idle(&mut r, idle, policy.idle_at);
    }
    r.rule_sources = effective_sources(policy, repo.values);
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
/// other check about whether someone is using the worktree. The age is
/// measured at `at` (`remove --as-of`) when given, otherwise now.
fn check_idle(r: &mut Report, idle: &crate::cli::Span, at: Option<i64>) {
    let now = at.unwrap_or_else(crate::output::now);
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

/// `deleted_at` is the length of the longest path `dir` can have where
/// residue deletes it (`remove::deleted_at`).
fn check_activity(
    r: &mut Report,
    dir: &Path,
    snapshot: &Snapshot,
    reach: Option<&Reach>,
    deleted_at: usize,
) {
    let evidence = snapshot.evidence_at(dir, reach, deleted_at);
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
            format!("ignored: {}", ignored_examples(&kept, 2, Ignored::brief)),
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
/// Either way, an environment file inside that no rule names keeps it, and
/// an entry the walk did not reach is not judged: a rule alone never makes
/// it regenerable, since the walk did not look at what it holds.
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
    let item = judge_rules(path, rules, bucket, usage, measured);
    if item.regenerable() {
        let unreached = match (usage, measured) {
            (Err(e), _) => Some(format!("could not walk the worktree: {e}")),
            (Ok(_), None) => Some("the walk did not reach it".to_string()),
            (Ok(_), Some(_)) => None,
        };
        if let Some(note) = unreached {
            return Ignored {
                judgement: Judgement::Unknown,
                note: Some(note),
                ..item
            };
        }
    }
    match measured.and_then(|b| b.env_file.clone()) {
        Some(env) if item.regenerable() => Ignored {
            judgement: Judgement::Kept,
            note: Some(format!("{env} is an environment file no rule names")),
            env: Some(env),
            ..item
        },
        _ => item,
    }
}

fn judge_rules(
    path: String,
    rules: &Rules,
    bucket: &Bucket,
    usage: &std::io::Result<Usage>,
    measured: Option<&BucketUsage>,
) -> Ignored {
    let mut item = Ignored {
        judgement: Judgement::Kept,
        files: measured.map(|b| b.files),
        bytes: measured.map(|b| b.bytes),
        note: None,
        env: None,
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

/// `hidden` is the answer `git::status` already read, if it read one.
fn check_index_flags(r: &mut Report, dir: &Path, hidden: Option<Result<Vec<String>, String>>) {
    match hidden.unwrap_or_else(|| git::hidden_from_status(dir)) {
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

/// The operations in progress in the worktree's Git directory: a rebase,
/// merge, cherry-pick, revert or bisect, each named once. `Err` when a
/// marker cannot be read.
pub fn operations(git_dir: &Path) -> Result<Vec<String>, String> {
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
                return Err(format!(
                    "cannot read {}: {e}",
                    git_dir.join(marker).display()
                ));
            }
        }
    }
    found.dedup();
    Ok(found)
}

fn check_operation(r: &mut Report, git_dir: &Path) {
    match operations(git_dir) {
        Err(e) => r.unknown("operation", "git dir unreadable", e),
        Ok(found) if found.is_empty() => r.pass(
            "operation",
            "no rebase, merge, cherry-pick, revert or bisect in progress",
        ),
        Ok(found) => r.block(
            "operation",
            format!("{} in progress", found.join(", ")),
            format!("{} in progress", found.join(", ")),
        ),
    }
}

/// The worktree's private refs (`refs/worktree/`, `refs/bisect/`,
/// `refs/rewritten/`), which `git worktree remove` deletes with it, as
/// Git lists them in `dir`.
pub fn private_refs(dir: &Path) -> Result<Vec<String>, String> {
    let out = git::git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/worktree/",
            "refs/bisect/",
            "refs/rewritten/",
        ],
    )?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

fn check_private_refs(r: &mut Report, dir: &Path) {
    match private_refs(dir) {
        Ok(refs) if refs.is_empty() => r.pass("private_refs", "no worktree-private refs"),
        Ok(refs) => r.block(
            "private_refs",
            "private refs",
            format!(
                "worktree-private refs, deleted with the worktree: {}",
                examples(&refs, 5)
            ),
        ),
        Err(e) => r.unknown("private_refs", "git failed", e),
    }
}

fn check_head(r: &mut Report, entry: &Entry, kept: &Kept, dir: &Path) {
    let Some(head) = entry.short_head() else {
        r.unknown(
            "head",
            "no HEAD",
            "Git reports no HEAD commit for this worktree",
        );
        return;
    };
    if let Some(branch) = entry.short_branch() {
        match kept {
            Kept::Branch(main) if entry.branch.as_deref() == Some(main.as_str()) => r.block(
                "head",
                "the bare repository's HEAD branch",
                format!(
                    "on {branch}, the branch the bare repository's HEAD names; kept like a main checkout"
                ),
            ),
            Kept::Unreadable(e) => r.unknown(
                "head",
                "bare HEAD unreadable",
                format!(
                    "cannot read which branch the bare repository's HEAD names, the one residue keeps like a main checkout: {e}"
                ),
            ),
            Kept::Nothing | Kept::Branch(_) => r.pass(
                "head",
                format!("on branch {branch}; removal keeps the branch and its commits"),
            ),
        }
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
                "contains a .git entry at {}: another repository or checkout, or files Git does not list; removal would delete it",
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

/// The upstream of `branch` as the local refs show it: `Ok(None)` when it
/// has none. `remove --only-if-upstream-gone` reads it again right before
/// the deletion.
pub fn upstream(dir: &Path, branch: &str) -> Result<Option<Upstream>, String> {
    let out = git::git(
        dir,
        &[
            "for-each-ref",
            "--format=%(upstream)%00%(upstream:short)%00%(upstream:track)",
            branch,
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let line = text.lines().next().unwrap_or("");
    let mut fields = line.split('\0');
    let (Some(full), Some(name), Some(track)) = (fields.next(), fields.next(), fields.next())
    else {
        return Ok(None);
    };
    if name.is_empty() {
        return Ok(None);
    }
    let gone = track.contains("gone") && ref_absent(dir, full, name)?;
    Ok(Some(Upstream {
        name: name.to_string(),
        full: full.to_string(),
        gone,
    }))
}

/// Git marks an upstream `[gone]` when its ref is missing, and also when
/// the ref exists but does not resolve to a commit: it names another kind
/// of object, or it is a symbolic ref to a missing ref. Only a missing ref
/// is a branch deleted on the remote, so anything else is an error, which
/// `--only-if-upstream-gone` treats as unknown and keeps the worktree.
fn ref_absent(dir: &Path, full: &str, name: &str) -> Result<bool, String> {
    if !full.starts_with("refs/") {
        return Err(format!(
            "Git marks the upstream {name} gone without naming its ref"
        ));
    }
    let symbolic = git::git_exit(dir, &["symbolic-ref", "-q", full])?;
    if symbolic.code != Some(1) {
        return Err(format!(
            "Git marks the upstream {name} gone, but {full} is a symbolic ref or cannot be read"
        ));
    }
    let shown = git::git_exit(dir, &["show-ref", "--verify", "--quiet", full])?;
    match shown.code {
        Some(1) => Ok(true),
        Some(0) => Err(format!(
            "Git marks the upstream {name} gone, but {full} exists: it does not resolve to a commit, or it came back just now"
        )),
        _ => Err(format!(
            "could not check whether {full} exists: {}",
            shown.stderr.trim()
        )),
    }
}

/// One worktree to judge: its entry, its repository's common directory, and
/// what that repository keeps (`Kept::of`, read once per repository).
pub type Job<'a> = (&'a Entry, &'a Path, &'a Kept);

/// Judge many worktrees in parallel; results come back in job order.
/// Measuring disk usage dominates, and it is independent per worktree.
pub fn all(jobs: &[Job], snapshot: &Snapshot, policy: &Policy) -> Vec<Report> {
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
                    let Some((entry, common, kept)) = jobs.get(i) else {
                        break;
                    };
                    // The receiver lives until every worker has finished.
                    drop(tx.send((i, worktree(entry, common, kept, snapshot, policy))));
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

/// Walk the worktree once, sizing each ignored entry of `status`. A
/// `--ignored=matching` list then loses the entries `--ignored=traditional`
/// does not print, so every check judges the list it always judged (#83).
/// When Git fails to answer for an entry, `status` becomes that error, as
/// if `git status` had failed. `wanted` is `Snapshot::wanted`.
fn walk(
    dir: &Path,
    status: &mut Result<git::Status, String>,
    rules: &Rules,
    wanted: &[(u64, u64)],
) -> (Vec<Bucket>, io::Result<Usage>) {
    let matcher = |rel: &str| rules.matches(rel);
    let named = |rel: &str| rules.names_file(rel);
    let Ok(listed) = status else {
        return (
            Vec::new(),
            fsinfo::measure(dir, &[], &matcher, &named, wanted),
        );
    };
    let mut buckets = buckets_for(listed, rules);
    let mut usage = fsinfo::measure(dir, &buckets, &matcher, &named, wanted);
    let settled = match &mut usage {
        Ok(usage) if listed.matching => drop_unlisted(dir, listed, &mut buckets, usage),
        _ => Ok(()),
    };
    if let Err(e) = settled {
        *status = Err(e);
    }
    (buckets, usage)
}

/// Whether `--ignored=traditional` prints an entry of a `--ignored=matching`
/// list. Both modes list a file. Matching mode also lists an ignored
/// directory that holds nothing Git lists, which traditional mode hides
/// (`treat_directory` in Git's dir.c).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Listing {
    Both,
    MatchingOnly,
    /// Only Git can tell: the walk did not reach the directory or read all
    /// of it, or found a `.git` entry, which may make a nested repository.
    Unsure,
}

fn listing(text: &str, measured: &BucketUsage) -> Listing {
    if !text.ends_with('/') || (measured.seen && measured.holds_file) {
        Listing::Both
    } else if !measured.seen || measured.holds_git || measured.incomplete {
        Listing::Unsure
    } else {
        Listing::MatchingOnly
    }
}

/// Drop the entries of a `--ignored=matching` list that
/// `--ignored=traditional` would not print, with their buckets. Entries in
/// the folders a limited traditional run listed (`git::Status::limited`)
/// are that run's, so they stay. The entries the walk cannot settle are
/// looked up in traditional mode's list of them, which costs one more `git
/// status`, limited to those entries.
fn drop_unlisted(
    dir: &Path,
    status: &mut git::Status,
    buckets: &mut Vec<Bucket>,
    usage: &mut Usage,
) -> Result<(), String> {
    if status.ignored.len() != buckets.len() || buckets.len() != usage.buckets.len() {
        return Err("the walk did not measure every ignored entry".to_string());
    }
    // An entry in a limited folder is traditional mode's own, so it stays,
    // even one the walk could not settle (#120).
    let kinds: Vec<Listing> = status
        .ignored
        .iter()
        .zip(&usage.buckets)
        .map(|((text, rel), measured)| {
            if status.traditional_listed(rel) {
                Listing::Both
            } else {
                listing(text, measured)
            }
        })
        .collect();
    if kinds.iter().all(|kind| *kind == Listing::Both) {
        return Ok(());
    }
    let unsure: Vec<&Path> = status
        .ignored
        .iter()
        .zip(&kinds)
        .filter(|(_, kind)| **kind == Listing::Unsure)
        .map(|((_, rel), _)| rel.as_path())
        .collect();
    let traditional: HashSet<String> = if unsure.is_empty() {
        HashSet::new()
    } else {
        let listed = git::traditional_ignored_in(dir, &unsure)?;
        listed.into_iter().map(|(text, _)| text).collect()
    };
    let entries = std::mem::take(&mut status.ignored);
    let walked = std::mem::take(buckets);
    let measured = std::mem::take(&mut usage.buckets);
    let rows = entries.into_iter().zip(walked).zip(measured).zip(kinds);
    for (((entry, bucket), measured), kind) in rows {
        let printed = match kind {
            Listing::Both => true,
            Listing::MatchingOnly => false,
            Listing::Unsure => traditional.contains(&entry.0),
        };
        if printed {
            status.ignored.push(entry);
            buckets.push(bucket);
            usage.buckets.push(measured);
        }
    }
    Ok(())
}

/// One walk bucket per ignored entry. A collapsed directory that no rule
/// matches but a path rule reaches into is judged entry by entry.
fn buckets_for(status: &git::Status, rules: &Rules) -> Vec<Bucket> {
    status
        .ignored
        .iter()
        .map(|(text, rel)| Bucket {
            rel: rel.clone(),
            expand: text.ends_with('/') && !rules.matches(text) && rules.reaches_inside(text),
        })
        .collect()
}

/// The rules beyond the built-in list that apply to one worktree, each with
/// its source: `--regenerable` first, then the residue config file, then
/// the repository's Git config rules in the order Git read them (system,
/// global, local, worktree). Rules add up, so no source overrides another:
/// a rule given twice is listed once per source, the most explicit first.
fn effective_sources(policy: &Policy, repo: Vec<RuleSource>) -> Vec<RuleSource> {
    policy.sources.iter().cloned().chain(repo).collect()
}

/// A modification time: seconds and nanoseconds.
pub type Stamp = (i64, i64);

/// What `remove` saw of one worktree when it made the plan. It is compared
/// with the worktree's state right before the worktree is moved aside, and
/// again after, so residue deletes what the plan showed or nothing.
#[derive(Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// The worktree's own Git directory (`.git/worktrees/NAME`). It stays
    /// where it is when the checkout is moved.
    pub git_dir: PathBuf,
    pub head: Option<String>,
    /// `index` and `logs/HEAD` in the Git directory; `None` when absent.
    /// `logs/HEAD` is fine here, unlike for `--idle`: a `git gc` between
    /// plan and removal only makes residue skip, never delete.
    pub index: Option<Stamp>,
    pub reflog: Option<Stamp>,
    pub tracked: Vec<String>,
    pub untracked: Vec<String>,
    pub ignored: Vec<Print>,
}

/// One ignored entry as the walk measured it. `None` when the walk did not
/// reach it.
#[derive(Clone, PartialEq, Eq)]
pub struct Print {
    pub path: String,
    pub files: Option<u64>,
    pub bytes: Option<u64>,
    pub newest: Option<Stamp>,
}

fn stamp(path: &Path) -> Result<Option<Stamp>, String> {
    use std::os::unix::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(Some((m.mtime(), m.mtime_nsec()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// What the walk of a moved worktree at `dir` found that the plan could not
/// have held, since only a worktree whose walk found none of it is a
/// candidate. An ignored entry is `incomplete` only beside one of these.
fn found_after_move(dir: &Path, usage: &Usage) -> Option<String> {
    let at = |paths: &[PathBuf]| {
        let rel: Vec<String> = paths
            .iter()
            .map(|p| p.strip_prefix(dir).unwrap_or(p).display().to_string())
            .collect();
        examples(&rel, 3)
    };
    if !usage.nested_repos.is_empty() {
        return Some(format!(
            "contains a .git entry at {} after residue moved it aside",
            at(&usage.nested_repos)
        ));
    }
    if !usage.mount_points.is_empty() {
        return Some(format!(
            "contains a mount point at {} after residue moved it aside",
            at(&usage.mount_points)
        ));
    }
    if usage.unreadable > 0 {
        let first = usage.first_error.as_deref().unwrap_or_default();
        let inside = format!("{}/", dir.display());
        return Some(format!(
            "could not read {} after residue moved it aside: {}",
            plural(usage.unreadable as usize, "entry", "entries"),
            first.strip_prefix(&inside).unwrap_or(first)
        ));
    }
    None
}

impl Fingerprint {
    fn of(
        head: Option<String>,
        git_dir: &Path,
        status: &git::Status,
        usage: &io::Result<Usage>,
    ) -> Result<Fingerprint, String> {
        let usage = usage
            .as_ref()
            .map_err(|e| format!("cannot walk the worktree: {e}"))?;
        let ignored = status
            .ignored
            .iter()
            .enumerate()
            .map(|(i, (path, _))| {
                let b = usage.buckets.get(i).filter(|b| b.seen);
                Print {
                    path: path.clone(),
                    files: b.map(|b| b.files),
                    bytes: b.map(|b| b.bytes),
                    newest: b.and_then(|b| b.newest),
                }
            })
            .collect();
        Ok(Fingerprint {
            git_dir: git_dir.to_path_buf(),
            head,
            index: stamp(&git_dir.join("index"))?,
            reflog: stamp(&git_dir.join("logs").join("HEAD"))?,
            tracked: status.tracked.clone(),
            untracked: status.untracked.clone(),
            ignored,
        })
    }

    /// The same measurement taken again at `dir`, the worktree's directory
    /// now (after a move, its new path), with `head` as Git lists it there.
    /// It refuses what `check_size` refuses, which the fingerprint does not
    /// hold: an entry the walk cannot read, a nested repository or a mount
    /// point (#106). Each error is the reason, a whole sentence. With it
    /// comes what the walk reached, for the activity check (`wanted` is
    /// `Snapshot::wanted`), when the walk ran.
    pub fn again(
        &self,
        dir: &Path,
        head: Option<String>,
        policy: &Policy,
        wanted: &[(u64, u64)],
    ) -> (Result<Fingerprint, String>, Option<Reach>) {
        let unmeasured = |e: String| format!("cannot measure it after moving it aside: {e}");
        let status = match git::status(dir) {
            Ok(status) => status,
            Err(e) => return (Err(unmeasured(e)), None),
        };
        let mut status = Ok(status);
        let repo = RepoRules::read(dir);
        if let Some((_, problem)) = &repo.problem {
            return (Err(unmeasured(problem.clone())), None);
        }
        let rules = policy.with(&repo.rules);
        let (_, usage) = walk(dir, &mut status, &rules, wanted);
        let reach = usage.as_ref().ok().map(|u| u.reach.clone());
        if let Some(found) = usage.as_ref().ok().and_then(|u| found_after_move(dir, u)) {
            return (Err(found), reach);
        }
        let now = match status {
            Ok(status) => Fingerprint::of(head, &self.git_dir, &status, &usage).map_err(unmeasured),
            Err(e) => Err(unmeasured(e)),
        };
        (now, reach)
    }

    /// The first difference from `now`, as a sentence that names what
    /// changed, such as `node_modules/ changed since you confirmed (1204 ->
    /// 1388 files)`. `since` is `you confirmed` or `the plan`.
    pub fn changed(&self, now: &Fingerprint, since: &str) -> Option<String> {
        if self == now {
            return None;
        }
        let short = |h: &Option<String>| {
            h.as_deref()
                .map_or("none".to_string(), |h| h[..h.len().min(12)].to_string())
        };
        if self.head != now.head {
            return Some(format!(
                "HEAD changed since {since} ({} -> {})",
                short(&self.head),
                short(&now.head)
            ));
        }
        if self.tracked != now.tracked {
            return Some(format!(
                "tracked files changed since {since} (now {} staged or modified: {})",
                now.tracked.len(),
                examples(&now.tracked, 3)
            ));
        }
        if self.untracked != now.untracked {
            return Some(format!(
                "untracked files changed since {since} (now {}: {})",
                plural(now.untracked.len(), "entry", "entries"),
                examples(&now.untracked, 3)
            ));
        }
        for before in &self.ignored {
            let Some(after) = now.ignored.iter().find(|p| p.path == before.path) else {
                return Some(format!("{} disappeared since {since}", before.path));
            };
            if before == after {
                continue;
            }
            let detail = match (before.files, after.files, before.bytes, after.bytes) {
                (Some(a), Some(b), _, _) if a != b => {
                    format!(
                        "{} -> {} files",
                        crate::output::grouped(a),
                        crate::output::grouped(b)
                    )
                }
                (_, _, Some(a), Some(b)) if a != b => {
                    format!(
                        "{} -> {} bytes",
                        crate::output::grouped(a),
                        crate::output::grouped(b)
                    )
                }
                (Some(_), None, _, _) => "the walk no longer reaches it".to_string(),
                _ => match after.newest {
                    Some((secs, _)) => {
                        format!("same size, modified at {}", crate::output::clock(secs))
                    }
                    None => "modified".to_string(),
                },
            };
            return Some(format!("{} changed since {since} ({detail})", before.path));
        }
        if let Some(added) = now
            .ignored
            .iter()
            .find(|p| !self.ignored.iter().any(|b| b.path == p.path))
        {
            return Some(format!(
                "ignored {} appeared since {since}",
                match (added.files, added.bytes) {
                    (Some(f), Some(b)) => format!(
                        "{} ({}, {})",
                        added.path,
                        plural(f as usize, "file", "files"),
                        crate::output::bytes(b)
                    ),
                    _ => added.path.clone(),
                }
            ));
        }
        if self.index != now.index {
            return Some(format!(
                "its index changed since {since} (git add, commit, reset, checkout, or a git status that refreshed it)"
            ));
        }
        if self.reflog != now.reflog {
            return Some(format!("its HEAD reflog changed since {since}"));
        }
        Some(format!("its Git directory changed since {since}"))
    }
}

#[cfg(test)]
mod tests {
    //! The verdict fold in `Report::finish`, on synthetic checks only: no
    //! filesystem, Git, process probe or clock. The oracle is the contract
    //! in the README ("What makes a worktree a candidate"): a stale entry is
    //! stale; any failed check keeps the worktree; otherwise any check
    //! residue could not complete, or no checks at all, makes it unknown;
    //! only a non-empty set of passing checks makes a candidate. And the
    //! `ignored` check on a synthetic walk.

    use super::{
        Bucket, BucketUsage, Check, Entry, Judgement, RepoRules, Report, Rules, Status, Usage,
        check_status, git,
    };
    use Status::{Block as B, Pass as P, Unknown as U};
    use std::path::PathBuf;

    const IDS: [&str; 4] = ["identity", "activity", "tracked_changes", "ignored"];

    fn entry() -> Entry {
        Entry {
            path: PathBuf::from("/nonexistent/residue-verdict-test"),
            head: None,
            branch: None,
            bare: false,
            detached: false,
            locked: None,
            prunable: None,
        }
    }

    fn report(statuses: &[Status], stale: bool) -> Report {
        let mut r = Report::new(&entry());
        r.checks = statuses
            .iter()
            .enumerate()
            .map(|(i, &status)| Check {
                id: IDS[i % IDS.len()],
                status,
                short: String::new(),
                detail: String::new(),
            })
            .collect();
        if stale {
            r.stale_reason = Some("the directory no longer exists".into());
        }
        r.finish()
    }

    /// The statuses as their JSON codes, for assertion messages.
    fn show(statuses: &[Status]) -> Vec<&'static str> {
        statuses.iter().map(|s| s.code()).collect()
    }

    fn verdict(statuses: &[Status]) -> &'static str {
        report(statuses, false).verdict.code()
    }

    /// The README contract as a table over what a set of checks contains.
    fn expected(statuses: &[Status]) -> &'static str {
        let has = |s: Status| statuses.contains(&s);
        match (statuses.is_empty(), has(B), has(U)) {
            (true, _, _) => "unknown",
            (false, true, _) => "keep",
            (false, false, true) => "unknown",
            (false, false, false) => "candidate",
        }
    }

    /// Every sequence of up to `max` statuses.
    fn sequences(max: usize) -> Vec<Vec<Status>> {
        let mut all = vec![Vec::new()];
        let mut layer = vec![Vec::new()];
        for _ in 0..max {
            let mut next = Vec::new();
            for s in &layer {
                for x in [P, B, U] {
                    let mut t = s.clone();
                    t.push(x);
                    next.push(t);
                }
            }
            all.extend(next.iter().cloned());
            layer = next;
        }
        all
    }

    #[test]
    fn no_checks_is_unknown_not_candidate() {
        assert_eq!(verdict(&[]), "unknown");
    }

    #[test]
    fn passing_checks_make_a_candidate() {
        assert_eq!(verdict(&[P]), "candidate");
        assert_eq!(verdict(&[P, P, P, P]), "candidate");
    }

    #[test]
    fn a_failed_check_keeps() {
        assert_eq!(verdict(&[B]), "keep");
        assert_eq!(verdict(&[B, B]), "keep");
        assert_eq!(verdict(&[P, B]), "keep");
        assert_eq!(verdict(&[B, P, P]), "keep");
    }

    #[test]
    fn an_unknown_check_without_a_failure_is_unknown() {
        assert_eq!(verdict(&[U]), "unknown");
        assert_eq!(verdict(&[U, U]), "unknown");
        assert_eq!(verdict(&[P, U]), "unknown");
        assert_eq!(verdict(&[U, P, P]), "unknown");
    }

    #[test]
    fn a_failed_check_outranks_an_unknown_one() {
        assert_eq!(verdict(&[B, U]), "keep");
        assert_eq!(verdict(&[U, B]), "keep");
        assert_eq!(verdict(&[P, B, U]), "keep");
        assert_eq!(verdict(&[U, P, B]), "keep");
    }

    #[test]
    fn stale_outranks_every_check() {
        for statuses in [&[][..], &[P], &[B], &[U], &[P, B, U]] {
            assert_eq!(
                report(statuses, true).verdict.code(),
                "stale",
                "{:?}",
                show(statuses)
            );
        }
    }

    #[test]
    fn every_set_of_up_to_five_checks_matches_the_contract() {
        for s in sequences(5) {
            assert_eq!(verdict(&s), expected(&s), "{:?}", show(&s));
        }
    }

    #[test]
    fn order_does_not_change_the_verdict() {
        for s in sequences(4) {
            let mut reversed = s.clone();
            reversed.reverse();
            assert_eq!(verdict(&s), verdict(&reversed), "{:?}", show(&s));
            for k in 0..s.len() {
                let mut rotated = s.clone();
                rotated.rotate_left(k);
                assert_eq!(
                    verdict(&s),
                    verdict(&rotated),
                    "{:?} rotated by {k}",
                    show(&s)
                );
            }
        }
    }

    /// An ignored directory a rule matches, and the `ignored` check, when
    /// the walk tied Git's entry to the folder it measured, when it did not,
    /// and when it could not walk at all.
    fn ignored_check(measured: Option<BucketUsage>, walked: bool) -> (Judgement, Status, String) {
        let mut rules = Rules::default();
        rules.add("build").unwrap();
        let status = git::Status {
            ignored: vec![("app/build/".to_string(), PathBuf::from("app/build"))],
            ..git::Status::default()
        };
        let buckets = [Bucket {
            rel: PathBuf::from("app/build"),
            expand: false,
        }];
        let usage = if walked {
            Ok(Usage {
                buckets: vec![measured.unwrap_or_default()],
                ..Usage::default()
            })
        } else {
            Err(std::io::Error::other("no folder"))
        };
        let repo = RepoRules {
            rules: Rules::default(),
            values: Vec::new(),
            problem: None,
        };
        let mut r = Report::new(&entry());
        check_status(&mut r, Ok(status), &repo, &rules, &buckets, &usage);
        let check = r.checks.iter().find(|c| c.id == "ignored").unwrap();
        (r.ignored[0].judgement, check.status, check.detail.clone())
    }

    #[test]
    fn an_ignored_entry_the_walk_did_not_reach_is_not_judged() {
        let seen = |env: Option<&str>| BucketUsage {
            seen: true,
            files: 2,
            bytes: 10,
            holds_file: true,
            env_file: env.map(str::to_string),
            ..BucketUsage::default()
        };
        let (j, s, _) = ignored_check(Some(seen(None)), true);
        assert!(
            j == Judgement::Regenerable && s == P,
            "measured, nothing kept"
        );
        let (j, s, _) = ignored_check(Some(seen(Some("app/build/.env"))), true);
        assert!(
            j == Judgement::Kept && s == B,
            "measured, with an environment file"
        );
        let (j, s, detail) = ignored_check(None, true);
        assert!(j == Judgement::Unknown && s == U, "never reached: {detail}");
        assert!(
            detail.contains("app/build/ (the walk did not reach it)"),
            "{detail}"
        );
        let (j, s, detail) = ignored_check(None, false);
        assert!(j == Judgement::Unknown && s == U, "no walk: {detail}");
        assert!(
            detail.contains("could not walk the worktree: no folder"),
            "{detail}"
        );
    }

    #[test]
    fn adding_a_failed_or_unknown_check_never_makes_a_candidate() {
        for s in sequences(4) {
            for x in [B, U] {
                for at in 0..=s.len() {
                    let mut t = s.clone();
                    t.insert(at, x);
                    assert_ne!(
                        verdict(&t),
                        "candidate",
                        "{:?} with {} at {at}",
                        show(&s),
                        x.code()
                    );
                }
            }
        }
    }
}
