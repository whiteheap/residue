//! Hardened Git invocations and porcelain parsers.
//!
//! Git runs with optional locks off (no index refresh writes), prompts off,
//! fsmonitor and hooks disabled, lazy fetches off, and with inherited
//! `GIT_DIR`-style variables removed so a hook environment cannot redirect
//! a query to another repository, and with config from the environment
//! removed so it cannot bend `git status` or widen the regenerable rules.
//! Arguments are passed as argv, never through a shell.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::run::{self, RunError};

const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const REMOVE_TIMEOUT: Duration = Duration::from_secs(1800);
const MAX_OUTPUT: usize = 256 << 20;
pub const MIN_VERSION: (u32, u32) = (2, 36);

const CONFIG: &[&str] = &[
    "core.fsmonitor=false",
    "core.untrackedCache=false",
    "core.hooksPath=/dev/null",
    "core.quotePath=false",
    "color.ui=false",
];

/// Variables that point Git at another repository, and `GIT_CONFIG`, which
/// makes `git config` read that one file in place of every config file.
const REDIRECT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG",
];

/// Variables that add config from the environment: `git -c` and
/// `--config-env` export `GIT_CONFIG_PARAMETERS` to their children, and
/// `GIT_CONFIG_COUNT` turns on the `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`
/// pairs. Environment is inherited by every subprocess and CI step, so it
/// must not widen what residue deletes. `GIT_CONFIG_GLOBAL`,
/// `GIT_CONFIG_SYSTEM` and `GIT_CONFIG_NOSYSTEM` stay: they choose which
/// config files Git reads, and residue reads the same files as the user's
/// own Git.
const CONFIG_ENV: &[&str] = &["GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT"];

fn command(dir: &Path) -> Command {
    let mut cmd = command_keeping_config_env(dir);
    for var in CONFIG_ENV {
        cmd.env_remove(var);
    }
    cmd
}

/// `command` without removing `CONFIG_ENV`. Only for reporting what the
/// environment sets; never for a query whose answer residue acts on.
fn command_keeping_config_env(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    for setting in CONFIG {
        cmd.arg("-c").arg(setting);
    }
    for var in REDIRECT_ENV {
        cmd.env_remove(var);
    }
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("LC_ALL", "C");
    cmd
}

fn describe(args: &[&OsStr]) -> String {
    let words: Vec<String> = args
        .iter()
        .take(2)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    format!("git {}", words.join(" "))
}

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no error output")
        .trim()
        .to_string()
}

/// The raw result of a git command: its exit code and stdout.
pub struct Exit {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

fn exec(dir: &Path, args: &[&OsStr], timeout: Duration) -> Result<Exit, String> {
    exec_command(command(dir), args, timeout)
}

fn exec_command(mut cmd: Command, args: &[&OsStr], timeout: Duration) -> Result<Exit, String> {
    cmd.args(args);
    match run::run(&mut cmd, timeout, MAX_OUTPUT) {
        Ok(out) => Ok(Exit {
            success: out.status.success(),
            code: out.status.code(),
            stdout: out.stdout,
            stderr: first_line(&out.stderr),
        }),
        Err(RunError::NotFound) => Err("git is not installed or not on PATH".into()),
        Err(e) => Err(format!("{}: {e}", describe(args))),
    }
}

/// Run git and require success.
pub fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let exit = exec(dir, &args, QUERY_TIMEOUT)?;
    if exit.success {
        Ok(exit.stdout)
    } else {
        Err(format!("{} failed: {}", describe(&args), exit.stderr))
    }
}

/// Run git and return the exit status as data, for commands whose nonzero
/// exit carries meaning (for example `git config --get` on an unset key).
pub fn git_exit(dir: &Path, args: &[&str]) -> Result<Exit, String> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    exec(dir, &args, QUERY_TIMEOUT)
}

pub fn require_version() -> Result<(), String> {
    let out = git(Path::new("."), &["--version"])?;
    let text = String::from_utf8_lossy(&out);
    let version = text
        .split_whitespace()
        .nth(2)
        .ok_or_else(|| format!("could not read the git version from {:?}", text.trim()))?;
    let mut parts = version.split('.').map(|p| p.parse::<u32>());
    let (Some(Ok(major)), Some(Ok(minor))) = (parts.next(), parts.next()) else {
        return Err(format!(
            "could not read the git version from {:?}",
            text.trim()
        ));
    };
    if (major, minor) < MIN_VERSION {
        return Err(format!(
            "git {major}.{minor} is too old; residue needs git {}.{} or later",
            MIN_VERSION.0, MIN_VERSION.1
        ));
    }
    Ok(())
}

