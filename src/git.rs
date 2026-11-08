//! Hardened Git invocations and porcelain parsers.
//!
//! Git runs with optional locks off (no index refresh writes), prompts off,
//! fsmonitor and hooks disabled, lazy fetches off, and with inherited
//! `GIT_DIR`-style variables removed so a hook environment cannot redirect
//! a query to another repository, and with config from the environment
//! removed so it cannot bend `git status` or widen the regenerable rules.
//! Arguments are passed as argv, never through a shell.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::policy::RuleSource;
use crate::run::{self, RunError};

const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const REMOVE_TIMEOUT: Duration = Duration::from_secs(1800);
const MAX_OUTPUT: usize = 256 << 20;
pub const MIN_VERSION: (u32, u32) = (2, 36);

const CONFIG: &[&str] = &[
    "core.fsmonitor=false",
    "core.untrackedCache=false",
    // Preload runs the index refresh's lstat calls on up to 20 threads. On a
    // warm 98,466-entry worktree, `git status` without it took 0.04 to 0.23 s
    // longer and used a third to a half of the system CPU; residue spends
    // the time to save the CPU (#75).
    "core.preloadIndex=false",
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

/// The program a PATH search finds, which every command runs unless
/// [`program`] finds the binary behind a launcher.
const GIT: &str = "git";

/// The Git program every command runs, resolved once per process.
///
/// On macOS, `git` on PATH is usually `/usr/bin/git`, Apple's launcher,
/// which finds and runs the real binary of the selected developer tools on
/// every call: 8 to 25 ms of wall time and 6.5 to 9 ms of CPU each (#81).
/// When the first `git` on PATH is that launcher, residue asks it once for
/// `--exec-path` and runs the file `<exec-path>/git` is, the binary the
/// launcher runs, by its path. Anything else first on PATH, such as
/// Homebrew's Git or a wrapper the user put there, still runs as `git`. So
/// does the launcher while `GIT_EXEC_PATH` is set: `--exec-path` then
/// prints that directory, not the launcher's own Git.
fn program() -> &'static OsStr {
    #[cfg(target_os = "macos")]
    {
        use std::sync::OnceLock;
        use std::sync::atomic::Ordering;
        static RESOLVED: OnceLock<OsString> = OnceLock::new();
        if !launcher::MOVED.load(Ordering::Relaxed) {
            return RESOLVED.get_or_init(launcher::resolve);
        }
    }
    OsStr::new(GIT)
}

/// The resolved binary is gone, as when the developer tools are reinstalled
/// under a long `watch`: from now on this process runs `git` from PATH.
fn program_moved() {
    #[cfg(target_os = "macos")]
    launcher::MOVED.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
mod launcher {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    use super::{GIT, QUERY_TIMEOUT, command};
    use crate::run;

    /// Set once the resolved binary is gone ([`super::program_moved`]).
    pub static MOVED: AtomicBool = AtomicBool::new(false);

    const LAUNCHER: &str = "/usr/bin/git";

    /// The most `git --exec-path` may print.
    const MAX_OUTPUT: usize = 64 << 10;

    /// `<exec-path>/git`, or `git` when the launcher is not first on PATH,
    /// `GIT_EXEC_PATH` is set, or any step fails, so a Mac without
    /// developer tools fails the same way it always did.
    pub fn resolve() -> OsString {
        real().map_or_else(|| OsString::from(GIT), PathBuf::into_os_string)
    }

    fn real() -> Option<PathBuf> {
        // Git ignores an empty GIT_EXEC_PATH, and so does this.
        if std::env::var_os("GIT_EXEC_PATH").is_some_and(|v| !v.is_empty()) {
            return None;
        }
        if std::fs::canonicalize(first_on_path()?).ok()? != Path::new(LAUNCHER) {
            return None;
        }
        // The file itself, at the path the launcher runs it by: here
        // `git-core/git` links to `../../bin/git`.
        let git = std::fs::canonicalize(exec_path()?.join(GIT)).ok()?;
        executable(&git).then_some(git)
    }

    /// The file a PATH search for `git` finds first. As in `execvp`, an
    /// empty entry is the current directory.
    fn first_on_path() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| {
                if dir.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    dir
                }
            })
            .map(|dir| dir.join(GIT))
            .find(|candidate| executable(candidate))
    }

    /// The launcher's `--exec-path`, asked with the same environment and
    /// config as every other Git call, in `/` so no repository is read.
    fn exec_path() -> Option<PathBuf> {
        let mut cmd = command(OsStr::new(GIT), Path::new("/"));
        cmd.arg("--exec-path");
        let out = run::run(&mut cmd, QUERY_TIMEOUT, MAX_OUTPUT).ok()?;
        if !out.status.success() {
            return None;
        }
        let mut bytes = out.stdout;
        while bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        let dir = PathBuf::from(OsString::from_vec(bytes));
        dir.is_absolute().then_some(dir)
    }

    /// A regular file, after symbolic links, that someone may execute.
    fn executable(path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
}

fn command(program: &OsStr, dir: &Path) -> Command {
    let mut cmd = command_keeping_config_env(program, dir);
    for var in CONFIG_ENV {
        cmd.env_remove(var);
    }
    cmd
}

