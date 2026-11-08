//! `assertrust check`: the rules (step 5) on real git history, and how findings are printed
//! (step 6).
//!
//! The fixtures in `tests/fixtures/rules/` are written from scratch: one file before and
//! after a change that weakens it in each way the rules know.

mod common;

use common::TestRepo;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/rules/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).expect("read fixture")
}

/// A repo where `test/checkout.test.ts` was committed, then weakened in a second commit.
fn weakened_checkout() -> (TestRepo, String) {
    let mut repo = TestRepo::new();
    repo.write("test/checkout.test.ts", &fixture("checkout.before.test.ts"));
    let base = repo.commit("Add checkout tests");
    repo.write("test/checkout.test.ts", &fixture("checkout.after.test.ts"));
    repo.commit("Get checkout green");
    (repo, base)
}

#[test]
fn every_rule_on_one_change() {
    let (repo, base) = weakened_checkout();
    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
    // Findings only warn for now.
    assert_eq!(run.code, 0, "{}", run.stderr);
    insta::assert_snapshot!("every_rule", run.stdout);
}

#[test]
fn verbose_shows_the_test_the_code_and_the_commit() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(a).toBe(1);\n  expect(b).toBe(2);\n  expect(c).toBe(3);\n});\nit('keeps', () => {});\n",
    );
    let base = repo.commit("Add tests");
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => {\n  expect(a).toBe(1);\n});\nit.skip('keeps', () => {});\n",
    );
    let head = repo.commit("Trim the refund test\n\nThe other checks were flaky.");
    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD"), "-v"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let sha = &head[..7];
    assert_eq!(
        run.stdout,
        format!(
            "test/a.test.ts:1  HIGH  check-removed\n\
             \x20 removed 2 checks, including expect(b).toBe(2) · {sha}\n\
             \x20   test    refunds\n\
             \x20   before  expect(b).toBe(2)\n\
             \x20           expect(c).toBe(3)\n\
             \x20   commit  Trim the refund test\n\
             \n\
             \x20           The other checks were flaky.\n\
             \n\
             test/a.test.ts:4  HIGH  test-disabled\n\
             \x20 now skipped (skip) · {sha}\n\
             \x20   test    keeps\n\
             \x20   before  keeps []\n\
             \x20   after   keeps [skip]\n\
             \x20   commit  Trim the refund test\n\
             \n\
             \x20           The other checks were flaky.\n\
             \n\
             2 findings · warn-only (exit 0)\n"
        )
    );
}

#[test]
fn a_pr_style_check_reports_what_lands() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('a', () => { expect(a).toBe(1); expect(b).toBe(2) });\n",
    );
    repo.commit("Add a test");
    repo.branch("feature");
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(1) });\n");
    repo.commit("Drop a check");
    repo.write(
        "test/a.test.ts",
        "it.skip('a', () => { expect(a).toBe(1); expect(b).toBe(2) });\n",
    );
    repo.commit("Put the check back, skip the test");
    let run = repo.assertrust(&["check", "--range", "main...HEAD"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    // The check came back, so only the skip lands. Which commit did it comes later.
    assert_eq!(
        run.stdout,
        "test/a.test.ts:1  HIGH  test-disabled\n\
         \x20 now skipped (skip)\n\
         \n\
         1 finding · warn-only (exit 0)\n"
    );
}

#[test]
fn a_reformat_finds_nothing() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('a', async () => { if (ok) { expect(a).toEqual({ x: 1, y: 2 }) } await expect(p).resolves.toBe(1) })\n",
    );
    let base = repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('a', async () => {\n  if (ok) {\n    expect(a).toEqual({ y: 2, x: 1 });\n  }\n  await expect(p).resolves.toBe(1);\n});\n",
    );
    repo.commit("Reformat");
    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "no findings\n");
}

#[test]
fn a_test_moved_to_another_file_is_not_deleted() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('refunds', () => { expect(refund(1)).toBe(true) });\nit('keeps', () => {});\n",
    );
    let base = repo.commit("Add tests");
    repo.write("test/a.test.ts", "it('keeps', () => {});\n");
    repo.write(
        "test/b.test.ts",
        "it('refunds', () => { expect(refund(1)).toBe(true) });\n",
    );
    repo.commit("Move a test");
    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.stdout, "no findings\n");
}

