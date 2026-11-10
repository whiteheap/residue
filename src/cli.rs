//! Command-line parsing. Hand-written to keep residue's dependency list to
//! one crate (libc).

use std::ffi::OsString;
use std::path::PathBuf;

use crate::removed::Reason;

pub const HELP: &str = "\
residue: remove a Git worktree only when nothing in it would be lost.

Usage:
  residue [scan] [PATH...]      List the linked worktrees under PATH (default .)
  residue explain PATH          Show every check for one worktree
  residue remove PATH...        Remove candidates, after you type yes
  residue remove --all PATH...  Remove every candidate under PATH
  residue restore [ENTRY]       List what residue can put back, or put back one
  residue stats                 Show how much disk residue has freed
  residue watch [ROOT...]       Clean up when a branch lands or disk is low
  residue service install|status|uninstall
                                Run `residue watch` from login; show it; stop it

Options:
  --json                   Print one JSON document
  --idle DURATION          Keep worktrees changed within it; remove: --trash too
  --all                    remove: every candidate under each PATH
  --yes                    remove: skip the prompt; only once the task is done
  --trash                  remove: into the trash; frees nothing until purged
  --no-trash               remove: delete, even with --idle
  --keep-trash DURATION    remove: purge trash entries older than this (7d);
                           new entries keep it, and it cannot shorten their own
  --keep-removed DURATION  remove: keep removal records at least this long (30d)
  --as-of TIME             remove: judge ages at TIME, a plan's generated_at
  --expect PLAN            remove --yes: act only while the plan is PLAN, as
                           a run without --yes reported it; else exit 4
  --removal-reason REASON  remove: watch-landed or watch-low-disk (set by watch)
  --only-if-upstream-gone  remove: only if local refs show the upstream gone
  --to PATH                restore: put the worktree at PATH
  --regenerable RULE       Treat ignored entries matching RULE as regenerable
  -h, --help               Show this help
  -V, --version            Show the version

DURATION is a whole number and m, h, d or w: 30m, 12h, 2d, 1w.

Examples:
  residue scan ~/src
  residue explain ~/src/app/.worktrees/fix-login
  residue remove ~/src/app/.worktrees/fix-login
  residue remove --all --yes --idle 2d ~/src    scheduled sweep, into the trash
  git worktree lock --reason \"resume later\" PATH  keep a worktree
  git config --add residue.regenerable .cache     a rule for one repository

Never removed: the main checkout; a locked or in-use worktree; one with
               staged, modified, untracked or non-regenerable ignored files
Kept:          branches, commits and the stash
Restore:       each deletion writes a removal record first; `residue restore`
               brings the worktree back for 30d, without its ignored files
Rules:         ~/.config/residue/regenerable, one RULE per line
Exit status:   0 ok, 1 error, 2 usage, 3 remove refused or skipped a path,
               or could not purge an expired trash entry, 4 remove --expect:
               the plan changed, so nothing was done
