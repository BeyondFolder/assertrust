//! `assertrust changes`: which commits and files a check would look at (step 1, input).

mod common;

use common::{TestRepo, assertrust};

/// Runs `assertrust changes` with `args` and returns stdout, failing the test on a non-zero exit.
fn changes(repo: &TestRepo, args: &[&str]) -> String {
    let mut full = vec!["changes"];
    full.extend_from_slice(args);
    let run = repo.assertrust(&full);
    assert_eq!(run.code, 0, "changes {args:?} failed: {}", run.stderr);
    run.stdout
}

/// main: "Add a test" → feature: "Loosen the test". Returns the repo on `main`.
fn repo_with_feature_branch() -> TestRepo {
    let mut repo = TestRepo::new();
    repo.write("src/math.ts", "export const add = (a, b) => a + b;\n");
    repo.write("test/math.test.ts", "expect(add(1, 2)).toBe(3);\n");
    repo.commit("Add a test");
    repo.branch("feature");
    repo.write("test/math.test.ts", "expect(add(1, 2)).toBeTruthy();\n");
    repo.commit("Loosen the test");
    repo.switch("main");
    repo
}

#[test]
fn two_dots_check_each_commit_and_skip_merges() {
    let mut repo = repo_with_feature_branch();
    repo.git(&["tag", "start", "HEAD"]);
    repo.write("README.md", "# demo\n");
    repo.commit("Add a readme");
    repo.merge("feature", "Merge branch 'feature'");

    insta::assert_snapshot!(changes(&repo, &["--range", "start..main"]), @r"
    per-commit: start..main
    commits (2, 1 merge skipped):
      1e35131 Loosen the test
        M test         test/math.test.ts
      d0d8f31 Add a readme
        A other        README.md
    ");
}

#[test]
fn three_dots_ignore_what_main_did_after_the_branch_split_off() {
    let mut repo = repo_with_feature_branch();
    repo.write("src/app.ts", "export const app = 1;\n");
    repo.commit("Main moves on");

    insta::assert_snapshot!(changes(&repo, &["--range", "main...feature"]), @r"
    pr-style: main...feature
    compare a6c672c → 1e35131
      M test         test/math.test.ts
    commits (1):
      1e35131 Loosen the test
        M test         test/math.test.ts
    ");
}

#[test]
fn main_merged_into_the_branch_is_not_part_of_the_pr() {
    // Mailyond's forward_email branch had this shape: main merged in halfway through.
    let mut repo = repo_with_feature_branch();
    repo.write("src/app.ts", "export const app = 1;\n");
    repo.commit("Main moves on");
    repo.switch("feature");
    repo.merge("main", "Merge branch 'main' into feature");
    repo.write("test/app.test.ts", "expect(app).toBe(1);\n");
    repo.commit("Add another test");

    insta::assert_snapshot!(changes(&repo, &["--range", "main...feature"]), @r"
    pr-style: main...feature
    compare 61f8239 → 456d263
      A test         test/app.test.ts
      M test         test/math.test.ts
    commits (2, 1 merge skipped):
      1e35131 Loosen the test
        M test         test/math.test.ts
      456d263 Add another test
        A test         test/app.test.ts
    ");
}

#[test]
fn renamed_tests_keep_both_paths() {
    let mut repo = TestRepo::new();
    let test = "import { add } from '../src/math';\n\nexpect(add(1, 2)).toBe(3);\n";
    repo.write("test/old.test.ts", test);
    repo.commit("Add a test");
    repo.remove("test/old.test.ts");
    repo.write("test/math.test.ts", test);
    repo.commit("Rename the test");

    insta::assert_snapshot!(changes(&repo, &["--range", "HEAD~1..HEAD"]), @r"
    per-commit: HEAD~1..HEAD
    commits (1):
      231247e Rename the test
        R test         test/old.test.ts → test/math.test.ts
    ");
}

#[test]
fn a_test_renamed_out_of_the_suite_is_still_a_test() {
    let mut repo = repo_with_feature_branch();
    repo.remove("test/math.test.ts");
    repo.write("test/math.test.ts.disabled", "expect(add(1, 2)).toBe(3);\n");
    repo.commit("Park the test");

    insta::assert_snapshot!(changes(&repo, &["--range", "HEAD~1..HEAD"]), @r"
    per-commit: HEAD~1..HEAD
    commits (1):
      c7d9bc5 Park the test
        R test         test/math.test.ts → test/math.test.ts.disabled
    ");
}

#[test]
fn files_are_labelled_by_kind() {
    let mut repo = TestRepo::new();
    repo.commit("Start");
    for file in [
        ".github/workflows/ci.yml",
        "README.md",
        "package.json",
        "public/logo.png",
        "src/__tests__/parse.ts",
        "src/app.ts",
        "test/app.test.ts",
        "test/utils.ts",
        "vitest.config.ts",
    ] {
        repo.write(file, "x\n");
    }
    repo.commit("Add files");

    insta::assert_snapshot!(changes(&repo, &["--range", "HEAD~1..HEAD"]), @r"
    per-commit: HEAD~1..HEAD
    commits (1):
      5c1e747 Add files
        A gate         .github/workflows/ci.yml
        A other        README.md
        A gate         package.json
        A other        public/logo.png
        A test         src/__tests__/parse.ts
        A source       src/app.ts
        A test         test/app.test.ts
        A test-support test/utils.ts
        A gate         vitest.config.ts
    ");
}

#[test]
fn staged_shows_only_staged_files() {
    let repo = repo_with_feature_branch();
    repo.write("test/math.test.ts", "expect(add(1, 2)).toBeDefined();\n");
    repo.stage("test/math.test.ts");
    repo.write("src/math.ts", "export const add = (a, b) => b + a;\n");

    insta::assert_snapshot!(changes(&repo, &["--staged"]), @r"
    staged
    compare a6c672c → index
      M test         test/math.test.ts
    ");
}

#[test]
fn worktree_includes_unstaged_and_untracked_files() {
    let repo = repo_with_feature_branch();
    repo.write("test/math.test.ts", "expect(add(1, 2)).toBeDefined();\n");
    repo.write("test/new.test.ts", "expect(true).toBe(true);\n");

    insta::assert_snapshot!(changes(&repo, &["--worktree"]), @r"
    worktree
    compare a6c672c → files on disk
      M test         test/math.test.ts
      A test         test/new.test.ts
    ");
}

#[test]
fn worktree_compares_with_what_is_on_disk() {
    // What a coding agent leaves behind: a file moved without `git add`, and a file
    // dropped from the index while it's still on disk.
    let repo = repo_with_feature_branch();
    repo.remove("test/math.test.ts");
    repo.write("test/sum.test.ts", "expect(add(1, 2)).toBe(3);\n");
    repo.git(&["rm", "--quiet", "--cached", "src/math.ts"]);

    insta::assert_snapshot!(changes(&repo, &["--worktree"]), @r"
    worktree
    compare a6c672c → files on disk
      R test         test/math.test.ts → test/sum.test.ts
    ");
}

#[test]
fn paths_are_from_the_repo_root_when_run_from_a_subdirectory() {
    let repo = repo_with_feature_branch();
    repo.write("test/helper.ts", "export const one = 1;\n");
    repo.write("src/new.ts", "export const two = 2;\n");

    let run = repo.assertrust_in("test", &["changes", "--worktree"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    insta::assert_snapshot!(run.stdout, @r"
    worktree
    compare a6c672c → files on disk
      A source       src/new.ts
      A test-support test/helper.ts
    ");
}

#[test]
fn no_flag_compares_the_branch_with_main() {
    let repo = repo_with_feature_branch();
    repo.switch("feature");

    insta::assert_snapshot!(changes(&repo, &[]), @r"
    pr-style: main...HEAD (default)
    compare a6c672c → 1e35131
      M test         test/math.test.ts
    commits (1):
      1e35131 Loosen the test
        M test         test/math.test.ts
    ");
}

#[test]
fn no_flag_prefers_what_origin_head_points_to() {
    let mut repo = repo_with_feature_branch();
    let base = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["update-ref", "refs/remotes/origin/trunk", &base]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    repo.write("test/math.test.ts", "expect(add(1, 2)).toBe(3); // ok\n");
    repo.commit("Work on main");

    let output = changes(&repo, &[]);
    assert!(
        output.starts_with("pr-style: origin/trunk...HEAD (default)\n"),
        "{output}"
    );
    assert!(output.contains("Work on main"), "{output}");
}

#[test]
fn no_flag_skips_an_origin_head_that_points_nowhere() {
    // The remote renamed its default branch and `git fetch --prune` removed the old one,
    // but `origin/HEAD` still points to it.
    let repo = repo_with_feature_branch();
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
    ]);
    repo.switch("feature");

    let output = changes(&repo, &[]);
    assert!(
        output.starts_with("pr-style: main...HEAD (default)\n"),
        "{output}"
    );
}

#[test]
fn nothing_to_check_on_main_itself() {
    let repo = repo_with_feature_branch();

    insta::assert_snapshot!(changes(&repo, &[]), @r"
    pr-style: main...HEAD (default)
    compare a6c672c → a6c672c
    commits (0):
    nothing to check
    ");
}

#[test]
fn a_single_revision_is_not_a_range() {
    let repo = repo_with_feature_branch();
    let run = repo.assertrust(&["changes", "--range", "main"]);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stderr,
        "assertrust: expected BASE..HEAD or BASE...HEAD, got `main`\n"
    );
}

#[test]
fn an_unknown_revision_is_a_tool_error() {
    let repo = repo_with_feature_branch();
    let run = repo.assertrust(&["changes", "--range", "main..nope"]);
    assert_eq!(run.code, 2);
    assert_eq!(run.stderr, "assertrust: unknown revision `nope`\n");
}

#[test]
fn a_range_cannot_smuggle_in_git_options() {
    let repo = repo_with_feature_branch();
    let run = repo.assertrust(&["changes", "--range=--output=pwned..HEAD"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert!(!repo.path().join("pwned").exists());
}

#[test]
fn a_shallow_clone_gets_a_hint_when_the_merge_base_is_missing() {
    // Like `actions/checkout` without `fetch-depth: 0`: the commit where the branches
    // split isn't in the clone.
    let mut repo = repo_with_feature_branch();
    repo.write("src/app.ts", "export const app = 1;\n");
    let main = repo.commit("Main moves on");
    let feature = repo.git(&["rev-parse", "feature"]);
    repo.write(".git/shallow", &format!("{main}\n{feature}\n"));

    let run = repo.assertrust(&["changes", "--range", "main...feature"]);
    assert_eq!(run.code, 2);
    assert!(
        run.stderr
            .contains("have no common ancestor. This clone is shallow"),
        "{}",
        run.stderr
    );
}

#[test]
fn a_reader_that_stops_early_is_not_an_error() {
    let repo = repo_with_feature_branch();
    let run = repo.assertrust_into_closed_pipe(&["changes", "--range", "main...feature"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");
}

#[test]
fn outside_a_repo_is_a_tool_error() {
    let run = assertrust(&["changes", "--staged"]);
    assert_eq!(run.code, 2);
    assert_eq!(run.stderr, "assertrust: not inside a git repository\n");
}

#[test]
fn pr_mode_is_not_supported_yet() {
    let repo = repo_with_feature_branch();
    let run = repo.assertrust(&["changes", "--pr", "12"]);
    assert_eq!(run.code, 2);
    assert!(
        run.stderr.contains("--pr isn't supported yet"),
        "{}",
        run.stderr
    );
}
