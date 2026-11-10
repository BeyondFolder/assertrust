//! Helpers in other files (`docs/architecture.md`, step 3, "Linked view"): a test file that
//! didn't change is checked again when a helper file it calls did, and imports are
//! followed only into test folders.

mod common;

use common::TestRepo;

const STRICT: &str = "export function expectOk(res) {\n  expect(res.status).toBe(200)\n  expect(res.body.ok).toBe(true)\n}\n";
const WEAKER: &str = "export function expectOk(res) {\n  expect(res.status).toBe(200)\n}\n";

/// Two tests calling `expectOk` from `helper` (an import path, like `./helpers`).
fn tests_importing(helper: &str) -> String {
    format!(
        "import {{ expectOk }} from '{helper}'\n\n\
         it('refunds', () => {{ expectOk(refund()) }})\n\
         it('charges', () => {{ expectOk(charge()) }})\n"
    )
}

/// A repo with `test/refund.test.ts` calling `expectOk` from `test/helpers.ts`, committed.
fn repo_with_helpers() -> (TestRepo, String) {
    let mut repo = TestRepo::new();
    repo.write("test/refund.test.ts", &tests_importing("./helpers"));
    repo.write("test/helpers.ts", STRICT);
    let base = repo.commit("Add tests");
    (repo, base)
}

fn check(repo: &TestRepo, args: &[&str]) -> String {
    let mut all = vec!["check"];
    all.extend_from_slice(args);
    let run = repo.assertrust(&all);
    assert_eq!(run.code, 0, "{}", run.stderr);
    run.stdout
}

#[test]
fn a_weakened_helper_file_is_reported_for_the_tests_that_call_it() {
    let (mut repo, base) = repo_with_helpers();
    repo.write("test/helpers.ts", WEAKER);
    let head = repo.commit("Relax expectOk");
    assert_eq!(
        check(&repo, &["--range", &format!("{base}..HEAD")]),
        format!(
            "test/helpers.ts:3  HIGH  check-removed\n\
             \x20 removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests) · {}\n\
             \n\
             1 finding · warn-only (exit 0)\n",
            &head[..7]
        )
    );
}

#[test]
fn staged_and_unsaved_helper_changes_are_checked_too() {
    let expected = "test/helpers.ts:3  HIGH  check-removed\n\
                    \x20 removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)\n\
                    \n\
                    1 finding · warn-only (exit 0)\n";
    let (repo, _) = repo_with_helpers();
    repo.write("test/helpers.ts", WEAKER);
    assert_eq!(check(&repo, &["--worktree"]), expected);
    repo.stage("test/helpers.ts");
    assert_eq!(check(&repo, &["--staged"]), expected);
}

#[test]
fn helpers_passed_on_through_an_index_file_are_followed() {
    // ESM TypeScript names the compiled file: `./support/index.js` is `index.ts`.
    let mut repo = TestRepo::new();
    repo.write(
        "test/refund.test.ts",
        &tests_importing("./support/index.js"),
    );
    repo.write("test/support/index.ts", "export * from './http.js'\n");
    repo.write("test/support/http.ts", STRICT);
    let base = repo.commit("Add tests");
    repo.write("test/support/http.ts", WEAKER);
    repo.commit("Relax expectOk");
    let out = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.starts_with(
            "test/support/http.ts:3  HIGH  check-removed\n  removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"
        ),
        "{out}"
    );
}

#[test]
fn helpers_outside_test_folders_are_not_followed() {
    let mut repo = TestRepo::new();
    repo.write("test/refund.test.ts", &tests_importing("../src/expect"));
    repo.write("src/expect.ts", STRICT);
    let base = repo.commit("Add tests");
    repo.write("src/expect.ts", WEAKER);
    repo.commit("Relax expectOk");
    assert_eq!(
        check(&repo, &["--range", &format!("{base}..HEAD")]),
        "no findings\n"
    );
}

#[test]
fn a_helper_file_that_stops_parsing_skips_the_tests_that_call_it() {
    let (mut repo, base) = repo_with_helpers();
    repo.write(
        "test/helpers.ts",
        "export function expectOk(res) {\n  expect(\n",
    );
    let head = repo.commit("Break expectOk");
    let out = check(&repo, &["--range", &format!("{base}..HEAD")]);
    let sha = &head[..7];
    assert!(
        out.trim_start().starts_with(&format!(
            "note: test/refund.test.ts: skipped, its helper file test/helpers.ts ({sha}) doesn't parse"
        )),
        "{out}"
    );
    assert!(out.ends_with("no findings\n"), "{out}");
}

#[test]
fn a_helper_change_that_only_moves_lines_loads_nothing() {
    let (mut repo, base) = repo_with_helpers();
    repo.write("test/helpers.ts", &format!("// Shared checks.\n\n{STRICT}"));
    repo.commit("Document expectOk");
    let run = repo.assertrust(&["pairs", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("no test files changed"),
        "{}",
        run.stdout
    );
}

#[test]
fn pairs_shows_the_helper_each_check_runs_through() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/refund.test.ts",
        "function expectStatus(res, status) {\n  expect(res.status).toBe(status)\n}\n\n\
         it('refunds', () => {\n  expectStatus(refund(), 200)\n})\n",
    );
    let base = repo.commit("Add tests");
    repo.write(
        "test/refund.test.ts",
        "function expectStatus(res, status) {\n  expect(res.status).toBe(status)\n}\n\n\
         it('refunds', () => {\n  expectStatus(refund(), 202)\n})\n",
    );
    repo.commit("Accept 202");
    let run = repo.assertrust(&["pairs", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    insta::assert_snapshot!(run.stdout.lines().skip(2).collect::<Vec<_>>().join("\n"), @r"
      test/refund.test.ts: 1 test paired
        same      refunds
          changed       2  expect(res.status).toBe(200)  via expectStatus(refund(), 200)
                   →    2  expect(res.status).toBe(202)  via expectStatus(refund(), 202)
    ");
}