/// `command` without removing `CONFIG_ENV`. Only for reporting what the
/// environment sets; never for a query whose answer residue acts on.
fn command_keeping_config_env(program: &OsStr, dir: &Path) -> Command {
    let mut cmd = Command::new(program);
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

/// What [`first_line`] says for a command that wrote nothing to stderr.
const NO_ERROR_OUTPUT: &str = "no error output";

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(NO_ERROR_OUTPUT)
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

/// Builds one Git command for a given program: [`command`], or a caller's
/// variant of it with more environment.
type Build<'a> = &'a dyn Fn(&OsStr) -> Command;

fn exec(dir: &Path, args: &[&OsStr], timeout: Duration) -> Result<Exit, String> {
    exec_command(&|program| command(program, dir), args, timeout)
}

fn exec_command(build: Build, args: &[&OsStr], timeout: Duration) -> Result<Exit, String> {
    exec_input(build, args, None, timeout)
}

/// [`exec_command`] with `input` on git's standard input.
fn exec_input(
    build: Build,
    args: &[&OsStr],
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Exit, String> {
    let result = spawn(build, args, |cmd| {
        run::run_with_input(cmd, input.map(<[u8]>::to_vec), timeout, MAX_OUTPUT)
    });
    exit_of(result, args)
}

/// Run the command `build` makes for [`program`], with `args`, through
/// `run`. When that program is a resolved path that no longer exists, run
/// `git` from PATH instead, now and for the rest of the process.
fn spawn(
    build: Build,
    args: &[&OsStr],
    run: impl Fn(&mut Command) -> Result<run::Captured, RunError>,
) -> Result<run::Captured, RunError> {
    let attempt = |program: &OsStr| {
        let mut cmd = build(program);
        cmd.args(args);
        run(&mut cmd)
    };
    let program = program();
    match attempt(program) {
        Err(RunError::NotFound) if program != GIT => {
            program_moved();
            attempt(OsStr::new(GIT))
        }
        result => result,
    }
}

fn exit_of(result: Result<run::Captured, RunError>, args: &[&OsStr]) -> Result<Exit, String> {
    match result {
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

/// [`git`], keeping only the output records that `filter` accepts. Each
/// attempt [`spawn`] makes reads with its own copy of `filter`, so a retry
/// after a moved binary starts from the state the caller gave.
fn git_filtered<K: FnMut(&[u8]) -> bool + Clone + Send + 'static>(
    dir: &Path,
    args: &[&str],
    filter: run::Filter<K>,
) -> Result<Vec<u8>, String> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let result = spawn(&|program| command(program, dir), &args, |cmd| {
        run::run_filtered(cmd, QUERY_TIMEOUT, MAX_OUTPUT, filter.clone())
    });
    let exit = exit_of(result, &args)?;
    if exit.success {
        Ok(exit.stdout)
    } else {
        Err(format!("{} failed: {}", describe(&args), exit.stderr))
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
    /// True when `ignored` comes from `--ignored=matching`, apart from the
    /// entries under `limited`. [`status`] takes that list only where it
    /// lists what `--ignored=traditional` lists, plus the empty ignored
    /// directories traditional mode hides.
    pub matching: bool,
    /// The folders whose ignored entries come from a traditional run
    /// limited to them, so `ignored` holds just what traditional mode
    /// prints there.
    pub limited: Vec<PathBuf>,
    /// What [`hidden_from_status`] returns, when `status` read `git
    /// ls-files -v` for its own check, so the caller need not read it again.
    pub hidden: Option<Result<Vec<String>, String>>,
}

impl Status {
    /// Whether `--ignored=traditional` printed the ignored entry at `rel`:
    /// the whole list came from it, or the entry is in a limited folder.
    pub fn traditional_listed(&self, rel: &Path) -> bool {
        !self.matching || self.limited.iter().any(|folder| rel.starts_with(folder))
    }
}

/// Staged, modified, untracked and ignored entries, as `git status
/// --ignored=traditional` lists them: an ignored or untracked directory
/// once, as `dir/`, the way Git collapses it.
///
/// Traditional mode reads every path inside each ignored directory, such as
/// `node_modules/`, only to print the directory once. `--ignored=matching`
/// does not go inside a directory an ignore pattern matches (#83). Its list
/// differs in two ways (`treat_directory` in Git's dir.c, the same from 2.36
/// to 2.56):
///
/// - Git collapses an untracked directory that holds only ignored entries.
///   Traditional mode lists the directory, matching mode what is in it. Git
///   collapses only a directory with no index entry below it, so where the
///   parent folder of an ignored entry below the top level has one, the
///   entry is in no collapsed directory. [`index_scan`] answers that from
///   the `git ls-files -v` the `index_flags` check reads anyway. The
///   topmost folders with none are listed again by one traditional run
///   limited to them ([`limited_status`]), and its entries replace matching
///   mode's there (#120). When the read or that run fails, or the folders
///   pass [`MAX_LIMITED`] or [`MAX_LIMITED_BYTES`], this runs traditional
///   mode on the whole worktree, as residue did before.
/// - Matching mode also lists an ignored directory that holds no file or
///   symbolic link, which traditional mode hides. The walk finds those, and
///   `inspect::drop_unlisted` drops them outside the limited folders; it
///   asks [`traditional_ignored_in`] about the ones it cannot settle.
///
/// `matching` and `limited` say which list each entry is from. A top-level
/// entry needs no check: Git never collapses the worktree root.
pub fn status(dir: &Path) -> Result<Status, String> {
    // A matching run that fails (it can print more than traditional mode,
    // one line per file of a collapsed directory) is answered by the
    // traditional run, whose own failure is the error.
    let Ok(mut status) = status_with(dir, "--ignored=matching") else {
        return traditional_status(dir);
    };
    status.matching = true;
    let parents = parent_folders(&status.ignored);
    if parents.is_empty() {
        return Ok(status);
    }
    let asked: HashSet<Vec<u8>> = parents
        .iter()
        .flat_map(|parent| folder_and_ancestors(parent))
        .map(<[u8]>::to_vec)
        .collect();
    let scan = index_scan(dir, &asked);
    let folders = scan
        .as_ref()
        .ok()
        .map(|scan| topmost_untracked(&parents, &scan.tracked));
    status.hidden = Some(scan.map(|scan| scan.hidden));
    // A limited run that fails is answered by the full one, as before.
    let listed = match &folders {
        Some(folders) if folders.is_empty() => return Ok(status),
        Some(folders) if within_limit(folders) => limited_status(dir, folders).ok(),
        _ => None,
    };
    if let (Some(folders), Some(listed)) = (folders, listed) {
        merge_limited(&mut status, folders, listed);
        return Ok(status);
    }
    let hidden = status.hidden.take();
    drop(status);
    let mut traditional = traditional_status(dir)?;
    traditional.hidden = hidden;
    Ok(traditional)
}

/// [`status`] from `git status --ignored=traditional`, always.
pub fn traditional_status(dir: &Path) -> Result<Status, String> {
    status_with(dir, "--ignored=traditional")
}

/// What `git status --ignored=traditional` lists of `paths`, relative
/// paths without a trailing slash, none inside another: one run limited to
/// them ([`limited_status`]), or, when that fails or they pass the cap,
/// the whole worktree's list.
pub fn traditional_ignored_in(
    dir: &Path,
    paths: &[&Path],
) -> Result<Vec<(String, PathBuf)>, String> {
    let paths: Vec<Vec<u8>> = paths
        .iter()
        .map(|p| p.as_os_str().as_bytes().to_vec())
        .collect();
    let limited = within_limit(&paths)
        .then(|| limited_status(dir, &paths).ok())
        .flatten();
    match limited {
        Some(listed) => Ok(listed),
        None => Ok(traditional_status(dir)?.ignored),
    }
}

/// The most folders one limited run takes; past it, [`status`] runs
/// traditional mode on the whole worktree. Git matches every index entry
/// and every path it reads against each folder, so the run's CPU grows
/// with their number. On a worktree of 20,000 tracked files and a
/// 25,000-file `node_modules/`, the whole traditional run took 0.11 to
/// 0.14 s, and one limited to 64 such folders 0.02 to 0.04 s, to 256 0.05
/// to 0.06 s and to 1,024 0.21 to 0.23 s (#120). 64 stays well below the
/// point where limiting costs more.
const MAX_LIMITED: usize = 64;

/// The most bytes of folder paths one limited run takes. Each folder is one
/// argument, and 64 KiB keeps the command line well inside the smallest
/// `ARG_MAX` residue runs on, 256 KiB on NetBSD and DragonFly, environment
/// included.
const MAX_LIMITED_BYTES: usize = 64 << 10;

fn within_limit(folders: &[Vec<u8>]) -> bool {
    folders.len() <= MAX_LIMITED && folders.iter().map(Vec::len).sum::<usize>() <= MAX_LIMITED_BYTES
}

/// Pathspec settings from the environment, which would change how Git
/// reads the folders [`limited_status`] names, or make it refuse
/// `--literal-pathspecs`.
const PATHSPEC_ENV: &[&str] = &[
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
    "GIT_LITERAL_PATHSPECS",
];

/// The ignored entries `git status --ignored=traditional` prints in
/// `folders` (relative paths without a trailing slash), in one run. Each
/// folder is read as a literal path, so a name with `*`, `?`, `[` or a
/// leading `:` means only itself.
fn limited_status(dir: &Path, folders: &[Vec<u8>]) -> Result<Vec<(String, PathBuf)>, String> {
    let mut args: Vec<&OsStr> = [
        "--literal-pathspecs",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=normal",
        "--ignored=traditional",
        "--ignore-submodules=none",
        "--",
    ]
    .iter()
    .map(OsStr::new)
    .collect();
    args.extend(folders.iter().map(|folder| OsStr::from_bytes(folder)));
    let build = |program: &OsStr| {
        let mut cmd = command(program, dir);
        for var in PATHSPEC_ENV {
            cmd.env_remove(var);
        }
        cmd
    };
    let exit = exec_command(&build, &args, QUERY_TIMEOUT)?;
    if !exit.success {
        return Err(format!("{} failed: {}", describe(&args), exit.stderr));
    }
    Ok(parse_status(&exit.stdout)?.ignored)
}

/// The topmost folder above each of `parents` that holds no index entry,
/// once each and sorted, given the folders that hold one. These are the
/// folders Git can collapse; none is inside another.
fn topmost_untracked(parents: &HashSet<Vec<u8>>, tracked: &HashSet<Vec<u8>>) -> Vec<Vec<u8>> {
    let folders: std::collections::BTreeSet<&[u8]> = parents
        .iter()
        .filter_map(|parent| folder_and_ancestors(parent).find(|f| !tracked.contains(*f)))
        .collect();
    folders.into_iter().map(<[u8]>::to_vec).collect()
}

/// Replace the ignored entries of `status` in `folders` with `listed`, the
/// limited run's, and keep Git's order: by the path as printed, a folder
/// with its trailing slash.
fn merge_limited(status: &mut Status, folders: Vec<Vec<u8>>, listed: Vec<(String, PathBuf)>) {
    let folders: Vec<PathBuf> = folders.iter().map(|f| path_from(f)).collect();
    let inside = |rel: &Path| folders.iter().any(|folder| rel.starts_with(folder));
    status.ignored.retain(|(_, rel)| !inside(rel));
    status
        .ignored
        .extend(listed.into_iter().filter(|(_, rel)| inside(rel)));
    status.ignored.sort_by_cached_key(|(text, rel)| {
        let mut printed = rel.as_os_str().as_bytes().to_vec();
        if text.ends_with('/') {
            printed.push(b'/');
        }
        printed
    });
    status.limited = folders;
}

/// The parent folder of each ignored entry below the top level, once.
fn parent_folders(ignored: &[(String, PathBuf)]) -> HashSet<Vec<u8>> {
    ignored
        .iter()
        .filter_map(|(_, rel)| parent_of(rel.as_os_str().as_bytes()))
        .map(<[u8]>::to_vec)
        .collect()
}

/// `a/b` for `a/b/c`; `None` at the top level.
fn parent_of(path: &[u8]) -> Option<&[u8]> {
    path.iter()
        .rposition(|b| *b == b'/')
        .map(|end| &path[..end])
}

/// `a`, `a/b` and `a/b/c` for the folder `a/b/c`.
fn folder_and_ancestors(folder: &[u8]) -> impl Iterator<Item = &[u8]> {
    folder
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'/')
        .map(move |(end, _)| &folder[..end])
        .chain(std::iter::once(folder))
}

/// One `git status --porcelain=v1 -z` run with `ignored` as its
/// `--ignored` mode.
fn status_with(dir: &Path, ignored: &str) -> Result<Status, String> {
    let out = git(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            ignored,
            "--ignore-submodules=none",
        ],
    )?;
    parse_status(&out)
}

/// The records of `git status --porcelain=v1 -z`.
fn parse_status(out: &[u8]) -> Result<Status, String> {
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
/// the order Git reads them, each with the scope and file Git read it from.
/// An unset key is an empty list.
pub fn regenerable_config(dir: &Path) -> Result<Vec<RuleSource>, String> {
    let key = crate::policy::GIT_CONFIG_KEY;
    let exit = git_exit(
        dir,
        &[
            "config",
            "--show-scope",
            "--show-origin",
            "--null",
            "--get-all",
            key,
        ],
    )?;
    if exit.code == Some(1) {
        return Ok(Vec::new());
    }
    if !exit.success {
        return Err(format!(
            "git config --get-all {key} failed: {}",
            exit.stderr
        ));
    }
    // Triples of scope, origin and value. An empty value stays in the list
    // so the caller rejects it rather than silently skipping it.
    let fields = nul_fields(&exit.stdout);
    if fields.len() % 3 != 0 {
        return Err(format!(
            "git config --show-scope --show-origin --get-all {key} returned an answer residue cannot read"
        ));
    }
    let text = |v: &[u8]| String::from_utf8_lossy(v).into_owned();
    let mut sources = Vec::new();
    let mut fields = fields.into_iter();
    while let (Some(scope), Some(origin), Some(value)) =
        (fields.next(), fields.next(), fields.next())
    {
        let rule = String::from_utf8(value.to_vec()).map_err(|_| {
            format!(
                "{key} has a value that is not valid UTF-8: {:?}",
                String::from_utf8_lossy(value)
            )
        })?;
        let origin = text(origin);
        sources.push(RuleSource {
            rule,
            scope: text(scope),
            origin: Some(origin.strip_prefix("file:").unwrap_or(&origin).to_string()),
        });
    }
    Ok(sources)
}

/// The time of the newest entry in this worktree's HEAD reflog, and its
/// message (`commit: fix login`), read by Git so any ref backend works.
/// `None` when the reflog is empty or disabled (`core.logAllRefUpdates`).
/// The entry's own timestamp, not the log file's mtime: `git gc` in any
/// worktree of the repository rewrites every worktree's `logs/HEAD`.
pub fn reflog_newest(dir: &Path) -> Result<Option<(i64, String)>, String> {
    let out = git(
        dir,
        &[
            "log",
            "-g",
            "-1",
            "--date=unix",
            "--format=%gd%x00%gs",
            "HEAD",
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let line = text.trim_end_matches('\n');
    if line.is_empty() {
        return Ok(None);
    }
    let unreadable = || format!("git log -g printed {:?}", line);
    let (selector, message) = line.split_once('\0').ok_or_else(unreadable)?;
    let seconds = selector
        .strip_prefix("HEAD@{")
        .and_then(|s| s.strip_suffix('}'))
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(unreadable)?;
    Ok(Some((seconds, message.to_string())))
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
            &|program| command_keeping_config_env(program, Path::new("/")),
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
    index_scan(dir, &HashSet::new()).map(|scan| scan.hidden)
}

/// What one `git ls-files -v -z` says about the index.
struct IndexScan {
    /// The entries [`hidden_from_status`] returns.
    hidden: Vec<String>,
    /// The folders asked about that have an index entry below them.
    tracked: HashSet<Vec<u8>>,
}

/// Read the index once for [`hidden_from_status`] and for [`status`]: the
/// entries that hide edits, and which of `folders` (relative paths without
/// a trailing slash) have an index entry below them, as Git's
/// `directory_exists_in_index` asks before it collapses a directory.
fn index_scan(dir: &Path, folders: &HashSet<Vec<u8>>) -> Result<IndexScan, String> {
    // Only the hidden records and the first record found below each folder
    // are held, as Git writes them: the whole list was 3.4 MB for a worktree
    // of 59,888 tracked files (#78).
    let mut unseen = folders.clone();
    let mut last = Vec::new();
    let keep = move |record: &[u8]| {
        if hides_edits(record) {
            return true;
        }
        if unseen.is_empty() {
            return false;
        }
        let Some(parent) = index_path(record).and_then(parent_of) else {
            return false;
        };
        // A record in the same folder as the one before it can mark no
        // folder that one did not.
        if parent == last.as_slice() {
            return false;
        }
        last.clear();
        last.extend_from_slice(parent);
        let mut marked = false;
        for folder in folder_and_ancestors(parent) {
            marked |= unseen.remove(folder);
        }
        marked
    };
    let filter = run::Filter { separator: 0, keep };
    let out = git_filtered(dir, &["ls-files", "-v", "-z"], filter)?;
    let mut hidden = Vec::new();
    let mut tracked = HashSet::new();
    for record in out.split(|b| *b == 0) {
        if hides_edits(record) {
            hidden.push(String::from_utf8_lossy(&record[2..]).into_owned());
        }
        if let Some(parent) = index_path(record).and_then(parent_of) {
            tracked.extend(
                folder_and_ancestors(parent)
                    .filter(|f| folders.contains(*f))
                    .map(<[u8]>::to_vec),
            );
        }
    }
    Ok(IndexScan { hidden, tracked })
}

/// The path of an `ls-files -v` record, after its tag and a space.
fn index_path(record: &[u8]) -> Option<&[u8]> {
    (record.len() >= 3 && record[1] == b' ').then(|| &record[2..])
}

/// An `ls-files -v` record whose tag hides edits from `git status`: a
/// lowercase tag (assume-unchanged) or `S` (skip-worktree).
fn hides_edits(record: &[u8]) -> bool {
    record.len() >= 3 && record[1] == b' ' && (record[0].is_ascii_lowercase() || record[0] == b'S')
}

/// Run one `git worktree` subcommand that changes state, from `main` (the
/// main checkout or the bare repository), and turn a refusal into an error
/// that quotes Git.
fn worktree_change(main: &Path, args: &[&OsStr]) -> Result<(), String> {
    let exit = exec(main, args, QUERY_TIMEOUT)?;
    if exit.success {
        Ok(())
    } else {
        let words: Vec<String> = args
            .iter()
            .take(2)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        Err(format!(
            "git {} refused (exit {}): {}",
            words.join(" "),
            exit.code.map_or("signal".to_string(), |c| c.to_string()),
            exit.stderr
        ))
    }
}

/// `git worktree move FROM TO`: one rename(2) by Git, which updates only
/// this worktree's record. Git refuses a locked worktree, a destination
/// that exists, and a move to another filesystem (it never copies).
pub fn move_worktree(main: &Path, from: &Path, to: &Path) -> Result<(), String> {
    worktree_change(
        main,
        &[
            OsStr::new("worktree"),
            OsStr::new("move"),
            from.as_os_str(),
            to.as_os_str(),
        ],
    )
}

pub fn lock_worktree(main: &Path, worktree: &Path, reason: &str) -> Result<(), String> {
    worktree_change(
        main,
        &[
            OsStr::new("worktree"),
            OsStr::new("lock"),
            OsStr::new("--reason"),
            OsStr::new(reason),
            worktree.as_os_str(),
        ],
    )
}

/// Only for a lock residue placed itself (`remove::REMOVING`,
/// `trash::LOCK`); residue never unlocks anyone else's.
pub fn unlock_worktree(main: &Path, worktree: &Path) -> Result<(), String> {
    worktree_change(
        main,
        &[
            OsStr::new("worktree"),
            OsStr::new("unlock"),
            worktree.as_os_str(),
        ],
    )
}

/// How long one `git fetch` may run before residue stops it. Long enough
/// for a slow link to bring a large batch of commits; short enough that a
/// connection that hangs does not hold `watch` for long.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The remotes that `branches` (full refnames, `refs/heads/fix`) track
/// through a remote-tracking upstream (`refs/remotes/...`), sorted and
/// without duplicates. A branch whose upstream is local (`.`), or a URL
/// without a remote-tracking ref, adds none: fetching it could not change
/// whether the branch counts as landed.
pub fn upstream_remotes(
    dir: &Path,
    branches: &std::collections::BTreeSet<String>,
) -> Result<Vec<String>, String> {
    let out = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(upstream)%00%(upstream:remotename)",
            "refs/heads/",
        ],
    )?;
    let mut remotes = std::collections::BTreeSet::new();
    for line in lines(&out) {
        let text = String::from_utf8_lossy(line);
        let mut fields = text.split('\0');
        let (Some(branch), Some(upstream), Some(remote)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if branches.contains(branch)
            && upstream.starts_with("refs/remotes/")
            && !remote.is_empty()
            && remote != "."
        {
            remotes.insert(remote.to_string());
        }
    }
    Ok(remotes.into_iter().collect())
}

/// Every remote-tracking ref and the object it names, to tell what a fetch
/// changed.
pub fn remote_refs(dir: &Path) -> Result<std::collections::BTreeMap<String, String>, String> {
    let out = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "refs/remotes/",
        ],
    )?;
    Ok(lines(&out)
        .into_iter()
        .filter_map(|line| {
            let text = String::from_utf8_lossy(line);
            let (name, object) = text.split_once('\0')?;
            Some((name.to_string(), object.to_string()))
        })
        .collect())
}

/// `git fetch --prune --no-tags REMOTE`, run by `watch` from the main
/// checkout (or the bare repository), so it notices a branch deleted on the
/// remote without waiting for the user's own fetch.
///
/// It changes only remote-tracking refs (and their reflogs) and adds
/// objects: no `FETCH_HEAD` (`--no-write-fetch-head`; a linked worktree's
/// `FETCH_HEAD` is part of its last change), no tags, no submodules, no
/// automatic `git maintenance` or `gc`, and hooks stay off (`command`).
///
/// It never waits for a person. Git's terminal prompt is off (`command`);
/// it runs in a new session, so neither Git nor SSH nor a credential
/// helper has a terminal to ask on; SSH gets `BatchMode=yes` through
/// `GIT_SSH_COMMAND`, unless the user chose their own SSH command
/// (`GIT_SSH_COMMAND`, `GIT_SSH`, `core.sshCommand`), which is respected;
/// SSH never runs an askpass program (`SSH_ASKPASS_REQUIRE=never`), and
/// `SSH_ASKPASS` is removed so Git does not fall back to it for a
/// password; Git Credential Manager is told never to prompt
/// (`GCM_INTERACTIVE=never`). An askpass program the user set for Git
/// itself (`GIT_ASKPASS`, `core.askPass`) is theirs, and stays. A fetch
/// that needs a person fails instead, and so does one that runs past
/// `FETCH_TIMEOUT`. The error is the first line Git or SSH printed.
pub fn fetch(dir: &Path, remote: &str) -> Result<(), String> {
    let user_ssh = std::env::var_os("GIT_SSH_COMMAND").is_some_and(|v| !v.is_empty())
        || std::env::var_os("GIT_SSH").is_some_and(|v| !v.is_empty())
        || ssh_command_configured(dir);
    let build = |program: &OsStr| {
        let mut cmd = command(program, dir);
        if !user_ssh {
            cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        cmd.env("SSH_ASKPASS_REQUIRE", "never")
            .env_remove("SSH_ASKPASS")
            .env("GCM_INTERACTIVE", "never");
        new_session(&mut cmd);
        cmd
    };
    let args: Vec<&OsStr> = [
        "fetch",
        "--prune",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-auto-maintenance",
        "--no-recurse-submodules",
        "--quiet",
        "--",
        remote,
    ]
    .into_iter()
    .map(OsStr::new)
    .collect();
    let exit = exec_command(&build, &args, FETCH_TIMEOUT)?;
    if exit.success {
        Ok(())
    } else {
        Err(format!(
            "exit {}: {}",
            exit.code.map_or("signal".to_string(), |c| c.to_string()),
            exit.stderr
        ))
    }
}

/// How `add_worktree` checks out the new worktree. Never `--force`: Git
/// refuses a branch checked out elsewhere, an existing branch for `-b`,
/// and a path that is not empty.
pub enum Checkout<'a> {
    /// `git worktree add PATH BRANCH`: the existing local branch.
    Branch(&'a str),
    /// `git worktree add -b BRANCH PATH COMMIT`: a new branch at COMMIT.
    NewBranch(&'a str, &'a str),
    /// `git worktree add --detach PATH COMMIT`.
    Detached(&'a str),
}

/// `git worktree add`, from `main`, with a checkout's time limit.
pub fn add_worktree(main: &Path, path: &Path, checkout: &Checkout) -> Result<(), String> {
    let mut args = vec![OsStr::new("worktree"), OsStr::new("add")];
    match checkout {
        Checkout::Branch(branch) => args.extend([path.as_os_str(), OsStr::new(branch)]),
        Checkout::NewBranch(branch, commit) => args.extend([
            OsStr::new("-b"),
            OsStr::new(branch),
            path.as_os_str(),
            OsStr::new(commit),
        ]),
        Checkout::Detached(commit) => {
            args.extend([OsStr::new("--detach"), path.as_os_str(), OsStr::new(commit)]);
        }
    }
    let exit = exec(main, &args, REMOVE_TIMEOUT)?;
    if exit.success {
        Ok(())
    } else {
        Err(format!(
            "git worktree add refused (exit {}): {}",
            exit.code.map_or("signal".to_string(), |c| c.to_string()),
            exit.stderr
        ))
    }
}

/// Whether `core.sshCommand` is set for `dir`. When Git cannot answer,
/// assume it is, so residue does not replace a command the user chose.
fn ssh_command_configured(dir: &Path) -> bool {
    match git_exit(dir, &["config", "--get", "core.sshCommand"]) {
        Ok(exit) if exit.code == Some(1) => false,
        Ok(exit) => !(exit.success && exit.stdout.trim_ascii().is_empty()),
        Err(_) => true,
    }
}

/// Start `cmd` in a new session, with no controlling terminal: a program
/// that would ask on `/dev/tty` gets an error instead of a prompt.
fn new_session(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs in the child between fork and exec, and
    // calls only setsid(2), which is async-signal-safe and touches no
    // memory of this process.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// The commit `name` (a full ref name or an object name) resolves to, or
/// `None` when it names no commit in this repository: `rev-parse --verify
/// --quiet` exits 1 for that, and anything else is an error.
pub fn commit_of(dir: &Path, name: &str) -> Result<Option<String>, String> {
    let exit = git_exit(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ],
    )?;
    match exit.code {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&exit.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => Err(format!(
            "git rev-parse --verify {name} failed: {}",
            exit.stderr
        )),
    }
}

/// The ref `full` exists, whatever it points at.
pub fn ref_exists(dir: &Path, full: &str) -> Result<bool, String> {
    let exit = git_exit(dir, &["show-ref", "--verify", "--quiet", full])?;
    match exit.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "git show-ref --verify {full} failed: {}",
            exit.stderr
        )),
    }
}

