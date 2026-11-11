//! `assertrust check` output (`docs/architecture.md`, step 6): which commits a PR-style
//! finding names, weakening undone later in the PR, allow comments and trailers, grouped
//! deletions, `fail-on` and its exit codes, `assertrust.toml` (and a PR loosening it), and
//! the JSON and GitHub formats.

mod common;

use common::TestRepo;

const TWO_CHECKS: &str = "it('refunds', () => {\n  expect(a).toBe(1)\n  expect(b).toBe(2)\n})\n";
const ONE_CHECK: &str = "it('refunds', () => {\n  expect(a).toBe(1)\n})\n";

fn check(repo: &TestRepo, args: &[&str]) -> (i32, String) {
    let mut all = vec!["check"];
    all.extend_from_slice(args);
    let run = repo.assertrust(&all);
    assert!(run.code == 0 || run.code == 1, "{}", run.stderr);
    (run.code, run.stdout)
}

/// A repo with `test/a.test.ts` holding a test with two checks on `main`, and a `feature`
/// branch checked out.
fn feature_branch() -> TestRepo {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TWO_CHECKS);
    repo.commit("Add a test");
    repo.branch("feature");
    repo
}

#[test]
fn a_pr_style_finding_names_every_commit_that_made_it() {
    // Across the whole PR the test also gained a check (c), so it was rewritten: medium.
    let mut repo = feature_branch();
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(a).toBe(1)\n  expect(b).toBe(2)\n  expect(c).toBe(3)\n})\n",
    );
    repo.commit("Add a third check");
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(b).toBe(2)\n  expect(c).toBe(3)\n})\n",
    );
    let first = repo.commit("Drop the first check");
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(c).toBe(3)\n})\n",
    );
    let second = repo.commit("Drop the second check");
    let (_, out) = check(&repo, &["--range", "main...HEAD", "-v"]);
    assert_eq!(
        out,
        format!(
            "test/a.test.ts:1  MEDIUM  check-removed\n\
             \x20 removed 2 checks, including expect(a).toBe(1) · {}, {}\n\
             \x20   test    refunds\n\
             \x20   before  expect(a).toBe(1)\n\
             \x20           expect(b).toBe(2)\n\
             \x20   after   expect(c).toBe(3)\n\
             \x20   commits {} Drop the first check\n\
             \x20           {} Drop the second check\n\
             \n\
             1 finding · warn-only (exit 0)\n",
            &first[..7],
            &second[..7],
            &first[..7],
            &second[..7],
        )
    );
}

#[test]
fn weakening_undone_later_shows_only_when_asked() {
    let mut repo = feature_branch();
    repo.write("test/a.test.ts", ONE_CHECK);
    let dropped = repo.commit("Drop a check");
    repo.write("test/a.test.ts", TWO_CHECKS);
    repo.commit("Put it back");
    assert_eq!(
        check(&repo, &["--range", "main...HEAD"]).1,
        "no findings · 1 undone later hidden (--show-transient shows them)\n"
    );
    assert_eq!(
        check(&repo, &["--range", "main...HEAD", "--show-transient"]).1,
        format!(
            "test/a.test.ts:1  HIGH  check-removed  (undone later)\n\
             \x20 removed expect(b).toBe(2) · {}\n\
             \n\
             no findings\n",
            &dropped[..7]
        )
    );
}

#[test]
fn an_allow_comment_added_with_the_change_waives_it() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TWO_CHECKS);
    let base = repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  // assertrust-allow: check-removed b is checked by the API tests now\n  expect(a).toBe(1)\n})\n",
    );
    let head = repo.commit("Move the b check");
    let (code, out) = check(
        &repo,
        &["--range", &format!("{base}..HEAD"), "--fail-on", "low"],
    );
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "test/a.test.ts:1  HIGH  check-removed  ALLOWED\n\
             \x20 removed expect(b).toBe(2) · {}\n\
             \x20 allowed: b is checked by the API tests now (comment)\n\
             \n\
             no findings · 1 allowed\n",
            &head[..7]
        )
    );
}

