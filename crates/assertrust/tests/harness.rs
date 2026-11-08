//! Self-tests for the test harness in `common/`, so later PRs can rely on it.

mod common;

use common::TestRepo;

#[test]
fn builds_a_branch_and_a_merge() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", "expect(x).toBe(1);\n");
    let base = repo.commit("Add a test");

    repo.branch("feature");
    repo.write("test/a.test.ts", "expect(x).toBeTruthy();\n");
    let change = repo.commit("Loosen the test");

    repo.switch("main");
    let merge = repo.merge("feature", "Merge branch 'feature'");

    let parents = repo.git(&["rev-list", "--parents", "-n", "1", &merge]);
    assert_eq!(parents, format!("{merge} {base} {change}"));
    assert_eq!(
        repo.git(&["show", "HEAD:test/a.test.ts"]),
        "expect(x).toBeTruthy();"
    );
}

#[test]
fn commit_shas_are_the_same_on_every_run() {
    let mut repo = TestRepo::new();
    repo.write("a.test.ts", "expect(x).toBe(1);\n");
    let first = repo.commit("First");
    repo.write("a.test.ts", "expect(x).toBe(2);\n");
    let second = repo.commit("Second");

    insta::assert_snapshot!(format!("{first}\n{second}"));
}

#[test]
fn ignores_the_developers_global_git_config() {
    let repo = TestRepo::new();
    // A developer may have `commit.gpgsign = true` globally; inside the harness it must be unset.
    let run = repo.try_git(&["config", "--get", "commit.gpgsign"]);
    assert_eq!(run.code, 1, "expected unset, got: {}", run.stdout);
}

#[test]
fn stages_without_committing() {
    let mut repo = TestRepo::new();
    repo.commit("Empty start");
    repo.write("a.test.ts", "expect(x).toBe(1);\n");
    repo.stage("a.test.ts");
    assert_eq!(repo.git(&["diff", "--cached", "--name-only"]), "a.test.ts");
}

#[test]
fn runs_the_binary_inside_the_repo() {
    let repo = TestRepo::new();
    let run = repo.assertrust(&["check", "--staged"]);
    assert_eq!(run.code, 2, "check is not implemented yet: {}", run.stderr);
}