/// `ancestor` is `descendant` or one of its ancestors.
pub fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let exit = git_exit(dir, &["merge-base", "--is-ancestor", ancestor, descendant])?;
    match exit.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "git merge-base --is-ancestor failed: {}",
            exit.stderr
        )),
    }
}

/// One change in a [`change_refs`] transaction.
pub enum RefChange<'a> {
    /// Create the ref at a commit; Git refuses when it already exists, so
    /// residue never overwrites one.
    Create { name: &'a str, commit: &'a str },
    /// Delete the ref only while it points at this commit; Git refuses when
    /// it points anywhere else or does not exist.
    Delete { name: &'a str, commit: &'a str },
}

/// Apply `changes` with `git update-ref --stdin`: one transaction, so all
/// of them happen or none. The refs are synced to disk: `core.fsync`
/// adds `reference` to the platform default (Git 2.36 documents that a
/// value starts from the default and adds to it), and `core.fsyncMethod`
/// `fsync` flushes the drive's cache on macOS, where Git's default does not.
pub fn change_refs(dir: &Path, changes: &[RefChange]) -> Result<(), String> {
    if changes.is_empty() {
        return Ok(());
    }
    let mut input = String::new();
    for change in changes {
        let line = match change {
            RefChange::Create { name, commit } => format!("create {name} {commit}\n"),
            RefChange::Delete { name, commit } => format!("delete {name} {commit}\n"),
        };
        input.push_str(&line);
    }
    let args = [
        "-c",
        "core.fsync=reference",
        "-c",
        "core.fsyncMethod=fsync",
        "update-ref",
        "--no-deref",
        "--stdin",
    ];
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let exit = exec_input(
        &|program| command(program, dir),
        &args,
        Some(input.as_bytes()),
        QUERY_TIMEOUT,
    )?;
    if exit.success {
        Ok(())
    } else {
        Err(format!("git update-ref refused: {}", exit.stderr))
    }
}

