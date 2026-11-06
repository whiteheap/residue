//! `residue service`: run `residue watch` in the background from login, as
//! a launchd agent on macOS or a systemd user service on Linux.

use crate::output;
use crate::watch::{self, Options};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const HELP: &str = "\
residue service: run `residue watch` in the background, from login.

Usage:
  residue service install [WATCH OPTIONS] ROOT...
  residue service status
  residue service uninstall

install writes a launchd agent (macOS) or a systemd user service (Linux)
that runs `residue watch` with these options and roots, starts it now, and
starts it at every login. Running install again replaces it. Try it first
with --dry-run: the service then logs what it would remove.
See `residue watch --help` for the options.
";

/// The launchd label on macOS.
#[cfg(target_os = "macos")]
pub const LABEL: &str = "com.whiteheap.residue.watch";
/// The systemd user unit on Linux.
#[cfg(target_os = "linux")]
pub const UNIT: &str = "residue-watch.service";

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
    let opts = match Options::parse(args) {
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
    let exe = match program() {
        Ok(exe) => exe,
        Err(message) => {
            eprintln!("residue service install: {message}");
            return 1;
        }
    };
    platform::install(&exe, &opts)
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
    use super::{LABEL, home, run, shown};
    use crate::watch::Options;
    use std::path::{Path, PathBuf};

    const LAUNCHCTL: &str = "/bin/launchctl";

    fn plist() -> Option<PathBuf> {
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

    pub fn install(exe: &Path, opts: &Options) -> u8 {
        let (Some(plist), Some(log)) = (plist(), log_file()) else {
            eprintln!("residue service install: HOME is not set");
            return 1;
        };
        let args = opts.to_args();
        let mut program = format!("    <string>{}</string>\n", xml(&exe.to_string_lossy()));
        for a in &args {
            program.push_str(&format!("    <string>{}</string>\n", xml(a)));
        }
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into());
        let log_text = xml(&log.to_string_lossy());
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
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
        println!("  runs:   {}", shown(exe, &args));
        println!("  plist:  {}", plist.display());
        println!("  log:    {}", log.display());
        println!("  check:  residue service status");
        println!("  remove: residue service uninstall");
        0
    }

    pub fn uninstall() -> u8 {
        let Some(plist) = plist() else {
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
                if let Some(log) = log_file() {
                    println!("log: {}", log.display());
                    super::tail(&log);
                }
                0
            }
            Err(_) => {
                println!("launchd agent {LABEL}: not installed");
                3
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{UNIT, home, run, shown};
    use crate::watch::Options;
    use std::path::{Path, PathBuf};

    const SYSTEMCTL: &str = "systemctl";

    fn unit_file() -> Option<PathBuf> {
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

    pub fn install(exe: &Path, opts: &Options) -> u8 {
        if let Err(e) = run(SYSTEMCTL, &["--user", "show-environment"]) {
            eprintln!("residue service install: no systemd user session ({e})");
            eprintln!("Run this from your system's service manager instead:");
            eprintln!("  {}", shown(exe, &opts.to_args()));
            return 1;
        }
        let Some(unit) = unit_file() else {
            eprintln!("residue service install: HOME is not set");
            return 1;
        };
        let args = opts.to_args();
        let exec = std::iter::once(quote(&exe.to_string_lossy()))
            .chain(args.iter().map(|a| quote(a)))
            .collect::<Vec<_>>()
            .join(" ");
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let body = format!(
            "[Unit]\n\
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
        println!("  runs:   {}", shown(exe, &args));
        println!("  unit:   {}", unit.display());
        println!("  log:    journalctl --user -u {UNIT}");
        println!("  check:  residue service status");
        println!("  remove: residue service uninstall");
        println!("User services stop when you log out. To keep it running, run once:");
        println!("  loginctl enable-linger \"$USER\"");
        0
    }

    pub fn uninstall() -> u8 {
        let Some(unit) = unit_file() else {
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
        let installed = unit_file().is_some_and(|u| u.exists());
        if !installed {
            println!("systemd user service {UNIT}: not installed");
            return 3;
        }
        let state = match run(SYSTEMCTL, &["--user", "is-active", UNIT]) {
            Ok(s) => s.trim().to_string(),
            Err(_) => "inactive".into(),
        };
        println!("systemd user service {UNIT}: {state}");
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
    use super::shown;
    use crate::watch::Options;
    use std::path::Path;

    pub fn install(exe: &Path, opts: &Options) -> u8 {
        eprintln!(
            "residue service install: residue knows launchd and systemd, and this system has neither."
        );
        eprintln!("Run this from your system's service manager instead:");
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
