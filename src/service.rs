//! `residue service`: run `residue watch` in the background from login, as
//! a launchd agent on macOS or a systemd user service on Linux.

use crate::output;
use crate::watch::{self, Options};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const HELP: &str = "\
residue service: run `residue watch` in the background, from login.

Usage:
  residue service install [OPTIONS] [ROOT...]  Install or replace it; start it
  residue service status                       Show what it runs, and its log
  residue service uninstall                    Stop it and remove it

Examples:
  residue service install --dry-run   log what it would remove; remove nothing
  residue service install ~/src       watch ~/src

Runs as:   a launchd agent (macOS) or a systemd user service (Linux)
Folders:   without a ROOT, the usual code folders that exist now; install
           again to add a new one
Options:   residue watch --help, and --expect (residue service install --help)
";

/// The lines `residue service install --help` adds above `watch`'s help.
const INSTALL_HELP: &str = "\
residue service install: install or replace the service, then start it.
It takes the options of residue watch, below, and one of its own:
  --expect FINGERPRINT     Replace the service only while `residue service
                           status` prints this fingerprint, or with `none`
                           install only while none is; else exit 4, unchanged
";

/// The exit status of `service install --expect` when the service is not
/// the one it names: nothing was written.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const CHANGED: u8 = crate::fingerprint::CHANGED;

/// The launchd label on macOS.
#[cfg(target_os = "macos")]
pub const LABEL: &str = "com.whiteheap.residue.watch";
/// The systemd user unit on Linux.
#[cfg(target_os = "linux")]
pub const UNIT: &str = "residue-watch.service";

/// The names Homebrew gives a `residue` service started with `brew
/// services`, newest first: `sh.brew.NAME`, then the older
/// `homebrew.mxcl.NAME` (launchd) and `homebrew.NAME` (systemd).
#[cfg(target_os = "macos")]
const HOMEBREW: &[&str] = &["sh.brew.residue.plist", "homebrew.mxcl.residue.plist"];
#[cfg(target_os = "linux")]
const HOMEBREW: &[&str] = &["sh.brew.residue.service", "homebrew.residue.service"];

/// What `residue service ARGS` does, decided from the arguments alone,
/// before anything runs.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// `install` with the rest of the arguments, which `watch` parses.
    Install,
    Uninstall,
    Status,
    /// Print `HELP`, exit 0.
    Help,
    /// No command: print `HELP` on stderr, exit 2.
    Missing,
    /// A usage error, exit 2: this line, then the pointer to `--help`.
    Usage(String),
}

fn action(args: &[OsString]) -> Action {
    let Some(first) = args.first() else {
        return Action::Missing;
    };
    let command = match first.to_str() {
        Some("install") => return Action::Install,
        Some("-h" | "--help") => return Action::Help,
        Some(command @ ("uninstall" | "status")) => command,
        _ => {
            return Action::Usage(format!(
                "residue service: unknown command {}",
                output::clean(&first.to_string_lossy())
            ));
        }
    };
    // `uninstall` and `status` take no arguments. One a person added, even
    // `--help`, must never run them: `uninstall --help` would remove the
    // service.
    let rest = &args[1..];
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        return Action::Help;
    }
    if let Some(extra) = rest.first() {
        return Action::Usage(format!(
            "residue service {command}: unexpected argument {}",
            output::clean(&extra.to_string_lossy())
        ));
    }
    if command == "uninstall" {
        Action::Uninstall
    } else {
        Action::Status
    }
}

pub fn main(args: &[OsString]) -> u8 {
    match action(args) {
        Action::Install => install(&args[1..]),
        Action::Uninstall => platform::uninstall(),
        Action::Status => platform::status(),
        Action::Help => {
            print!("{HELP}");
            0
        }
        Action::Missing => {
            eprint!("{HELP}");
            2
        }
        Action::Usage(message) => {
            eprintln!("{message}");
            eprintln!("Run `residue service --help` for usage.");
            2
        }
    }
}

fn install(args: &[OsString]) -> u8 {
    let parsed = expect_in(args).and_then(|(expect, rest)| Ok((expect, Options::parse(&rest)?)));
    let (expect, mut opts) = match parsed {
        Ok((expect, Some(opts))) => (expect, opts),
        Ok((_, None)) => {
            println!("{INSTALL_HELP}");
            print!("{}", watch::HELP);
            return 0;
        }
        Err(message) => {
            eprintln!("residue service install: {}", output::clean(&message));
            eprintln!("Run `residue service --help` for usage.");
            return 2;
        }
    };
    if opts.once {
        eprintln!("residue service install: --once makes no sense for a service");
        return 2;
    }
    // A service whose every pass would fail is no service: refuse it now,
    // as every other command refuses to run.
    if let Err(message) = crate::git::require_version() {
        eprintln!("residue: {}", output::clean(&message));
        return 1;
    }
    if let Some(file) = homebrew_service() {
        eprintln!(
            "residue service install: Homebrew already runs residue watch ({}), and only one can run. Keep it, or stop it first: brew services stop residue",
            output::clean(&file.to_string_lossy())
        );
        return 1;
    }
    // Compared now to stop before any work; the comparison that decides is
    // the one under the lock, right before the write.
    let replaced = match &expect {
        None => platform::installed(),
        Some(expect) => match check(expect) {
            Ok(installed) => installed,
            Err(code) => return code,
        },
    };
    let mut kept = false;
    if opts.default_roots {
        if let Some(roots) = replaced.as_ref().and_then(named_roots) {
            opts.roots = roots;
            opts.default_roots = false;
            kept = true;
        } else if let Err(message) = opts.use_default_roots("residue service install") {
            eprintln!("residue service install: {}", output::clean(&message));
            return 2;
        }
    }
    let exe = match program() {
        Ok(exe) => exe,
        Err(message) => {
            eprintln!("residue service install: {message}");
            return 1;
        }
    };
    let watches = format!(
        "{}{}",
        output::clean(&opts.shown_roots()),
        if kept {
            " (kept from the service it replaces; name ROOTs to change them)"
        } else if opts.default_roots {
            " (the code folders in your home folder; name ROOTs to choose others)"
        } else {
            ""
        }
    );
    // Right before the write: one install or uninstall at a time from here
    // until this one returns, and --expect compared under that lock, so
    // nothing another residue writes lands between the comparison and the
    // write.
    let before_write = || {
        let held = lock("residue service install", expect.is_some()).map_err(|message| {
            eprintln!("residue service install: {}", output::clean(&message));
            1
        })?;
        if let Some(expect) = &expect {
            check(expect)?;
        }
        Ok(held)
    };
    let code = platform::install(&exe, &opts, &watches, before_write);
    // Options are not kept, only ROOTs: say so for the one that matters.
    let was_dry_run = replaced
        .as_ref()
        .is_some_and(|r| r.program.iter().any(|a| a == "--dry-run"));
    if code == 0 && was_dry_run && !opts.dry_run {
        println!(
            "The service it replaced only logged what it would remove (--dry-run). This one removes."
        );
    }
    if let Some(old) = replaced.as_ref().and_then(keep_removed) {
        if code == 0 && old.seconds != opts.keep_removed.seconds {
            let old = output::clean(&old.text);
            println!(
                "removal records: new ones kept {} (was {old}); written ones keep their own",
                output::clean(&opts.keep_removed.text)
            );
            println!("to keep {old}: residue service install --keep-removed {old}");
        }
    }
    code
}

