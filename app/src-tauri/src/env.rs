//! The environment residue runs in. An app started from Finder gets launchd's
//! short PATH, which can miss the user's Git. residue runs Git and records PATH
//! in the login service it installs, so the app gives it the PATH the user's
//! login shell has, the same one a terminal gives it.

use crate::cli::Run;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

const MARK: &str = "__RESIDUE_DESKTOP_PATH__";

/// Directories every Mac has, and the usual Homebrew prefixes, added after the
/// shell's own entries when they exist.
const FALLBACK: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

static LOGIN_PATH: OnceLock<(OsString, PathSource)> = OnceLock::new();

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PathSource {
    /// Read from the user's login shell.
    LoginShell,
    /// The shell could not be read; only the standard directories.
    Fallback,
}

/// The PATH for every residue child process. The first call runs the login
/// shell once (at most five seconds); later calls reuse the answer.
pub fn path() -> &'static (OsString, PathSource) {
    LOGIN_PATH.get_or_init(|| match shell_path() {
        Some(p) => (with_fallback(&p), PathSource::LoginShell),
        None => (with_fallback(OsStr::new("")), PathSource::Fallback),
    })
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn shell_path() -> Option<OsString> {
    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into());
    let shell_path = Path::new(&shell);
    let print = if shell_path.file_name() == Some(OsStr::new("fish")) {
        format!("printf '{MARK}%s{MARK}' (string join : $PATH)")
    } else {
        format!("printf '{MARK}%s{MARK}' \"$PATH\"")
    };
    let home = home().unwrap_or_else(|| PathBuf::from("/"));
    // An interactive login shell reads the same files a terminal does. It
    // gets no terminal and no input, and its own PATH as a start.
    let run = Run {
        bin: shell_path,
        args: vec!["-ilc".into(), print.into()],
        cwd: &home,
        path_env: &std::env::var_os("PATH").unwrap_or_default(),
        deadline: Duration::from_secs(5),
    };
    let out = run.output().ok()?;
    let (_, rest) = out.stdout.split_once(MARK)?;
    let (value, _) = rest.split_once(MARK)?;
    (!value.trim().is_empty()).then(|| OsString::from(value.trim()))
}

fn with_fallback(path: &OsStr) -> OsString {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    for dir in FALLBACK {
        let dir = PathBuf::from(dir);
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| FALLBACK.join(":").into())
}
