//! `residue remove`: judge, confirm, re-check, then let Git remove.
//!
//! The directory is deleted by `git worktree remove` without `--force`, so
//! Git applies its own refusals (modified or untracked files, a lock,
//! submodules) on top of residue's. Right before each removal residue takes
//! a fresh process snapshot and repeats every check; any change since the
//! confirmation skips that worktree.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use crate::activity::Snapshot;
use crate::cli::Options;
use crate::commands::{self, Target, VERSION, fail, locate};
use crate::git;
use crate::inspect::{self, Report, Verdict};
use crate::json::Json;
use crate::output::{self, Places, Style, clean};
use crate::policy::Policy;

enum Outcome {
    Removed,
    Refused,
    Skipped(String),
    Failed(String),
    Invalid(String),
}

impl Outcome {
    fn code(&self) -> &'static str {
        match self {
            Outcome::Removed => "removed",
            Outcome::Refused => "refused",
            Outcome::Skipped(_) => "skipped",
            Outcome::Failed(_) => "failed",
            Outcome::Invalid(_) => "invalid",
        }
    }
}

struct Item {
    arg: PathBuf,
    target: Option<Target>,
    report: Option<Report>,
    outcome: Option<Outcome>,
}

pub fn remove(options: &Options) -> u8 {
    let started = output::now();
    let policy = match commands::prepare(options) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let places = Places::new();
    let style = Style::stdout();
    let mut items: Vec<Item> = Vec::new();
    for arg in &options.paths {
        let mut item = Item {
            arg: arg.clone(),
            target: None,
            report: None,
            outcome: None,
        };
        match locate(arg) {
            Ok(t)
                if items
                    .iter()
                    .any(|i| i.target.as_ref().map(|x| &x.canonical) == Some(&t.canonical)) =>
            {
                continue;
            }
            Ok(t) => item.target = Some(t),
            Err(e) => item.outcome = Some(Outcome::Invalid(e)),
        }
        items.push(item);
    }

    let snapshot = Snapshot::take();
    for item in &mut items {
        if let Some(t) = &item.target {
            let report = inspect::worktree(&t.entry, &t.repo.common_dir, &snapshot, &policy);
            if report.verdict != Verdict::Candidate {
                item.outcome = Some(Outcome::Refused);
            }
            item.report = Some(report);
        }
    }

    let ready: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].outcome.is_none())
        .collect();
    if !options.json {
        print!("{}", plan_text(&items, &ready, started, &places, &style));
    }

    let mut declined: Option<String> = None;
    if !ready.is_empty() && !options.yes {
        declined = match confirm(ready.len()) {
            Ok(true) => None,
            Ok(false) => Some("not confirmed".to_string()),
            Err(e) => Some(e),
        };
        if let Some(why) = &declined {
            for &i in &ready {
                items[i].outcome = Some(Outcome::Skipped(why.clone()));
            }
        }
    }

    for &i in &ready {
        if items[i].outcome.is_some() {
            continue;
        }
        let outcome = execute(&items[i], &policy);
        if !options.json {
            println!("{}", outcome_line(&items[i], &outcome, &places));
        }
        items[i].outcome = Some(outcome);
    }

    let all_removed = items
        .iter()
        .all(|i| matches!(i.outcome, Some(Outcome::Removed)));
    if options.json {
        println!("{}", json(&items, &snapshot, &policy, started).render());
    } else if let Some(why) = declined {
        println!("nothing removed: {}", clean(&why));
    }
    if all_removed { 0 } else { 3 }
}

fn confirm(n: usize) -> Result<bool, String> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err(
            "stdin is not a terminal, so residue cannot ask for confirmation; pass --yes only from the session that owns these worktrees and knows its task is finished"
                .into(),
        );
    }
    let style = Style::stderr();
    let mut err = io::stderr();
    let prompt = format!(
        "Remove {} with `git worktree remove`? Branches and commits stay. Type yes to continue: ",
        if n == 1 {
            "1 worktree".to_string()
        } else {
            format!("{n} worktrees")
        }
    );
    write!(err, "{}", style.bold(&prompt))
        .and_then(|_| err.flush())
        .map_err(|e| format!("cannot write the prompt: {e}"))?;
    let mut answer = String::new();
    stdin
        .lock()
        .read_line(&mut answer)
        .map_err(|e| format!("cannot read the answer: {e}"))?;
    Ok(answer.trim().eq_ignore_ascii_case("yes"))
}

/// Re-check one confirmed worktree against a fresh snapshot, then remove it.
fn execute(item: &Item, policy: &crate::policy::Policy) -> Outcome {
    let (Some(target), Some(before)) = (&item.target, &item.report) else {
        return Outcome::Failed("internal: no target".into());
    };
    let fresh = match git::worktrees(&target.canonical) {
        Ok(list) => list,
        Err(e) => return Outcome::Failed(e),
    };
    let listed = fresh
        .iter()
        .find(|e| git::canonical(&e.path).ok().as_deref() == Some(target.canonical.as_path()));
    let Some(listed) = listed else {
        return Outcome::Skipped("Git no longer lists it as a worktree".into());
    };
    if listed.head != before.entry.head
        || listed.branch != before.entry.branch
        || listed.locked.is_some()
    {
        return Outcome::Skipped("HEAD, branch or lock changed since review".into());
    }
    let snapshot = Snapshot::take();
    let now = inspect::worktree(listed, &target.repo.common_dir, &snapshot, policy);
    if now.verdict != Verdict::Candidate {
        let reasons: Vec<String> = now.blocking().map(|c| c.short.clone()).collect();
        return Outcome::Skipped(format!("changed since review: {}", reasons.join(", ")));
    }
    let ignored_before: Vec<&str> = before.ignored.iter().map(|i| i.path.as_str()).collect();
    let ignored_now: Vec<&str> = now.ignored.iter().map(|i| i.path.as_str()).collect();
    if ignored_before != ignored_now {
        return Outcome::Skipped("its ignored files changed since review".into());
    }
    let main = if target.repo.main.path.exists() {
        target.repo.main.path.clone()
    } else {
        target.repo.common_dir.clone()
    };
    if let Err(e) = git::remove_worktree(&main, &target.canonical) {
        return Outcome::Failed(e);
    }
    Outcome::Removed
}

