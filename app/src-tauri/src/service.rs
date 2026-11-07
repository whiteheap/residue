//! Automatic cleanup: the login service that `residue service install` writes
//! (a launchd agent on macOS) and that runs `residue watch`.
//!
//! The service records the absolute path of the binary that installed it, so
//! the app never installs it from the copy inside its own bundle: moving,
//! updating or deleting the app would break the agent. It installs from, in
//! order:
//!
//! 1. a `residue` the user installed (on the login shell's PATH, or in the
//!    usual Homebrew, Cargo and `~/.local/bin` directories) that is at least as
//!    new as the bundled one, with its directory first on PATH, so the agent
//!    records the stable link (`/opt/homebrew/bin/residue`) and keeps working
//!    after `brew upgrade`;
//! 2. otherwise a copy of the bundled binary at
//!    `~/Library/Application Support/<app id>/bin/residue`, which stays when
//!    the app is moved, updated or deleted.
//!
//! What the service does comes from its arguments and the version of the
//! binary it runs, which `brew upgrade` can change without the app: the
//! window never assumes the bundled residue's behavior for it. Saving other
//! folders passes the service's options on as they are written, and refuses
//! when an argument is one the app does not know, rather than drop it.

use crate::cli::{CliError, ErrorKind, Run, shown};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// The launchd label residue's service uses.
pub const LABEL: &str = "com.whiteheap.residue.watch";

/// `residue watch` options that take a value. `--fetch-every` is new in
/// residue 0.0.7, which writes it (or `--no-fetch`) into every service.
const VALUE_OPTIONS: &[&str] = &["--grace", "--idle", "--min-free", "--fetch-every"];

/// `residue watch` options without a value that a save passes on.
/// `--dry-run` is the dialog's own checkbox, so it is not one of them.
const FLAG_OPTIONS: &[&str] = &["--no-fetch"];

/// residue's defaults for the options a service does not set, from
/// `residue watch --help` and `residue --help` of 0.0.6 and of 0.0.7.
/// Update them with `sidecar.lock` when a release changes one.
pub const DEFAULT_GRACE: &str = "1h";
pub const DEFAULT_IDLE: &str = "2d";
pub const DEFAULT_MIN_FREE: &str = "10";
/// `--fetch-every` of residue 0.0.7 and newer; 0.0.6 never fetches.
pub const DEFAULT_FETCH_EVERY: &str = "30m";
/// `residue remove --keep-trash`. Automatic cleanup and the app's own
/// removals never pass one, so it is the trash's retention for both.
pub const DEFAULT_KEEP_TRASH: &str = "7d";
/// The first release whose `watch` fetches (`--fetch-every`, `--no-fetch`).
const FETCH_SINCE: (u64, u64, u64) = (0, 0, 7);
const FETCH_SINCE_TEXT: &str = "0.0.7";

/// Everything a service operation needs to know about this machine.
pub struct Context<'a> {
    pub sidecar: &'a Path,
    pub bundled_version: &'a str,
    pub data_dir: &'a Path,
    pub home: &'a Path,
    pub path_env: &'a OsStr,
}

/// What the installed agent runs, read from its plist.
#[derive(Debug, Clone, Serialize)]
pub struct Program {
    pub binary: String,
    pub binary_exists: bool,
    /// `residue --version` of that binary, when it runs.
    pub version: Option<String>,
    /// The binary is the app's own copy.
    pub managed: bool,
    /// The absolute paths among the arguments: residue writes every ROOT
    /// canonical.
    pub roots: Vec<String>,
    /// The options a save passes on, as written: `["--grace", "1h",
    /// "--no-fetch"]`.
    pub options: Vec<String>,
    pub dry_run: bool,
    /// Arguments the app does not know. While there are any, the app does not
    /// save the service, since a save would drop them.
    pub unknown: Vec<String>,
    /// What the service does: its options, and residue's defaults for the
    /// ones it does not set.
    pub settings: WatchSettings,
}

/// One `residue watch` setting.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Setting {
    /// As residue takes it: `1h`, `2d`, `10`.
    pub value: String,
    /// The service does not set it: this is residue's default.
    pub default: bool,
}