#[test]
fn an_old_allow_comment_does_not_waive_a_new_weakening() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  // assertrust-allow: check-removed c moved to the API tests\n  expect(a).toBe(1)\n  expect(b).toBe(2)\n})\n",
    );
    let base = repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  // assertrust-allow: check-removed c moved to the API tests\n  expect(a).toBe(1)\n})\n",
    );
    repo.commit("Drop the b check");
    let (_, out) = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.starts_with("test/a.test.ts:1  HIGH  check-removed\n"),
        "{out}"
    );
}

#[test]
fn an_allow_without_a_reason_does_not_count() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TWO_CHECKS);
    let base = repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(a).toBe(1) // assertrust-allow: check-removed\n})\n",
    );
    repo.commit("Drop the b check");
    let (_, out) = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.starts_with(
            "test/a.test.ts:1  HIGH  check-removed\n  removed expect(b).toBe(2); allow ignored: it gives no reason"
        ),
        "{out}"
    );
}

#[test]
fn a_commit_trailer_waives_a_deleted_test() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        &format!("{TWO_CHECKS}it('charges', () => {{ expect(c).toBe(3) }})\n"),
    );
    let base = repo.commit("Add tests");
    repo.write("test/a.test.ts", TWO_CHECKS);
    let head = repo.commit(
        "Drop the charges test\n\nCharging moved to the billing service.\n\nAssertrust-Allow: test-disabled charging lives in billing now",
    );
    let (_, out) = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert_eq!(
        out,
        format!(
            "test/a.test.ts  HIGH  test-disabled  ALLOWED\n\
             \x20 deleted: charges · {sha}\n\
             \x20 allowed: charging lives in billing now (trailer in {sha})\n\
             \n\
             no findings · 1 allowed\n",
            sha = &head[..7]
        )
    );
}

#[test]
fn allow_comments_work_in_helpers_and_ci_config() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/helpers.ts",
        "export function expectOk(res) {\n  expect(res.status).toBe(200)\n  expect(res.ok).toBe(true)\n}\n",
    );
    repo.write(
        "test/a.test.ts",
        "import { expectOk } from './helpers'\nit('refunds', () => { expectOk(refund()) })\n",
    );
    repo.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npm test\n",
    );
    let base = repo.commit("Add tests and CI");
    repo.write(
        "test/helpers.ts",
        "// assertrust-allow: check-removed ok is implied by status 200\nexport function expectOk(res) {\n  expect(res.status).toBe(200)\n}\n",
    );
    repo.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      # assertrust-allow: gate-loosened retried until the flaky DB test is fixed, #42\n      - run: npm test -- --retry 2\n",
    );
    repo.commit("Relax the helper and CI");
    let (_, out) = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.contains("allowed: ok is implied by status 200 (comment)"),
        "{out}"
    );
    assert!(
        out.contains("allowed: retried until the flaky DB test is fixed, #42 (comment)"),
        "{out}"
    );
    assert!(out.ends_with("no findings · 2 allowed\n"), "{out}");
}

#[test]
fn many_tests_deleted_from_a_file_are_one_finding() {
    let tests = |n: usize| -> String {
        (1..=n)
            .map(|i| format!("it('case {i}', () => {{ expect(f({i})).toBe({i}) }})\n"))
            .collect()
    };
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", &tests(6));
    repo.write("test/b.test.ts", &tests(4));
    let base = repo.commit("Add tests");
    repo.write("test/a.test.ts", &tests(1));
    repo.write("test/b.test.ts", &tests(1));
    let head = repo.commit("Drop most cases");
    let (_, out) = check(&repo, &["--range", &format!("{base}..HEAD")]);
    let sha = &head[..7];
    assert_eq!(
        out,
        format!(
            "test/a.test.ts  HIGH  test-disabled\n\
             \x20 deleted 5 tests, including case 2 · {sha}\n\
             test/b.test.ts  HIGH  test-disabled\n\
             \x20 deleted: case 2 · {sha}\n\
             test/b.test.ts  HIGH  test-disabled\n\
             \x20 deleted: case 3 · {sha}\n\
             test/b.test.ts  HIGH  test-disabled\n\
             \x20 deleted: case 4 · {sha}\n\
             \n\
             4 findings · warn-only (exit 0)\n"
        )
    );
}