/// The refs matching `patterns` (`refs/x/NAME` matches itself and refs
/// below it, never `refs/x/NAME-2`), with the object each points at.
pub fn refs_named(dir: &Path, patterns: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut args = vec!["for-each-ref", "--format=%(refname) %(objectname)"];
    args.extend_from_slice(patterns);
    let out = git(dir, &args)?;
    let mut refs = Vec::new();
    for line in lines(&out) {
        let line = String::from_utf8_lossy(line);
        let (name, oid) = line
            .rsplit_once(' ')
            .ok_or_else(|| format!("git for-each-ref printed {line:?}"))?;
        refs.push((name.to_string(), oid.to_string()));
    }
    Ok(refs)
}

/// The commits the HEAD reflog of the worktree at `dir` names, as Git
/// reads them, for a reflog that is no file (reftable), in lowercase: each
/// entry's new value, newest first, then the old value of the oldest
/// entry. No entry names that one as new once the entries before it are
/// gone (expired, or deleted with `git reflog delete`), and Git gives it
/// as `HEAD@{N}`, N the number of entries. Git reads no other old value,
/// so one that an entry after a gap in the middle names is not here
/// (`reflogs_commits` finds those). A worktree with no HEAD reflog answers
/// with none. A commit Git no longer has is left out (`--ignore-missing`).
pub fn head_reflog_commits(dir: &Path) -> Result<Vec<String>, String> {
    let out = git(
        dir,
        &["rev-list", "--ignore-missing", "--walk-reflogs", "HEAD"],
    )?;
    let mut commits: Vec<String> = lines(&out)
        .into_iter()
        .map(|l| String::from_utf8_lossy(l).to_ascii_lowercase())
        .collect();
    // No line: no HEAD reflog, where every `HEAD@{k}` exits 1.
    if commits.is_empty() {
        return Ok(commits);
    }
    // rev-list prints a line for each entry whose new value is a commit, so
    // the oldest entry's index is the number of lines or more. Past the
    // last commit entry, `HEAD@{k}` is the new value of an entry rev-list
    // skipped, then the old value of the oldest entry, then nothing:
    // `rev-parse --verify --quiet` exits 128 and prints nothing, as it does
    // for a null old value (the entry that created the ref). Some entries
    // past the last line are all a reflog has; a Git that answers more
    // stops the removal rather than keep residue asking.
    const PAST_THE_LAST_LINE: usize = 64;
    let listed = commits.len();
    for k in listed..listed + PAST_THE_LAST_LINE {
        let selector = format!("HEAD@{{{k}}}^{{commit}}");
        let exit = git_exit(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &selector,
            ],
        )?;
        match exit.code {
            Some(0) => commits.push(
                String::from_utf8_lossy(&exit.stdout)
                    .trim()
                    .to_ascii_lowercase(),
            ),
            // An entry whose new value is no commit Git has.
            Some(1) => {}
            Some(128) if exit.stderr == NO_ERROR_OUTPUT => return Ok(commits),
            _ => {
                return Err(format!(
                    "git rev-parse --verify HEAD@{{{k}}} failed: {}",
                    exit.stderr
                ));
            }
        }
    }
    Err(format!(
        "git rev-parse --verify still answers for HEAD@{{{}}}, {PAST_THE_LAST_LINE} entries past the last one git rev-list --walk-reflogs listed, and residue asks no further",
        listed + PAST_THE_LAST_LINE - 1
    ))
}