/// What `service install --expect` installs over.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    /// `none`: no service is installed.
    None,
    /// The `fingerprint:` line of `residue service status`.
    Fingerprint(String),
}

impl Expect {
    fn parse(text: &str) -> Result<Expect, String> {
        if text == "none" {
            return Ok(Expect::None);
        }
        if crate::fingerprint::is_fingerprint(text) {
            return Ok(Expect::Fingerprint(text.to_string()));
        }
        Err(format!(
            "--expect {text}: use `none`, or the fingerprint `residue service status` prints"
        ))
    }
}

/// `--expect VALUE` taken out of `service install`'s arguments; the rest
/// are `watch`'s.
fn expect_in(args: &[OsString]) -> Result<(Option<Expect>, Vec<OsString>), String> {
    let mut expect = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg != "--expect" {
            rest.push(arg.clone());
            continue;
        }
        let value = it.next().ok_or_else(|| {
            "--expect needs a value: `none`, or the fingerprint `residue service status` prints"
                .to_string()
        })?;
        if expect.is_some() {
            return Err("--expect can be given once".into());
        }
        expect = Some(Expect::parse(&value.to_string_lossy())?);
    }
    Ok((expect, rest))
}

/// The service as `--expect` compares it, read from the service manager and
/// the file `install` writes.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    /// launchd has the agent loaded, which is what `service status` calls
    /// installed on macOS. `None` on Linux, where the unit file is.
    loaded: Option<bool>,
    /// The service file's bytes; `None` when there is no file.
    file: Option<Vec<u8>>,
    /// The binary the file starts, resolved on disk: `brew upgrade` changes
    /// it under the same link. `None` when the file names none, or it does
    /// not resolve.
    binary: Option<PathBuf>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Seen {
    /// Read the file and the binary it names; `loaded` comes from the
    /// service manager.
    fn read(loaded: Option<bool>) -> Result<Seen, String> {
        let path = platform::file().ok_or("HOME is not set")?;
        let file = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut seen = Seen {
            loaded,
            file,
            binary: None,
        };
        seen.binary = seen
            .installed()
            .and_then(|i| i.program.into_iter().next())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .and_then(|p| p.canonicalize().ok());
        Ok(seen)
    }

    /// No service: no file, and nothing loaded.
    fn absent(&self) -> bool {
        self.file.is_none() && self.loaded != Some(true)
    }

    /// What the file runs.
    fn installed(&self) -> Option<Installed> {
        platform::parse(std::str::from_utf8(self.file.as_deref()?).ok()?)
    }
}

/// The fingerprint `service status` prints and `--expect` takes: 32 hex
/// digits that change with anything `Seen` holds and with the residue
/// release that computes it, which is the residue an install by this binary
/// runs as. So it names the service and the residue that would replace it,
/// and only the release that printed it can match it.
///
/// `fingerprint::Hasher`: FNV-1a, 128 bits, over each part with its length,
/// a check against change by accident, not a secret. Whoever can write the
/// file can change the service anyway.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn fingerprint(seen: &Seen, release: &str) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut hash = crate::fingerprint::Hasher::start(b"residue service fingerprint");
    let mut part = |bytes: &[u8]| hash.part(bytes);
    part(release.as_bytes());
    part(match seen.loaded {
        Some(true) => b"loaded",
        Some(false) => b"not loaded",
        None => b"",
    });
    match &seen.file {
        Some(bytes) => {
            part(b"file");
            part(bytes);
        }
        None => part(b"no file"),
    }
    match &seen.binary {
        Some(path) => {
            part(b"binary");
            part(path.as_os_str().as_bytes());
        }
        None => part(b"no binary"),
    }
    hash.hex()
}

/// Whether `seen` is the service `expect` names, to an install by `release`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_expected(expect: &Expect, seen: &Seen, release: &str) -> bool {
    match expect {
        Expect::None => seen.absent(),
        Expect::Fingerprint(want) => *want == fingerprint(seen, release),
    }
}

/// Read the service and compare it with `expect`: what it runs, or on a
/// mismatch the exit status, with its one line printed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn check(expect: &Expect) -> Result<Option<Installed>, u8> {
    let seen = Seen::read(platform::loaded()).map_err(|message| {
        eprintln!(
            "residue service install: cannot read the service to compare with --expect: {}",
            output::clean(&message)
        );
        1
    })?;
    if is_expected(expect, &seen, crate::commands::VERSION) {
        return Ok(seen.installed());
    }
    match expect {
        Expect::None => eprintln!(
            "residue service install: a service or its file exists, and --expect none installs only when neither does; nothing changed. See it: residue service status"
        ),
        Expect::Fingerprint(_) => eprintln!(
            "residue service install: the service changed since that fingerprint was read, or another residue release printed it; nothing changed. Read it again: residue service status"
        ),
    }
    Err(CHANGED)
}

/// Here residue writes no service, so it has nothing to compare: `install`
/// only prints the command to run.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn check(_expect: &Expect) -> Result<Option<Installed>, u8> {
    Ok(None)
}

