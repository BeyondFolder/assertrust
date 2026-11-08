//! What the input layer hands to the next steps.

use std::fmt;

/// The result of [`analyze`](crate::analyze): what to compare, and the commits behind it.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub kind: ModeKind,
    /// The range as given, e.g. `main...HEAD`. Empty for staged and worktree checks.
    pub label: String,
    /// The overall comparison: merge-base → head for PR-style checks, `HEAD` → index or
    /// files on disk for staged and worktree checks. `None` for per-commit checks.
    pub net: Option<Comparison>,
    /// One comparison per commit (parent → commit), oldest first. Empty for staged and
    /// worktree checks.
    pub steps: Vec<Step>,
    /// Merge commits in the range. They aren't compared, only listed.
    pub skipped_merges: Vec<Commit>,
}

impl Analysis {
    /// True when there is nothing to compare: no commits and no changed files.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.net.as_ref().is_none_or(|net| net.files.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    /// `BASE..HEAD`: every commit on its own.
    PerCommit,
    /// `BASE...HEAD` or no flag: what the branch adds since it split off, plus its commits.
    PrStyle,
    /// `HEAD` → the git index.
    Staged,
    /// `HEAD` → the files on disk, including untracked ones.
    WorkTree,
}

/// One commit and what it changed compared with its first parent.
#[derive(Debug, Clone)]
pub struct Step {
    pub commit: Commit,
    pub comparison: Comparison,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    /// The full commit message. Step 7 (the judge) reads it as the stated intent.
    pub message: String,
}

impl Commit {
    pub fn short_sha(&self) -> &str {
        short(&self.sha)
    }

    /// The first line of the message.
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }

    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// Two sides of a change, and the files that differ between them.
#[derive(Debug, Clone)]
pub struct Comparison {
    pub before: Rev,
    pub after: Rev,
    pub files: Vec<FileChange>,
}

/// One side of a comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rev {
    /// A commit, or the empty tree (the "before" side of a repo's first commit).
    Commit(String),
    /// The git index (staged changes).
    Index,
    /// The files on disk.
    WorkTree,
}

impl fmt::Display for Rev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit(sha) => f.write_str(short(sha)),
            Self::Index => f.write_str("index"),
            Self::WorkTree => f.write_str("files on disk"),
        }
    }
}

/// A file that differs between the two sides of a comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub kind: FileKind,
    pub status: ChangeStatus,
    /// `None` when the file was added.
    pub path_before: Option<String>,
    /// `None` when the file was deleted.
    pub path_after: Option<String>,
}

impl FileChange {
    /// The path after the change, or before it for a deleted file.
    pub fn path(&self) -> &str {
        self.path_after
            .as_deref()
            .or(self.path_before.as_deref())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl ChangeStatus {
    /// The same letter `git status` uses.
    pub fn letter(self) -> char {
        match self {
            Self::Added => 'A',
            Self::Modified => 'M',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
        }
    }
}

/// What a changed file is, which decides whether later steps read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileKind {
    /// A test file, e.g. `forwarding.test.ts`.
    Test,
    /// Helpers and setup used by tests, e.g. `test/utils.ts`.
    TestSupport,
    /// CI and test-runner config, e.g. `.github/workflows/ci.yml`, `vitest.config.ts`.
    Gate,
    /// Other code. Only read when something needs it (the judge, later).
    Source,
    /// Everything else (docs, images, …). Never read.
    Other,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::TestSupport => "test-support",
            Self::Gate => "gate",
            Self::Source => "source",
            Self::Other => "other",
        }
    }
}

/// The first 7 characters of a SHA, as git shows it.
pub(crate) fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}
