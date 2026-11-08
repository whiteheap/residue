//! `residue service`: run `residue watch` in the background from login, as
//! a launchd agent on macOS or a systemd user service on Linux.

use crate::output;
use crate::watch::{self, Options};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const HELP: &str = "\
residue service: run `residue watch` in the background, from login.

Usage:
  residue service install [WATCH OPTIONS] [ROOT...]
  residue service status
  residue service uninstall

install writes a launchd agent (macOS) or a systemd user service (Linux)
that runs `residue watch` with these options and roots, starts it now, and
starts it at every login. Without a ROOT it uses the usual code folders
that exist in your home folder (`residue watch --help` lists them) and
writes them into the service, so it watches the same folders on every
start; install again to pick up a new one. Running install again replaces
it. Try it first with --dry-run: the service then logs what it would
remove. status shows what it runs and the last lines of its log.
See `residue watch --help` for the options.
";

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

pub fn main(args: &[OsString]) -> u8 {
    match args.first().and_then(|a| a.to_str()) {
        Some("install") => install(&args[1..]),
        Some("uninstall") => platform::uninstall(),
        Some("status") => platform::status(),
        Some("-h" | "--help") => {
            print!("{HELP}");
            0
        }
        Some(other) => {
            eprintln!("residue service: unknown command {}", output::clean(other));
            eprintln!("Run `residue service --help` for usage.");
            2
        }
        None => {
            eprint!("{HELP}");
            2
        }
    }
}

fn install(args: &[OsString]) -> u8 {
    let mut opts = match Options::parse(args) {
        Ok(Some(opts)) => opts,
        Ok(None) => {
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
    if let Some(file) = homebrew_service() {
        eprintln!(
            "residue service install: Homebrew already runs residue watch as a service ({}), and only one watch runs per user. Keep that one, or stop it first: brew services stop residue",
            output::clean(&file.to_string_lossy())
        );
        return 1;
    }
    let replaced = platform::installed();
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
    let code = platform::install(&exe, &opts, &watches);
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
                "The service it replaced kept removal records {old} (--keep-removed). This one keeps new ones {}; records already written keep their own. Pass --keep-removed {old} to keep the old value.",
                output::clean(&opts.keep_removed.text)
            );
        }
    }
    code
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
                    " (the code folders in your home folder at install; install again to pick up a new one)"
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
    let command = if covered {
        "residue service install".to_string()
    } else {
        let paths: Vec<String> = scanned
            .iter()
            .map(|p| {
                let p = p.canonicalize().unwrap_or_else(|_| p.clone());
                let shown = output::shell(&p);
                output::home(&shown)
            })
            .collect();
        format!("residue service install {}", paths.join(" "))
    };
    Some(format!("Automatic cleanup is off. Turn it on: {command}"))
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
    let out = std::process::Command::new(program)
        .args(args)
        .output()
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

    /// What the installed plist runs, from its `ProgramArguments`.
    pub fn installed() -> Option<Installed> {
        let text = std::fs::read_to_string(file()?).ok()?;
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

    pub fn install(exe: &Path, opts: &Options, watches: &str) -> u8 {
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
        println!("  check:   residue service status");
        println!("  turn it off: residue service uninstall");
        0
    }

    pub fn uninstall() -> u8 {
        let Some(plist) = file() else {
            eprintln!("residue service uninstall: HOME is not set");
            return 1;
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
        match run(LAUNCHCTL, &["print", &format!("{}/{LABEL}", domain())]) {
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
                if let Some(installed) = installed() {
                    installed.print();
                }
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

    /// What the installed unit runs, from its `ExecStart=`.
    pub fn installed() -> Option<Installed> {
        let text = std::fs::read_to_string(file()?).ok()?;
        let exec = text.lines().find_map(|l| l.strip_prefix("ExecStart="))?;
        Some(Installed {
            program: unquote(exec),
            default_roots: text.contains(DEFAULT_MARK),
        })
    }

    pub fn install(exe: &Path, opts: &Options, watches: &str) -> u8 {
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
             Documentation=https://github.com/whiteheap/residue\n\
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
        if !installed {
            println!("systemd user service {UNIT}: not installed");
            super::homebrew_note();
            super::freed(false);
            return 3;
        }
        let state = match run(SYSTEMCTL, &["--user", "is-active", UNIT]) {
            Ok(s) => s.trim().to_string(),
            Err(_) => "inactive".into(),
        };
        println!("systemd user service {UNIT}: {state}");
        if let Some(installed) = self::installed() {
            installed.print();
        }
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

    pub fn install(exe: &Path, opts: &Options, watches: &str) -> u8 {
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
