//! Test harness: throwaway git repos, and a way to run the built `assertrust` binary.
//!
//! Every git command (and the binary, which will shell out to git) runs isolated:
//! - the user's global and system git config are switched off, so commit signing, hooks
//!   and aliases on the developer's machine can't change or hang a test;
//! - git variables inherited from a surrounding git hook (`GIT_DIR`, …) are removed;
//! - author, committer and dates are fixed, so the same steps give the same commit SHAs
//!   on every machine and every run, which keeps snapshots stable.

// Each test file uses a different subset of these helpers.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// The built binary. Cargo sets this variable for integration tests.
const BINARY: &str = env!("CARGO_BIN_EXE_assertrust");

/// 2026-01-01T00:00:00Z. Each commit is one minute after the previous one.
const START_TIMESTAMP: u64 = 1_767_225_600;

/// Git variables a surrounding git hook may set, which would point git at the wrong repo.
const LEAKY_GIT_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
];

/// The result of running a command: exit code and captured output.
#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `assertrust` with `args` in an empty temp dir, outside any git repo (for `--help`,
/// `--version`, flag errors, and the "not inside a git repository" error).
pub fn assertrust(args: &[&str]) -> Run {
    let dir = tempfile::tempdir().expect("create temp dir");
    let parent = dir.path().parent().expect("temp dir has a parent");
    run(Command::new(BINARY)
        .args(args)
        .current_dir(dir.path())
        // Stop git from finding a repo in a parent directory.
        .env("GIT_CEILING_DIRECTORIES", parent))
}

/// A git repo in a temp dir, deleted when the value is dropped.
///
/// ```ignore
/// let mut repo = TestRepo::new();
/// repo.write("a.test.ts", "expect(x).toBe(1)");
/// let base = repo.commit("add test");
/// repo.branch("feature");
/// repo.write("a.test.ts", "expect(x).toBeTruthy()");
/// repo.commit("loosen it");
/// let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
/// ```
pub struct TestRepo {
    root: TempDir,
    commits: u64,
}

impl TestRepo {
    /// An empty repo on branch `main`.
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("create temp dir");
        fs::create_dir(root.path().join("home")).expect("create fake home");
        fs::write(root.path().join("home/.gitconfig"), "").expect("create empty git config");
        fs::create_dir(root.path().join("repo")).expect("create repo dir");

        let repo = Self { root, commits: 0 };
        repo.git(&["init", "--quiet", "--initial-branch=main"]);
        repo
    }

    /// The repo's working directory.
    pub fn path(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    /// Writes a file (creating parent directories), relative to the repo root.
    pub fn write(&self, file: &str, contents: &str) {
        let path = self.path().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write file");
    }

    /// Deletes a file, relative to the repo root.
    pub fn remove(&self, file: &str) {
        fs::remove_file(self.path().join(file)).expect("remove file");
    }

    /// Stages one file, without committing.
    pub fn stage(&self, file: &str) {
        self.git(&["add", "--", file]);
    }

    /// Stages everything and commits it. Returns the new commit's SHA.
    pub fn commit(&mut self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.with_next_date(&["commit", "--quiet", "--allow-empty", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Creates a branch at the current commit and switches to it.
    pub fn branch(&self, name: &str) {
        self.git(&["switch", "--quiet", "--create", name]);
    }

    /// Switches to an existing branch.
    pub fn switch(&self, name: &str) {
        self.git(&["switch", "--quiet", name]);
    }

    /// Merges `branch` into the current branch with a merge commit (like GitHub's
    /// "Create a merge commit"). Returns the merge commit's SHA.
    pub fn merge(&mut self, branch: &str, message: &str) -> String {
        self.with_next_date(&["merge", "--quiet", "--no-ff", "-m", message, branch]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Runs git in the repo and returns its trimmed stdout. Panics if git fails.
    pub fn git(&self, args: &[&str]) -> String {
        let run = self.try_git(args);
        assert_eq!(run.code, 0, "git {args:?} failed:\n{}", run.stderr);
        run.stdout.trim_end().to_owned()
    }

    /// Runs git in the repo and returns the result, even if git fails.
    pub fn try_git(&self, args: &[&str]) -> Run {
        run(self.isolated(Command::new("git").args(args)))
    }

    /// Runs `assertrust` in the repo with the same isolation as git.
    pub fn assertrust(&self, args: &[&str]) -> Run {
        run(self.isolated(Command::new(BINARY).args(args)))
    }

    /// Runs `assertrust` in the repo with extra environment variables.
    pub fn assertrust_with_env(&self, args: &[&str], env: &[(&str, &std::path::Path)]) -> Run {
        let mut command = Command::new(BINARY);
        command.args(args);
        self.isolated(&mut command);
        run(command.envs(env.iter().copied()))
    }

    /// Runs `assertrust` from a subdirectory of the repo (relative to the repo root).
    pub fn assertrust_in(&self, subdir: &str, args: &[&str]) -> Run {
        run(self
            .isolated(Command::new(BINARY).args(args))
            .current_dir(self.path().join(subdir)))
    }

    /// Runs `assertrust` with its stdout closed before it writes anything, like
    /// `assertrust … | head -0`.
    pub fn assertrust_into_closed_pipe(&self, args: &[&str]) -> Run {
        let mut child = self
            .isolated(Command::new(BINARY).args(args))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start process");
        drop(child.stdout.take());
        to_run(child.wait_with_output().expect("wait for process"))
    }

    /// Runs a committing git command with the next fixed date.
    fn with_next_date(&mut self, args: &[&str]) {
        self.commits += 1;
        let date = format!("@{} +0000", START_TIMESTAMP + 60 * self.commits);
        let run = run(self
            .isolated(Command::new("git").args(args))
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date));
        assert_eq!(run.code, 0, "git {args:?} failed:\n{}", run.stderr);
    }

    fn isolated<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        let home = self.root.path().join("home");
        for var in LEAKY_GIT_VARS {
            cmd.env_remove(var);
        }
        // On GitHub Actions, a test must not write to the real job summary.
        cmd.env_remove("GITHUB_STEP_SUMMARY");
        cmd.current_dir(self.path())
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.test")
            .env("GIT_COMMITTER_NAME", "Test Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.test")
    }
}

impl Default for TestRepo {
    fn default() -> Self {
        Self::new()
    }
}

fn run(cmd: &mut Command) -> Run {
    to_run(cmd.output().expect("start process"))
}

fn to_run(output: Output) -> Run {
    Run {
        code: output
            .status
            .code()
            .expect("process was killed by a signal"),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}
