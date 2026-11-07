//! `residue restore [ENTRY]`: list what residue can bring back, and bring
//! one back.
//!
//! Two kinds of entry: a trash entry (`remove --trash`), whose worktree is
//! still on disk and moves back with `git worktree move`; and a removal
//! record (every deletion), whose worktree `git worktree add` recreates on
//! its branch and HEAD (`removed::restore`). Either way the restored
//! worktree is marked (`removed::MARKER`), so `residue watch` never removes
//! it again because its work landed.

use std::path::{Component, Path, PathBuf};

use crate::cli::Options;
use crate::commands::{VERSION, fail, header};
use crate::discover;
use crate::git;
use crate::json::Json;
use crate::output::{self, Places, Style, clean};
use crate::removed::{self, How};
use crate::trash;

/// Everything `residue restore` can bring back in some repositories.
struct Found {
    trash: Vec<trash::Item>,
    removed: Vec<removed::Item>,
    errors: Vec<String>,
}

#[derive(Clone, Copy)]
enum Pick {
    Trash(usize),
    Removed(usize),
}

pub fn command(options: &Options) -> u8 {
    let started = output::now();
    if let Err(e) = git::require_version() {
        return fail(&e);
    }
    let places = Places::new();
    let Some(wanted) = options.paths.first() else {
        let found = gather(&[PathBuf::from(".")]);
        if options.json {
            println!("{}", list_json(&found, started, None).render());
        } else {
            print!("{}", list_text(&found, started, &places));
        }
        return if found.errors.is_empty() { 0 } else { 1 };
    };
    // A path names its repository directly, from any directory, even once
    // the worktree it names is gone; a name is looked up in the
    // repositories at or under this one.
    let mut chosen = None;
    let mut unsearched = None;
    let mut missed = None;
    match repository_of(wanted) {
        Ok(Some(root)) => {
            let found = gather(std::slice::from_ref(&root));
            match choose(&found, wanted) {
                Ok(pick) => chosen = Some((found, pick)),
                // More than one entry where the path points is the answer:
                // the message names them.
                Err(Miss::Many(e)) => return fail(&e),
                Err(Miss::Nothing) => missed = Some(root),
            }
        }
        Ok(None) => {}
        Err(e) => unsearched = Some(e),
    }
    let (found, pick) = match chosen {
        Some(c) => c,
        None => {
            // A path ENTRY that is not where it points leaves the current
            // directory to search, but never the home folder or a folder
            // above it, for the reason `repository_of` gives.
            if at_or_above_home(Path::new(".")) {
                if let Some(e) = unsearched {
                    return fail(&e);
                }
                if let Some(root) = missed {
                    return fail(&format!(
                        "nothing named {} in the trash or the removal records of the repositories at or under {}, and residue does not search your home folder or the folders above it for it; run residue restore inside its repository, or pass the entry's path it lists there",
                        wanted.display(),
                        root.display()
                    ));
                }
            }
            let found = gather(&[PathBuf::from(".")]);
            match choose(&found, wanted) {
                Ok(pick) => (found, pick),
                Err(miss) => return fail(&unsearched.unwrap_or_else(|| miss.text(wanted))),
            }
        }
    };
    let to = match options.to.as_deref().map(destination) {
        None => None,
        Some(Ok(to)) => Some(to),
        Some(Err(e)) => return fail(&e),
    };
    let result = match pick {
        Pick::Trash(i) => restore_trash(&found, i, to.as_deref()),
        Pick::Removed(i) => restore_removed(&found, i, to.as_deref()),
    };
    match result {
        Ok(done) => {
            for note in &done.notes {
                eprintln!("residue: note: {}", clean(note));
            }
            if options.json {
                println!(
                    "{}",
                    list_json(&found, started, Some((pick, &done))).render()
                );
            } else {
                print!("{}", done_text(&found, pick, &done, &places));
            }
            0
        }
        Err(e) => fail(&format!("cannot restore {}: {e}", name_of(&found, pick))),
    }
}

