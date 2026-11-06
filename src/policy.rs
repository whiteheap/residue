//! Which ignored entries residue may let `git worktree remove` delete.
//!
//! Git deletes ignored files without asking when it removes a clean
//! worktree. Being ignored does not make a file disposable (`.env`, local
//! databases and notes are usually ignored too), so residue blocks removal
//! unless every ignored entry matches this list: dependency and build
//! output directories whose owning tool recreates them. Operators extend it
//! with a config file or `--regenerable`.

use std::fs;
use std::io;
use std::path::PathBuf;

pub const DEFAULTS: &[&str] = &[
    // JavaScript and TypeScript
    "node_modules",
    ".pnpm-store",
    "bower_components",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".output",
    ".turbo",
    ".parcel-cache",
    ".vite",
    ".angular",
    ".eslintcache",
    "*.tsbuildinfo",
    "dist",
    "build",
    "coverage",
    ".nyc_output",
    // Rust, Swift, JVM, Dart
    "target",
    ".build",
    "DerivedData",
    ".gradle",
    ".dart_tool",
    // Python
    ".venv",
    "venv",
    "__pycache__",
    "*.pyc",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    // Operating system litter
    ".DS_Store",
];

pub struct Policy {
    names: Vec<String>,
    suffixes: Vec<String>,
    pub config: Option<PathBuf>,
}

impl Policy {
    pub fn load(extra: &[String]) -> Result<Policy, String> {
        let mut policy = Policy {
            names: Vec::new(),
            suffixes: Vec::new(),
            config: None,
        };
        for name in DEFAULTS {
            policy
                .add(name)
                .map_err(|e| format!("built-in list: {e}"))?;
        }
        if let Some(path) = config_path() {
            match fs::read_to_string(&path) {
                Ok(text) => {
                    for (n, line) in text.lines().enumerate() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }
                        policy
                            .add(line)
                            .map_err(|e| format!("{} line {}: {e}", path.display(), n + 1))?;
                    }
                    policy.config = Some(path);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
            }
        }
        for name in extra {
            policy
                .add(name)
                .map_err(|e| format!("--regenerable {name}: {e}"))?;
        }
        Ok(policy)
    }

    fn add(&mut self, entry: &str) -> Result<(), String> {
        let entry = entry.trim_end_matches('/');
        if entry.is_empty() || entry == "." || entry == ".." {
            return Err("an empty name matches nothing".into());
        }
        if entry.contains('/') {
            return Err("a name matches one path component; drop the slash".into());
        }
        if let Some(suffix) = entry.strip_prefix('*') {
            if suffix.len() < 2 || !suffix.starts_with('.') || suffix.contains('*') {
                return Err("only *.ext patterns are supported".into());
            }
            self.suffixes.push(suffix.to_string());
        } else if entry.contains('*') {
            return Err("only *.ext patterns are supported".into());
        } else {
            self.names.push(entry.to_string());
        }
        Ok(())
    }

    /// `entry` is a path as `git status` prints it, relative to the
    /// worktree root, with a trailing slash for a directory.
    pub fn regenerable(&self, entry: &str) -> bool {
        let trimmed = entry.trim_end_matches('/');
        let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
        self.names.iter().any(|n| n == name)
            || self
                .suffixes
                .iter()
                .any(|s| name.len() > s.len() && name.ends_with(s.as_str()))
    }
}

/// `$XDG_CONFIG_HOME/residue/regenerable`, else `~/.config/residue/regenerable`.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("residue").join("regenerable"))
}
