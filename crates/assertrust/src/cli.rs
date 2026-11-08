//! The command-line interface: commands and flags, parsed with clap's derive API.
//!
//! The flags follow `docs/architecture.md`: the mode flags pick what to compare (step 1),
//! `check` runs the rules (step 5), `--format` and `--verbose` control its output (step 6),
//! `tests` shows what parsing finds in one file (steps 2–3), and `pairs` shows what matching
//! pairs up (step 4).

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
    /// Show which commits and files a check would look at (for debugging)
    Changes(ChangesArgs),
    /// Show the tests and checks assertrust finds in a file (for debugging)
    Tests(TestsArgs),
    /// Show how tests and checks pair up before and after a change (for debugging)
    Pairs(PairsArgs),
}

#[derive(Debug, Args)]
pub struct ChangesArgs {
    #[command(flatten)]
    pub mode: Mode,
}

#[derive(Debug, Args)]
pub struct PairsArgs {
    #[command(flatten)]
    pub mode: Mode,
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

    /// Show the test name, before/after snippets, the full commit message, and changed outcomes
    #[arg(short, long)]
    pub verbose: bool,
}

/// What to compare. At most one of these can be given. With none, the current branch is
/// compared with the default branch, PR-style.
#[derive(Debug, Args)]
#[group(multiple = false)]
pub struct Mode {
    /// BASE..HEAD checks each commit; BASE...HEAD checks it like a PR (what HEAD adds since it
    /// split off from BASE)
    #[arg(long, value_name = "RANGE")]
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

impl Mode {
    /// The input layer's mode. `--pr` isn't supported until the GitHub Action exists.
    pub fn to_input(&self) -> Result<assertrust_git::Mode, String> {
        if self.pr.is_some() {
            return Err(
                "--pr isn't supported yet (it comes with the GitHub Action); use --range BASE...HEAD"
                    .to_owned(),
            );
        }
        Ok(if let Some(range) = &self.range {
            assertrust_git::Mode::Range(range.clone())
        } else if self.staged {
            assertrust_git::Mode::Staged
        } else if self.worktree {
            assertrust_git::Mode::WorkTree
        } else {
            assertrust_git::Mode::Default
        })
    }
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
pub struct TestsArgs {
    /// The file to read, e.g. test/forwarding.test.ts
    pub file: PathBuf,

    /// Read the file as it was at this commit (e.g. HEAD~1 or main) instead of from disk
    #[arg(long, value_name = "REV", conflicts_with = "staged")]
    pub rev: Option<String>,

    /// Read the staged version of the file (from the git index)
    #[arg(long)]
    pub staged: bool,
}
