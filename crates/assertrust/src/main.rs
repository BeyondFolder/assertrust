//! assertrust: catch changes that quietly weaken a test suite.
//!
//! How it works is described in `docs/architecture.md`. This binary is the entry point:
//! it parses the command line and maps the outcome to an exit code.

mod changes;
mod cli;
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
        Command::Check(_) => Err(not_implemented("check")),
        Command::Changes(args) => changes::run(&args),
        Command::Tests(args) => tests_command::run(&args),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
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

fn not_implemented(command: &str) -> String {
    format!("`{command}` is not implemented yet (see the roadmap in README.md)")
}
