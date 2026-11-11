//! The command-line interface: commands and flags, parsed with clap's derive API.
//!
//! The flags follow `docs/architecture.md`: the mode flags pick what to compare (step 1),
//! `check` runs the rules (step 5), `--format` and `--verbose` control its output (step 6),
//! `backtest` replays a branch's history with `check` (step 6), `tests` shows what parsing
//! finds in one file (steps 2–3), and `pairs` shows what matching pairs up (step 4).

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
    /// Replay a branch's history as CI would have checked it: each merged PR, each push
    Backtest(BacktestArgs),
    /// Show which commits and files a check would look at (for debugging)
    Changes(ChangesArgs),
    /// Show the tests and checks assertrust finds in a file, or a CI or test config's gate facts (for debugging)
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

    /// Also show weakening of tests the PR itself added (it can't fail the check)
    #[arg(long)]
    pub show_pr_added: bool,

    /// Exit with 1 when a finding is at least this sure (default: never, or `fail-on` in
    /// assertrust.toml)
    #[arg(long, value_enum, value_name = "LEVEL")]
    pub fail_on: Option<FailOn>,

    /// Show the test name, before/after snippets, the full commit message, and changed outcomes
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Debug, Args)]
pub struct BacktestArgs {
    /// The commits to replay: BASE..HEAD, those on HEAD's first-parent line after BASE
    #[arg(long, value_name = "RANGE", conflicts_with = "last")]
    pub range: Option<String>,

    /// Replay the last N commits on the current branch's first-parent line (default: 100, or
    /// the labels file's range)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub last: Option<u64>,

    /// A file with a verdict on each finding, to measure assertrust: prints how often it's
    /// right, and exits with 1 when a finding isn't labelled yet, one labelled right is no
    /// longer found, or a commit couldn't be checked
    #[arg(long, value_name = "FILE")]
    pub labels: Option<PathBuf>,

    /// Add new findings to the labels file, not labelled yet, and drop false alarms no longer
    /// found
    #[arg(long, requires = "labels")]
    pub update_labels: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = BacktestFormat::Terminal)]
    pub format: BacktestFormat,

    /// Also show weakening that was undone later in the same PR
    #[arg(long)]
    pub show_transient: bool,

    /// Also show weakening of tests the PR itself added (it can't fail the check)
    #[arg(long)]
    pub show_pr_added: bool,

    /// Show the test name, before/after snippets, the full commit message, and changed outcomes
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BacktestFormat {
    /// Short, human-readable findings, by PR and push
    Terminal,
    /// Machine-readable findings, for scripts and other tools
    Json,
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

/// The least sure finding that makes `assertrust check` fail (exit 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FailOn {
    /// Never fail: findings only warn
    Never,
    /// Fail on high-confidence findings
    High,
    /// Fail on high- and medium-confidence findings
    Medium,
    /// Fail on any finding
    Low,
}