/// Whether `residue watch` fetches, so it sees a branch deleted on the
/// remote without the user's own `git fetch --prune`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Fetch {
    /// About every `value` (residue 0.0.7 and newer).
    Every { value: String, default: bool },
    /// `--no-fetch`.
    Off,
    /// The binary is older than `since`: it never fetches. From `since` on,
    /// residue fetches about every `every` unless `--no-fetch`.
    Never { since: String, every: String },
    /// The binary's version is unknown, so whether it fetches is too.
    Unknown { since: String, every: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WatchSettings {
    pub grace: Setting,
    pub idle: Setting,
    pub min_free: Setting,
    pub fetch: Fetch,
    /// The trash retention of the removals watch runs: it passes no
    /// `--keep-trash`, so this is always residue's default.
    pub keep_trash: Setting,
}

/// What turning the service on, or saving other folders, installs.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    /// The binary that installs the service, and that the service then runs.
    pub binary: String,
    pub version: Option<String>,
    /// The app's own copy of the bundled binary.
    pub managed: bool,
    /// The options a save passes on, as the running service has them. Empty
    /// when the service is off: turning it on starts from residue's defaults.
    pub keep: Vec<String>,
    /// What the service does after the save.
    pub settings: WatchSettings,
    /// Why the app will not save, when it will not.
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub installed: bool,
    /// launchd's state, such as `running`.
    pub state: Option<String>,
    pub pid: Option<u32>,
    /// The first line of `residue service status`.
    pub summary: String,
    /// The agent's plist, when the file exists.
    pub agent_file: Option<String>,
    pub log_path: Option<String>,
    pub log_lines: Vec<String>,
    /// The log exists but could not be read.
    pub log_error: Option<String>,
    pub program: Option<Program>,
    pub plan: Plan,
    /// The development preview mode is on: nothing was installed.
    pub dev_dry_run: bool,
}

/// The result of turning the service on or off.
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub command: String,
    pub output: Vec<String>,
    pub dev_dry_run: bool,
    pub status: Status,
}

/// What `residue watch --once --dry-run` decided, which removes nothing.
#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub command: String,
    pub lines: Vec<String>,
    /// Options of the service that the preview did not pass, with the reason.
    pub left_out: Vec<String>,
    pub left_out_reason: Option<String>,
}

/// The preview mode for development builds. With
/// `RESIDUE_DESKTOP_DEV_SERVICE=preview`, a debug build never runs
/// `residue service install` or `uninstall`: launchd acts on the user's real
/// session and the label is shared with any residue service already running.
/// The switch then shows the command it would run and the output of
/// `residue watch --once --dry-run`, which removes nothing, and remembers the
/// pretend state until the app quits. With `RESIDUE_DESKTOP_DEV_AGENT_FILE`
/// set to a plist as well, the preview starts as if that file were the
/// installed service; the app only reads it. Release builds ignore both.
pub struct DevPreview {
    pub enabled: bool,
    state: Mutex<Option<(Program, Vec<String>)>>,
    seed: Mutex<Option<PathBuf>>,
}

/// A poisoned lock still holds a whole value here: every writer replaces it
/// in one assignment.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl DevPreview {
    pub fn from_env() -> Self {
        let enabled = cfg!(debug_assertions)
            && std::env::var("RESIDUE_DESKTOP_DEV_SERVICE").as_deref() == Ok("preview");
        let seed = std::env::var_os("RESIDUE_DESKTOP_DEV_AGENT_FILE")
            .filter(|p| enabled && !p.is_empty())
            .map(PathBuf::from);
        DevPreview {
            enabled,
            state: Mutex::new(None),
            seed: Mutex::new(seed),
        }
    }

    fn get(&self) -> Option<(Program, Vec<String>)> {
        lock(&self.state).clone()
    }

    fn set(&self, value: Option<(Program, Vec<String>)>) {
        *lock(&self.state) = value;
    }

    /// Start from the seed agent file, the first time the state is read. The
    /// seed stays locked until the state is set, so a read that runs at the
    /// same time waits and sees it; a file that cannot be read stays the seed
    /// and fails every read with its reason.
    fn seed(&self, ctx: &Context) -> Result<(), CliError> {
        let mut seed = lock(&self.seed);
        let Some(path) = seed.as_ref() else {
            return Ok(());
        };
        let program = read_program(ctx, path)?;
        self.set(Some((
            program,
            vec![format!(
                "development preview: started from {}, which the app only reads",
                path.display()
            )],
        )));
        *seed = None;
        Ok(())
    }
}