/// The `fingerprint:` line of `service status`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn print_fingerprint(seen: &Result<Seen, String>) {
    match seen {
        Ok(seen) => println!(
            "fingerprint: {}",
            fingerprint(seen, crate::commands::VERSION)
        ),
        Err(message) => println!("fingerprint: unknown ({})", output::clean(message)),
    }
}

/// How long `lock` waits for another install or uninstall: longer than one
/// takes (launchd's bootstrap is tried for up to 5 seconds, and systemd's
/// restart waits for the old `watch` to stop).
#[cfg(any(target_os = "macos", target_os = "linux"))]
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// One `service install` or `uninstall` at a time: an exclusive flock(2) on
/// the folder that holds the service file, held while the returned file is
/// open, so two of them (a terminal and the Mac app, or an app that quit
/// and opened again) run one after the other. The folder, not a file of
/// residue's own, so every residue that writes that service file takes the
/// same lock, whatever `XDG_STATE_HOME` it runs with.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn lock(command: &str, required: bool) -> Result<Option<std::fs::File>, String> {
    let dir = platform::file()
        .and_then(|f| f.parent().map(Path::to_path_buf))
        .ok_or("HOME is not set")?;
    lock_folder(&dir, command, required, LOCK_WAIT)
}

/// `lock` on `dir`. It waits up to `wait` for another one to end, then
/// fails. `None` when the folder is missing (there is no service to change
/// then) or cannot be locked, such as on a filesystem with no flock(2):
/// then the command runs without it, as before, with a warning, unless
/// `required`, when either is an error.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn lock_folder(
    dir: &Path,
    command: &str,
    required: bool,
    wait: std::time::Duration,
) -> Result<Option<std::fs::File>, String> {
    use std::os::unix::io::AsRawFd;
    let unlocked = |e: std::io::Error| {
        let why = format!("cannot lock {}: {e}", dir.display());
        if required {
            Err(format!("{why}, and --expect needs the lock"))
        } else {
            eprintln!(
                "{command}: warning: {}; another install could run at the same time",
                output::clean(&why)
            );
            Ok(None)
        }
    };
    let folder = match std::fs::File::open(dir) {
        Ok(folder) => folder,
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return unlocked(e),
    };
    let start = std::time::Instant::now();
    let mut said = false;
    loop {
        // SAFETY: flock on a descriptor we own; the lock lives as long as
        // `folder`, and the kernel drops it when this process exits.
        if unsafe { libc::flock(folder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(folder));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::WouldBlock {
            return unlocked(e);
        }
        if start.elapsed() >= wait {
            return Err(
                "another residue service install or uninstall is still running; try again when it ends"
                    .into(),
            );
        }
        if !said {
            eprintln!("{command}: waiting for another residue service install or uninstall to end");
            said = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn lock(_command: &str, _required: bool) -> Result<Option<std::fs::File>, String> {
    Ok(None)
}

/// The lines `service install` prints under what it installed: what the
/// service removes, with the values it was installed with, how to get a
/// worktree back, and the command that previews a pass.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn removes(opts: &Options) -> Vec<String> {
    let what = [
        format!(
            "a worktree whose upstream branch is gone, once unchanged for {};",
            opts.grace.text
        ),
        format!(
            "         below {}% free, worktrees unchanged for {}, largest first",
            opts.min_free, opts.idle.text
        ),
    ];
    let mut lines = Vec::new();
    if opts.dry_run {
        lines.push(format!(
            "  logs:    what it would remove (--dry-run): {}",
            what[0]
        ));
        lines.push(format!("  {}", what[1]));
    } else {
        lines.push(format!("  removes: {}", what[0]));
        lines.push(format!("  {}", what[1]));
        lines.push(format!(
            "  deletes: without the trash; `residue restore` brings one back for {}",
            opts.keep_removed.text
        ));
    }
    lines.push(format!("  preview: {}", preview_command(opts)));
    lines
}

/// `residue watch --once --dry-run`, with the ROOTs and the options that
/// change what a pass would remove when they are not the defaults.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn preview_command(opts: &Options) -> String {
    let mut words = vec!["residue watch --once --dry-run".to_string()];
    let defaults = Options::parse(&[]).ok().flatten();
    if let Some(d) = &defaults {
        if opts.grace.text != d.grace.text {
            words.push(format!("--grace {}", opts.grace.text));
        }
        if opts.idle.text != d.idle.text {
            words.push(format!("--idle {}", opts.idle.text));
        }
        if opts.min_free != d.min_free {
            words.push(format!("--min-free {}", opts.min_free));
        }
    }
    if opts.fetch_every.is_none() {
        words.push("--no-fetch".into());
    }
    if !opts.default_roots {
        words.extend(opts.roots.iter().map(|r| output::home(&output::shell(r))));
    }
    words.join(" ")
}

/// The `--keep-removed` an installed service runs with: the default when
/// it names none (a service installed before `watch` took it). `None` when
/// it does not run `watch` or its value does not parse.
fn keep_removed(installed: &Installed) -> Option<crate::cli::Span> {
    if installed.program.get(1).map(String::as_str) != Some("watch") {
        return None;
    }
    let value = watch::option_in(installed.program.get(2..)?, "--keep-removed");
    crate::cli::Span::parse(
        "--keep-removed",
        value.as_deref().unwrap_or(crate::cli::DEFAULT_KEEP_REMOVED),
    )
    .ok()
}

/// The ROOTs a user named for the installed service, when `service install`
/// runs again without one: running it again keeps them. `None` when its
/// roots were the default ones (then they are looked for again, so a code
/// folder created since is added), or when none of them is still a folder.
/// A root that is gone is dropped, with a note.
fn named_roots(installed: &Installed) -> Option<Vec<PathBuf>> {
    if installed.default_roots {
        return None;
    }
    let args = installed.program.get(2..)?;
    if installed.program.get(1).map(String::as_str) != Some("watch") {
        return None;
    }
    let mut roots = Vec::new();
    for root in watch::roots_in(args) {
        match Path::new(&root).canonicalize() {
            Ok(path) if path.is_dir() => roots.push(path),
            _ => eprintln!(
                "residue service install: {} is no longer a folder; the new service does not watch it",
                output::clean(&output::home(&root))
            ),
        }
    }
    (!roots.is_empty()).then_some(roots)
}

/// What an installed service runs, read back from its file.
pub struct Installed {
    /// The program and its arguments: `/opt/homebrew/bin/residue watch ...`.
    program: Vec<String>,
    /// Its roots are the default code folders, not ones a user named
    /// (`DEFAULT_MARK`).
    default_roots: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Installed {
    /// The `runs:`, `watches:` and `removal records:` lines of `service
    /// status`.
    fn print(&self) {
        let Some((exe, args)) = self.program.split_first() else {
            return;
        };
        println!("runs: {}", output::clean(&shown(Path::new(exe), args)));
        if args.first().map(String::as_str) == Some("watch") {
            let roots: Vec<String> = watch::roots_in(&args[1..])
                .iter()
                .map(|r| output::home(r))
                .collect();
            println!(
                "watches: {}{}",
                output::clean(&roots.join(", ")),
                if self.default_roots {
                    " (the code folders at install; install again to add new ones)"
                } else {
                    ""
                }
            );
            // A service installed before `watch` took --keep-removed runs
            // its removals with the default.
            let keep = watch::option_in(&args[1..], "--keep-removed");
            println!(
                "removal records: kept {} (--keep-removed{})",
                output::clean(keep.as_deref().unwrap_or(crate::cli::DEFAULT_KEEP_REMOVED)),
                if keep.is_some() { "" } else { ", the default" }
            );
        }
    }
}

/// Written into a service file whose roots are the default ones, so a later
/// `service install` without a ROOT looks for them again instead of keeping
/// them.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const DEFAULT_MARK: &str = "residue: default roots";

/// The service file of a `brew services start residue`, if there is one.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn homebrew_service() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let dir = home()?.join("Library/LaunchAgents");
    #[cfg(target_os = "linux")]
    let dir = home()?.join(".config/systemd/user");
    HOMEBREW
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.exists())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn homebrew_service() -> Option<PathBuf> {
    None
}

/// The line `residue scan` ends with when automatic cleanup is off: no
/// `residue service` or Homebrew service is installed, and no `residue
/// watch` of this user is running. `None` when it is on, or when residue
/// cannot tell. `common_dirs` are the repositories the scan found, and
/// `scanned` the PATHs it scanned: when the default roots do not hold every
/// repository, the command names the scanned PATHs, so following it covers
/// what the user just looked at.
pub fn hint(common_dirs: &[&Path], scanned: &[PathBuf]) -> Option<String> {
    // Elsewhere `service install` cannot turn it on: it prints a command
    // for the system's service manager.
    if cfg!(not(any(target_os = "macos", target_os = "linux")))
        || platform::file().is_some_and(|f| f.exists())
        || homebrew_service().is_some()
        || watch::lock_held() != Some(false)
    {
        return None;
    }
    let covered = watch::default_roots().is_ok_and(|roots| {
        common_dirs
            .iter()
            .all(|dir| roots.iter().any(|root| dir.starts_with(root)))
    });
    let roots = if covered {
        String::new()
    } else {
        let paths: Vec<String> = scanned
            .iter()
            .map(|p| {
                let p = p.canonicalize().unwrap_or_else(|_| p.clone());
                let shown = output::shell(&p);
                output::home(&shown)
            })
            .collect();
        format!(" {}", paths.join(" "))
    };
    // The preview first: it removes nothing, and shows what turning it on
    // would remove now.
    Some(format!(
        "Automatic cleanup is off. Preview it: residue watch --once --dry-run{roots} · turn it on: residue service install{roots}"
    ))
}

/// The path the service should start. When the `residue` on `PATH` is this
/// binary, use it: under Homebrew it is the stable link, not the versioned
/// file, so the service keeps working after `brew upgrade`.
fn program() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
    let real = current.canonicalize().unwrap_or_else(|_| current.clone());
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("residue");
            if candidate.is_absolute() && candidate.canonicalize().ok().as_ref() == Some(&real) {
                return Ok(candidate);
            }
        }
    }
    Ok(current)
}