fn path_from(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

fn lines(out: &[u8]) -> Vec<&[u8]> {
    out.split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect()
}

/// The canonical Git common directory of the repository containing `dir`.
pub fn common_dir(dir: &Path) -> Result<PathBuf, String> {
    let out = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let found = lines(&out);
    let [only] = found.as_slice() else {
        return Err("git rev-parse returned an unexpected answer".into());
    };
    canonical(&path_from(only))
}

pub fn canonical(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub struct Identity {
    pub toplevel: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// Where Git itself says this checkout lives. Used to confirm that a listed
/// worktree path is the checkout residue thinks it is before judging it.
pub fn identity(dir: &Path) -> Result<Identity, String> {
    let out = git(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
    )?;
    let found = lines(&out);
    let [top, git_dir, common] = found.as_slice() else {
        return Err("git rev-parse returned an unexpected answer (a path with a newline?)".into());
    };
    Ok(Identity {
        toplevel: canonical(&path_from(top))?,
        git_dir: canonical(&path_from(git_dir))?,
        common_dir: canonical(&path_from(common))?,
    })
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Full ref name, for example `refs/heads/fix-login`.
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    /// `Some("")` when locked without a reason.
    pub locked: Option<String>,
    pub prunable: Option<String>,
}

impl Entry {
    fn new(path: PathBuf) -> Entry {
        Entry {
            path,
            head: None,
            branch: None,
            bare: false,
            detached: false,
            locked: None,
            prunable: None,
        }
    }

    pub fn short_branch(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b))
    }

    pub fn short_head(&self) -> Option<&str> {
        self.head.as_deref().map(|h| &h[..h.len().min(12)])
    }
}

/// `git worktree list --porcelain -z`. The first entry is the main
/// worktree (or the bare repository); the rest are linked worktrees.
pub fn worktrees(dir: &Path) -> Result<Vec<Entry>, String> {
    let out = git(dir, &["worktree", "list", "--porcelain", "-z"])?;
    parse_worktrees(&out)
}

fn parse_worktrees(out: &[u8]) -> Result<Vec<Entry>, String> {
    let text = |v: &[u8]| String::from_utf8_lossy(v).into_owned();
    let mut entries = Vec::new();
    let mut current: Option<Entry> = None;
    for field in out.split(|b| *b == 0) {
        if field.is_empty() {
            entries.extend(current.take());
            continue;
        }
        let (label, value) = match field.iter().position(|b| *b == b' ') {
            Some(i) => (&field[..i], Some(&field[i + 1..])),
            None => (field, None),
        };
        if label == b"worktree" {
            entries.extend(current.take());
            let path = value.ok_or("git worktree list: a worktree record has no path")?;
            current = Some(Entry::new(path_from(path)));
            continue;
        }
        let entry = current
            .as_mut()
            .ok_or("git worktree list: an attribute came before its worktree")?;
        match label {
            b"HEAD" => entry.head = value.map(text),
            b"branch" => entry.branch = value.map(text),
            b"bare" => entry.bare = true,
            b"detached" => entry.detached = true,
            b"locked" => entry.locked = Some(value.map(text).unwrap_or_default()),
            b"prunable" => entry.prunable = Some(value.map(text).unwrap_or_default()),
            // Attributes added by newer Git versions carry no safety meaning
            // residue relies on; every check below asks Git directly.
            _ => {}
        }
    }
    entries.extend(current.take());
    if entries.is_empty() {
        return Err("git worktree list returned no worktrees".into());
    }
    Ok(entries)
}

#[derive(Default)]
pub struct Status {
    pub tracked: Vec<String>,
    pub untracked: Vec<String>,
    /// Each ignored entry as text (for display and rule matching), and as
    /// the exact relative path without a trailing slash (for the size walk).
    pub ignored: Vec<(String, PathBuf)>,
}

/// Staged, modified, untracked and ignored entries. Ignored and untracked
/// directories are reported once, as `dir/`, the way Git collapses them.
pub fn status(dir: &Path) -> Result<Status, String> {
    let out = git(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignored=traditional",
            "--ignore-submodules=none",
        ],
    )?;
    let mut status = Status::default();
    let mut fields = out.split(|b| *b == 0);
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        if record.len() < 4 || record[2] != b' ' {
            return Err("git status returned a record residue cannot read".into());
        }
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        match &record[..2] {
            b"??" => status.untracked.push(path),
            b"!!" => {
                let raw = &record[3..];
                let raw = raw.strip_suffix(b"/").unwrap_or(raw);
                status.ignored.push((path, path_from(raw)));
            }
            xy => {
                status.tracked.push(path);
                // A rename or copy is followed by its source path.
                if matches!(xy[0], b'R' | b'C') || matches!(xy[1], b'R' | b'C') {
                    fields.next();
                }
            }
        }
    }
    Ok(status)
}