fn agent_file(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn default_log(home: &Path) -> PathBuf {
    home.join("Library/Logs/residue/watch.log")
}

fn managed_binary(data_dir: &Path) -> PathBuf {
    data_dir.join("bin/residue")
}

pub fn status(ctx: &Context, dev: &DevPreview) -> Result<Status, CliError> {
    read(ctx, dev).map(|(status, _)| status)
}

/// The service's status, and the binary a save would install with.
fn read(ctx: &Context, dev: &DevPreview) -> Result<(Status, Installer), CliError> {
    if dev.enabled {
        dev.seed(ctx)?;
        let current = dev.get();
        let installed = current.is_some();
        let program = current.as_ref().map(|(p, _)| p);
        let (plan, installer) = plan(ctx, installed, program)?;
        let status = match current {
            Some((program, log_lines)) => Status {
                installed: true,
                state: Some("running".into()),
                pid: None,
                summary: format!(
                    "launchd agent {LABEL}: running (development preview, not installed)"
                ),
                agent_file: None,
                log_path: None,
                log_lines,
                log_error: None,
                program: Some(program),
                plan,
                dev_dry_run: true,
            },
            None => Status {
                installed: false,
                state: None,
                pid: None,
                summary: format!("launchd agent {LABEL}: not installed (development preview)"),
                agent_file: None,
                log_path: None,
                log_lines: Vec::new(),
                log_error: None,
                program: None,
                plan,
                dev_dry_run: true,
            },
        };
        return Ok((status, installer));
    }

    // `residue service status` only reads: launchctl print and the log.
    let out = Run {
        bin: ctx.sidecar,
        args: vec!["service".into(), "status".into()],
        cwd: ctx.home,
        path_env: ctx.path_env,
        deadline: Duration::from_secs(20),
    }
    .output()?;
    let installed = match out.code {
        Some(0) => true,
        Some(3) => false,
        _ => {
            return Err(out.error(
                ErrorKind::Failed,
                "residue service status did not report a state".into(),
            ));
        }
    };
    let mut lines = out.stdout.lines();
    let summary = lines.next().unwrap_or_default().trim().to_string();
    let (state, pid) = parse_state(&summary);
    let log_path = out
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("log: "))
        .map(PathBuf::from)
        .unwrap_or_else(|| default_log(ctx.home));

    let plist = agent_file(ctx.home);
    let program = if plist.exists() {
        Some(read_program(ctx, &plist)?)
    } else {
        None
    };
    let (plan, installer) = plan(ctx, installed, program.as_ref())?;
    let (log_lines, log_error) = match tail(&log_path, 40) {
        Ok(lines) => (lines, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let status = Status {
        installed,
        state: if installed { state } else { None },
        pid: if installed { pid } else { None },
        summary,
        agent_file: plist.exists().then(|| plist.to_string_lossy().into_owned()),
        log_lines,
        log_error,
        log_path: Some(log_path.to_string_lossy().into_owned()),
        program,
        plan,
        dev_dry_run: false,
    };
    Ok((status, installer))
}

/// `launchd agent LABEL: running (pid 84497)` -> (`running`, 84497).
fn parse_state(line: &str) -> (Option<String>, Option<u32>) {
    let Some((_, rest)) = line.split_once(": ") else {
        return (None, None);
    };
    match rest.split_once(" (pid ") {
        Some((state, pid)) => (
            Some(state.trim().to_string()),
            pid.trim_end_matches(')').trim().parse().ok(),
        ),
        None => (Some(rest.trim().to_string()), None),
    }
}

/// The last `n` lines of the log, read from at most its last 64 KB. A log
/// that does not exist yet has no lines; any other failure is reported.
fn tail(path: &Path, n: usize) -> Result<Vec<String>, String> {
    use std::io::Seek;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let read = (|| -> std::io::Result<(u64, Vec<u8>)> {
        let len = file.metadata()?.len();
        let start = len.saturating_sub(64 * 1024);
        file.seek(std::io::SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok((start, bytes))
    })();
    let (start, bytes) = read.map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // The first line is probably cut in the middle.
        lines.remove(0);
    }
    Ok(lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect())
}

/// What the agent runs, from its plist's `ProgramArguments`.
fn read_program(ctx: &Context, plist: &Path) -> Result<Program, CliError> {
    let out = Run {
        bin: Path::new("/usr/bin/plutil"),
        args: vec![
            "-convert".into(),
            "json".into(),
            "-o".into(),
            "-".into(),
            plist.as_os_str().to_owned(),
        ],
        cwd: ctx.home,
        path_env: ctx.path_env,
        deadline: Duration::from_secs(10),
    }
    .output()?;
    if out.code != Some(0) {
        return Err(out.error(
            ErrorKind::Failed,
            format!("cannot read {}", plist.display()),
        ));
    }
    let doc: Value = serde_json::from_str(&out.stdout).map_err(|e| {
        out.error(
            ErrorKind::BadOutput,
            format!(
                "{} is not a property list the app can read ({e})",
                plist.display()
            ),
        )
    })?;
    let args: Vec<String> = doc
        .get("ProgramArguments")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let Some((binary, rest)) = args.split_first() else {
        return Err(CliError::failed(format!(
            "{} has no ProgramArguments",
            plist.display()
        )));
    };
    let binary_path = PathBuf::from(binary);
    let binary_exists = binary_path.is_file();
    let version = if binary_exists {
        version_of(&binary_path, ctx)
    } else {
        None
    };
    let parsed = match rest.split_first() {
        Some((command, args)) if command == "watch" => parse_watch(args),
        _ => Parsed {
            unknown: rest.to_vec(),
            ..Parsed::default()
        },
    };
    let settings = watch_settings(&parsed.options, version.as_deref());
    Ok(Program {
        binary: binary.clone(),
        binary_exists,
        version,
        managed: binary_path == managed_binary(ctx.data_dir),
        roots: parsed.roots,
        options: parsed.options,
        dry_run: parsed.dry_run,
        unknown: parsed.unknown,
        settings,
    })
}

#[derive(Default)]
struct Parsed {
    roots: Vec<String>,
    options: Vec<String>,
    dry_run: bool,
    unknown: Vec<String>,
}

/// Sort the arguments after `watch` the way residue reads them. An
/// argument that is neither an option the app knows nor an absolute path is
/// unknown, never a folder: residue writes every ROOT canonical.
fn parse_watch(args: &[String]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if VALUE_OPTIONS.contains(&arg.as_str()) {
            match rest.next() {
                Some(value) => parsed.options.extend([arg.clone(), value.clone()]),
                None => parsed.unknown.push(arg.clone()),
            }
        } else if FLAG_OPTIONS.contains(&arg.as_str()) {
            parsed.options.push(arg.clone());
        } else if arg == "--dry-run" {
            parsed.dry_run = true;
        } else if !arg.starts_with('-') && Path::new(arg).is_absolute() {
            parsed.roots.push(arg.clone());
        } else {
            parsed.unknown.push(arg.clone());
        }
    }
    parsed
}

/// What `residue watch` does with these options when a binary of this
/// `residue --version` runs it: the options it sets, and residue's defaults
/// for the rest.
fn watch_settings(options: &[String], version: Option<&str>) -> WatchSettings {
    let mut set: HashMap<&str, &str> = HashMap::new();
    let mut no_fetch = false;
    let mut it = options.iter();
    while let Some(option) = it.next() {
        if VALUE_OPTIONS.contains(&option.as_str()) {
            // A later one replaces an earlier one, as in residue.
            if let Some(value) = it.next() {
                set.insert(option, value);
            }
        } else if option == "--no-fetch" {
            no_fetch = true;
        }
    }
    let setting = |flag: &str, default: &str| match set.get(flag) {
        Some(value) => Setting {
            value: (*value).to_string(),
            default: false,
        },
        None => Setting {
            value: default.to_string(),
            default: true,
        },
    };
    let fetch = if no_fetch {
        Fetch::Off
    } else if let Some(value) = set.get("--fetch-every") {
        Fetch::Every {
            value: (*value).to_string(),
            default: false,
        }
    } else {
        match fetches(version) {
            Some(true) => Fetch::Every {
                value: DEFAULT_FETCH_EVERY.into(),
                default: true,
            },
            Some(false) => Fetch::Never {
                since: FETCH_SINCE_TEXT.into(),
                every: DEFAULT_FETCH_EVERY.into(),
            },
            None => Fetch::Unknown {
                since: FETCH_SINCE_TEXT.into(),
                every: DEFAULT_FETCH_EVERY.into(),
            },
        }
    };
    WatchSettings {
        grace: setting("--grace", DEFAULT_GRACE),
        idle: setting("--idle", DEFAULT_IDLE),
        min_free: setting("--min-free", DEFAULT_MIN_FREE),
        fetch,
        keep_trash: Setting {
            value: DEFAULT_KEEP_TRASH.into(),
            default: true,
        },
    }
}

/// The fetch options (`--fetch-every X`, `--no-fetch`), and the others.
fn split_fetch(options: &[String]) -> (Vec<String>, Vec<String>) {
    let mut fetch = Vec::new();
    let mut other = Vec::new();
    let mut it = options.iter();
    while let Some(option) = it.next() {
        let into = if option == "--fetch-every" || FLAG_OPTIONS.contains(&option.as_str()) {
            &mut fetch
        } else {
            &mut other
        };
        into.push(option.clone());
        if VALUE_OPTIONS.contains(&option.as_str())
            && let Some(value) = it.next()
        {
            into.push(value.clone());
        }
    }
    (fetch, other)
}

/// `residue --version` of a binary: `residue 0.0.6`.
fn version_of(bin: &Path, ctx: &Context) -> Option<String> {
    let out = Run {
        bin,
        args: vec!["--version".into()],
        cwd: ctx.home,
        path_env: ctx.path_env,
        deadline: Duration::from_secs(5),
    }
    .output()
    .ok()?;
    let text = out.stdout.trim();
    (out.code == Some(0) && text.starts_with("residue ")).then(|| text.to_string())
}

/// `residue 0.0.6` -> (0, 0, 6).
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let v = text.strip_prefix("residue ")?.trim();
    let mut parts = v.split(['.', '-', '+']).map(|p| p.parse::<u64>());
    Some((
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ))
}