/// The command line, quoted for a shell, to show what the service runs.
fn shown(exe: &Path, args: &[String]) -> String {
    std::iter::once(exe.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|a| {
            if !a.is_empty()
                && a.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-%".contains(&b))
            {
                a
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    use std::process::{Command, Stdio};
    // What `Command::output` does, with the child started by run::spawn.
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = crate::run::spawn(&mut cmd)
        .and_then(|child| child.wait_with_output())
        .map_err(|e| format!("{program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = stderr.trim();
        Err(format!(
            "{program} {} exited with {}{}{}",
            args.join(" "),
            out.status,
            if detail.is_empty() { "" } else { ": " },
            detail
        ))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{DEFAULT_MARK, Installed, LABEL, home, run, shown};
    use crate::watch::Options;
    use std::path::{Path, PathBuf};

    const LAUNCHCTL: &str = "/bin/launchctl";

    /// The plist `install` writes.
    pub fn file() -> Option<PathBuf> {
        Some(
            home()?
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
        )
    }

    fn log_file() -> Option<PathBuf> {
        Some(home()?.join("Library/Logs/residue/watch.log"))
    }

    fn domain() -> String {
        // SAFETY: getuid cannot fail.
        format!("gui/{}", unsafe { libc::getuid() })
    }

    fn xml(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    fn unxml(text: &str) -> String {
        text.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&amp;", "&")
    }

    /// What the installed plist runs.
    pub fn installed() -> Option<Installed> {
        parse(&std::fs::read_to_string(file()?).ok()?)
    }

    /// What a plist runs, from its `ProgramArguments`.
    pub fn parse(text: &str) -> Option<Installed> {
        let after = text.split_once("<key>ProgramArguments</key>")?.1;
        let array = after.split_once("<array>")?.1.split_once("</array>")?.0;
        let program = array
            .split("<string>")
            .skip(1)
            .filter_map(|s| s.split_once("</string>").map(|(v, _)| unxml(v)))
            .collect();
        Some(Installed {
            program,
            default_roots: text.contains(DEFAULT_MARK),
        })
    }

    /// `launchctl print` of the agent, which fails when launchd has not
    /// loaded it.
    fn launchd_print() -> Result<String, String> {
        run(LAUNCHCTL, &["print", &format!("{}/{LABEL}", domain())])
    }

    /// Whether launchd has the agent loaded.
    pub fn loaded() -> Option<bool> {
        Some(launchd_print().is_ok())
    }

    /// `before_write` runs right before the plist is written. It returns the
    /// lock, held until this returns, or an exit status: then nothing is
    /// written.
    pub fn install(
        exe: &Path,
        opts: &Options,
        watches: &str,
        before_write: impl FnOnce() -> Result<Option<std::fs::File>, u8>,
    ) -> u8 {
        let (Some(plist), Some(log)) = (file(), log_file()) else {
            eprintln!("residue service install: HOME is not set");
            return 1;
        };
        let args = opts.to_args();
        let mut program = format!("    <string>{}</string>\n", xml(&exe.to_string_lossy()));
        for a in &args {
            program.push_str(&format!("    <string>{}</string>\n", xml(a)));
        }
        let mark = if opts.default_roots {
            format!(
                "  <!-- {DEFAULT_MARK}: the code folders that existed in the home folder at install -->\n"
            )
        } else {
            String::new()
        };
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into());
        let log_text = xml(&log.to_string_lossy());
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
{mark}  <key>ProgramArguments</key>
  <array>
{program}  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>60</integer>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>Nice</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>{log_text}</string>
  <key>StandardErrorPath</key>
  <string>{log_text}</string>
</dict>
</plist>
"#,
            path = xml(&path)
        );
        for dir in [plist.parent(), log.parent()].into_iter().flatten() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("residue service install: {}: {e}", dir.display());
                return 1;
            }
        }
        // Held until this returns, so the write and the start are one
        // install's too.
        let _lock = match before_write() {
            Ok(lock) => lock,
            Err(code) => return code,
        };
        if let Err(e) = std::fs::write(&plist, body) {
            eprintln!("residue service install: {}: {e}", plist.display());
            return 1;
        }
        let domain = domain();
        // Replace a running one. Not running is fine.
        let _ = run(LAUNCHCTL, &["bootout", &format!("{domain}/{LABEL}")]);
        let plist_text = plist.to_string_lossy();
        // bootstrap right after bootout can fail while launchd finishes the
        // bootout, so try a few times.
        let mut last = Ok(String::new());
        for _ in 0..5 {
            last = run(LAUNCHCTL, &["bootstrap", &domain, &plist_text]);
            if last.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        if let Err(e) = last {
            eprintln!("residue service install: {e}");
            return 1;
        }
        println!("installed and started the launchd agent {LABEL}");
        println!("  watches: {watches}");
        println!("  runs:    {}", shown(exe, &args));
        println!("  plist:   {}", plist.display());
        println!("  log:     {}", log.display());
        for line in super::removes(opts) {
            println!("{line}");
        }
        println!("  check:   residue service status");
        println!("  turn it off: residue service uninstall");
        0
    }

    pub fn uninstall() -> u8 {
        let Some(plist) = file() else {
            eprintln!("residue service uninstall: HOME is not set");
            return 1;
        };
        let _lock = match super::lock("residue service uninstall", false) {
            Ok(lock) => lock,
            Err(message) => {
                eprintln!("residue service uninstall: {message}");
                return 1;
            }
        };
        let was_loaded = run(LAUNCHCTL, &["bootout", &format!("{}/{LABEL}", domain())]).is_ok();
        let had_file = plist.exists();
        if had_file {
            if let Err(e) = std::fs::remove_file(&plist) {
                eprintln!("residue service uninstall: {}: {e}", plist.display());
                return 1;
            }
        }
        if was_loaded || had_file {
            println!("stopped and removed the launchd agent {LABEL}");
        } else {
            println!("the launchd agent {LABEL} is not installed");
        }
        0
    }

    pub fn status() -> u8 {
        let printed = launchd_print();
        // Its file and binary, read once for what it runs and its
        // fingerprint.
        let seen = super::Seen::read(Some(printed.is_ok()));
        match printed {
            Ok(text) => {
                let field = |name: &str| {
                    text.lines()
                        .map(str::trim)
                        .find_map(|l| l.strip_prefix(name).map(|v| v.trim().to_string()))
                };
                let state = field("state = ").unwrap_or_else(|| "unknown".into());
                match field("pid = ") {
                    Some(pid) => println!("launchd agent {LABEL}: {state} (pid {pid})"),
                    None => println!("launchd agent {LABEL}: {state}"),
                }
                if let Some(installed) = seen.as_ref().ok().and_then(super::Seen::installed) {
                    installed.print();
                }
                super::print_fingerprint(&seen);
                super::freed(true);
                if let Some(log) = log_file() {
                    println!("log: {}", log.display());
                    super::tail(&log);
                }
                0
            }
            Err(_) => {
                println!("launchd agent {LABEL}: not installed");
                super::homebrew_note();
                super::print_fingerprint(&seen);
                super::freed(false);
                3
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{DEFAULT_MARK, Installed, UNIT, home, run, shown};
    use crate::watch::Options;
    use std::path::{Path, PathBuf};

    const SYSTEMCTL: &str = "systemctl";

    /// The unit file `install` writes.
    pub fn file() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| Some(home()?.join(".config")))?;
        Some(config.join("systemd/user").join(UNIT))
    }

    /// A value for a unit file: double-quoted, with `\`, `"`, `%` and `$`
    /// escaped so systemd passes it through unchanged.
    fn quote(text: &str) -> String {
        format!(
            "\"{}\"",
            text.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%")
                .replace('$', "$$")
        )
    }

    /// The words of an `ExecStart=` value that `quote` wrote: each one
    /// double-quoted (an unquoted word is taken as it is), `\\` and `\"`
    /// inside quotes, `%%` and `$$` for `%` and `$`.
    fn unquote(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut chars = line.chars().peekable();
        loop {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
            let Some(first) = chars.next() else {
                break;
            };
            let mut word = String::new();
            if first == '"' {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => word.extend(chars.next()),
                        '"' => break,
                        c => word.push(c),
                    }
                }
            } else {
                word.push(first);
                while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                    word.push(c);
                }
            }
            words.push(word.replace("%%", "%").replace("$$", "$"));
        }
        words
    }

    /// What the installed unit runs.
    pub fn installed() -> Option<Installed> {
        parse(&std::fs::read_to_string(file()?).ok()?)
    }

    /// What a unit runs, from its `ExecStart=`.
    pub fn parse(text: &str) -> Option<Installed> {
        let exec = text.lines().find_map(|l| l.strip_prefix("ExecStart="))?;
        Some(Installed {
            program: unquote(exec),
            default_roots: text.contains(DEFAULT_MARK),
        })
    }

    /// The unit file is the service here: `service status` calls it
    /// installed when the file exists.
    pub fn loaded() -> Option<bool> {
        None
    }

    /// `before_write` runs right before the unit is written. It returns the
    /// lock, held until this returns, or an exit status: then nothing is
    /// written.
    pub fn install(
        exe: &Path,
        opts: &Options,
        watches: &str,
        before_write: impl FnOnce() -> Result<Option<std::fs::File>, u8>,
    ) -> u8 {
        if let Err(e) = run(SYSTEMCTL, &["--user", "show-environment"]) {
            eprintln!("residue service install: no systemd user session ({e})");
            eprintln!(
                "It would watch {watches}. Run this from your system's service manager instead:"
            );
            eprintln!("  {}", shown(exe, &opts.to_args()));
            return 1;
        }
        let Some(unit) = file() else {
            eprintln!("residue service install: HOME is not set");
            return 1;
        };
        let args = opts.to_args();
        let exec = std::iter::once(quote(&exe.to_string_lossy()))
            .chain(args.iter().map(|a| quote(a)))
            .collect::<Vec<_>>()
            .join(" ");
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let mark = if opts.default_roots {
            format!(
                "# {DEFAULT_MARK}: the code folders that existed in the home folder at install\n"
            )
        } else {
            String::new()
        };
        let body = format!(
            "{mark}[Unit]\n\
             Description=residue watch: reclaim storage from Git worktrees\n\
             Documentation=https://github.com/whiteheap/residue-cli\n\
             \n\
             [Service]\n\
             ExecStart={exec}\n\
             Environment={}\n\
             Restart=on-failure\n\
             RestartSec=60\n\
             Nice=10\n\
             IOSchedulingClass=idle\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            quote(&format!("PATH={path}"))
        );
        if let Some(dir) = unit.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("residue service install: {}: {e}", dir.display());
                return 1;
            }
        }
        // Held until this returns, so the write and the start are one
        // install's too.
        let _lock = match before_write() {
            Ok(lock) => lock,
            Err(code) => return code,
        };
        if let Err(e) = std::fs::write(&unit, body) {
            eprintln!("residue service install: {}: {e}", unit.display());
            return 1;
        }
        for step in [
            vec!["--user", "daemon-reload"],
            vec!["--user", "enable", UNIT],
            vec!["--user", "restart", UNIT],
        ] {
            if let Err(e) = run(SYSTEMCTL, &step) {
                eprintln!("residue service install: {e}");
                return 1;
            }
        }
        println!("installed and started the systemd user service {UNIT}");
        println!("  watches: {watches}");
        println!("  runs:    {}", shown(exe, &args));
        println!("  unit:    {}", unit.display());
        println!("  log:     journalctl --user -u {UNIT}");
        for line in super::removes(opts) {
            println!("{line}");
        }
        println!("  check:   residue service status");
        println!("  turn it off: residue service uninstall");
        println!("User services stop when you log out. To keep it running, run once:");
        println!("  loginctl enable-linger \"$USER\"");
        0
    }

    pub fn uninstall() -> u8 {
        let Some(unit) = file() else {
            eprintln!("residue service uninstall: HOME is not set");
            return 1;
        };
        let _lock = match super::lock("residue service uninstall", false) {
            Ok(lock) => lock,
            Err(message) => {
                eprintln!("residue service uninstall: {message}");
                return 1;
            }
        };
        let had_file = unit.exists();
        let _ = run(SYSTEMCTL, &["--user", "disable", "--now", UNIT]);
        if had_file {
            if let Err(e) = std::fs::remove_file(&unit) {
                eprintln!("residue service uninstall: {}: {e}", unit.display());
                return 1;
            }
            let _ = run(SYSTEMCTL, &["--user", "daemon-reload"]);
            println!("stopped and removed the systemd user service {UNIT}");
        } else {
            println!("the systemd user service {UNIT} is not installed");
        }
        0
    }

    pub fn status() -> u8 {
        let installed = file().is_some_and(|u| u.exists());
        // Its file and binary, read once for what it runs and its
        // fingerprint.
        let seen = super::Seen::read(None);
        if !installed {
            println!("systemd user service {UNIT}: not installed");
            super::homebrew_note();
            super::print_fingerprint(&seen);
            super::freed(false);
            return 3;
        }
        let state = match run(SYSTEMCTL, &["--user", "is-active", UNIT]) {
            Ok(s) => s.trim().to_string(),
            Err(_) => "inactive".into(),
        };
        println!("systemd user service {UNIT}: {state}");
        if let Some(installed) = seen.as_ref().ok().and_then(super::Seen::installed) {
            installed.print();
        }
        super::print_fingerprint(&seen);
        super::freed(true);
        if let Ok(log) = run(
            "journalctl",
            &["--user", "-u", UNIT, "-n", "5", "--no-pager", "-o", "cat"],
        ) {
            print!("{log}");
        }
        0
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::{Installed, shown};
    use crate::watch::Options;
    use std::path::{Path, PathBuf};

    pub fn file() -> Option<PathBuf> {
        None
    }

    pub fn installed() -> Option<Installed> {
        None
    }

    /// Writes nothing, so `before_write` never runs.
    pub fn install(
        exe: &Path,
        opts: &Options,
        watches: &str,
        _before_write: impl FnOnce() -> Result<Option<std::fs::File>, u8>,
    ) -> u8 {
        eprintln!(
            "residue service install: residue knows launchd and systemd, and this system has neither."
        );
        eprintln!("It would watch {watches}. Run this from your system's service manager instead:");
        eprintln!("  {}", shown(exe, &opts.to_args()));
        1
    }

    pub fn uninstall() -> u8 {
        eprintln!("residue service: residue installs services only with launchd and systemd");
        1
    }

    pub fn status() -> u8 {
        eprintln!("residue service: residue installs services only with launchd and systemd");
        1
    }
}