/// Every commit the reflogs Git reads in the worktree at `dir` name, as
/// the old or the new value of an entry, in lowercase: those of the refs
/// all worktrees share, and the worktree's own (HEAD's, and its private
/// refs'), never another worktree's. `--single-worktree` must come before
/// `--reflog`, which reads every worktree's otherwise. A commit Git no
/// longer has is left out.
/// With no reflog at all (`git gc` with `gc.reflogExpire=now` leaves
/// none), rev-list prints its usage and exits 129 unless it is to read
/// commits from standard input: `--stdin`, on the null standard input
/// `git` gives it, adds none.
pub fn reflogs_commits(dir: &Path) -> Result<Vec<String>, String> {
    let out = git(
        dir,
        &[
            "rev-list",
            "--no-walk",
            "--ignore-missing",
            "--single-worktree",
            "--reflog",
            "--stdin",
        ],
    )?;
    Ok(lines(&out)
        .into_iter()
        .map(|l| String::from_utf8_lossy(l).to_ascii_lowercase())
        .collect())
}

/// The repository at `dir` keeps its refs in reftable: `git rev-parse
/// --show-ref-format` (Git 2.45 and later) says so. An earlier Git, which
/// cannot read reftable, prints the option back instead.
pub fn is_reftable(dir: &Path) -> Result<bool, String> {
    let out = git(dir, &["rev-parse", "--show-ref-format"])?;
    Ok(String::from_utf8_lossy(&out).trim() == "reftable")
}

