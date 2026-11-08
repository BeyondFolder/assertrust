//! `assertrust tests`: the tests and checks parsing finds in one file (steps 2–3).
//!
//! The fixtures in `tests/fixtures/parsing/` are written from scratch, one per pattern. Their
//! output is stored as snapshot files in `tests/snapshots/`.

mod common;

use common::TestRepo;

/// Copies a fixture into a temp repo as `test/<name>` and runs `assertrust tests` on it.
fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/parsing/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let source = std::fs::read_to_string(&path).expect("read fixture");
    let repo = TestRepo::new();
    let file = format!("test/{name}");
    repo.write(&file, &source);
    let run = repo.assertrust(&["tests", &file]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    run.stdout
}

#[test]
fn test_structure() {
    insta::assert_snapshot!("structure", fixture("structure.test.ts"));
}

#[test]
fn check_shapes() {
    insta::assert_snapshot!("assertions", fixture("assertions.test.ts"));
}

#[test]
fn where_checks_sit() {
    insta::assert_snapshot!("context", fixture("context.test.ts"));
}

#[test]
fn playwright() {
    insta::assert_snapshot!("playwright", fixture("checkout.spec.ts"));
}

#[test]
fn a_file_that_does_not_parse_is_skipped() {
    insta::assert_snapshot!(fixture("broken.test.ts"), @"test/broken.test.ts: skipped, doesn't parse (line 3: Unexpected token)");
}

/// A repo with `test/math.test.ts` committed twice: strict, then loosened.
fn repo_with_history() -> TestRepo {
    let mut repo = TestRepo::new();
    repo.write(
        "test/math.test.ts",
        "it('adds', () => {\n  expect(add(1, 2)).toBe(3);\n});\n",
    );
    repo.commit("Add a test");
    repo.write(
        "test/math.test.ts",
        "it('adds', () => {\n  expect(add(1, 2)).toBeTruthy();\n});\n",
    );
    repo.commit("Loosen the test");
    repo
}

fn tests(repo: &TestRepo, args: &[&str]) -> String {
    let mut full = vec!["tests"];
    full.extend_from_slice(args);
    let run = repo.assertrust(&full);
    assert_eq!(run.code, 0, "tests {args:?} failed: {}", run.stderr);
    run.stdout
}

#[test]
fn rev_reads_the_file_as_it_was_at_a_commit() {
    let repo = repo_with_history();
    insta::assert_snapshot!(tests(&repo, &["test/math.test.ts", "--rev", "HEAD~1"]), @r"
    test/math.test.ts (HEAD~1): 1 test
       1  test adds
       2        equal      expect(add(1, 2)).toBe(3)
    ");
    insta::assert_snapshot!(tests(&repo, &["test/math.test.ts"]), @r"
    test/math.test.ts: 1 test
       1  test adds
       2        truthiness expect(add(1, 2)).toBeTruthy()
    ");
}

#[test]
fn staged_reads_the_index() {
    let repo = repo_with_history();
    repo.write(
        "test/math.test.ts",
        "it('adds', () => {\n  expect(add(1, 2)).toBeDefined();\n});\n",
    );
    repo.stage("test/math.test.ts");
    repo.write("test/math.test.ts", "it('adds', () => {});\n");

    insta::assert_snapshot!(tests(&repo, &["test/math.test.ts", "--staged"]), @r"
    test/math.test.ts (staged): 1 test
       1  test adds
       2        truthiness expect(add(1, 2)).toBeDefined()
    ");
}

#[test]
fn paths_are_relative_to_where_assertrust_runs() {
    let repo = repo_with_history();
    let run = repo.assertrust_in("test", &["tests", "math.test.ts", "--rev", "HEAD~1"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .starts_with("test/math.test.ts (HEAD~1): 1 test\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_file_missing_at_the_revision_is_a_tool_error() {
    let mut repo = repo_with_history();
    repo.write("test/new.test.ts", "it('is new', () => {});\n");
    repo.commit("Add another test");

    let run = repo.assertrust(&["tests", "test/new.test.ts", "--rev", "HEAD~1"]);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stderr,
        "assertrust: `test/new.test.ts` doesn't exist at HEAD~1\n"
    );
}

#[test]
fn a_missing_file_is_a_tool_error() {
    let repo = repo_with_history();
    let run = repo.assertrust(&["tests", "test/nope.test.ts"]);
    assert_eq!(run.code, 2);
    assert!(
        run.stderr
            .starts_with("assertrust: couldn't read `test/nope.test.ts`:"),
        "{}",
        run.stderr
    );
}

#[test]
fn only_js_and_ts_files_are_read() {
    let repo = repo_with_history();
    repo.write("README.md", "# demo\n");
    let run = repo.assertrust(&["tests", "README.md"]);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stderr,
        "assertrust: `README.md` isn't a JS or TS file\n"
    );
}

#[test]
fn a_path_outside_the_repo_is_a_tool_error() {
    let repo = repo_with_history();
    let run = repo.assertrust(&["tests", "../elsewhere.test.ts", "--rev", "HEAD"]);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stderr,
        "assertrust: `../elsewhere.test.ts` is outside the repository\n"
    );
}

#[test]
fn rev_and_staged_cant_be_combined() {
    let repo = repo_with_history();
    let run = repo.assertrust(&["tests", "test/math.test.ts", "--rev", "HEAD", "--staged"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("cannot be used with"), "{}", run.stderr);
}

#[test]
fn gate_facts_of_a_workflow_a_config_and_package_json() {
    let repo = TestRepo::new();
    repo.write(
        ".github/workflows/ci.yml",
        "jobs:\n  lint:\n    steps:\n      - run: npm run lint\n  test:\n    continue-on-error: true\n    steps:\n      - run: npm ci\n      - run: npm test -- --retry=2 || true\n",
    );
    repo.write(
        "playwright.config.ts",
        "export default defineConfig({\n  retries: 1,\n  projects: [{ name: 'chromium', use: { channel: 'chrome' } }],\n});\n",
    );
    repo.write(
        "package.json",
        "{\n  \"scripts\": {\n    \"build\": \"nuxt build\",\n    \"test\": \"vitest run --passWithNoTests\"\n  }\n}\n",
    );
    let run = |file: &str| {
        let run = repo.assertrust(&["tests", file]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        run.stdout
    };
    assert_eq!(
        run(".github/workflows/ci.yml"),
        ".github/workflows/ci.yml: 1 job runs tests\n\
         \x20  5  job test  [continue-on-error: true]\n\
         \x20  9    npm test -- --retry=2 || true  (swallows failures; retries 2)\n"
    );
    assert_eq!(
        run("playwright.config.ts"),
        "playwright.config.ts: 3 settings\n\
         \x20  2  retries = 1\n\
         \x20  3  projects[chromium].name = 'chromium'\n\
         \x20  3  projects[chromium].use.channel = 'chrome'\n"
    );
    assert_eq!(
        run("package.json"),
        "package.json: 1 test script\n\
         \x20  4  test: vitest run --passWithNoTests  (passes with no tests)\n"
    );
}