Details:       https://github.com/whiteheap/residue-cli#readme
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
            format!("{flag} {text:?}: use a whole number and a unit, such as 30m, 12h, 2d or 1w")
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
/// How long a removal record is kept when `--keep-removed` is not given.
pub const DEFAULT_KEEP_REMOVED: &str = "30d";

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
    pub keep_removed: Option<Span>,
    /// `remove --as-of TIME`, in Unix seconds: the time every age the run
    /// judges is measured at, such as the `generated_at` of the plan a
    /// person confirmed.
    pub as_of: Option<i64>,
    /// `remove --yes --expect PLAN`: the `plan` a run without `--yes`
    /// reported, which this run's plan must be before it acts.
    pub expect: Option<String>,
    pub regenerable: Vec<String>,
    /// `--only-if-upstream-gone`: remove only worktrees whose branch has an
    /// upstream that the local refs show as gone, checked at the plan, right
    /// before the move and right before the deletion.
    pub only_if_upstream_gone: bool,
    /// `--removal-reason`: why `residue watch` asked for this removal, as
    /// each removal record says it.
    pub removal_reason: Option<Reason>,
    /// `restore --to PATH`: where to recreate the worktree.
    pub to: Option<PathBuf>,
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

    pub fn keep_removed(&self) -> Span {
        self.keep_removed.clone().unwrap_or_else(|| Span {
            seconds: 30 * 86_400,
            text: DEFAULT_KEEP_REMOVED.to_string(),
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
    Stats(Options),
}

fn utf8(value: OsString, what: &str) -> Result<String, String> {
    value
        .into_string()
        .map_err(|_| format!("{what} must be valid UTF-8"))
}

pub fn parse(args: Vec<OsString>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    // No command word first: `scan`, whose first PATH `not_a_command`
    // checks in the loop below, after any options and their values.
    let mut implied = false;
    let kind = match args.peek().map(|a| a.to_str()) {
        None => return Ok(Command::Scan(Options::default())),
        Some(Some("-h" | "--help" | "help")) => return Ok(Command::Help),
        Some(Some("-V" | "--version" | "version")) => return Ok(Command::Version),
        Some(Some(word @ ("scan" | "explain" | "remove" | "restore" | "stats"))) => {
            let word = word.to_string();
            args.next();
            word
        }
        // An option, a PATH, or a word that is not UTF-8 (so no command).
        Some(_) => {
            implied = true;
            "scan".to_string()
        }
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
                    "--keep-removed" => {
                        let value = args
                            .next()
                            .ok_or("--keep-removed needs a DURATION (7d, 30d, 12w)")?;
                        options.keep_removed = Some(Span::parse(
                            "--keep-removed",
                            &utf8(value, "--keep-removed DURATION")?,
                        )?);
                        continue;
                    }
                    _ if text.starts_with("--keep-removed=") => {
                        options.keep_removed = Some(Span::parse(
                            "--keep-removed",
                            &text["--keep-removed=".len()..],
                        )?);
                        continue;
                    }
                    "--as-of" => {
                        let value = args
                            .next()
                            .ok_or("--as-of needs a TIME, such as 2026-11-09T12:00:00Z")?;
                        options.as_of = Some(as_of(&utf8(value, "--as-of TIME")?)?);
                        continue;
                    }
                    _ if text.starts_with("--as-of=") => {
                        options.as_of = Some(as_of(&text["--as-of=".len()..])?);
                        continue;
                    }
                    "--expect" => {
                        let value = args.next().ok_or(
                            "--expect needs a PLAN: the plan a `residue remove --json` without --yes reported",
                        )?;
                        set_expect(&mut options, &utf8(value, "--expect PLAN")?)?;
                        continue;
                    }
                    _ if text.starts_with("--expect=") => {
                        set_expect(&mut options, &text["--expect=".len()..])?;
                        continue;
                    }
                    "--removal-reason" => {
                        let value = args
                            .next()
                            .ok_or("--removal-reason needs watch-landed or watch-low-disk")?;
                        options.removal_reason =
                            Some(removal_reason(&utf8(value, "--removal-reason REASON")?)?);
                        continue;
                    }
                    _ if text.starts_with("--removal-reason=") => {
                        options.removal_reason =
                            Some(removal_reason(&text["--removal-reason=".len()..])?);
                        continue;
                    }
                    "--to" => {
                        let value = args.next().ok_or("--to needs a PATH")?;
                        options.to = Some(PathBuf::from(value));
                        continue;
                    }
                    _ if text.starts_with("--to=") => {
                        options.to = Some(PathBuf::from(&text["--to=".len()..]));
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
                    _ if implied && options.paths.is_empty() => {
                        not_a_command(text, |p| std::fs::symlink_metadata(p).is_ok())?;
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
        return Err("--only-if-upstream-gone takes worktree PATHs, not --all".into());
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
        if options.keep_removed.is_some() {
            return Err(
                "--keep-removed applies only to `residue remove` and `residue watch`".into(),
            );
        }
        if options.removal_reason.is_some() {
            return Err("--removal-reason applies only to `residue remove`".into());
        }
        if options.as_of.is_some() {
            return Err("--as-of applies only to `residue remove`".into());
        }
        if options.expect.is_some() {
            return Err(
                "--expect applies only to `residue remove --yes` and `residue service install`"
                    .into(),
            );
        }
    }
    if options.expect.is_some() && !options.yes {
        return Err(
            "--expect needs --yes: it names the plan a run without --yes reported, for the run that acts on it"
                .into(),
        );
    }
    if options.to.is_some() && kind != "restore" {
        return Err("--to applies only to `residue restore ENTRY`".into());
    }
    if kind == "restore" && (options.idle.is_some() || !options.regenerable.is_empty()) {
        return Err("`residue restore` takes only --json, --to PATH and an ENTRY".into());
    }
    if kind == "stats"
        && (options.idle.is_some() || !options.regenerable.is_empty() || !options.paths.is_empty())
    {
        return Err(
            "`residue stats` takes only --json: it reports every repository on this machine".into(),
        );
    }
    if kind == "restore" && options.to.is_some() && options.paths.is_empty() {
        return Err("--to needs an ENTRY to restore; run `residue restore` to list them".into());
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
        "stats" => Ok(Command::Stats(options)),
        "remove" if options.all && options.paths.is_empty() => Err(
            "`residue remove --all` needs at least one PATH to scan, such as ~/src".into(),
        ),
        "remove" if options.paths.is_empty() => {
            Err("`residue remove` needs at least one worktree PATH".into())
        }
        "remove" if options.all && options.yes && options.idle.is_none() => Err(
            "`residue remove --all --yes` needs --idle DURATION, such as --idle 2d: a paused session looks finished".into(),
        ),
        "remove" => Ok(Command::Remove(options)),
        _ => Ok(Command::Scan(options)),
    }
}

/// Every command, for the message a mistyped one gets.
const COMMANDS: &str = "scan, explain, remove, restore, stats, watch, service";

/// `residue WORD` with a WORD that is no command scans WORD as a PATH, and
/// so does the first PATH after options (`residue --json WORD`). A word no
/// PATH can mean, with no `/`, not an option, and with nothing by that name
/// here (`exists`), is a usage error: a mistyped command, or a command after
/// an option. So `residue clean` never exits 0 having scanned nothing.
/// `residue -- clean` still scans `clean`.
fn not_a_command(word: &str, exists: impl Fn(&str) -> bool) -> Result<(), String> {
    if word.is_empty() || word.starts_with('-') || word.contains('/') || exists(word) {
        return Ok(());
    }
    if COMMANDS.split(", ").any(|c| c == word) {
        return Err(format!(
            "{word} is a command, and goes first: residue {word} [OPTIONS]"
        ));
    }
    Err(format!(
        "{word} is not a command, and there is no folder named {word} here; commands: {COMMANDS}"
    ))
}

/// `--removal-reason`: only the reasons `residue watch` gives; a person's
/// own `remove` records `remove` or `sweep` by itself.
fn removal_reason(text: &str) -> Result<Reason, String> {
    match text {
        "watch-landed" => Ok(Reason::WatchLanded),
        "watch-low-disk" => Ok(Reason::WatchLowDisk),
        _ => Err(format!(
            "--removal-reason {text:?}: use watch-landed or watch-low-disk"
        )),
    }
}

/// `--as-of`: an RFC 3339 time with whole seconds, as `generated_at`
/// gives it, or with a `+HH:MM` offset.
fn as_of(text: &str) -> Result<i64, String> {
    crate::output::parse_rfc3339(text).ok_or_else(|| {
        format!(
            "--as-of {text:?}: use a time as generated_at gives it, such as 2026-11-09T12:00:00Z"
        )
    })
}

/// `--expect PLAN`: once, and 32 lowercase hex digits, as `plan` gives it.
fn set_expect(options: &mut Options, text: &str) -> Result<(), String> {
    if options.expect.is_some() {
        return Err("--expect can be given once".into());
    }
    if !crate::fingerprint::is_fingerprint(text) {
        return Err(format!(
            "--expect {text:?}: use the plan a `residue remove --json` without --yes reported, 32 lowercase hex digits"
        ));
    }
    options.expect = Some(text.to_string());
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn paths(command: Result<Command, String>) -> Vec<PathBuf> {
        match command {
            Ok(Command::Scan(options)) => options.paths,
            Ok(_) => panic!("not a scan"),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn a_mistyped_command_is_a_usage_error() {
        let none = |_: &str| false;
        let err = not_a_command("clean", none).unwrap_err();
        assert_eq!(
            err,
            "clean is not a command, and there is no folder named clean here; commands: scan, explain, remove, restore, stats, watch, service"
        );
        // A folder by that name, anything with a slash, an option or an
        // empty word is scanned as before.
        assert!(not_a_command("clean", |w| w == "clean").is_ok());
        for word in [
            ".", "..", "./clean", "src/app", "/tmp/x", "-x", "--json", "",
        ] {
            let exists = |w: &str| w == "." || w == "..";
            assert!(not_a_command(word, exists).is_ok(), "{word}");
        }
    }

    /// With no command first, the first PATH after options is checked the
    /// same way, and an option's value never is.
    #[test]
    fn the_first_path_after_options_is_checked_too() {
        let os = |w: &[&str]| w.iter().map(OsString::from).collect::<Vec<_>>();
        let err = |w: &[&str]| match parse(os(w)) {
            Err(e) => e,
            Ok(_) => panic!("{w:?} was accepted"),
        };
        assert!(err(&["--json", "clean"]).starts_with("clean is not a command"));
        assert!(err(&["--idle", "2d", "clean"]).starts_with("clean is not a command"));
        assert_eq!(
            err(&["--json", "scan"]),
            "scan is a command, and goes first: residue scan [OPTIONS]"
        );
        // A value that is no PATH: `--regenerable`'s RULE is not checked.
        match parse(os(&["--regenerable", "clean", "--json", "."])) {
            Ok(Command::Scan(o)) => {
                assert_eq!(o.paths, [PathBuf::from(".")]);
                assert_eq!(o.regenerable, ["clean"]);
            }
            _ => panic!("--regenerable clean was not taken as its RULE"),
        }
        // Only the first PATH, and none after `--`.
        assert_eq!(
            paths(parse(os(&["--json", ".", "clean"]))),
            [PathBuf::from("."), PathBuf::from("clean")]
        );
        assert_eq!(
            paths(parse(os(&["--json", "--", "clean"]))),
            [PathBuf::from("clean")]
        );
        // A command named first checks no PATH.
        assert_eq!(
            paths(parse(os(&["scan", "--json", "clean"]))),
            [PathBuf::from("clean")]
        );
    }

    /// `--expect PLAN` goes with `remove --yes`, once, with 32 lowercase hex
    /// digits; anything else is a usage error and nothing runs.
    #[test]
    fn expect_takes_one_plan_for_remove_yes() {
        let plan = "0123456789abcdef0123456789abcdef";
        let os = |w: &[&str]| w.iter().map(OsString::from).collect::<Vec<_>>();
        let expect = |w: &[&str]| match parse(os(w)) {
            Ok(Command::Remove(o)) => o.expect,
            Ok(_) => panic!("{w:?} is not a remove"),
            Err(e) => panic!("{w:?}: {e}"),
        };
        assert_eq!(
            expect(&["remove", "--yes", "--expect", plan, "w"]).as_deref(),
            Some(plan)
        );
        let joined = format!("--expect={plan}");
        assert_eq!(
            expect(&["remove", "--all", "--yes", "--idle", "2d", &joined, "."]).as_deref(),
            Some(plan)
        );
        assert_eq!(expect(&["remove", "--yes", "w"]), None);
        let err = |w: &[&str]| match parse(os(w)) {
            Err(e) => e,
            Ok(_) => panic!("{w:?} was accepted"),
        };
        assert_eq!(
            err(&["remove", "--expect", plan, "w"]),
            "--expect needs --yes: it names the plan a run without --yes reported, for the run that acts on it"
        );
        for command in ["scan", "explain", "restore", "stats"] {
            assert_eq!(
                err(&[command, "--expect", plan]),
                "--expect applies only to `residue remove --yes` and `residue service install`",
                "{command}"
            );
        }
        assert_eq!(
            err(&["remove", "--yes", "--expect", plan, "--expect", plan, "w"]),
            "--expect can be given once"
        );
        assert_eq!(
            err(&["remove", "--yes", "w", "--expect"]),
            "--expect needs a PLAN: the plan a `residue remove --json` without --yes reported"
        );
        let upper = plan.to_uppercase();
        for bad in [
            "",
            "none",
            upper.as_str(),
            &plan[1..],
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert_eq!(
                err(&["remove", "--yes", "--expect", bad, "w"]),
                format!(
                    "--expect {bad:?}: use the plan a `residue remove --json` without --yes reported, 32 lowercase hex digits"
                )
            );
        }
    }

    /// Every help line fits in 80 columns.
    #[test]
    fn help_fits_80_columns() {
        for line in HELP.lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
    }

    #[test]
    fn paths_after_the_separator_and_not_utf8_are_scanned() {
        let os = |w: &[&str]| w.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(paths(parse(os(&["--", "clean"]))), [PathBuf::from("clean")]);
        let odd = OsString::from(std::ffi::OsStr::from_bytes(b"repo\xff"));
        assert_eq!(
            paths(parse(vec![odd.clone(), OsString::from("--json")])),
            [PathBuf::from(odd)]
        );
        assert!(paths(parse(Vec::new())).is_empty());
    }
}