/// Those of `commits` that no ref outside `refs/residue/` and no
/// worktree's HEAD reaches. The commits go in on standard input, before
/// `--not`, so their number is not bounded by the command line. A commit
/// Git no longer has is left out (`--ignore-missing`): there is nothing
/// left of it to keep.
pub fn unreferenced(dir: &Path, commits: &[&str]) -> Result<Vec<String>, String> {
    if commits.is_empty() {
        return Ok(Vec::new());
    }
    let args = [
        "rev-list",
        "--ignore-missing",
        "--stdin",
        "--not",
        "--exclude=refs/residue/*",
        "--all",
    ];
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let mut input = commits.join("\n");
    input.push('\n');
    let exit = exec_input(
        &|program| command(program, dir),
        &args,
        Some(input.as_bytes()),
        QUERY_TIMEOUT,
    )?;
    if !exit.success {
        return Err(format!("git rev-list --not --all failed: {}", exit.stderr));
    }
    let unreached: std::collections::HashSet<&[u8]> = lines(&exit.stdout).into_iter().collect();
    Ok(commits
        .iter()
        .filter(|c| unreached.contains(c.as_bytes()))
        .map(|c| (*c).to_string())
        .collect())
}

/// The fewest of `commits` that reach all of them: `git merge-base
/// --independent`.
pub fn independent(dir: &Path, commits: &[String]) -> Result<Vec<String>, String> {
    if commits.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["merge-base", "--independent"];
    args.extend(commits.iter().map(String::as_str));
    let out = git(dir, &args)?;
    Ok(lines(&out)
        .into_iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    //! How [`status`] joins a limited traditional run to the matching list,
    //! on synthetic lists only: which folders it limits the run to, the
    //! merged list's order, which entries count as traditional mode's, and
    //! the cap. What Git prints in each layout is checked against real Git
    //! by `scripts/contract-check.sh` (section 13).

    use super::{
        MAX_LIMITED, MAX_LIMITED_BYTES, Status, merge_limited, topmost_untracked, within_limit,
    };
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn set(items: &[&str]) -> HashSet<Vec<u8>> {
        items.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    fn entry(text: &str) -> (String, PathBuf) {
        (text.to_string(), PathBuf::from(text.trim_end_matches('/')))
    }

    fn texts(status: &Status) -> Vec<&str> {
        status.ignored.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn limits_the_run_to_the_topmost_untracked_folders() {
        // a/ and t/u/ hold index entries; a/b/, x/ and everything under
        // them hold none. a/b/c and a/b/d share a/b; t/u needs no run.
        let parents = set(&["a/b/c", "a/b/d", "a/b", "x/y/z", "t/u"]);
        let tracked = set(&["a", "t", "t/u"]);
        let folders = topmost_untracked(&parents, &tracked);
        assert_eq!(folders, vec![b"a/b".to_vec(), b"x".to_vec()]);
    }

    #[test]
    fn no_folder_when_every_parent_is_tracked() {
        let parents = set(&["src", "src/new"]);
        let tracked = set(&["src", "src/new"]);
        assert!(topmost_untracked(&parents, &tracked).is_empty());
    }

    #[test]
    fn merged_list_replaces_the_folder_and_keeps_git_order() {
        let mut status = Status {
            ignored: [
                "a-b.log",
                "a.log",
                "a/x/one.log",
                "a/y.log",
                "a0.log",
                "secrets/",
            ]
            .into_iter()
            .map(entry)
            .collect(),
            matching: true,
            ..Status::default()
        };
        // The limited run's list, with one entry outside its folder that
        // must not be taken twice.
        let listed = vec![entry("a/"), entry("a.log")];
        merge_limited(&mut status, vec![b"a".to_vec()], listed);
        // Git sorts by the path as printed: `-` < `.` < `/` < `0`. Sorting
        // by the path without its slash would put `a/` first.
        assert_eq!(
            texts(&status),
            ["a-b.log", "a.log", "a/", "a0.log", "secrets/"]
        );
        assert_eq!(status.limited, vec![PathBuf::from("a")]);
    }

    #[test]
    fn entries_in_a_limited_folder_are_traditional_modes() {
        let status = Status {
            matching: true,
            limited: vec![PathBuf::from("logs"), PathBuf::from("app/[slug]")],
            ..Status::default()
        };
        assert!(status.traditional_listed(Path::new("logs")));
        assert!(status.traditional_listed(Path::new("logs/old/a.log")));
        assert!(status.traditional_listed(Path::new("app/[slug]")));
        assert!(!status.traditional_listed(Path::new("logsx/a.log")));
        assert!(!status.traditional_listed(Path::new("app/s")));
        assert!(!status.traditional_listed(Path::new("secrets")));
        let traditional = Status::default();
        assert!(traditional.traditional_listed(Path::new("secrets")));
    }

    #[test]
    fn past_the_cap_the_whole_run_is_used() {
        let folders = |n: usize, len: usize| vec![vec![b'f'; len]; n];
        assert!(within_limit(&folders(MAX_LIMITED, 10)));
        assert!(!within_limit(&folders(MAX_LIMITED + 1, 10)));
        assert!(within_limit(&folders(1, MAX_LIMITED_BYTES)));
        assert!(!within_limit(&folders(2, MAX_LIMITED_BYTES / 2 + 1)));
    }
}
