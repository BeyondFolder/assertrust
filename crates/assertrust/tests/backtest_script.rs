//! `backtest/run.sh` (`docs/development.md`, "Backtests"): fetching a labels file's repo and
//! its squash-merged PRs' commits, then replaying it. A local repo stands in for GitHub.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::Path;

use common::{Run, TestRepo};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../backtest/run.sh");
const BINARY: &str = env!("CARGO_BIN_EXE_assertrust");

const TESTS: &str = "\
it('refunds', () => {
  expect(a).toBe(1)
})
it('charges', () => {
  expect(c).toBe(3)
  expect(d).toBe(4)
})
";

/// A repo standing in for GitHub, with two squash merges on `main`: PR #2, whose own
/// commits are on `refs/pull/2/head` (its subject ends in a space), and `(#9)`, which has no
/// PR there. Returns it and the range to replay.
fn github() -> (TestRepo, String) {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TESTS);
    let base = repo.commit("Add tests");
    repo.branch("pr2");
    repo.write(
        "test/a.test.ts",
        &TESTS.replace("  expect(c).toBe(3)\n", ""),
    );
    repo.commit("Drop c");
    repo.write("README.md", "Docs\n");
    repo.commit("Docs");
    repo.switch("main");
    repo.git(&["merge", "--squash", "--quiet", "pr2"]);
    repo.git(&[
        "commit",
        "--quiet",
        "--cleanup=verbatim",
        "-m",
        "Trim charges (#2) ",
    ]);
    repo.git(&["update-ref", "refs/pull/2/head", "pr2"]);
    let current = fs::read_to_string(repo.path().join("test/a.test.ts")).unwrap();
    repo.write(
        "test/a.test.ts",
        &current.replace("it('refunds'", "it.skip('refunds'"),
    );
    repo.commit("Skip refunds (#9)");
    let head = repo.git(&["rev-parse", "HEAD"]);
    (repo, format!("{base}..{head}"))
}

/// A work dir holding `private/fixture.toml`, a labels file for `github`'s range.
fn work_dir(github: &TestRepo, range: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("private")).unwrap();
    fs::write(
        dir.path().join("private/fixture.toml"),
        format!(
            "repo = \"file://{}\"\nrange = \"{range}\"\n",
            github.path().display()
        ),
    )
    .unwrap();
    dir
}

/// Runs the script from `dir` on the fixture, with relative paths for the labels and clones.
fn run_script(github: &TestRepo, dir: &Path) -> Run {
    github.program_in(
        dir,
        SCRIPT,
        &["fixture"],
        &[
            ("ASSERTRUST_BIN", BINARY),
            ("ASSERTRUST_PRIVATE_LABELS", "private"),
            ("ASSERTRUST_BACKTEST_CLONES", "clones"),
        ],
    )
}

fn missing_prs(dir: &Path) -> String {
    fs::read_to_string(dir.join("clones/fixture/.git/assertrust-missing-prs")).unwrap()
}

#[test]
fn fetches_squash_merged_prs_and_finds_relative_paths() {
    let (github, range) = github();
    let dir = work_dir(&github, &range);
    let run = run_script(&github, dir.path());
    // The findings aren't labelled, so the replay exits with 1.
    assert_eq!(run.code, 1, "{}\n{}", run.stdout, run.stderr);
    assert!(run.stdout.starts_with("== fixture\n"), "{}", run.stdout);
    assert!(
        run.stdout
            .contains("Trim charges (#2) · squashed, 2 commits\n"),
        "{}",
        run.stdout
    );
    // GitHub has no PR #9: remembered, so it's asked about once.
    assert_eq!(missing_prs(dir.path()), "9\n");
}

#[test]
fn a_pr_that_failed_to_fetch_is_fetched_next_time() {
    let (github, range) = github();
    let dir = work_dir(&github, &range);
    let url = format!("file://{}", github.path().display());
    github.git(&[
        "clone",
        "--quiet",
        "--no-checkout",
        &url,
        dir.path().join("clones/fixture").to_str().unwrap(),
    ]);
    // GitHub can't be reached: no PR's commits come, and none is taken as missing.
    let clone = dir.path().join("clones/fixture");
    let clone = clone.to_str().unwrap();
    github.git(&[
        "-C",
        clone,
        "remote",
        "set-url",
        "origin",
        "file:///nowhere",
    ]);
    let run = run_script(&github, dir.path());
    assert!(
        run.stdout
            .contains("Trim charges (#2) · squashed, checked as one commit\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stderr.contains("warning: couldn't fetch PR #2: "),
        "{}",
        run.stderr
    );
    assert_eq!(missing_prs(dir.path()), "");
    // Back online, PR #2's commits are fetched.
    github.git(&["-C", clone, "remote", "set-url", "origin", &url]);
    let run = run_script(&github, dir.path());
    assert!(
        run.stdout
            .contains("Trim charges (#2) · squashed, 2 commits\n"),
        "{}\n{}",
        run.stdout,
        run.stderr
    );
    assert_eq!(missing_prs(dir.path()), "9\n");
}