/// For an ENTRY given as a path, a directory to look for its repository
/// in: the path itself, or for a worktree that is gone, the nearest
/// directory above it that exists (the current one for a relative path
/// none of whose folders exists). Never the home folder or a directory
/// above it: from there the search would read every folder under it,
/// Documents and Desktop among them, which macOS guards with a permission
/// prompt. `Err`, asking for the repository, when that is the only place
/// left to look.
fn repository_of(wanted: &Path) -> Result<Option<PathBuf>, String> {
    let exists = std::fs::symlink_metadata(wanted).is_ok();
    let dir = if exists {
        wanted.to_path_buf()
    } else {
        if wanted.components().count() < 2 && !wanted.is_absolute() {
            return Ok(None);
        }
        wanted
            .ancestors()
            .skip(1)
            .find(|a| !a.as_os_str().is_empty() && a.is_dir())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    };
    if at_or_above_home(&dir) {
        return Err(if exists {
            format!(
                "{} is your home folder or a folder above it, which residue does not search; run residue restore inside a repository, or pass an entry's path",
                wanted.display()
            )
        } else {
            format!(
                "{} is gone, and residue does not search your home folder or the folders above it for it; run residue restore inside its repository, or pass the entry name it lists there",
                wanted.display()
            )
        });
    }
    Ok(Some(dir))
}

/// `dir` is the home folder, `/`, or a directory above the home folder.
fn at_or_above_home(dir: &Path) -> bool {
    let Ok(dir) = git::canonical(dir) else {
        return false;
    };
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .and_then(|h| git::canonical(Path::new(&h)).ok());
    dir.parent().is_none() || home.is_some_and(|h| h.starts_with(&dir))
}

