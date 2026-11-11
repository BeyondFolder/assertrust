//! assertrust: catch changes that quietly weaken a test suite.
//!
//! How it works is described in `docs/architecture.md`. This binary is the entry point:
//! it parses the command line and maps the outcome to an exit code.

mod allow;
mod changes;
mod check_command;
mod cli;
mod config;
mod github;
mod json;
mod load;
mod modules;
mod output;
mod pairs_command;
mod report;
mod tests_command;

use std::io::{self, Write as _};
use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};

/// Exit code when assertrust itself failed (bad flags, no git, bad range), as opposed to
/// "found something" (1). See `docs/architecture.md`, step 6. clap uses the same code for
/// usage errors, so a typo in a flag also exits with 2.
const EXIT_TOOL_ERROR: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Check(args) => check_command::run(&args),
        Command::Changes(args) => changes::run(&args).map(|()| ExitCode::SUCCESS),
        Command::Tests(args) => tests_command::run(&args).map(|()| ExitCode::SUCCESS),
        Command::Pairs(args) => pairs_command::run(&args).map(|()| ExitCode::SUCCESS),
    };

    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("assertrust: {message}");
            ExitCode::from(EXIT_TOOL_ERROR)
        }
    }
}

/// Writes `text` to stdout. A reader that stops early (`assertrust changes | head`) isn't an
/// error: `print!` would panic there, so output goes through this instead.
pub(crate) fn print_stdout(text: &str) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(err) if err.kind() != io::ErrorKind::BrokenPipe => {
            Err(format!("couldn't write the output: {err}"))
        }
        _ => Ok(()),
    }
}
