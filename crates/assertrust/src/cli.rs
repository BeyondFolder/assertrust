//! The command-line interface: commands and flags, parsed with clap's derive API.
//!
//! The flags follow `docs/architecture.md`: the mode flags pick what to compare (step 1),
//! `--format` and `--verbose` control output (step 6).

#![expect(
    dead_code,
    reason = "every flag is parsed now; the commands that read them land in later PRs"
)]

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Catch changes that quietly weaken your test suite.
#[derive(Debug, Parser)]
#[command(name = "assertrust", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check a change for weakened tests
    Check(CheckArgs),
    /// Print the tests and checks found in one file, as JSON (for debugging)
    Ir(IrArgs),
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    #[command(flatten)]
    pub mode: Mode,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Terminal)]
    pub format: Format,

    /// Also show weakening that was undone later in the same PR
    #[arg(long)]
    pub show_transient: bool,

    /// Show the test name, before/after snippets and the full commit message
    #[arg(short, long)]
    pub verbose: bool,
}

/// What to compare. At most one of these can be given.
#[derive(Debug, Args)]
#[group(multiple = false)]
pub struct Mode {
    /// Check each commit in a range, e.g. origin/main..HEAD
    #[arg(long, value_name = "BASE..HEAD")]
    pub range: Option<String>,

    /// Check a GitHub pull request (merge-base to head)
    #[arg(long, value_name = "NUMBER")]
    pub pr: Option<u64>,

    /// Check staged changes (HEAD to the git index), for pre-commit hooks
    #[arg(long)]
    pub staged: bool,

    /// Check uncommitted changes (HEAD to files on disk), for coding-agent hooks
    #[arg(long)]
    pub worktree: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Short, human-readable findings
    Terminal,
    /// Machine-readable findings, for scripts and other tools
    Json,
    /// GitHub Actions annotations
    Github,
}

#[derive(Debug, Args)]
pub struct IrArgs {
    /// The test file to read
    pub file: PathBuf,

    /// Read the file as it was at this git revision instead of from disk
    #[arg(long, value_name = "REV")]
    pub rev: Option<String>,
}