/// A repo whose last commit removes a check (high) and swaps a matcher (low).
fn high_and_low() -> (TestRepo, String) {
    high_and_low_with(None)
}

/// The same, with `config` as `assertrust.toml` from the start.
fn high_and_low_with(config: Option<&str>) -> (TestRepo, String) {
    let mut repo = TestRepo::new();
    if let Some(config) = config {
        repo.write("assertrust.toml", config);
    }
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(a).toBe(1)\n  expect(b).toBe(obj)\n})\n",
    );
    let base = repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(b).toEqual(obj)\n})\n",
    );
    repo.commit("Rework the test");
    (repo, base)
}

#[test]
fn fail_on_sets_the_exit_code() {
    let (repo, base) = high_and_low();
    let range = format!("{base}..HEAD");
    let summary = |level: &str| {
        let (code, out) = check(&repo, &["--range", &range, "--fail-on", level]);
        (code, out.lines().last().unwrap_or_default().to_owned())
    };
    assert_eq!(
        summary("never"),
        (0, "2 findings · warn-only (exit 0)".to_owned())
    );
    assert_eq!(
        summary("high"),
        (
            1,
            "2 findings · 1 finding at high or above (exit 1)".to_owned()
        )
    );
    assert_eq!(
        summary("medium"),
        (
            1,
            "2 findings · 1 finding at medium or above (exit 1)".to_owned()
        )
    );
    assert_eq!(
        summary("low"),
        (
            1,
            "2 findings · 2 findings at low or above (exit 1)".to_owned()
        )
    );
}

#[test]
fn the_config_file_sets_fail_on_and_rules_and_flags_win() {
    let (repo, base) = high_and_low_with(Some(
        "fail-on = \"low\"\n\n[rules]\ncheck-removed = \"off\"\n",
    ));
    let range = format!("{base}..HEAD");
    let (code, out) = check(&repo, &["--range", &range]);
    assert_eq!(code, 1);
    assert!(
        out.starts_with("test/a.test.ts:2  LOW  matcher-swapped\n"),
        "{out}"
    );
    assert!(
        out.ends_with("1 finding · 1 finding at low or above (exit 1)\n"),
        "{out}"
    );
    // The flag wins.
    assert_eq!(
        check(&repo, &["--range", &range, "--fail-on", "never"]).0,
        0
    );
    // A worktree check reads the file on disk.
    repo.write("assertrust.toml", "fail_on = \"low\"\n");
    let run = repo.assertrust(&["check", "--worktree"]);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stderr,
        "assertrust: assertrust.toml: unknown key `fail_on`\n"
    );
}

#[test]
fn the_config_file_says_which_files_are_tests() {
    let mut repo = TestRepo::new();
    repo.write("checks/refund.ts", TWO_CHECKS);
    repo.write("assertrust.toml", "[files]\ntest = [\"checks/**/*.ts\"]\n");
    let base = repo.commit("Add a test");
    repo.write("checks/refund.ts", ONE_CHECK);
    repo.commit("Drop a check");
    let (_, out) = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.starts_with("checks/refund.ts:1  HIGH  check-removed\n"),
        "{out}"
    );
}