/// On `service status` when residue's own service is not installed: say so
/// when Homebrew runs one instead.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn homebrew_note() {
    if let Some(file) = homebrew_service() {
        println!(
            "Homebrew runs residue watch instead ({}): brew services info residue",
            output::clean(&file.to_string_lossy())
        );
    }
}

/// On `service status`: what automatic cleanup freed in the last 7 days
/// and when it last removed a worktree (`stats::service_line`). Without
/// residue's own service, only once `watch` has freed something, as a
/// Homebrew service's `watch` would.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn freed(installed: bool) {
    if let Some(line) = crate::stats::service_line(installed) {
        println!("{}", output::clean(&line));
    }
}

/// Print the last lines of a log file, if any.
#[cfg(target_os = "macos")]
fn tail(log: &Path) {
    if let Ok(text) = std::fs::read_to_string(log) {
        let lines: Vec<&str> = text.lines().collect();
        for line in &lines[lines.len().saturating_sub(5)..] {
            println!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    /// `uninstall` and `status` run only when nothing follows them: help
    /// anywhere prints help, and any other argument is a usage error.
    #[test]
    fn uninstall_and_status_take_no_arguments() {
        for command in ["uninstall", "status"] {
            for help in ["-h", "--help"] {
                assert_eq!(action(&args(&[command, help])), Action::Help);
                assert_eq!(action(&args(&[command, "extra", help])), Action::Help);
            }
            for extra in ["extra", "--dry-run", "~/src", "-", ""] {
                assert_eq!(
                    action(&args(&[command, extra])),
                    Action::Usage(format!(
                        "residue service {command}: unexpected argument {extra}"
                    ))
                );
            }
            assert_eq!(
                action(&args(&[command, "a", "b"])),
                Action::Usage(format!("residue service {command}: unexpected argument a"))
            );
        }
        assert_eq!(action(&args(&["uninstall"])), Action::Uninstall);
        assert_eq!(action(&args(&["status"])), Action::Status);
    }

    #[test]
    fn other_commands() {
        assert_eq!(action(&args(&[])), Action::Missing);
        assert_eq!(action(&args(&["--help"])), Action::Help);
        assert_eq!(action(&args(&["-h"])), Action::Help);
        assert_eq!(action(&args(&["install", "--help"])), Action::Install);
        assert_eq!(action(&args(&["install", "~/src"])), Action::Install);
        assert_eq!(
            action(&args(&["remove"])),
            Action::Usage("residue service: unknown command remove".into())
        );
        let bad = std::ffi::OsStr::from_bytes(b"un\xffinstall").to_os_string();
        assert!(matches!(action(&[bad]), Action::Usage(_)));
        let bad = std::ffi::OsStr::from_bytes(b"x\xff").to_os_string();
        assert!(matches!(
            action(&[OsString::from("uninstall"), bad]),
            Action::Usage(_)
        ));
    }

    /// What `service install` says it removes names the values it was
    /// installed with, and the preview runs a pass with them.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn removes_names_the_installed_values() {
        let parse = |words: &[&str]| Options::parse(&args(words)).unwrap().unwrap();
        let lines = removes(&parse(&[]));
        assert_eq!(
            lines,
            [
                "  removes: a worktree whose upstream branch is gone, once unchanged for 1h;",
                "           below 10% free, worktrees unchanged for 2d, largest first",
                "  deletes: without the trash; `residue restore` brings one back for 30d",
                "  preview: residue watch --once --dry-run",
            ]
        );
        let opts = parse(&[
            "--grace",
            "3h",
            "--idle",
            "1w",
            "--min-free",
            "15",
            "--keep-removed",
            "12w",
            "--no-fetch",
            "/",
        ]);
        let lines = removes(&opts).join("\n");
        for want in [
            "for 3h;",
            "below 15% free",
            "unchanged for 1w",
            "back for 12w",
        ] {
            assert!(lines.contains(want), "{want} missing from {lines}");
        }
        assert_eq!(
            preview_command(&opts),
            "residue watch --once --dry-run --grace 3h --idle 1w --min-free 15 --no-fetch /"
        );
        let lines = removes(&parse(&["--dry-run"]));
        assert!(lines[0].starts_with("  logs:    what it would remove (--dry-run):"));
        assert!(!lines.iter().any(|l| l.contains("deletes:")));
    }

    /// `service install --help` names `--expect` and its exit status, in
    /// lines as wide as `watch --help`'s.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn install_help_names_expect() {
        assert!(INSTALL_HELP.contains("--expect FINGERPRINT"));
        assert!(INSTALL_HELP.contains(&format!("exit {CHANGED}")));
        assert!(HELP.contains("--expect"));
        for line in INSTALL_HELP.lines().chain(HELP.lines()) {
            assert!(line.chars().count() <= 80, "{line}");
        }
    }

    /// `--expect VALUE` comes out of the arguments once, wherever it is,
    /// and leaves the rest, in order, for `watch`.
    #[test]
    fn expect_takes_one_value() {
        let fp = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            expect_in(&args(&["--dry-run", "--expect", fp, "~/src"])),
            Ok((
                Some(Expect::Fingerprint(fp.into())),
                args(&["--dry-run", "~/src"])
            ))
        );
        assert_eq!(
            expect_in(&args(&["--expect", "none"])),
            Ok((Some(Expect::None), Vec::new()))
        );
        assert_eq!(
            expect_in(&args(&["--grace", "3h", "/"])),
            Ok((None, args(&["--grace", "3h", "/"])))
        );
        assert_eq!(
            expect_in(&args(&["--grace", "3h", "--expect"])),
            Err(
                "--expect needs a value: `none`, or the fingerprint `residue service status` prints"
                    .into()
            )
        );
        assert_eq!(
            expect_in(&args(&["--expect", "none", "--expect", fp])),
            Err("--expect can be given once".into())
        );
        let upper = fp.to_uppercase();
        let long = format!("{fp}0");
        for bad in [
            "",
            "None",
            "NONE",
            "--dry-run",
            &upper,
            &fp[1..],
            &long,
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert_eq!(
                Expect::parse(bad),
                Err(format!(
                    "--expect {bad}: use `none`, or the fingerprint `residue service status` prints"
                ))
            );
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const RELEASE: &str = "0.0.12";

    /// A service launchd has loaded, from a file that starts a Homebrew
    /// binary.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn installed() -> Seen {
        Seen {
            loaded: Some(true),
            file: Some(b"<plist>runs residue watch /src</plist>\n".to_vec()),
            binary: Some(PathBuf::from(
                "/opt/homebrew/Cellar/residue/0.0.12/bin/residue",
            )),
        }
    }

    /// The fingerprint is 32 lowercase hex digits; the same service and
    /// release give the same one, and a change to any part gives another.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn fingerprint_changes_with_each_input() {
        let base = installed();
        let fp = fingerprint(&base, RELEASE);
        assert_eq!(fp.len(), 32);
        assert!(fp.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert_eq!(Expect::parse(&fp), Ok(Expect::Fingerprint(fp.clone())));
        assert_eq!(fingerprint(&base.clone(), RELEASE), fp);
        let with = |change: fn(&mut Seen)| {
            let mut seen = installed();
            change(&mut seen);
            fingerprint(&seen, RELEASE)
        };
        let all = [
            ("same", fp.clone()),
            ("another release", fingerprint(&base, "0.0.13")),
            ("not loaded", with(|s| s.loaded = Some(false))),
            ("Linux, no load state", with(|s| s.loaded = None)),
            ("no file", with(|s| s.file = None)),
            ("an empty file", with(|s| s.file = Some(Vec::new()))),
            (
                "one more byte",
                with(|s| s.file.as_mut().unwrap().push(b' ')),
            ),
            (
                "one byte changed",
                with(|s| s.file.as_mut().unwrap()[1] = b'P'),
            ),
            ("no binary", with(|s| s.binary = None)),
            (
                "brew upgrade",
                with(|s| s.binary = Some("/opt/homebrew/Cellar/residue/0.0.13/bin/residue".into())),
            ),
            // Each part is hashed with its length, so a byte cannot move
            // from one part to the next and leave the same fingerprint.
            (
                "a byte moved from the file to the binary",
                with(|s| {
                    s.file.as_mut().unwrap().pop();
                    s.binary = Some("\n/opt/homebrew/Cellar/residue/0.0.12/bin/residue".into());
                }),
            ),
        ];
        for (i, (what, a)) in all.iter().enumerate() {
            for (other, b) in &all[i + 1..] {
                assert_ne!(a, b, "{what} and {other}");
            }
        }
    }

    /// The fingerprint `service status` printed before `fingerprint::Hasher`
    /// took over the hash is the one it prints now: the Mac app passes it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn fingerprint_is_unchanged() {
        assert_eq!(
            fingerprint(&installed(), RELEASE),
            "f4e4989bb9a72db8de1e168008b8ad33"
        );
        let none = Seen {
            loaded: None,
            file: None,
            binary: None,
        };
        assert_eq!(
            fingerprint(&none, RELEASE),
            "cd127761c2dcd6d6564aeb6f8e5d5858"
        );
    }

    /// `--expect none` holds only with no file and nothing loaded, and a
    /// fingerprint only for the service and the release that printed it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn expect_decides_on_the_service_seen() {
        let none = Seen {
            loaded: Some(false),
            file: None,
            binary: None,
        };
        let linux_none = Seen {
            loaded: None,
            ..none.clone()
        };
        let file_not_loaded = Seen {
            loaded: Some(false),
            ..installed()
        };
        let loaded_no_file = Seen {
            file: None,
            binary: None,
            ..installed()
        };
        assert!(is_expected(&Expect::None, &none, RELEASE));
        assert!(is_expected(&Expect::None, &linux_none, RELEASE));
        for some in [&installed(), &file_not_loaded, &loaded_no_file] {
            assert!(!is_expected(&Expect::None, some, RELEASE), "{some:?}");
        }
        let shown = Expect::Fingerprint(fingerprint(&installed(), RELEASE));
        assert!(is_expected(&shown, &installed(), RELEASE));
        assert!(!is_expected(&shown, &installed(), "0.0.13"));
        let mut edited = installed();
        edited.file.as_mut().unwrap().insert(0, b' ');
        let mut upgraded = installed();
        upgraded.binary = Some("/opt/homebrew/Cellar/residue/0.0.13/bin/residue".into());
        for changed in [&edited, &upgraded, &file_not_loaded, &loaded_no_file, &none] {
            assert!(!is_expected(&shown, changed, RELEASE), "{changed:?}");
        }
        // The fingerprint of no service names that, as `none` does.
        let nothing = Expect::Fingerprint(fingerprint(&none, RELEASE));
        assert!(is_expected(&nothing, &none, RELEASE));
        assert!(!is_expected(&nothing, &installed(), RELEASE));
    }

    /// One holder at a time: a second waits for the first to let go, and
    /// fails once it has waited long enough. A missing folder is no lock,
    /// unless one is required.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn lock_folder_lets_one_hold_it() {
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!(
            "residue-service-test-{}-{}",
            std::process::id(),
            output::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let first = lock_folder(&dir, "test", true, Duration::ZERO).unwrap();
        assert!(first.is_some());
        let busy = lock_folder(&dir, "test", true, Duration::from_millis(200)).unwrap_err();
        assert_eq!(
            busy,
            "another residue service install or uninstall is still running; try again when it ends"
        );
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(first);
        });
        let second = lock_folder(&dir, "test", true, Duration::from_secs(10)).unwrap();
        assert!(second.is_some());
        release.join().unwrap();
        drop(second);
        let missing = dir.join("missing");
        assert!(matches!(
            lock_folder(&missing, "test", false, Duration::ZERO),
            Ok(None)
        ));
        let required = lock_folder(&missing, "test", true, Duration::ZERO).unwrap_err();
        assert!(
            required.ends_with(", and --expect needs the lock"),
            "{required}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
