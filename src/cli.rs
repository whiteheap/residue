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
  residue remove PATH...     Confirm, re-check, move aside, check again, then
                             `git worktree remove` each (or keep it in the trash)
  residue remove --all PATH...
                             The same for every candidate a scan of PATH finds
  residue restore [ENTRY]    List the trash of the repositories at or under the
                             current directory, or put one ENTRY back (its name
                             from the list, the worktree's name or branch, or a
                             path)
  residue watch [ROOT...]    Stay running; remove a worktree when its branch
                             lands or the disk runs low. Without a ROOT, the
                             usual code folders (`residue watch --help`)
  residue service install [ROOT...]
                             Run `residue watch` from login (launchd, systemd)

Options:
  --json                Print one JSON document (scan, explain, remove, restore)
  --idle DURATION       Keep any worktree changed within DURATION (30m, 12h,
                        2d, 1w): its files, its index and other files in its
                        Git directory, or its HEAD reflog. For scheduled runs,
                        where a paused session leaves no process inside.
                        With remove, it also turns on --trash
  --all                 remove: treat each PATH as a scan root and remove
                        every candidate under it. With --yes it needs --idle
  --yes                 remove: skip the prompt. Only for the session that owns
                        the worktree and knows its task is finished, or for a
                        scheduled `remove --all --idle`; every check still runs
  --trash               remove: move each worktree into its repository's trash
                        (GIT_COMMON_DIR/residue-trash) instead of deleting it,
                        so `residue restore` can put it back
  --no-trash            remove: delete, even with --idle
  --keep-trash DURATION remove: purge trash entries older than DURATION
                        (default 7d). Every remove run purges, after the same
                        confirmation; the purge is what frees the disk
  --only-if-upstream-gone
                        remove: keep any worktree whose branch's upstream is
                        not gone, as the local refs show it at the plan,
                        right before the move and right before the deletion
                        (remove never fetches). `residue watch` passes it
                        for a branch that landed
  --regenerable RULE    Also treat ignored entries matching RULE as regenerable
                        output that removal may delete. A NAME or *.ext matches
                        at any depth; a PATH with a slash is anchored at the
                        worktree root and may use * within a segment
                        (web/.tscache, .local-dev/*.lock)
  -h, --help            Show this help
  -V, --version         Show the version

residue never removes the main checkout, a locked worktree, one a visible
process is using, or one with staged, modified or untracked files or with
ignored files outside the regenerable list. It never deletes branches, never
kills processes and never passes --force to Git. Right before it deletes, it
moves the worktree aside and checks again; anything that changed since you
confirmed keeps it.

Keep a worktree:  git worktree lock --reason \"resume later\" PATH
Per repository:   git config --add residue.regenerable RULE
Config file:      ~/.config/residue/regenerable (one RULE per line, every repo)
Scheduled sweep:  residue remove --all --yes --idle 2d ~/src   (into the trash)
Exit status:      0 ok, 1 error, 2 usage, 3 remove refused or skipped a path

Rules apply only to ignored entries: an untracked file that is not ignored
always blocks. They come from the built-in list, the config file,
--regenerable, and residue.regenerable in Git config files: the
repository's, the global and the system file, including files chosen by
GIT_CONFIG_GLOBAL and GIT_CONFIG_SYSTEM. residue ignores residue.regenerable
set through the environment (git -c, GIT_CONFIG_COUNT, GIT_CONFIG_PARAMETERS,
GIT_CONFIG) and prints a note when it finds one, because an inherited
environment must not widen what residue deletes.
";

/// A length of time given on the command line, such as `--idle 2d` or
/// `--keep-trash 7d`.
#[derive(Clone)]
pub struct Span {
    pub seconds: i64,
    /// As the user wrote it, for messages: `2d`.
    pub text: String,
}

impl Span {
    /// A positive whole number and one unit: `30m`, `12h`, `2d`, `1w`.
    /// `flag` names the option in error messages.
    pub fn parse(flag: &str, text: &str) -> Result<Span, String> {
        let invalid = || {
            format!(
                "{flag} {text:?}: use a whole number and one unit, m (minutes), h (hours), d (days) or w (weeks), such as 30m, 12h, 2d or 1w"
            )
        };
        let unit = text.chars().last().ok_or_else(invalid)?;
        let scale: i64 = match unit {
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            'w' => 604_800,
            _ => return Err(invalid()),
        };
        let digits = &text[..text.len() - 1];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let n: i64 = digits.parse().map_err(|_| invalid())?;
        if n == 0 {
            return Err(if flag == "--idle" {
                format!(
                    "{flag} {text:?}: must be longer than zero; leave --idle out to skip the check"
                )
            } else {
                format!("{flag} {text:?}: must be longer than zero")
            });
        }
        let seconds = n
            .checked_mul(scale)
            .filter(|s| *s <= 100 * 366 * 86_400)
            .ok_or_else(|| format!("{flag} {text:?}: longer than 100 years"))?;
        Ok(Span {
            seconds,
            text: text.to_string(),
        })
    }
}

/// How long the trash keeps an entry when `--keep-trash` is not given.
pub const DEFAULT_KEEP_TRASH: &str = "7d";

#[derive(Default)]
pub struct Options {
    pub paths: Vec<PathBuf>,
    pub json: bool,
    pub yes: bool,
    pub all: bool,
    pub idle: Option<Span>,
    /// `--trash` (`Some(true)`) or `--no-trash` (`Some(false)`).
    pub trash: Option<bool>,
    pub keep_trash: Option<Span>,
    pub regenerable: Vec<String>,
    /// `--only-if-upstream-gone`: remove only worktrees whose branch has an
    /// upstream that the local refs show as gone, checked at the plan, right
    /// before the move and right before the deletion.
    pub only_if_upstream_gone: bool,
}

impl Options {
    /// `remove` moves worktrees into the trash with `--trash`, and with
    /// `--idle` unless `--no-trash` is given: an unattended run is
    /// reversible by default.
    pub fn trash(&self) -> bool {
        self.trash.unwrap_or(self.idle.is_some())
    }

    pub fn keep_trash(&self) -> Span {
        self.keep_trash.clone().unwrap_or_else(|| Span {
            seconds: 7 * 86_400,
            text: DEFAULT_KEEP_TRASH.to_string(),
        })
    }
}

pub enum Command {
    Help,
    Version,
    Scan(Options),
    Explain(Options),
    Remove(Options),
    Restore(Options),
}

fn utf8(value: OsString, what: &str) -> Result<String, String> {
    value
        .into_string()
        .map_err(|_| format!("{what} must be valid UTF-8"))
}

pub fn parse(args: Vec<OsString>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    let kind = match args.peek().and_then(|a| a.to_str()) {
        None => return Ok(Command::Scan(Options::default())),
        Some("-h" | "--help" | "help") => return Ok(Command::Help),
        Some("-V" | "--version" | "version") => return Ok(Command::Version),
        Some(word @ ("scan" | "explain" | "remove" | "restore")) => {
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
                    "--all" => {
                        options.all = true;
                        continue;
                    }
                    "--only-if-upstream-gone" => {
                        options.only_if_upstream_gone = true;
                        continue;
                    }
                    "--trash" => {
                        set_trash(&mut options, true)?;
                        continue;
                    }
                    "--no-trash" => {
                        set_trash(&mut options, false)?;
                        continue;
                    }
                    "--idle" => {
                        let value = args
                            .next()
                            .ok_or("--idle needs a DURATION (30m, 12h, 2d, 1w)")?;
                        options.idle =
                            Some(Span::parse("--idle", &utf8(value, "--idle DURATION")?)?);
                        continue;
                    }
                    _ if text.starts_with("--idle=") => {
                        options.idle = Some(Span::parse("--idle", &text["--idle=".len()..])?);
                        continue;
                    }
                    "--keep-trash" => {
                        let value = args
                            .next()
                            .ok_or("--keep-trash needs a DURATION (12h, 7d, 2w)")?;
                        options.keep_trash = Some(Span::parse(
                            "--keep-trash",
                            &utf8(value, "--keep-trash DURATION")?,
                        )?);
                        continue;
                    }
                    _ if text.starts_with("--keep-trash=") => {
                        options.keep_trash =
                            Some(Span::parse("--keep-trash", &text["--keep-trash=".len()..])?);
                        continue;
                    }
                    "--regenerable" => {
                        let value = args.next().ok_or("--regenerable needs a RULE")?;
                        options.regenerable.push(utf8(value, "--regenerable RULE")?);
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
    if options.all && kind != "remove" {
        return Err(
            "--all applies only to `residue remove`; `residue scan` already reports every worktree"
                .into(),
        );
    }
    if options.only_if_upstream_gone && kind != "remove" {
        return Err("--only-if-upstream-gone applies only to `residue remove`".into());
    }
    if options.only_if_upstream_gone && options.all {
        return Err(
            "--only-if-upstream-gone takes worktree PATHs, not --all: name each worktree, such as the paths `residue scan --json` reports with upstream_gone true".into(),
        );
    }
    if kind != "remove" {
        if let Some(value) = options.trash {
            return Err(format!(
                "{} applies only to `residue remove`",
                if value { "--trash" } else { "--no-trash" }
            ));
        }
        if options.keep_trash.is_some() {
            return Err("--keep-trash applies only to `residue remove`".into());
        }
    }
    if kind == "restore" && (options.idle.is_some() || !options.regenerable.is_empty()) {
        return Err("`residue restore` takes only --json and an ENTRY".into());
    }
    match kind.as_str() {
        "explain" if options.paths.len() != 1 => {
            Err("`residue explain` takes exactly one worktree PATH".into())
        }
        "explain" => Ok(Command::Explain(options)),
        "restore" if options.paths.len() > 1 => Err(
            "`residue restore` puts back one ENTRY at a time; run `residue restore` to list them"
                .into(),
        ),
        "restore" => Ok(Command::Restore(options)),
        "remove" if options.all && options.paths.is_empty() => Err(
            "`residue remove --all` needs at least one PATH to scan, such as ~/src".into(),
        ),
        "remove" if options.paths.is_empty() => {
            Err("`residue remove` needs at least one worktree PATH".into())
        }
        "remove" if options.all && options.yes && options.idle.is_none() => Err(
            "`residue remove --all --yes` needs --idle DURATION, such as --idle 2d: a session that is paused, not finished, has no process in its worktree, so an unattended sweep keeps only what changed recently".into(),
        ),
        "remove" => Ok(Command::Remove(options)),
        _ => Ok(Command::Scan(options)),
    }
}

fn set_trash(options: &mut Options, value: bool) -> Result<(), String> {
    match options.trash {
        Some(old) if old != value => Err("--trash and --no-trash cannot both be given".into()),
        _ => {
            options.trash = Some(value);
            Ok(())
        }
    }
}