/// The values of `residue.regenerable` in this worktree's git config, in
/// the order Git reads them. An unset key is an empty list.
pub fn regenerable_config(dir: &Path) -> Result<Vec<String>, String> {
    let exit = git_exit(
        dir,
        &[
            "config",
            "--null",
            "--get-all",
            crate::policy::GIT_CONFIG_KEY,
        ],
    )?;
    if exit.code == Some(1) {
        return Ok(Vec::new());
    }
    if !exit.success {
        return Err(format!(
            "git config --get-all {} failed: {}",
            crate::policy::GIT_CONFIG_KEY,
            exit.stderr
        ));
    }
    // An empty value stays in the list so the caller rejects it rather than
    // silently skipping it.
    nul_fields(&exit.stdout)
        .into_iter()
        .map(|v| {
            String::from_utf8(v.to_vec()).map_err(|_| {
                format!(
                    "{} has a value that is not valid UTF-8: {:?}",
                    crate::policy::GIT_CONFIG_KEY,
                    String::from_utf8_lossy(v)
                )
            })
        })
        .collect()
}

/// The fields of `git config --null` output, each of which ends in a NUL.
/// An empty field (a value set to the empty string) is kept.
fn nul_fields(out: &[u8]) -> Vec<&[u8]> {
    if out.is_empty() {
        return Vec::new();
    }
    let body = out.strip_suffix(&[0]).unwrap_or(out);
    body.split(|b| *b == 0).collect()
}

/// The `residue.regenerable` values the inherited environment sets, which
/// residue ignores: `git -c` and `--config-env` (`GIT_CONFIG_PARAMETERS`),
/// `GIT_CONFIG_COUNT` with `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`, and
/// the file named by the legacy `GIT_CONFIG`. Git itself parses the
/// variables, so quoting and key case follow Git's rules. Used only to tell
/// the user; the rules come from `regenerable_config`. Runs no Git when
/// none of the variables is set.
pub fn environment_regenerable() -> Result<Vec<String>, String> {
    let key = OsStr::new(crate::policy::GIT_CONFIG_KEY);
    let set = |var: &str| std::env::var_os(var).filter(|v| !v.is_empty());
    let mut found = Vec::new();
    if CONFIG_ENV.iter().any(|var| set(var).is_some()) {
        // `/` so no repository's own config is read or can fail the query.
        let args = ["config", "--show-scope", "--null", "--get-all"].map(OsStr::new);
        let exit = exec_command(
            command_keeping_config_env(Path::new("/")),
            &[&args[..], &[key]].concat(),
            QUERY_TIMEOUT,
        )?;
        // Pairs of scope and value; the environment is the `command` scope.
        let fields = config_values(exit, "the environment")?;
        for pair in fields.chunks(2) {
            if let [scope, value] = pair {
                if scope.as_slice() == b"command" {
                    found.push(String::from_utf8_lossy(value).into_owned());
                }
            }
        }
    }
    if let Some(file) = set("GIT_CONFIG") {
        let file = std::path::absolute(&file).map_err(|e| format!("GIT_CONFIG: {e}"))?;
        let args = [
            OsStr::new("config"),
            OsStr::new("--file"),
            file.as_os_str(),
            OsStr::new("--null"),
            OsStr::new("--get-all"),
            key,
        ];
        let exit = exec(Path::new("/"), &args, QUERY_TIMEOUT)?;
        let fields = config_values(exit, "the file named by GIT_CONFIG")?;
        found.extend(
            fields
                .iter()
                .map(|v| String::from_utf8_lossy(v).into_owned()),
        );
    }
    Ok(found)
}

/// The NUL-separated fields of a `git config --get-all` that may find no
/// value (exit 1).
fn config_values(exit: Exit, what: &str) -> Result<Vec<Vec<u8>>, String> {
    if exit.code == Some(1) {
        return Ok(Vec::new());
    }
    if !exit.success {
        return Err(format!("git could not read {what}: {}", exit.stderr));
    }
    Ok(nul_fields(&exit.stdout)
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect())
}

pub fn remove_worktree(main: &Path, worktree: &Path) -> Result<(), String> {
    let args = [
        OsStr::new("worktree"),
        OsStr::new("remove"),
        worktree.as_os_str(),
    ];
    let exit = exec(main, &args, REMOVE_TIMEOUT)?;
    if exit.success {
        Ok(())
    } else {
        Err(format!(
            "git worktree remove refused (exit {}): {}",
            exit.code.map_or("signal".to_string(), |c| c.to_string()),
            exit.stderr
        ))
    }
}

/// Tracked paths whose index entry hides worktree edits from `git status`:
/// marked assume-unchanged (a lowercase tag) or skip-worktree (`S`).
/// `git worktree remove` would delete such edits without noticing them.
pub fn hidden_from_status(dir: &Path) -> Result<Vec<String>, String> {
    let out = git(dir, &["ls-files", "-v", "-z"])?;
    let mut hidden = Vec::new();
    for record in out.split(|b| *b == 0) {
        if record.len() < 3 || record[1] != b' ' {
            continue;
        }
        if record[0].is_ascii_lowercase() || record[0] == b'S' {
            hidden.push(String::from_utf8_lossy(&record[2..]).into_owned());
        }
    }
    Ok(hidden)
}
