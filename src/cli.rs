//! Command-line parsing. Hand-written to keep residue's dependency list to
//! one crate (libc).

use std::ffi::OsString;
use std::path::PathBuf;

pub const HELP: &str = "\
residue: find what coding agents left behind on disk, and remove a Git
worktree only when nothing in it would be lost.

Usage:
  residue [scan] [PATH...]   Report the linked worktrees of each repository at
                             or under PATH (default: the current directory)
  residue explain PATH       Show every check for one linked worktree
  residue remove PATH...     Re-check, confirm, then `git worktree remove` each

Options:
  --json                Print one JSON document (scan, explain, remove)
  --yes                 remove: skip the prompt. Only for the session that owns
                        the worktree and knows its task is finished; every
                        check still runs
  --regenerable NAME    Also treat ignored entries named NAME (or *.ext) as
                        regenerable output that removal may delete
  -h, --help            Show this help
  -V, --version         Show the version

residue never removes the main checkout, a locked worktree, one a visible
process is using, or one with staged, modified or untracked files or with
ignored files outside the regenerable list. It never deletes branches, never
kills processes and never passes --force to Git.

Keep a worktree:  git worktree lock --reason \"resume later\" PATH
Config file:      ~/.config/residue/regenerable (one name or *.ext per line)
Exit status:      0 ok, 1 error, 2 usage, 3 remove refused or skipped a path
";

#[derive(Default)]
pub struct Options {
    pub paths: Vec<PathBuf>,
    pub json: bool,
    pub yes: bool,
    pub regenerable: Vec<String>,
}

pub enum Command {
    Help,
    Version,
    Scan(Options),
    Explain(Options),
    Remove(Options),
}

pub fn parse(args: Vec<OsString>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    let kind = match args.peek().and_then(|a| a.to_str()) {
        None => return Ok(Command::Scan(Options::default())),
        Some("-h" | "--help" | "help") => return Ok(Command::Help),
        Some("-V" | "--version" | "version") => return Ok(Command::Version),
        Some(word @ ("scan" | "explain" | "remove")) => {
            let word = word.to_string();
            args.next();
            word
        }
        Some(_) => "scan".to_string(),
    };
    let mut options = Options::default();
    let mut paths_only = false;
    while let Some(arg) = args.next() {
        if !paths_only {
            if let Some(text) = arg.to_str() {
                match text {
                    "--" => {
                        paths_only = true;
                        continue;
                    }
                    "-h" | "--help" => return Ok(Command::Help),
                    "--json" => {
                        options.json = true;
                        continue;
                    }
                    "--yes" | "-y" => {
                        options.yes = true;
                        continue;
                    }
                    "--regenerable" => {
                        let value = args.next().ok_or("--regenerable needs a NAME")?;
                        let value = value
                            .into_string()
                            .map_err(|_| "--regenerable NAME must be valid UTF-8")?;
                        options.regenerable.push(value);
                        continue;
                    }
                    _ if text.starts_with("--regenerable=") => {
                        options
                            .regenerable
                            .push(text["--regenerable=".len()..].to_string());
                        continue;
                    }
                    _ if text.starts_with('-') && text.len() > 1 => {
                        return Err(format!("unknown option {text}"));
                    }
                    _ => {}
                }
            }
        }
        options.paths.push(PathBuf::from(arg));
    }
    if options.yes && kind != "remove" {
        return Err("--yes applies only to `residue remove`".into());
    }
    match kind.as_str() {
        "explain" if options.paths.len() != 1 => {
            Err("`residue explain` takes exactly one worktree PATH".into())
        }
        "explain" => Ok(Command::Explain(options)),
        "remove" if options.paths.is_empty() => {
            Err("`residue remove` needs at least one worktree PATH".into())
        }
        "remove" => Ok(Command::Remove(options)),
        _ => Ok(Command::Scan(options)),
    }
}