#[test]
fn json_lists_every_finding_with_its_status() {
    let mut repo = feature_branch();
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(a).toBe(2)\n})\n",
    );
    repo.commit("Change a, drop b");
    let run = repo.assertrust(&["check", "--range", "main...HEAD", "--format", "json"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    // SHAs are fixed by the harness, so the output is stable.
    insta::assert_snapshot!(run.stdout);
}

#[test]
fn github_format_annotates_and_writes_the_job_summary() {
    let (repo, base) = high_and_low();
    let range = format!("{base}..HEAD");
    let summary = tempfile::NamedTempFile::new().expect("temp file");
    let run = repo.assertrust_with_env(
        &[
            "check",
            "--range",
            &range,
            "--format",
            "github",
            "--fail-on",
            "high",
        ],
        &[("GITHUB_STEP_SUMMARY", summary.path())],
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert!(
        lines[0].starts_with(
            "::error file=test/a.test.ts,line=1,title=assertrust%3A check-removed (high)::removed expect(a).toBe(1) ("
        ),
        "{}",
        run.stdout
    );
    assert!(
        lines[1].starts_with(
            "::warning file=test/a.test.ts,line=2,title=assertrust%3A matcher-swapped (low)::"
        ),
        "{}",
        run.stdout
    );
    assert_eq!(
        lines[2],
        "assertrust: 2 findings · 1 finding at high or above (exit 1)"
    );
    let written = std::fs::read_to_string(summary.path()).expect("read summary");
    assert!(
        written.starts_with("<!-- assertrust -->\n### assertrust: 2 findings\n"),
        "{written}"
    );
    assert!(
        written
            .contains("| `test/a.test.ts:1` | check-removed (high) | removed expect(a).toBe(1) · "),
        "{written}"
    );
}

#[test]
fn a_new_test_loosened_before_landing_is_shown_but_cannot_fail() {
    // An agent writes a test, it fails, the next commit loosens it. Main never had the strong
    // version, so nothing it had got weaker, but the reviewer only sees the loose one.
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", ONE_CHECK);
    repo.commit("Add a test");
    repo.branch("feature");
    repo.write(
        "test/total.test.ts",
        "it('totals', () => {\n  expect(total()).toBe(30)\n})\n",
    );
    repo.commit("Test the total");
    repo.write(
        "test/total.test.ts",
        "it('totals', () => {\n  expect(total()).toBeGreaterThan(0)\n})\n",
    );
    let loosened = repo.commit("Make the total test pass");
    let (code, out) = check(&repo, &["--range", "main...HEAD", "--fail-on", "low"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "test/total.test.ts:2  MEDIUM  matcher-loosened  (before landing)\n\
             \x20 toBe(30) → toBeGreaterThan(0): now a range of values passes · {}\n\
             \n\
             no findings · 1 before landing (can't fail)\n",
            &loosened[..7]
        )
    );
}

#[test]
fn a_finding_names_only_commits_about_its_own_test() {
    // c1 skips `charges` and c2 puts it back; c3 skips `refunds`. Both skips say "now
    // skipped", but only c3 made the PR's finding, and c1's was undone.
    let tests = |refunds: &str, charges: &str| {
        format!(
            "it{refunds}('refunds', () => {{ expect(a).toBe(1) }})\nit{charges}('charges', () => {{ expect(b).toBe(2) }})\n"
        )
    };
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", &tests("", ""));
    repo.commit("Add tests");
    repo.branch("feature");
    repo.write("test/a.test.ts", &tests("", ".skip"));
    repo.commit("Skip charges");
    repo.write("test/a.test.ts", &tests("", ""));
    repo.commit("Unskip charges");
    repo.write("test/a.test.ts", &tests(".skip", ""));
    let skipped = repo.commit("Skip refunds");
    let (_, out) = check(&repo, &["--range", "main...HEAD"]);
    assert_eq!(
        out,
        format!(
            "test/a.test.ts:1  HIGH  test-disabled\n\
             \x20 now skipped (skip) · {}\n\
             \n\
             1 finding · 1 undone later hidden (--show-transient shows them) · warn-only (exit 0)\n",
            &skipped[..7]
        )
    );
}

#[test]
fn a_grouped_deletion_names_the_commits_that_deleted_its_tests() {
    let tests = |cases: std::ops::RangeInclusive<usize>| -> String {
        cases
            .map(|i| format!("it('case {i}', () => {{ expect(f({i})).toBe({i}) }})\n"))
            .collect()
    };
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", &tests(1..=6));
    repo.commit("Add tests");
    repo.branch("feature");
    repo.write("test/a.test.ts", &tests(3..=6));
    let first = repo.commit("Drop cases 1 and 2");
    repo.write("test/a.test.ts", &tests(5..=6));
    let second = repo.commit("Drop cases 3 and 4");
    let (_, out) = check(&repo, &["--range", "main...HEAD"]);
    assert_eq!(
        out,
        format!(
            "test/a.test.ts  HIGH  test-disabled\n\
             \x20 deleted 4 tests, including case 1 · {}, {}\n\
             \n\
             1 finding · warn-only (exit 0)\n",
            &first[..7],
            &second[..7]
        )
    );
}

#[test]
fn weakening_undone_in_a_renamed_file_or_test_is_not_before_landing() {
    // The file, or the test, was there before the PR under another name.
    for (path, renamed) in [
        ("test/b.test.ts", TWO_CHECKS.to_owned()),
        (
            "test/a.test.ts",
            TWO_CHECKS.replace("refunds", "refunds money"),
        ),
    ] {
        let mut repo = feature_branch();
        repo.remove("test/a.test.ts");
        repo.write(path, &renamed);
        repo.commit("Rename");
        repo.write(path, &renamed.replace("  expect(b).toBe(2)\n", ""));
        repo.commit("Drop a check");
        repo.write(path, &renamed);
        repo.commit("Put it back");
        assert_eq!(
            check(&repo, &["--range", "main...HEAD"]).1,
            "no findings · 1 undone later hidden (--show-transient shows them)\n",
            "{path}"
        );
    }
}

#[test]
fn a_pr_cannot_loosen_the_config_that_checks_it() {
    let mut repo = TestRepo::new();
    repo.write("assertrust.toml", "fail-on = \"high\"\n");
    repo.write("test/a.test.ts", TWO_CHECKS);
    let base = repo.commit("Add a test");
    repo.branch("feature");
    repo.write(
        "assertrust.toml",
        "fail-on = \"never\"\n\n[rules]\ncheck-removed = \"off\"\n",
    );
    repo.write("test/a.test.ts", ONE_CHECK);
    let head = repo.commit("Drop a check");
    // The config at the base applies: the check still fails.
    let (code, out) = check(&repo, &["--range", "main...HEAD"]);
    assert_eq!(code, 1);
    let sha = &head[..7];
    assert_eq!(
        out,
        format!(
            "test/a.test.ts:1  HIGH  check-removed\n\
             \x20 removed expect(b).toBe(2) · {sha}\n\
             assertrust.toml:1  HIGH  gate-loosened\n\
             \x20 lowered fail-on from high to never · {sha}\n\
             assertrust.toml:4  HIGH  gate-loosened\n\
             \x20 turned check-removed off: its findings are no longer reported · {sha}\n\
             \n\
             note: assertrust.toml differs from the one at {}, which this check uses: a change to it applies once it lands\n\
             3 findings · 3 findings at high or above (exit 1)\n",
            &base[..7]
        )
    );
}

#[test]
fn an_allow_comment_in_the_config_waives_its_change() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TWO_CHECKS);
    let base = repo.commit("Add a test");
    repo.write(
        "assertrust.toml",
        "[rules]\n# assertrust-allow: gate-loosened too noisy with our custom matchers\nmatcher-swapped = \"off\"\n",
    );
    let head = repo.commit("Turn off matcher-swapped");
    let (code, out) = check(
        &repo,
        &["--range", &format!("{base}..HEAD"), "--fail-on", "low"],
    );
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "assertrust.toml:3  HIGH  gate-loosened  ALLOWED\n\
             \x20 turned matcher-swapped off: its findings are no longer reported · {}\n\
             \x20 allowed: too noisy with our custom matchers (comment)\n\
             \n\
             note: assertrust.toml differs from the one at {}, which this check uses: a change to it applies once it lands\n\
             no findings · 1 allowed\n",
            &head[..7],
            &base[..7]
        )
    );
}

