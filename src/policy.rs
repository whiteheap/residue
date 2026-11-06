//! Which ignored entries residue may let `git worktree remove` delete.
//!
//! Git deletes ignored files without asking when it removes a clean
//! worktree. Being ignored does not make a file disposable (`.env`, local
//! databases and notes are usually ignored too), so residue blocks removal
//! unless every ignored entry matches a rule: dependency and build output
//! directories whose owning tool recreates them.
//!
//! Two kinds of rule:
//! - a name (`node_modules`) or `*.ext` matches one path component at any
//!   depth;
//! - a path (`web/.tscache`, `.local-dev/*.lock`) contains a slash and is
//!   anchored at the worktree root. Each segment is literal or uses `*` for
//!   any run of characters within that segment. It matches that path and
//!   everything below it.
//!
//! Rules come from the built-in list, the user's config file,
//! `--regenerable`, and each repository's own git config
//! (`residue.regenerable`). Never from a file committed to the repository:
//! a cloned repository must not be able to declare a user's files
//! disposable, and git config is never transferred by clone. Never from
//! the environment either (`git -c`, `GIT_CONFIG_COUNT`, the legacy
//! `GIT_CONFIG`): every subprocess inherits it, so it must not widen what
//! residue deletes. residue reports those values instead of using them.

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

/// The git config key a repository uses to add its own rules.
pub const GIT_CONFIG_KEY: &str = "residue.regenerable";

#[derive(Clone, Default)]
pub struct Rules {
    names: Vec<String>,
    suffixes: Vec<String>,
    /// Path rules, split into segments.
    paths: Vec<Vec<String>>,
}

impl Rules {
    /// Parse and add one rule. The error says why the rule is invalid.
    pub fn add(&mut self, entry: &str) -> Result<(), String> {
        let entry = entry.trim_end_matches('/');
        if entry.is_empty() || entry == "." || entry == ".." {
            return Err("an empty name matches nothing".into());
        }
        if entry.contains('/') {
            let segments: Vec<String> = entry
                .strip_prefix('/')
                .unwrap_or(entry)
                .split('/')
                .map(str::to_string)
                .collect();
            for segment in &segments {
                check_segment(segment)?;
            }
            self.paths.push(segments);
        } else if let Some(suffix) = entry.strip_prefix('*') {
            if suffix.len() < 2 || !suffix.starts_with('.') || suffix.contains('*') {
                return Err(
                    "a name supports only the *.ext pattern; use a path with a slash for other patterns"
                        .into(),
                );
            }
            self.suffixes.push(suffix.to_string());
        } else if entry.contains('*') {
            return Err(
                "a name supports only the *.ext pattern; use a path with a slash for other patterns"
                    .into(),
            );
        } else {
            self.names.push(entry.to_string());
        }
        Ok(())
    }

    fn extend(&mut self, other: &Rules) {
        self.names.extend(other.names.iter().cloned());
        self.suffixes.extend(other.suffixes.iter().cloned());
        self.paths.extend(other.paths.iter().cloned());
    }

    /// `entry` is a path relative to the worktree root, as `git status`
    /// prints it, with or without a trailing slash for a directory.
    pub fn matches(&self, entry: &str) -> bool {
        let trimmed = entry.trim_end_matches('/');
        let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
        if self.names.iter().any(|n| n == name)
            || self
                .suffixes
                .iter()
                .any(|s| name.len() > s.len() && name.ends_with(s.as_str()))
        {
            return true;
        }
        let parts: Vec<&str> = trimmed.split('/').collect();
        self.paths
            .iter()
            .any(|rule| rule.len() <= parts.len() && prefix_matches(rule, &parts))
    }

    /// True when a path rule names something strictly below the directory
    /// `entry`: `.local-dev/*.lock` reaches inside `.local-dev/`.
    pub fn reaches_inside(&self, entry: &str) -> bool {
        let trimmed = entry.trim_end_matches('/');
        let parts: Vec<&str> = trimmed.split('/').collect();
        self.paths
            .iter()
            .any(|rule| rule.len() > parts.len() && prefix_matches(&rule[..parts.len()], &parts))
    }
}

fn check_segment(segment: &str) -> Result<(), String> {
    if segment.is_empty() || segment == "." || segment == ".." {
        return Err("a path cannot have an empty, `.` or `..` segment".into());
    }
    if segment.contains("**") {
        return Err("`**` is not supported; `*` matches within one segment".into());
    }
    if let Some(c) = segment.chars().find(|c| matches!(c, '?' | '[' | '\\')) {
        return Err(format!(
            "`{c}` is not supported; the only wildcard is `*`, within one segment"
        ));
    }
    Ok(())
}

/// Each rule segment matches the entry segment at the same position.
/// The caller guarantees `rule.len() <= parts.len()`.
fn prefix_matches(rule: &[String], parts: &[&str]) -> bool {
    rule.iter().zip(parts).all(|(r, p)| glob(r, p))
}

/// `*` matches any run of characters (including none); every other
/// character matches itself.
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Position after the last `*` seen, and the text position it resumes at.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi + 1, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((after, from)) = star {
            pi = after;
            ti = from + 1;
            star = Some((after, from + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

pub struct Policy {
    rules: Rules,
    pub config: Option<PathBuf>,
    /// `residue.regenerable` values set through the environment, which
    /// residue ignores and reports (`git::environment_regenerable`). An
    /// error when Git could not read that environment.
    pub ignored_environment: Result<Vec<String>, String>,
}

impl Policy {
    pub fn load(extra: &[String]) -> Result<Policy, String> {
        let mut rules = Rules::default();
        for name in DEFAULTS {
            rules.add(name).map_err(|e| format!("built-in list: {e}"))?;
        }
        let mut config = None;
        if let Some(path) = config_path() {
            match fs::read_to_string(&path) {
                Ok(text) => {
                    for (n, line) in text.lines().enumerate() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }
                        rules
                            .add(line)
                            .map_err(|e| format!("{} line {}: {e}", path.display(), n + 1))?;
                    }
                    config = Some(path);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
            }
        }
        for name in extra {
            rules
                .add(name)
                .map_err(|e| format!("--regenerable {name}: {e}"))?;
        }
        Ok(Policy {
            rules,
            config,
            ignored_environment: Ok(Vec::new()),
        })
    }

    /// The rules for one worktree: everything machine-wide plus the
    /// repository's own git config rules.
    pub fn with(&self, repo: &Rules) -> Rules {
        let mut rules = self.rules.clone();
        rules.extend(repo);
        rules
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
