//! The folders the app scans, kept in `settings.json` in the app's config
//! directory. Until the user saves a choice, the app offers the usual code
//! folders that exist in their home directory.

use crate::cli::CliError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Folder names that commonly hold repositories, in the order they are
/// offered. Only the ones that exist are used.
const USUAL: &[&str] = &[
    "Work",
    "work",
    "src",
    "code",
    "Code",
    "Projects",
    "projects",
    "Developer",
    "dev",
    "repos",
    "git",
    "GitHub",
    "workspace",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    /// The folders to scan.
    pub roots: Vec<String>,
    /// False until the user saves folders: `roots` are then the suggestions.
    pub saved: bool,
    /// The usual code folders that exist, for the root pickers.
    pub suggested: Vec<String>,
}

fn file(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

/// The usual code folders under `home` that exist, one entry per directory
/// (on a case-insensitive disk `Work` and `work` are the same folder).
pub fn suggested(home: &Path) -> Vec<String> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for name in USUAL {
        let dir = home.join(name);
        if !dir.is_dir() {
            continue;
        }
        let Ok(real) = dir.canonicalize() else {
            continue;
        };
        if seen.contains(&real) {
            continue;
        }
        // Keep the name as the disk spells it.
        let shown = real
            .file_name()
            .map(|n| home.join(n))
            .unwrap_or_else(|| dir.clone());
        seen.push(real);
        out.push(shown.to_string_lossy().into_owned());
    }
    out
}

pub fn load(config_dir: &Path, home: &Path) -> Result<Settings, CliError> {
    let suggested = suggested(home);
    let path = file(config_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings {
                roots: suggested.clone(),
                saved: false,
                suggested,
            });
        }
        Err(e) => {
            return Err(CliError::failed(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    let stored: Stored = serde_json::from_str(&text).map_err(|e| {
        CliError::failed(format!(
            "{} is not valid settings ({e}); fix or delete it",
            path.display()
        ))
    })?;
    Ok(Settings {
        roots: stored
            .roots
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect(),
        saved: true,
        suggested,
    })
}

/// Check that every root is an absolute path to a directory, drop repeats,
/// and save the list.
pub fn save(config_dir: &Path, home: &Path, roots: &[String]) -> Result<Settings, CliError> {
    let roots = folders(roots)?;
    std::fs::create_dir_all(config_dir)
        .map_err(|e| CliError::failed(format!("cannot create {}: {e}", config_dir.display())))?;
    let path = file(config_dir);
    let tmp = config_dir.join(format!(".settings.json.{}", std::process::id()));
    let body = serde_json::to_string_pretty(&Stored {
        roots: roots.clone(),
    })
    .map_err(|e| CliError::failed(format!("cannot write the settings: {e}")))?;
    std::fs::write(&tmp, body + "\n")
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| CliError::failed(format!("cannot save {}: {e}", path.display())))?;
    load(config_dir, home)
}

/// Absolute paths to existing directories, without repeats, in order.
pub fn folders(roots: &[String]) -> Result<Vec<PathBuf>, CliError> {
    let mut out: Vec<PathBuf> = Vec::new();
    for root in roots {
        let path = PathBuf::from(root);
        if !path.is_absolute() {
            return Err(CliError::invalid(format!("{root} is not an absolute path")));
        }
        if !path.is_dir() {
            return Err(CliError::invalid(format!("{root} is not a folder")));
        }
        if !out.contains(&path) {
            out.push(path);
        }
    }
    Ok(out)
}