#[test]
fn a_test_file_renamed_so_it_no_longer_runs() {
    let tests = "it('a', () => { expect(a).toBe(1) });\nit('b', () => { expect(b).toBe(2) });\n";
    let mut repo = TestRepo::new();
    repo.write("test/dca.test.ts", tests);
    repo.write("test/swap.test.ts", tests);
    let base = repo.commit("Add tests");
    repo.git(&["mv", "test/dca.test.ts", "test/dca.ts"]);
    repo.git(&["mv", "test/swap.test.ts", "test/swap.test.ts.disabled"]);
    let renamed = repo.commit("Park two test files");
    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let sha = &renamed[..7];
    assert_eq!(
        run.stdout,
        format!(
            "test/dca.test.ts  HIGH  test-disabled\n\
             \x20 renamed to test/dca.ts, which isn't a test file name: its 2 tests no longer run · {sha}\n\
             test/swap.test.ts  HIGH  test-disabled\n\
             \x20 renamed to test/swap.test.ts.disabled, which isn't a test file name: its 2 tests no longer run · {sha}\n\
             \n\
             2 findings · warn-only (exit 0)\n"
        )
    );
}

#[test]
fn uncommitted_changes() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(1) });\n");
    repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('a', () => { expect(a).toBe(1) });\nit.only('b', () => { expect(b).toBe(2) });\n",
    );
    let run = repo.assertrust(&["check", "--worktree"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "test/a.test.ts:2  HIGH  test-disabled\n\
         \x20 only added: the other test in this file won't run\n\
         \n\
         1 finding · warn-only (exit 0)\n"
    );
}

#[test]
fn a_file_that_does_not_parse_is_noted() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(1) })\n");
    let base = repo.commit("Add a test");
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(\n");
    repo.commit("Break it");
    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.starts_with("\nnote: test/a.test.ts ("),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("skipped, doesn't parse"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.ends_with("\nno findings\n"), "{}", run.stdout);
}

#[test]
fn nothing_to_check() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "it('a', () => {})\n");
    repo.commit("Add a test");
    let run = repo.assertrust(&["check", "--range", "HEAD..HEAD"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "nothing to check\n");
}

// Found in code review

#[test]
fn a_pr_style_check_covers_merges() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(1) });\n");
    repo.commit("Add a test");
    repo.branch("feature");
    repo.write("test/b.test.ts", "it('b', () => { expect(b).toBe(1) });\n");
    repo.commit("Add another test");
    repo.switch("main");
    repo.write("src/app.ts", "export const a = 1;\n");
    repo.commit("Change the app");
    repo.switch("feature");
    repo.merge("main", "Merge main");
    let run = repo.assertrust(&["check", "--range", "main...HEAD"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "no findings\n");
}

#[test]
fn a_range_of_merges_only_says_they_were_skipped() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(1) });\n");
    repo.commit("Add a test");
    repo.branch("feature");
    repo.write("src/app.ts", "export const a = 1;\n");
    repo.commit("Change the app");
    repo.switch("main");
    repo.write("src/other.ts", "export const b = 1;\n");
    repo.commit("Change something else");
    // Another branch already has both sides, so the range holds only the merge on main.
    repo.branch("other");
    let base = repo.merge("feature", "Merge feature into other");
    repo.switch("main");
    repo.merge("feature", "Merge feature");
    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "note: 1 merge commit not checked (a PR-style check, BASE...HEAD, covers what they changed)\n\
         nothing to check\n"
    );
}

#[test]
fn a_file_skipped_in_several_commits_is_noted_once() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "it('a', () => {})\n");
    repo.write("test/b.test.ts", "it('b', () => {})\n");
    let base = repo.commit("Add tests");
    repo.write("test/a.test.ts", "it('a', () => {\n");
    repo.write("test/b.test.ts", "it('b', () => {\n");
    repo.commit("Break both");
    repo.write("test/a.test.ts", "it('a', () => { x\n");
    repo.write("test/b.test.ts", "it('b', () => { y\n");
    repo.commit("Still broken");
    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let notes: Vec<&str> = run
        .stdout
        .lines()
        .filter(|l| l.starts_with("note:"))
        .collect();
    let mut unique = notes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(notes.len(), unique.len(), "{}", run.stdout);
    // Each file, once per version that doesn't parse.
    assert_eq!(notes.len(), 4, "{}", run.stdout);
}

#[test]
fn changed_outcomes_show_only_with_verbose() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('adds', () => { expect(add(1, 2)).toBe(3) });\n",
    );
    let base = repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('adds', () => { expect(add(1, 2)).toBe(4) });\n",
    );
    let head = repo.commit("Change the answer");
    let range = format!("{base}..HEAD");
    let run = repo.assertrust(&["check", "--range", &range]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "no findings · 1 changed outcome hidden (-v shows them)\n"
    );
    let run = repo.assertrust(&["check", "--range", &range, "-v"]);
    assert!(
        run.stdout.starts_with(&format!(
            "test/a.test.ts:1  LOW  outcome-changed\n  toBe(3) → toBe(4) · {}\n",
            &head[..7]
        )),
        "{}",
        run.stdout
    );
}