/// `path` made absolute against a directory that exists: its parent
/// resolved, its own name kept, so a path that is gone can still be
/// compared with a recorded one.
fn absolute(path: &Path) -> Option<PathBuf> {
    if let Ok(p) = git::canonical(path) {
        return Some(p);
    }
    let name = path.file_name()?;
    if matches!(path.components().next_back(), Some(Component::ParentDir)) {
        return None;
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    git::canonical(&parent).ok().map(|p| p.join(name))
}

/// `--to PATH`, absolute: Git records worktree paths as given.
fn destination(to: &Path) -> Result<PathBuf, String> {
    absolute(to).ok_or_else(|| format!("--to {}: its parent directory must exist", to.display()))
}

/// The trash entries and removal records of the repositories at or under
/// `roots`.
fn gather(roots: &[PathBuf]) -> Found {
    let repos = discover::find(roots);
    let mut found = Found {
        trash: Vec::new(),
        removed: Vec::new(),
        errors: repos.errors,
    };
    for repo in &repos.repos {
        match trash::list(&repo.common_dir, &repo.command_dir(), &repo.trash) {
            Ok(list) => found.trash.extend(list),
            Err(e) => found.errors.push(e),
        }
        match removed::of_repo(repo) {
            Ok(list) => found.removed.extend(list),
            Err(e) => found.errors.push(e),
        }
    }
    found
}

fn name_of(found: &Found, pick: Pick) -> String {
    clean(match pick {
        Pick::Trash(i) => &found.trash[i].name,
        Pick::Removed(i) => &found.removed[i].name,
    })
}

/// Why [`choose`] picked no entry.
enum Miss {
    /// No entry matched.
    Nothing,
    /// More than one did; the message names them.
    Many(String),
}

impl Miss {
    /// The message for a search of the repositories at or under the
    /// current directory.
    fn text(self, wanted: &Path) -> String {
        match self {
            Miss::Nothing => format!(
                "nothing named {} in the trash or the removal records of the repositories at or under the current directory; `residue restore` lists them",
                wanted.display()
            ),
            Miss::Many(e) => e,
        }
    }
}

/// The entry ENTRY names: an entry's path, or the path its worktree had;
/// an entry name from the list; or the directory name or the branch of
/// its worktree, when only one entry of either kind matches.
fn choose(found: &Found, wanted: &Path) -> Result<Pick, Miss> {
    let shown = wanted.display();
    if let Ok(path) = git::canonical(wanted) {
        if let Some(i) = found.trash.iter().position(|i| {
            git::canonical(&i.dir).ok().as_deref() == Some(path.as_path())
                || i.worktree.as_deref() == Some(path.as_path())
        }) {
            return Ok(Pick::Trash(i));
        }
        if let Some(i) = found
            .removed
            .iter()
            .position(|i| git::canonical(&i.dir).ok().as_deref() == Some(path.as_path()))
        {
            return Ok(Pick::Removed(i));
        }
    }
    let text = wanted.as_os_str();
    // New entries never share a name across the two kinds; two
    // repositories under the current directory can still both have one.
    let named: Vec<Pick> = found
        .trash
        .iter()
        .enumerate()
        .filter(|(_, i)| i.name.as_bytes() == text.as_encoded_bytes())
        .map(|(i, _)| Pick::Trash(i))
        .chain(
            found
                .removed
                .iter()
                .enumerate()
                .filter(|(_, i)| i.name.as_bytes() == text.as_encoded_bytes())
                .map(|(i, _)| Pick::Removed(i)),
        )
        .collect();
    match named.as_slice() {
        [one] => return Ok(*one),
        [] => {}
        many => {
            let paths: Vec<String> = many
                .iter()
                .map(|p| match *p {
                    Pick::Trash(i) => found.trash[i].dir.display().to_string(),
                    Pick::Removed(i) => found.removed[i].dir.display().to_string(),
                })
                .collect();
            return Err(Miss::Many(format!(
                "{shown} names {} entries ({}); pass the entry's path",
                many.len(),
                paths.join(", ")
            )));
        }
    }
    // The path the worktree had, when ENTRY looks like a path.
    let mut matches: Vec<Pick> = Vec::new();
    if wanted.components().count() > 1 || wanted.is_absolute() {
        if let Some(abs) = absolute(wanted) {
            for (i, item) in found.trash.iter().enumerate() {
                if item.record.as_ref().is_ok_and(|r| r.original == abs) {
                    matches.push(Pick::Trash(i));
                }
            }
            for (i, item) in found.removed.iter().enumerate() {
                if item.record.as_ref().is_ok_and(|r| r.original == abs) {
                    matches.push(Pick::Removed(i));
                }
            }
        }
    }
    // The worktree's directory name or its branch.
    if matches.is_empty() {
        let short = |b: &str| b.strip_prefix("refs/heads/").unwrap_or(b).to_string();
        for (i, item) in found.trash.iter().enumerate() {
            if item.record.as_ref().is_ok_and(|r| {
                r.original.file_name() == Some(text)
                    || r.branch.as_deref().map(short).as_deref() == text.to_str()
            }) {
                matches.push(Pick::Trash(i));
            }
        }
        for (i, item) in found.removed.iter().enumerate() {
            if item.record.as_ref().is_ok_and(|r| {
                r.original.file_name() == Some(text) || r.short_branch() == text.to_str()
            }) {
                matches.push(Pick::Removed(i));
            }
        }
    }
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => Err(Miss::Nothing),
        many => {
            let names: Vec<String> = many.iter().map(|p| name_of(found, *p)).collect();
            Err(Miss::Many(format!(
                "{shown} names {} entries ({}); pass the entry name",
                many.len(),
                names.join(", ")
            )))
        }
    }
}

/// What a restore did, for the report.
struct Done {
    path: PathBuf,
    how: How,
    branch: Option<String>,
    head: Option<String>,
    not_kept: Vec<removed::Gone>,
    notes: Vec<String>,
}