/// Whether `residue watch` of this `residue --version` fetches; `None` when
/// the version is unknown.
fn fetches(version: Option<&str>) -> Option<bool> {
    version.and_then(parse_version).map(|v| v >= FETCH_SINCE)
}

/// The binary a save installs with, and the PATH to install it with.
struct Installer {
    bin: PathBuf,
    managed: bool,
    path_env: OsString,
    version: Option<String>,
}

/// Find the binary to install with. This only reads: the app's own copy is
/// written when a save uses it.
fn find_installer(ctx: &Context) -> Result<Installer, CliError> {
    let bundled = parse_version(&format!("residue {}", ctx.bundled_version));
    let managed = managed_binary(ctx.data_dir);
    let skip: Vec<PathBuf> = [ctx.sidecar, managed.as_path()]
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(ctx.path_env).collect();
    for extra in [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        ctx.home.join(".cargo/bin"),
        ctx.home.join(".local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    for dir in dirs {
        let candidate = dir.join("residue");
        if !candidate.is_file() || !candidate.is_absolute() {
            continue;
        }
        let Ok(real) = candidate.canonicalize() else {
            continue;
        };
        // Never a binary inside an app bundle: it moves with the app.
        if skip.contains(&real) || real.to_string_lossy().contains(".app/Contents/") {
            continue;
        }
        let version = version_of(&candidate, ctx);
        if let (Some(found), Some(bundled)) = (version.as_deref().and_then(parse_version), bundled)
            && found >= bundled
        {
            let mut path: Vec<PathBuf> = vec![dir.clone()];
            path.extend(std::env::split_paths(ctx.path_env).filter(|d| d != &dir));
            let path_env = std::env::join_paths(path)
                .map_err(|e| CliError::failed(format!("cannot build PATH: {e}")))?;
            return Ok(Installer {
                bin: candidate,
                managed: false,
                path_env,
                version,
            });
        }
    }
    Ok(Installer {
        bin: managed,
        managed: true,
        path_env: ctx.path_env.to_owned(),
        // The copy is written from the bundled binary when a save uses it.
        version: Some(format!("residue {}", ctx.bundled_version)),
    })
}

/// What a save installs: the binary, the options it passes on, and why it
/// will not save, when it will not. Only a service launchd runs has options
/// to keep: an agent file it has not loaded is left over, and turning the
/// service on starts from residue's defaults.
fn plan(
    ctx: &Context,
    installed: bool,
    program: Option<&Program>,
) -> Result<(Plan, Installer), CliError> {
    let installer = find_installer(ctx)?;
    let mut blocked = None;
    let keep = match (installed, program) {
        (false, _) => Vec::new(),
        (true, Some(program)) => {
            if !program.unknown.is_empty() {
                blocked = Some(format!(
                    "The service runs residue with arguments the app does not know: {}. A save \
                     here would drop them. Change the service in a terminal with residue \
                     service install, or turn automatic cleanup off and on again to start from \
                     residue's defaults.",
                    program.unknown.join(" ")
                ));
            }
            program.options.clone()
        }
        (true, None) => {
            blocked = Some(format!(
                "The service is running, but its agent file {} is missing, so the app cannot \
                 read its settings, and a save would replace them with residue's defaults. Turn \
                 automatic cleanup off and on again to start from those defaults.",
                agent_file(ctx.home).display()
            ));
            Vec::new()
        }
    };
    let (fetch, _) = split_fetch(&keep);
    if blocked.is_none() && !fetch.is_empty() && fetches(installer.version.as_deref()) != Some(true)
    {
        blocked = Some(format!(
            "The service sets {}, which needs residue {FETCH_SINCE_TEXT} or newer, and the app \
             would install it with {} ({}). Install residue {FETCH_SINCE_TEXT} or newer, or turn \
             automatic cleanup off and on again to start from residue's defaults.",
            fetch.join(" "),
            installer.bin.display(),
            installer.version.as_deref().unwrap_or("version unknown"),
        ));
    }
    let settings = watch_settings(&keep, installer.version.as_deref());
    let plan = Plan {
        binary: installer.bin.to_string_lossy().into_owned(),
        version: installer.version.clone(),
        managed: installer.managed,
        keep,
        settings,
        blocked,
    };
    Ok((plan, installer))
}

/// Copy the bundled binary to `dest`: written next to it, then renamed over
/// it, so a running service keeps the file it started from.
fn copy_sidecar(sidecar: &Path, dest: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = dest
        .parent()
        .ok_or_else(|| CliError::failed(format!("{} has no parent", dest.display())))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| CliError::failed(format!("cannot create {}: {e}", dir.display())))?;
    let tmp = dir.join(format!(".residue.{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut from = std::fs::File::open(sidecar)?;
        let mut to = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(&tmp)?;
        std::io::copy(&mut from, &mut to)?;
        to.sync_all()?;
        std::fs::rename(&tmp, dest)
    })();
    result.map_err(|e| {
        // The temporary file is ours; a failed copy leaves nothing behind.
        if let Err(cleanup) = std::fs::remove_file(&tmp)
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            return CliError::failed(format!(
                "cannot copy residue to {}: {e} (and cannot remove {}: {cleanup})",
                dest.display(),
                tmp.display()
            ));
        }
        CliError::failed(format!("cannot copy residue to {}: {e}", dest.display()))
    })
}

