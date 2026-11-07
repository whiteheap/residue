//! `residue scan` and `residue explain`, plus the path lookup `remove` shares.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::activity::Snapshot;
use crate::cli::Options;
use crate::discover::{self, Repo};
use crate::fsinfo::{self, Space};
use crate::git::{self, Entry};
use crate::inspect::{self, Report, Verdict};
use crate::json::Json;
use crate::output::{self, Places, Style, clean};
use crate::policy::{GIT_CONFIG_KEY, Policy};
use crate::remove::{self, Leftover};
use crate::removed;
use crate::trash;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn fail(message: &str) -> u8 {
    eprintln!("residue: {}", clean(message));
    1
}

pub fn prepare(options: &Options) -> Result<Policy, String> {
    git::require_version()?;
    let mut policy = Policy::load(&options.regenerable)?;
    policy.ignored_environment = git::environment_regenerable();
    policy.idle = options.idle.clone();
    note_ignored_environment(&policy.ignored_environment);
    Ok(policy)
}

const ENVIRONMENT_SOURCES: &str = "git -c, GIT_CONFIG_COUNT, GIT_CONFIG_PARAMETERS or GIT_CONFIG";

/// residue ignores rules the environment sets (`git::CONFIG_ENV`), and
/// says so on stderr, in text and JSON mode alike, so a user who set one
/// learns why it had no effect and what to use instead.
fn note_ignored_environment(ignored: &Result<Vec<String>, String>) {
    let instead = format!(
        "residue reads rules only from Git config files, the residue config file and --regenerable. \
         Pass --regenerable RULE for one run, or run `git config --add {GIT_CONFIG_KEY} RULE` \
         in the repository to keep the rule"
    );
    match ignored {
        Ok(values) if values.is_empty() => {}
        Ok(values) => {
            let list: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
            eprintln!(
                "residue: note: ignored {GIT_CONFIG_KEY} set through the environment ({ENVIRONMENT_SOURCES}): {}",
                clean(&list.join(", "))
            );
            eprintln!("residue: note: {instead}");
        }
        Err(e) => {
            eprintln!(
                "residue: note: could not check the environment ({ENVIRONMENT_SOURCES}) for {GIT_CONFIG_KEY}: {}",
                clean(e)
            );
            eprintln!("residue: note: any value set there is ignored. {instead}");
        }
    }
}

/// `residue 0.0.4 · as of 14:03:12 +02:00`: the moment the snapshot started.
/// Worktrees created after it are not in the report.
pub fn header(started: i64) -> String {
    format!("residue {VERSION} · as of {}", output::clock(started))
}