fn restore_trash(found: &Found, i: usize, to: Option<&Path>) -> Result<Done, String> {
    let item = &found.trash[i];
    let from = item.worktree.clone().unwrap_or_default();
    let (path, note) = trash::restore(item, to)?;
    let mut notes: Vec<String> = note.into_iter().collect();
    match git::identity(&path) {
        Ok(id) => {
            if let Err(e) = removed::mark(&id.git_dir, &format!("trash entry {}", item.name)) {
                notes.push(format!(
                    "could not mark it as restored ({e}); `residue watch` may remove it once its upstream is gone: lock it with `git worktree lock` to keep it"
                ));
            }
        }
        Err(e) => notes.push(format!(
            "could not find its Git directory to mark it as restored ({e}); `residue watch` may remove it once its upstream is gone: lock it with `git worktree lock` to keep it"
        )),
    }
    // A purge of this entry that was interrupted left a removal record for
    // the worktree that is now back; it has nothing left to restore.
    for r in &found.removed {
        if r.interrupted() && r.record.as_ref().is_ok_and(|rec| rec.aside == from) {
            if let Err(e) = removed::forget(r) {
                notes.push(format!(
                    "could not drop the removal record {} of its interrupted purge ({e})",
                    r.name
                ));
            }
        }
    }
    let record = item.record.as_ref().ok();
    Ok(Done {
        path,
        how: How::MovedBack(from),
        branch: record.and_then(|r| r.branch.clone()),
        head: record.and_then(|r| r.head.clone()),
        not_kept: Vec::new(),
        notes,
    })
}

fn restore_removed(found: &Found, i: usize, to: Option<&Path>) -> Result<Done, String> {
    let item = &found.removed[i];
    let listed = git::worktrees(&item.command_dir)?;
    let restored = removed::restore(item, to, &listed)?;
    let record = item.record.as_ref().ok();
    let moved_back = matches!(restored.how, How::MovedBack(_));
    Ok(Done {
        path: restored.path,
        how: restored.how,
        branch: restored.branch,
        head: record.map(|r| r.head.clone()),
        // A worktree moved back kept whatever its removal did not delete.
        not_kept: if moved_back {
            Vec::new()
        } else {
            record.map_or_else(Vec::new, |r| r.gone.clone())
        },
        notes: restored.notes,
    })
}

fn short(head: &str) -> &str {
    &head[..head.len().min(12)]
}

/// How it came back, as a phrase.
fn how_text(done: &Done) -> String {
    let branch = done
        .branch
        .as_deref()
        .map(|b| clean(b.strip_prefix("refs/heads/").unwrap_or(b)))
        .unwrap_or_default();
    let head = done.head.as_deref().map(short).unwrap_or("");
    match &done.how {
        How::MovedBack(_) if done.branch.is_some() => format!("on branch {branch}"),
        How::MovedBack(_) => "detached, as it was".into(),
        How::Branch { tip } if done.head.as_deref() == Some(tip.as_str()) => {
            format!("on branch {branch}, at {head} as it was")
        }
        How::Branch { tip } => format!(
            "on branch {branch}, which has moved on from {head} to {}",
            short(tip)
        ),
        How::Recreated => format!("branch {branch} was gone; created it again at {head}"),
        How::NewBranch { name, tip } => format!(
            "on the new branch {}, at {head}: the recorded branch no longer contains it (it is at {}), so residue left it alone",
            clean(name),
            short(tip)
        ),
        How::Detached => format!("detached at {head}, as it was"),
    }
}

fn done_text(found: &Found, pick: Pick, done: &Done, places: &Places) -> String {
    let from = match pick {
        Pick::Trash(_) => format!("the trash entry {}", name_of(found, pick)),
        Pick::Removed(i) if found.removed[i].interrupted() => {
            format!("the interrupted removal {}", name_of(found, pick))
        }
        Pick::Removed(_) => format!("the removal record {}", name_of(found, pick)),
    };
    let mut out = format!(
        "restored {} from {from}\n  {}\n",
        places.show(&done.path),
        how_text(done)
    );
    if !done.not_kept.is_empty() {
        let list: Vec<String> = done.not_kept.iter().map(removed::Gone::sized).collect();
        out.push_str(&format!(
            "  not kept: {}; the project's install or build makes them again\n",
            clean(&list.join(", "))
        ));
    }
    out
}

