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
use crate::fsinfo::{self, Usage};
use crate::git::{self, Entry};
use crate::policy::Policy;

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

pub struct Ignored {
    pub path: String,
    pub regenerable: bool,
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
    check_status(&mut r, &canonical, policy);
    check_index_flags(&mut r, &canonical);
    check_operation(&mut r, &identity.git_dir);
    check_private_refs(&mut r, &canonical);
    check_head(&mut r, entry, &canonical);
    check_layout(&mut r, &canonical);
    check_size(&mut r, &canonical);
    gather_facts(&mut r, entry, &canonical);
    r.finish()
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

fn check_status(r: &mut Report, dir: &Path, policy: &Policy) {
    let status = match git::status(dir) {
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
        .map(|path| Ignored {
            regenerable: policy.regenerable(&path),
            path,
        })
        .collect();
    let kept: Vec<String> = r
        .ignored
        .iter()
        .filter(|i| !i.regenerable)
        .map(|i| i.path.clone())
        .collect();
    let deletable: Vec<String> = r
        .ignored
        .iter()
        .filter(|i| i.regenerable)
        .map(|i| i.path.clone())
        .collect();
    if !kept.is_empty() {
        r.block(
            "ignored",
            format!("ignored: {}", examples(&kept, 2)),
            format!(
                "ignored entries not known to be regenerable: {}. `git worktree remove` would delete them without asking. If they are regenerable, add their names to the residue config or pass --regenerable",
                examples(&kept, 8)
            ),
        );
    } else if deletable.is_empty() {
        r.pass("ignored", "no ignored files");
    } else {
        r.pass(
            "ignored",
            format!(
                "every ignored entry is regenerable: {}",
                examples(&deletable, 6)
            ),
        );
    }
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

fn check_size(r: &mut Report, dir: &Path) {
    let usage = match fsinfo::measure(dir) {
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