fn plan_text(
    items: &[Item],
    ready: &[usize],
    started: i64,
    places: &Places,
    style: &Style,
) -> String {
    let mut out = String::new();
    out.push_str(&style.dim(&format!("{}\n", commands::header(started))));
    if !ready.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            style.bold(&format!("will remove ({}):", ready.len()))
        ));
        for &i in ready {
            let (Some(t), Some(r)) = (&items[i].target, &items[i].report) else {
                continue;
            };
            let size = r.usage.as_ref().map_or(String::new(), output::usage_phrase);
            let branch = r
                .entry
                .short_branch()
                .map_or("detached".to_string(), |b| format!("[{}]", clean(b)));
            out.push_str(&format!(
                "  {}  {}\n    {}\n",
                style.bold(&places.show(&t.canonical)),
                style.dim(&branch),
                size
            ));
            let deleted: Vec<String> = r.ignored.iter().map(|i| i.sized()).collect();
            if !deleted.is_empty() {
                out.push_str(&format!(
                    "    deletes ignored: {}\n",
                    clean(&deleted.join(", "))
                ));
            }
            if let (Ok(Some(n)), Some(b)) = (&r.local_commits, r.entry.short_branch()) {
                if *n > 0 {
                    out.push_str(&format!(
                        "    {n} commit{} on no remote-tracking ref {} on branch {}\n",
                        if *n == 1 { "" } else { "s" },
                        if *n == 1 { "stays" } else { "stay" },
                        clean(b)
                    ));
                }
            }
            if let Some(scope) = r.checks.iter().find(|c| c.id == "activity") {
                out.push_str(&format!("    {}\n", style.dim(&clean(&scope.detail))));
            }
        }
    }
    let refused: Vec<&Item> = items.iter().filter(|i| i.outcome.is_some()).collect();
    if !refused.is_empty() {
        out.push_str(&format!("\n{}\n", style.bold("not removed:")));
        for item in refused {
            let shown = item.target.as_ref().map_or_else(
                || clean(&item.arg.to_string_lossy()),
                |t| places.show(&t.canonical),
            );
            let why = match (&item.outcome, &item.report) {
                (Some(Outcome::Invalid(e)), _) => clean(e),
                (_, Some(r)) => {
                    let reasons: Vec<String> = r.blocking().map(|c| clean(&c.short)).collect();
                    format!("{}: {}", r.verdict.code(), reasons.join(" · "))
                }
                _ => String::new(),
            };
            out.push_str(&format!("  {shown}  {}\n", style.dim(&why)));
        }
        out.push_str(&style.dim("  details: residue explain PATH\n"));
    }
    out.push('\n');
    out
}

fn outcome_line(item: &Item, outcome: &Outcome, places: &Places) -> String {
    let shown = item.target.as_ref().map_or_else(
        || clean(&item.arg.to_string_lossy()),
        |t| places.show(&t.canonical),
    );
    let branch = item
        .report
        .as_ref()
        .and_then(|r| r.entry.short_branch())
        .map(|b| format!(" (branch {} kept)", clean(b)))
        .unwrap_or_default();
    match outcome {
        Outcome::Removed => {
            let reclaimable = item
                .report
                .as_ref()
                .and_then(|r| r.usage.as_ref())
                .map(|u| format!("; about {} reclaimable", output::bytes(u.reclaimable)))
                .unwrap_or_default();
            format!("removed {shown}{branch}{reclaimable}")
        }
        Outcome::Skipped(why) => format!("skipped {shown}: {}", clean(why)),
        Outcome::Failed(why) => format!("failed {shown}: {}", clean(why)),
        Outcome::Refused | Outcome::Invalid(_) => format!("not removed {shown}"),
    }
}

fn json(items: &[Item], snapshot: &Snapshot, policy: &Policy, started: i64) -> Json {
    let results = items
        .iter()
        .map(|item| {
            let reason = match &item.outcome {
                Some(Outcome::Removed) => Json::Null,
                Some(Outcome::Skipped(r) | Outcome::Failed(r) | Outcome::Invalid(r)) => {
                    Json::str(r.clone())
                }
                Some(Outcome::Refused) => Json::str("one or more checks did not pass"),
                None => Json::str("not attempted"),
            };
            Json::Obj(vec![
                ("path", Json::path(&item.arg)),
                (
                    "result",
                    Json::str(item.outcome.as_ref().map_or("not_attempted", Outcome::code)),
                ),
                ("reason", reason),
                (
                    "worktree",
                    Json::opt(item.report.as_ref(), output::report_json),
                ),
            ])
        })
        .collect();
    Json::Obj(vec![
        ("schema", Json::str(output::SCHEMA_REMOVE)),
        ("version", Json::str(VERSION)),
        ("generated_at", Json::str(output::rfc3339(started))),
        ("activity", output::activity_json(snapshot)),
        (
            "ignored_environment_rules",
            output::ignored_environment_json(policy),
        ),
        ("results", Json::Arr(results)),
    ])
}