// ---------------------------------------------------------------- list

/// One line per entry, newest first within each repository.
fn list_text(found: &Found, started: i64, places: &Places) -> String {
    enum Row<'a> {
        Trash(&'a trash::Item),
        Removed(&'a removed::Item),
    }
    let style = Style::stdout();
    let now = output::now();
    let mut out = style.dim(&format!("{}\n", header(started)));
    if found.trash.is_empty() && found.removed.is_empty() {
        out.push_str("\nnothing to restore: the trash is empty and there are no removal records\n");
    }
    let mut repos: Vec<(&Path, &Path)> = Vec::new();
    for (common, command) in found
        .trash
        .iter()
        .map(|i| (i.common_dir.as_path(), i.command_dir.as_path()))
        .chain(
            found
                .removed
                .iter()
                .map(|i| (i.common_dir.as_path(), i.command_dir.as_path())),
        )
    {
        if !repos.iter().any(|(c, _)| *c == common) {
            repos.push((common, command));
        }
    }
    for (common, command) in repos {
        out.push_str(&format!("\n{}\n", style.bold(&places.show(command))));
        let mut rows: Vec<(i64, Row)> = Vec::new();
        for i in found.trash.iter().filter(|i| i.common_dir == common) {
            let at = i.record.as_ref().map_or(i64::MIN, |r| r.trashed);
            rows.push((at, Row::Trash(i)));
        }
        for i in found.removed.iter().filter(|i| i.common_dir == common) {
            let at = i.record.as_ref().map_or(i64::MIN, |r| r.removed);
            rows.push((at, Row::Removed(i)));
        }
        rows.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        for (_, row) in rows {
            out.push_str(&match row {
                Row::Trash(i) => trash_row(i, now, places, &style),
                Row::Removed(i) => removed_row(i, now, places, &style),
            });
        }
    }
    if !found.errors.is_empty() {
        out.push_str("\ncould not inspect:\n");
        for e in &found.errors {
            out.push_str(&format!("  {}\n", clean(e)));
        }
    }
    if !found.trash.is_empty() || !found.removed.is_empty() {
        out.push_str(&style.dim(
            "\nput one back: residue restore NAME  ·  a remove run purges trash entries older than --keep-trash (default 7d) and forgets removal records older than --keep-removed (default 30d)\n",
        ));
    }
    out
}

fn branch_label(branch: Option<&str>, head: Option<&str>) -> String {
    match (branch, head) {
        (Some(b), _) => format!("[{}]", clean(b.strip_prefix("refs/heads/").unwrap_or(b))),
        (None, Some(h)) => format!("(detached {})", short(h)),
        (None, None) => String::new(),
    }
}

fn trash_row(item: &trash::Item, now: i64, places: &Places, style: &Style) -> String {
    let mut out = format!(
        "  {}  {}",
        style.dim("trash  "),
        style.bold(&clean(&item.name))
    );
    match &item.record {
        Ok(r) => {
            let size = r
                .allocated
                .map(|b| format!(" · {}", output::bytes(b)))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {}\n    was {}\n    {}\n",
                style.dim(&branch_label(r.branch.as_deref(), r.head.as_deref())),
                places.show(&r.original),
                style.dim(&format!(
                    "trashed {} ({}){size}{}",
                    output::date(r.trashed),
                    output::age(now - r.trashed),
                    if item.is_empty_record() {
                        " · holds no worktree; the next purge clears it"
                    } else {
                        " · still on disk; restore moves it back"
                    }
                ))
            ));
        }
        Err(e) => out.push_str(&format!(
            "\n    {}\n",
            style.dim(&format!("unreadable: {}", clean(e)))
        )),
    }
    out
}