#[test]
fn an_allow_comment_like_one_in_another_test_still_counts() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('previews', () => {\n  // assertrust-allow: check-removed moved to MCP\n  expect(p).toBe(1)\n})\nit('replies', () => {\n  expect(a).toBe(1)\n  expect(b).toBe(2)\n})\n",
    );
    let base = repo.commit("Add tests");
    repo.write(
        "test/a.test.ts",
        "it('previews', () => {\n  // assertrust-allow: check-removed moved to MCP\n  expect(p).toBe(1)\n})\nit('replies', () => {\n  // assertrust-allow: check-removed moved to MCP\n  expect(a).toBe(1)\n})\n",
    );
    repo.commit("Move the b check");
    let (_, out) = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.starts_with("test/a.test.ts:5  HIGH  check-removed  ALLOWED\n"),
        "{out}"
    );
}

#[test]
fn a_file_broken_in_the_middle_of_a_pr_is_not_noted() {
    let mut repo = feature_branch();
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(a).toBe(1\n",
    );
    repo.commit("Work in progress");
    repo.write("test/a.test.ts", TWO_CHECKS);
    repo.commit("Fix it");
    assert_eq!(check(&repo, &["--range", "main...HEAD"]).1, "no findings\n");
}

#[test]
fn github_format_counts_findings_as_the_terminal_does() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", ONE_CHECK);
    repo.commit("Add a test");
    repo.branch("feature");
    repo.write(
        "test/total.test.ts",
        "it('totals', () => {\n  expect(total()).toBe(30)\n})\n",
    );
    repo.commit("Test the total");
    repo.write(
        "test/total.test.ts",
        "it('totals', () => {\n  expect(total()).toBeGreaterThan(0)\n})\n",
    );
    repo.commit("Make the total test pass");
    let summary = tempfile::NamedTempFile::new().expect("temp file");
    let run = repo.assertrust_with_env(
        &["check", "--range", "main...HEAD", "--format", "github"],
        &[("GITHUB_STEP_SUMMARY", summary.path())],
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.ends_with(
            "\nassertrust: no findings · 1 before landing (can't fail) · warn-only (exit 0)\n"
        ),
        "{}",
        run.stdout
    );
    let written = std::fs::read_to_string(summary.path()).expect("read summary");
    assert!(
        written.starts_with(
            "<!-- assertrust -->\n### assertrust: no findings · 1 before landing (can't fail)\n"
        ),
        "{written}"
    );
}

