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

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use crate::cli::Span;

/// Folders a package manager fills from a registry. An environment file in
/// one came with a package, and the next install puts it back, so it does
/// not keep its worktree the way one in `build/` does ([`env_file`]).
pub const PACKAGE_DIRS: &[&str] = &[
    "node_modules",
    ".pnpm-store",
    "bower_components",
    ".venv",
    "venv",
    ".tox",
    ".nox",
];

/// `.env` and `.env.NAME`, in any case: where a project keeps its secrets
/// and local settings, which no build or install recreates. A rule that
/// matches the folder around one does not cover it, so it keeps its
/// worktree until a rule names the file itself (`Rules::names_file`). The
/// templates `.env.example`, `.env.sample`, `.env.template` and `.env.dist`
/// are not, since a repository usually tracks them.
pub fn env_file(name: &OsStr) -> bool {
    let name = name.as_bytes().to_ascii_lowercase();
    if name == b".env" {
        return true;
    }
    let Some(rest) = name.strip_prefix(b".env.") else {
        return false;
    };
    let last = rest.rsplit(|&b| b == b'.').next().unwrap_or(rest);
    !rest.is_empty() && !matches!(last, b"example" | b"sample" | b"template" | b"dist")
}

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
        if self.name_matches(trimmed) {
            return true;
        }
        let parts: Vec<&str> = trimmed.split('/').collect();
        self.paths
            .iter()
            .any(|rule| rule.len() <= parts.len() && prefix_matches(rule, &parts))
    }

    /// True when a rule names the file at `entry` itself: a name or `*.ext`
    /// rule that matches its name, or a path rule exactly as long as its
    /// path. A rule that matches a folder above it does not count, so
    /// `build` covers `build/` but does not name `build/.env`.
    pub fn names_file(&self, entry: &str) -> bool {
        if self.name_matches(entry) {
            return true;
        }
        let parts: Vec<&str> = entry.split('/').collect();
        self.paths
            .iter()
            .any(|rule| rule.len() == parts.len() && prefix_matches(rule, &parts))
    }

    /// A name or `*.ext` rule matches the last component of `entry`.
    fn name_matches(&self, entry: &str) -> bool {
        let name = entry.rsplit('/').next().unwrap_or(entry);
        self.names.iter().any(|n| n == name)
            || self
                .suffixes
                .iter()
                .any(|s| name.len() > s.len() && name.ends_with(s.as_str()))
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

/// Where one rule beyond the built-in list came from, for `explain` and
/// JSON `rule_sources`.
#[derive(Clone)]
pub struct RuleSource {
    pub rule: String,
    /// `local`, `worktree`, `global` or `system` for Git config, as Git
    /// names the scope; `residue_config` for the residue config file;
    /// `command_line` for `--regenerable`.
    pub scope: String,
    /// The file, as Git reports it (`file:/home/me/.gitconfig` becomes
    /// `/home/me/.gitconfig`); `None` for `--regenerable`.
    pub origin: Option<String>,
}

#[derive(Clone)]
pub struct Policy {
    rules: Rules,
    pub config: Option<PathBuf>,
    /// Rules from `--regenerable` and the residue config file, in that
    /// order, with their source. Each worktree adds its Git config rules.
    pub sources: Vec<RuleSource>,
    /// `residue.regenerable` values set through the environment, which
    /// residue ignores and reports (`git::environment_regenerable`). An
    /// error when Git could not read that environment.
    pub ignored_environment: Result<Vec<String>, String>,
    /// `--idle`: keep a worktree changed more recently than this.
    pub idle: Option<Span>,
    /// `remove --as-of`: measure `--idle` at this time, in Unix seconds,
    /// not at the moment of each check.
    pub idle_at: Option<i64>,
}

impl Policy {
    pub fn load(extra: &[String]) -> Result<Policy, String> {
        let mut rules = Rules::default();
        for name in DEFAULTS {
            rules.add(name).map_err(|e| format!("built-in list: {e}"))?;
        }
        let mut config = None;
        let mut sources = Vec::new();
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
                        sources.push(RuleSource {
                            rule: line.to_string(),
                            scope: "residue_config".into(),
                            origin: Some(path.display().to_string()),
                        });
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
            sources.push(RuleSource {
                rule: name.clone(),
                scope: "command_line".into(),
                origin: None,
            });
        }
        // The order every rule_sources list uses: --regenerable, then the
        // residue config file (each worktree then adds its Git config).
        sources.sort_by_key(|s| s.scope != "command_line");
        Ok(Policy {
            rules,
            config,
            sources,
            ignored_environment: Ok(Vec::new()),
            idle: None,
            idle_at: None,
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

#[cfg(test)]
mod tests {
    //! Which names are environment files, and when a rule names a file
    //! itself rather than a folder above it.

    use super::{Rules, env_file};
    use std::ffi::OsStr;

    #[test]
    fn env_files_are_dotenv_and_its_variants_but_not_its_templates() {
        for name in [
            ".env",
            ".ENV",
            ".env.local",
            ".env.production",
            ".env.development.local",
        ] {
            assert!(env_file(OsStr::new(name)), "{name} is an environment file");
        }
        for name in [
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            ".env.local.example",
            ".env.",
            ".envrc",
            "env",
            ".environment",
            "prod.env",
        ] {
            assert!(
                !env_file(OsStr::new(name)),
                "{name} is not an environment file"
            );
        }
    }

    #[test]
    fn a_rule_names_a_file_by_its_name_or_its_whole_path_only() {
        let mut rules = Rules::default();
        for rule in ["build", "*.local", "dist/.env", "out/*"] {
            rules.add(rule).unwrap();
        }
        assert!(rules.matches("build/"), "the folder rule covers build/");
        assert!(
            !rules.names_file("build/.env"),
            "a folder rule names no file in it"
        );
        assert!(rules.names_file("build/.env.local"), "*.local names it");
        assert!(rules.names_file("dist/.env"), "the whole path");
        assert!(
            !rules.names_file("dist/.env/x"),
            "a path rule shorter than the path"
        );
        assert!(!rules.names_file("dist/sub/.env"), "another path");
        assert!(rules.names_file("out/.env"), "* matches within its segment");
        assert!(
            !rules.names_file("out/sub/.env"),
            "* stays within one segment"
        );
        assert!(!Rules::default().names_file(".env"), "no rule");
    }
}