fn removed_row(item: &removed::Item, now: i64, places: &Places, style: &Style) -> String {
    let mut out = format!(
        "  {}  {}",
        style.dim("removed"),
        style.bold(&clean(&item.name))
    );
    match &item.record {
        Ok(r) => {
            let size = r
                .allocated
                .map(|b| format!(" · {} freed", output::bytes(b)))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {}\n    was {}\n    {}\n",
                style.dim(&branch_label(r.branch.as_deref(), Some(&r.head))),
                places.show(&r.original),
                style.dim(&format!(
                    "removed {} ({}) {}{size}",
                    output::date(r.removed),
                    output::age(now - r.removed),
                    clean(&removed::Reason::describe(&r.reason)),
                ))
            ));
            if item.interrupted() {
                let how = if item.head_gone() {
                    "Git can no longer read its HEAD; restore says how to bring it back"
                } else {
                    "restore moves it back"
                };
                out.push_str(&format!(
                    "    {}\n",
                    style.dim(&format!(
                        "interrupted: its removal did not finish, and Git still lists it at {}; {how}",
                        places.show(&r.aside)
                    ))
                ));
            } else if !r.gone.is_empty() {
                let list: Vec<String> = r.gone.iter().map(removed::Gone::sized).collect();
                out.push_str(&format!(
                    "    {}\n",
                    style.dim(&format!("not kept: {}", clean(&list.join(", "))))
                ));
            }
        }
        Err(e) => out.push_str(&format!(
            "\n    {}\n",
            style.dim(&format!("unreadable: {}", clean(e)))
        )),
    }
    out
}

/// `residue.restore/v1`. A listing has every entry of both kinds; after a
/// restore, `trash` or `removed` holds only the entry it restored, as it
/// was before, and `restore` says how it came back.
fn list_json(found: &Found, started: i64, done: Option<(Pick, &Done)>) -> Json {
    let (trash, removed): (Vec<Json>, Vec<Json>) = match done {
        None => (
            found.trash.iter().map(trash::item_json).collect(),
            found.removed.iter().map(removed::item_json).collect(),
        ),
        Some((Pick::Trash(i), _)) => (vec![trash::item_json(&found.trash[i])], Vec::new()),
        Some((Pick::Removed(i), _)) => (Vec::new(), vec![removed::item_json(&found.removed[i])]),
    };
    let restore = done.map(|(pick, d)| {
        Json::Obj(vec![
            (
                "kind",
                Json::str(match pick {
                    Pick::Trash(_) => "trash",
                    Pick::Removed(_) => "removed",
                }),
            ),
            ("entry", Json::str(name_of(found, pick))),
            ("path", Json::path(&d.path)),
            ("branch", Json::opt(d.branch.clone(), Json::Str)),
            ("head", Json::opt(d.head.clone(), Json::Str)),
            ("checkout", Json::str(d.how.code())),
            (
                "moved_from",
                match &d.how {
                    How::MovedBack(from) => Json::path(from),
                    _ => Json::Null,
                },
            ),
            (
                "new_branch",
                match &d.how {
                    How::NewBranch { name, .. } => Json::str(name.clone()),
                    _ => Json::Null,
                },
            ),
            (
                "branch_tip",
                match &d.how {
                    How::Branch { tip } | How::NewBranch { tip, .. } => Json::str(tip.clone()),
                    _ => Json::Null,
                },
            ),
            (
                "not_kept",
                Json::Arr(d.not_kept.iter().map(removed::Gone::json).collect()),
            ),
            (
                "notes",
                Json::Arr(d.notes.iter().map(|n| Json::str(n.clone())).collect()),
            ),
        ])
    });
    Json::Obj(vec![
        ("schema", Json::str(output::SCHEMA_RESTORE)),
        ("version", Json::str(VERSION)),
        ("generated_at", Json::str(output::rfc3339(started))),
        (
            "restored",
            Json::opt(done.map(|(_, d)| d.path.as_path()), Json::path),
        ),
        ("restore", Json::opt(restore, |r| r)),
        ("trash", Json::Arr(trash)),
        ("removed", Json::Arr(removed)),
        (
            "errors",
            Json::Arr(found.errors.iter().map(|e| Json::str(e.clone())).collect()),
        ),
    ])
}