/// The `residue watch` arguments: kept options, `--dry-run`, then the roots.
fn watch_args(roots: &[PathBuf], options: &[String], dry_run: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = options.iter().map(OsString::from).collect();
    if dry_run {
        args.push("--dry-run".into());
    }
    args.extend(roots.iter().map(|r| r.as_os_str().to_owned()));
    args
}

/// Install (or replace) the service for `roots`, passing on the options of
/// the running service as they are written. `expect_keep` is the options the
/// window showed: when the service changed since, nothing is installed.
pub fn install(
    ctx: &Context,
    dev: &DevPreview,
    roots: &[PathBuf],
    dry_run: bool,
    expect_keep: &[String],
) -> Result<Change, CliError> {
    if roots.is_empty() {
        return Err(CliError::invalid("choose at least one folder to watch"));
    }
    let (current, installer) = read(ctx, dev)?;
    let plan = current.plan;
    if let Some(reason) = plan.blocked {
        return Err(CliError::invalid(reason));
    }
    if plan.keep != expect_keep {
        return Err(CliError::invalid(format!(
            "The service changed after this window read it: a save now keeps {}. Check the \
             settings and save again.",
            if plan.keep.is_empty() {
                "no options, so residue's defaults apply".to_string()
            } else {
                plan.keep.join(" ")
            }
        )));
    }
    let options = plan.keep;
    if installer.managed {
        copy_sidecar(ctx.sidecar, &installer.bin)?;
    }
    let mut args: Vec<OsString> = vec!["service".into(), "install".into()];
    args.extend(watch_args(roots, &options, dry_run));
    let command = shown(&installer.bin, &args);

    if dev.enabled {
        let preview = preview(ctx, roots, &options)?;
        let settings = watch_settings(&options, installer.version.as_deref());
        let program = Program {
            binary: installer.bin.to_string_lossy().into_owned(),
            binary_exists: true,
            version: installer.version,
            managed: installer.managed,
            roots: roots
                .iter()
                .map(|r| r.to_string_lossy().into_owned())
                .collect(),
            options,
            dry_run,
            unknown: Vec::new(),
            settings,
        };
        dev.set(Some((program, preview.lines.clone())));
        return Ok(Change {
            command,
            output: preview.lines,
            dev_dry_run: true,
            status: status(ctx, dev)?,
        });
    }

    let out = Run {
        bin: &installer.bin,
        args,
        cwd: ctx.home,
        path_env: &installer.path_env,
        deadline: Duration::from_secs(60),
    }
    .output()?;
    if out.code != Some(0) {
        return Err(out.error(
            ErrorKind::Failed,
            "residue could not install the service".into(),
        ));
    }
    Ok(Change {
        command: out.command.clone(),
        output: out.stdout.lines().map(str::to_string).collect(),
        dev_dry_run: false,
        status: status(ctx, dev)?,
    })
}

