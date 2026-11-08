//! assertrust: catch changes that quietly weaken a test suite.
//!
//! How it works is described in `docs/architecture.md`. This binary is the entry point:
//! it parses the command line and maps the outcome to an exit code.

mod cli;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};

/// Exit code when assertrust itself failed (bad flags, no git, bad range), as opposed to
/// "found something" (1). See `docs/architecture.md`, step 6. clap uses the same code for
/// usage errors, so a typo in a flag also exits with 2.
const EXIT_TOOL_ERROR: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Check(_) => not_implemented("check"),
        Command::Ir(_) => not_implemented("ir"),
    }
}

fn not_implemented(command: &str) -> ExitCode {
    eprintln!("assertrust: `{command}` is not implemented yet (see the roadmap in README.md)");
    ExitCode::from(EXIT_TOOL_ERROR)
}
