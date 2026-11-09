//! residue: find what coding agents left behind on disk, and remove a Git
//! worktree only when nothing in it would be lost.

mod activity;
mod cli;
mod commands;
mod discover;
mod events;
mod fsinfo;
mod git;
mod hold;
mod inspect;
mod json;
mod jsonread;
mod output;
mod policy;
mod remove;
mod removed;
mod restore;
mod run;
mod service;
mod stats;
mod trash;
mod watch;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // `watch` and `service` parse their own arguments.
    match args.first().and_then(|a| a.to_str()) {
        Some("watch") => return ExitCode::from(watch::main(&args[1..])),
        Some("service") => return ExitCode::from(service::main(&args[1..])),
        _ => {}
    }
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("residue: {}", output::clean(&message));
            eprintln!("Run `residue --help` for usage.");
            return ExitCode::from(2);
        }
    };
    let code = match command {
        cli::Command::Help => {
            print!("{}", cli::HELP);
            0
        }
        cli::Command::Version => {
            println!("residue {}", commands::VERSION);
            0
        }
        cli::Command::Scan(options) => commands::scan(&options),
        cli::Command::Explain(options) => commands::explain(&options),
        cli::Command::Remove(options) => remove::remove(&options),
        cli::Command::Restore(options) => restore::command(&options),
        cli::Command::Stats(options) => stats::command(&options),
    };
    ExitCode::from(code)
}