pub fn uninstall(ctx: &Context, dev: &DevPreview) -> Result<Change, CliError> {
    let args: Vec<OsString> = vec!["service".into(), "uninstall".into()];
    if dev.enabled {
        dev.set(None);
        return Ok(Change {
            command: shown(ctx.sidecar, &args),
            output: vec!["development preview: nothing was uninstalled".into()],
            dev_dry_run: true,
            status: status(ctx, dev)?,
        });
    }
    let out = Run {
        bin: ctx.sidecar,
        args,
        cwd: ctx.home,
        path_env: ctx.path_env,
        deadline: Duration::from_secs(60),
    }
    .output()?;
    if out.code != Some(0) {
        return Err(out.error(
            ErrorKind::Failed,
            "residue could not uninstall the service".into(),
        ));
    }
    Ok(Change {
        command: out.command.clone(),
        output: out.stdout.lines().map(str::to_string).collect(),
        dev_dry_run: false,
        status: status(ctx, dev)?,
    })
}

/// What automatic cleanup would remove right now, as the bundled residue
/// sees it: `residue watch --once --dry-run`, which removes nothing. The
/// preview never fetches: the fetch options of the service are left out and
/// named, and a bundled residue that fetches gets `--no-fetch`.
pub fn preview(ctx: &Context, roots: &[PathBuf], options: &[String]) -> Result<Preview, CliError> {
    if roots.is_empty() {
        return Err(CliError::invalid("choose at least one folder to watch"));
    }
    let bundled = format!("residue {}", ctx.bundled_version);
    let (fetch, mut passed) = split_fetch(options);
    let (left_out, reason) = if fetches(Some(&bundled)) == Some(true) {
        passed.push("--no-fetch".into());
        let left: Vec<String> = fetch.into_iter().filter(|o| o != "--no-fetch").collect();
        (left, "the preview does not fetch".to_string())
    } else {
        (
            fetch,
            format!("{bundled} runs the preview and has no fetch options"),
        )
    };
    let mut args: Vec<OsString> = vec!["watch".into(), "--once".into()];
    args.extend(watch_args(roots, &passed, true));
    let out = Run {
        bin: ctx.sidecar,
        args,
        cwd: ctx.home,
        path_env: ctx.path_env,
        deadline: Duration::from_secs(15 * 60),
    }
    .output()?;
    if out.code != Some(0) {
        return Err(out.error(
            ErrorKind::Failed,
            "residue watch --once --dry-run failed".into(),
        ));
    }
    Ok(Preview {
        command: out.command.clone(),
        // watch logs to stderr, one line per decision.
        lines: out
            .stderr
            .lines()
            .chain(out.stdout.lines())
            .map(str::to_string)
            .collect(),
        left_out_reason: (!left_out.is_empty()).then_some(reason),
        left_out,
    })
}