#[test]
fn changed_outcomes_need_v_even_when_allowed_or_undone() {
    // `totals` changes and changes back; `fees` changes with an allow comment.
    let tests = |total: u32, fees: &str| {
        format!(
            "it('totals', () => {{\n  expect(total()).toBe({total})\n}})\nit('fees', () => {{\n{fees}}})\n"
        )
    };
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", &tests(3, "  expect(fee()).toBe(1)\n"));
    repo.commit("Add tests");
    repo.branch("feature");
    repo.write("test/a.test.ts", &tests(4, "  expect(fee()).toBe(1)\n"));
    repo.commit("New prices");
    repo.write("test/a.test.ts", &tests(3, "  expect(fee()).toBe(1)\n"));
    repo.commit("Old prices again");
    repo.write(
        "test/a.test.ts",
        &tests(
            3,
            "  // assertrust-allow: outcome-changed the fee went up\n  expect(fee()).toBe(2)\n",
        ),
    );
    repo.commit("New fee");
    assert_eq!(
        check(&repo, &["--range", "main...HEAD", "--show-transient"]).1,
        "no findings\n"
    );
    let (_, verbose) = check(&repo, &["--range", "main...HEAD", "--show-transient", "-v"]);
    assert!(verbose.contains("outcome-changed  ALLOWED\n"), "{verbose}");
    assert!(
        verbose.contains("outcome-changed  (undone later)\n"),
        "{verbose}"
    );
}
