//! residue: find what coding agents left behind on disk, and remove a Git
//! worktree only when nothing in it would be lost.

mod activity;
mod cli;
mod commands;
mod discover;
mod fsinfo;
mod git;
mod inspect;
mod json;
mod output;
mod policy;
mod remove;
mod run;

use std::process::ExitCode;

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args_os().skip(1).collect()) {
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
    };
    ExitCode::from(code)
}
