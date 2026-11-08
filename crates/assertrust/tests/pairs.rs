//! `assertrust pairs`: how tests and checks pair up before and after a change (step 4).
//!
//! The fixtures in `tests/fixtures/matching/` are written from scratch: one file before and
//! after a change that has one of each case.

mod common;

use common::TestRepo;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/matching/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).expect("read fixture")
}

/// Commits `before`, then `after`, as `test/billing.test.ts`, and runs `pairs` on the
/// second commit.
fn pairs_between(before: &str, after: &str) -> String {
    let mut repo = TestRepo::new();
    repo.write("test/billing.test.ts", before);
    repo.commit("Add billing tests");
    repo.write("test/billing.test.ts", after);
    repo.commit("Rework billing tests");
    let run = repo.assertrust(&["pairs", "--range", "HEAD~1..HEAD"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    run.stdout
}

#[test]
fn every_kind_of_pairing() {
    let output = pairs_between(
        &fixture("billing.before.test.ts"),
        &fixture("billing.after.test.ts"),
    );
    insta::assert_snapshot!("every_kind", output);
}

#[test]
fn an_unchanged_file_has_nothing_to_show() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(1) })\n");
    let base = repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('a', () => {\n  expect(a).toBe(1);\n});\n",
    );
    repo.commit("Reformat");
    let run = repo.assertrust(&["pairs", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .contains("test/a.test.ts: 1 test paired (1 unchanged)\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_test_moved_to_another_file() {
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
    let run = repo.assertrust(&["pairs", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .contains("    same      refunds\n           →  test/b.test.ts: refunds\n"),
        "{}",
        run.stdout
    );
    // The file it moved to says where its test came from.
    assert!(
        run.stdout
            .contains("  test/b.test.ts: 1 moved in from another file\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_test_file_renamed_out_of_the_suite() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/dca.test.ts",
        "it('swaps', () => { expect(swap(1)).toBe(2) });\n",
    );
    let base = repo.commit("Add a test");
    repo.git(&["mv", "test/dca.test.ts", "test/dca.ts"]);
    repo.commit("Park it");
    let run = repo.assertrust(&["pairs", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains(
            "  test/dca.test.ts → test/dca.ts (not a test file name): 1 deleted\n    deleted   swaps\n"
        ),
        "{}",
        run.stdout
    );
}

#[test]
fn uncommitted_changes() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('a', () => { expect(a).toEqual({ x: 1, y: 2 }) })\n",
    );
    repo.commit("Add a test");
    repo.write(
        "test/a.test.ts",
        "it('a', () => { expect(a).toEqual({ x: 1 }) })\n",
    );
    let run = repo.assertrust(&["pairs", "--worktree"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let head = repo.git(&["rev-parse", "--short=7", "HEAD"]);
    let expected = format!(
        "worktree\n\
         compare {} → files on disk\n\
         \x20 test/a.test.ts: 1 test paired\n\
         \x20   same      a\n\
         \x20     changed       1  expect(a).toEqual({{ x: 1, y: 2 }})\n\
         \x20              →    1  expect(a).toEqual({{ x: 1 }})\n",
        head.trim()
    );
    assert_eq!(run.stdout, expected);
}

#[test]
fn a_file_that_does_not_parse_is_noted() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(1) })\n");
    let base = repo.commit("Add a test");
    repo.write("test/a.test.ts", "it('a', () => { expect(a).toBe(\n");
    repo.commit("Break it");
    let run = repo.assertrust(&["pairs", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("skipped, doesn't parse"),
        "{}",
        run.stdout
    );
    // The whole file is skipped: its tests don't all look deleted.
    assert!(!run.stdout.contains("deleted"), "{}", run.stdout);
}

#[test]
fn no_test_files_changed() {
    let mut repo = TestRepo::new();
    repo.write("src/app.ts", "export const a = 1;\n");
    let base = repo.commit("Add the app");
    repo.write("src/app.ts", "export const a = 2;\n");
    repo.commit("Change the app");
    let run = repo.assertrust(&["pairs", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("  no test files changed\n"),
        "{}",
        run.stdout
    );
}