pub fn scan(options: &Options) -> u8 {
    let started = output::now();
    let policy = match prepare(options) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let roots = if options.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        options.paths.clone()
    };
    let found = discover::find(&roots);
    let snapshot = Snapshot::take();
    let jobs: Vec<(&Entry, &Path)> = found
        .repos
        .iter()
        .flat_map(|repo| {
            repo.linked
                .iter()
                .map(move |e| (e, repo.common_dir.as_path()))
        })
        .collect();
    let mut reports = inspect::all(&jobs, &snapshot, &policy).into_iter();
    let mut errors = found.errors.clone();
    let mut grouped: Vec<Group> = Vec::new();
    for repo in &found.repos {
        let mut list: Vec<Report> = reports.by_ref().take(repo.linked.len()).collect();
        list.sort_by_key(|r| std::cmp::Reverse(r.usage.as_ref().map_or(0, |u| u.allocated)));
        let space = fsinfo::space(&repo.main.path)
            .or_else(|_| fsinfo::space(&repo.common_dir))
            .ok();
        let trash =
            trash::list(&repo.common_dir, &repo.command_dir(), &repo.trash).unwrap_or_else(|e| {
                errors.push(e);
                Vec::new()
            });
        let records = removed::of_repo(repo).unwrap_or_else(|e| {
            errors.push(e);
            Vec::new()
        });
        let leftovers = repo
            .interrupted
            .iter()
            .filter_map(|e| Leftover::of(e, &repo.command_dir()))
            .collect();
        grouped.push(Group {
            repo,
            list,
            space,
            trash,
            records,
            leftovers,
        });
    }
    let candidates: Vec<&Report> = grouped
        .iter()
        .flat_map(|g| g.list.iter())
        .filter(|r| r.verdict == Verdict::Candidate)
        .collect();
    let allocated: u64 = candidates
        .iter()
        .filter_map(|r| r.usage.as_ref())
        .map(|u| u.allocated)
        .sum();
    let reclaimable: u64 = candidates
        .iter()
        .filter_map(|r| r.usage.as_ref())
        .map(|u| u.reclaimable)
        .sum();
    let total = grouped.iter().map(|g| g.list.len()).sum::<usize>();
    let trashed = grouped.iter().map(|g| g.trash.len()).sum::<usize>();
    let records = grouped.iter().map(|g| g.records.len()).sum::<usize>();
    let leftovers: Vec<&Leftover> = grouped.iter().flat_map(|g| g.leftovers.iter()).collect();

    if options.json {
        let repos = grouped
            .iter()
            .map(|g| {
                Json::Obj(vec![
                    ("common_dir", Json::path(&g.repo.common_dir)),
                    ("main_path", Json::path(&g.repo.main.path)),
                    ("bare", Json::Bool(g.repo.main.bare)),
                    (
                        "free_space",
                        Json::opt(g.space.as_ref(), output::space_json),
                    ),
                    (
                        "worktrees",
                        Json::Arr(g.list.iter().map(output::report_json).collect()),
                    ),
                    (
                        "trash",
                        Json::Arr(g.trash.iter().map(trash::item_json).collect()),
                    ),
                    (
                        "interrupted",
                        Json::Arr(g.leftovers.iter().map(Leftover::json).collect()),
                    ),
                    (
                        "removed",
                        Json::Arr(g.records.iter().map(removed::item_json).collect()),
                    ),
                ])
            })
            .collect();
        let doc = Json::Obj(vec![
            ("schema", Json::str(output::SCHEMA_SCAN)),
            ("version", Json::str(VERSION)),
            ("generated_at", Json::str(output::rfc3339(started))),
            ("activity", output::activity_json(&snapshot)),
            (
                "regenerable_config",
                Json::opt(policy.config.as_deref(), Json::path),
            ),
            (
                "ignored_environment_rules",
                output::ignored_environment_json(&policy),
            ),
            ("rule_sources", output::policy_sources_json(&policy)),
            ("idle", output::idle_json(&policy)),
            ("repositories", Json::Arr(repos)),
            (
                "orphans",
                Json::Arr(
                    found
                        .orphans
                        .iter()
                        .map(|o| {
                            Json::Obj(vec![
                                ("path", Json::path(&o.path)),
                                ("missing_gitdir", Json::path(&o.gitdir)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "errors",
                Json::Arr(errors.iter().map(|e| Json::str(e.clone())).collect()),
            ),
            (
                "summary",
                Json::Obj(vec![
                    ("linked_worktrees", Json::UInt(total as u64)),
                    ("candidates", Json::UInt(candidates.len() as u64)),
                    ("candidate_allocated_bytes", Json::UInt(allocated)),
                    ("candidate_reclaimable_bytes", Json::UInt(reclaimable)),
                    ("trash_entries", Json::UInt(trashed as u64)),
                    ("interrupted_removals", Json::UInt(leftovers.len() as u64)),
                    ("removal_records", Json::UInt(records as u64)),
                ]),
            ),
        ]);
        println!("{}", doc.render());
        return 0;
    }

    let style = Style::stdout();
    let places = Places::new();
    let mut out = String::new();
    out.push_str(&style.dim(&format!(
        "{} · activity: {}\n",
        header(started),
        clean(&snapshot.scope())
    )));
    for g in &grouped {
        let size: u64 = g
            .list
            .iter()
            .filter_map(|r| r.usage.as_ref())
            .map(|u| u.allocated)
            .sum();
        let kind = if g.repo.main.bare { " (bare)" } else { "" };
        out.push('\n');
        if g.list.is_empty() {
            out.push_str(&format!(
                "{}{kind} · no linked worktrees\n",
                style.bold(&places.show(&g.repo.main.path))
            ));
        } else {
            out.push_str(&format!(
                "{}{kind} · {} linked worktree{} · {}\n",
                style.bold(&places.show(&g.repo.main.path)),
                g.list.len(),
                if g.list.len() == 1 { "" } else { "s" },
                output::bytes(size)
            ));
        }
        for report in &g.list {
            out.push_str(&output::scan_entry(report, &places, &style));
        }
        if !g.trash.is_empty() {
            let bytes: u64 = g
                .trash
                .iter()
                .filter_map(|i| i.record.as_ref().ok().and_then(|r| r.allocated))
                .sum();
            out.push_str(&format!(
                "  {}\n",
                style.dim(&format!(
                    "trash: {} entr{} · {} · list or restore: residue restore · a remove run purges entries older than --keep-trash",
                    g.trash.len(),
                    if g.trash.len() == 1 { "y" } else { "ies" },
                    output::bytes(bytes)
                ))
            ));
        }
        let restorable = g.records.iter().filter(|i| i.record.is_ok()).count();
        if restorable > 0 {
            out.push_str(&format!(
                "  {}\n",
                style.dim(&format!(
                    "removed: {restorable} restorable · list or restore: residue restore · a remove run forgets records older than --keep-removed"
                ))
            ));
        }
    }
    if found.repos.is_empty() && errors.is_empty() {
        out.push_str("\nno Git repositories found\n");
    }
    if !leftovers.is_empty() {
        out.push_str(&format!("\n{}\n", remove::LEFTOVER_TITLE));
        for l in &leftovers {
            out.push_str(&l.text(&places, &style));
            // A run that stopped after writing its removal record: restore
            // moves it back, with the same checks.
            let record = grouped
                .iter()
                .flat_map(|g| g.records.iter())
                .find(|i| i.record.as_ref().is_ok_and(|r| r.aside == l.path));
            if let Some(item) = record {
                out.push_str(&format!(
                    "    or: residue restore {}\n",
                    output::shell(&item.dir)
                ));
            }
        }
    }
    if !found.orphans.is_empty() {
        out.push_str(
            "\norphaned checkouts (their Git directory is gone; residue does not remove these):\n",
        );
        for o in &found.orphans {
            out.push_str(&format!(
                "  {}  {}\n",
                places.show(&o.path),
                style.dim(&format!("missing {}", places.show(&o.gitdir)))
            ));
        }
    }
    if !errors.is_empty() {
        out.push_str("\ncould not inspect:\n");
        for e in &errors {
            out.push_str(&format!("  {}\n", clean(e)));
        }
    }
    let mut spaces: BTreeMap<u64, (&Repo, Space)> = BTreeMap::new();
    for g in &grouped {
        if let Some(space) = g.space {
            spaces.entry(space.device).or_insert((g.repo, space));
        }
    }
    if !spaces.is_empty() {
        out.push('\n');
    }
    for (repo, space) in spaces.values() {
        out.push_str(&format!(
            "disk  {}  {}\n",
            output::space_phrase(space),
            style.dim(&format!("(filesystem of {})", places.show(&repo.main.path)))
        ));
    }
    if total > 0 {
        out.push_str(&format!(
            "{} candidate{} of {} linked worktree{} · {} allocated · about {} reclaimable\n",
            candidates.len(),
            if candidates.len() == 1 { "" } else { "s" },
            total,
            if total == 1 { "" } else { "s" },
            output::bytes(allocated),
            output::bytes(reclaimable)
        ));
        out.push_str(&style.dim(
            "next: residue explain PATH  ·  residue remove PATH  ·  keep one: git worktree lock PATH\n",
        ));
        // Only for a person at a terminal: a pipe or a script reads the
        // report, not advice.
        if std::io::stdout().is_terminal() {
            let dirs: Vec<&Path> = grouped
                .iter()
                .map(|g| g.repo.common_dir.as_path())
                .collect();
            if let Some(hint) = crate::service::hint(&dirs, &roots) {
                out.push_str(&style.dim(&hint));
                out.push('\n');
            }
        }
    }
    print!("{out}");
    0
}

/// One repository of a scan: its judged worktrees, free space, trash,
/// removal records and interrupted removals.
struct Group<'a> {
    repo: &'a Repo,
    list: Vec<Report>,
    space: Option<Space>,
    trash: Vec<trash::Item>,
    /// Removal records `residue restore` can bring back.
    records: Vec<removed::Item>,
    leftovers: Vec<Leftover>,
}

/// A linked worktree named on the command line, with its repository.
pub struct Target {
    pub repo: Repo,
    pub entry: Entry,
    pub canonical: PathBuf,
}

/// Resolve PATH to the root of one linked worktree. Refuses the main
/// checkout, a bare repository, and a directory inside a worktree (so a
/// mistyped subdirectory can never select the whole checkout).
pub fn locate(path: &Path) -> Result<Target, String> {
    let canonical = git::canonical(path)?;
    let common_dir = git::common_dir(&canonical)
        .map_err(|e| format!("{}: not inside a Git repository ({e})", path.display()))?;
    let mut entries = git::worktrees(&canonical)?;
    let main = entries.remove(0);
    if !main.bare && git::canonical(&main.path).ok().as_deref() == Some(canonical.as_path()) {
        return Err(format!(
            "{} is the main checkout of its repository; residue never removes it",
            path.display()
        ));
    }
    let position = entries
        .iter()
        .position(|e| git::canonical(&e.path).ok().as_deref() == Some(canonical.as_path()));
    let Some(position) = position else {
        let top = git::identity(&canonical)
            .map(|id| id.toplevel)
            .unwrap_or_else(|_| canonical.clone());
        return Err(if top != canonical {
            format!(
                "{} is inside the checkout {}; pass the worktree root itself",
                path.display(),
                top.display()
            )
        } else {
            format!("{} is not a linked worktree", path.display())
        });
    };
    let entry = entries.remove(position);
    let repo = Repo::new(common_dir, main, entries);
    Ok(Target {
        repo,
        entry,
        canonical,
    })
}

pub fn explain(options: &Options) -> u8 {
    let started = output::now();
    let policy = match prepare(options) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let target = match locate(&options.paths[0]) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    let snapshot = Snapshot::take();
    let report = inspect::worktree(&target.entry, &target.repo.common_dir, &snapshot, &policy);
    if options.json {
        let doc = Json::Obj(vec![
            ("schema", Json::str(output::SCHEMA_EXPLAIN)),
            ("version", Json::str(VERSION)),
            ("generated_at", Json::str(output::rfc3339(started))),
            ("activity", output::activity_json(&snapshot)),
            (
                "ignored_environment_rules",
                output::ignored_environment_json(&policy),
            ),
            ("rule_sources", output::policy_sources_json(&policy)),
            ("idle", output::idle_json(&policy)),
            ("main_path", Json::path(&target.repo.main.path)),
            ("worktree", output::report_json(&report)),
        ]);
        println!("{}", doc.render());
        return 0;
    }
    let style = Style::stdout();
    print!(
        "{}\n{}",
        style.dim(&format!(
            "{} · activity: {}\n",
            header(started),
            clean(&snapshot.scope())
        )),
        explain_text(&report, &target.repo, &Places::new(), &style)
    );
    0
}

pub fn explain_text(r: &Report, repo: &Repo, places: &Places, style: &Style) -> String {
    let mut out = String::new();
    let row = |out: &mut String, label: &str, value: String| {
        out.push_str(&format!("{:<11}{}\n", style.dim(label), value));
    };
    row(
        &mut out,
        "worktree",
        style.bold(&places.show(r.canonical.as_deref().unwrap_or(&r.entry.path))),
    );
    row(&mut out, "repository", places.show(&repo.main.path));
    let head = r.entry.short_head().unwrap_or("none");
    row(
        &mut out,
        "HEAD",
        match r.entry.short_branch() {
            Some(b) => format!("{head} on branch {}", clean(b)),
            None => format!("{head} (detached)"),
        },
    );
    let blockers = r
        .checks
        .iter()
        .filter(|c| c.status == inspect::Status::Block)
        .count();
    let unknowns = r
        .checks
        .iter()
        .filter(|c| c.status == inspect::Status::Unknown)
        .count();
    let verdict = match r.verdict {
        Verdict::Candidate => "every check passed. `residue remove` deletes the directory with `git worktree remove`; the branch and its commits stay.".to_string(),
        Verdict::Keep => format!(
            "{} block{} removal.",
            if blockers == 1 { "1 check".to_string() } else { format!("{blockers} checks") },
            if blockers == 1 { "s" } else { "" }
        ),
        Verdict::Unknown => format!(
            "residue could not establish {} and keeps the worktree.",
            if unknowns == 1 { "1 check".to_string() } else { format!("{unknowns} checks") }
        ),
        Verdict::Stale => format!(
            "{}. Only Git metadata remains; `git worktree prune` clears it.",
            clean(r.stale_reason.as_deref().unwrap_or("stale"))
        ),
    };
    row(
        &mut out,
        "verdict",
        format!("{}: {}", style.verdict(r.verdict, 0), verdict),
    );
    if !r.checks.is_empty() {
        out.push_str("\nchecks\n");
        for c in &r.checks {
            out.push_str(&format!(
                "  {} {:<16}{}\n",
                style.status(c.status),
                c.id,
                clean(&c.detail)
            ));
        }
    }
    if r.verdict == Verdict::Stale {
        return out;
    }
    out.push_str("\nfacts (never block)\n");
    let fact = |out: &mut String, label: &str, value: String| {
        out.push_str(&format!("  {:<24}{}\n", label, value));
    };
    fact(
        &mut out,
        "local-only commits",
        match &r.local_commits {
            Ok(Some(n)) => {
                format!("{n} (compared with local remote-tracking refs, as of the last fetch)")
            }
            Ok(None) => "unknown: the repository has no remote-tracking refs".into(),
            Err(e) => format!("unknown: {}", clean(e)),
        },
    );
    if r.entry.branch.is_some() {
        fact(
            &mut out,
            "upstream",
            match &r.upstream {
                Ok(Some(u)) if u.gone => format!("{} (gone from the remote)", clean(&u.name)),
                Ok(Some(u)) => clean(&u.name),
                Ok(None) => "none configured".into(),
                Err(e) => format!("unknown: {}", clean(e)),
            },
        );
    }
    match &r.last_change {
        Ok(Some(c)) => fact(
            &mut out,
            "last change",
            format!(
                "{} ({}; {})",
                output::date(c.time),
                output::age(output::now() - c.time),
                clean(&c.described())
            ),
        ),
        Ok(None) => {}
        Err(e) => fact(&mut out, "last change", format!("unknown: {}", clean(e))),
    }
    match &r.restored {
        Ok(Some(m)) => fact(
            &mut out,
            "restored",
            format!(
                "{}from {}; residue watch never removes it again because its work landed",
                m.at.map(|t| format!("{} ({}), ", output::date(t), output::age(output::now() - t)))
                    .unwrap_or_default(),
                clean(&m.from)
            ),
        ),
        Ok(None) => {}
        Err(e) => fact(
            &mut out,
            "restored",
            format!("unknown: {}; residue watch treats it as restored", clean(e)),
        ),
    }
    if let Some(u) = &r.usage {
        fact(&mut out, "size", output::usage_phrase(u));
        fact(
            &mut out,
            "",
            style.dim("reclaimable excludes files hard-linked from outside; copy-on-write clones are not detected"),
        );
    }
    if !r.ignored.is_empty() {
        const SHOWN: usize = 8;
        let mut names: Vec<String> = r
            .ignored
            .iter()
            .take(SHOWN)
            .map(|i| {
                format!(
                    "{}: {}",
                    clean(&i.sized()),
                    match i.judgement {
                        inspect::Judgement::Regenerable => "regenerable",
                        inspect::Judgement::Kept => "kept",
                        inspect::Judgement::Unknown => "not judged",
                    }
                )
            })
            .collect();
        if r.ignored.len() > SHOWN {
            let files: u64 = r.ignored.iter().filter_map(|i| i.files).sum();
            let bytes: u64 = r.ignored.iter().filter_map(|i| i.bytes).sum();
            names.push(format!(
                "+{} more; {} entries hold {} file{}, {}",
                r.ignored.len() - SHOWN,
                r.ignored.len(),
                files,
                if files == 1 { "" } else { "s" },
                output::bytes(bytes)
            ));
        }
        // One entry per line, aligned under the first.
        fact(
            &mut out,
            "ignored entries",
            names.join(&format!("\n{:26}", "")),
        );
    }
    out
}
