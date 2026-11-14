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
            "test/helpers.ts:1  HIGH  check-removed\n\
             \x20 removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests) · {}\n\
             \n\
             1 finding · warn-only (exit 0)\n",
            &head[..7]
        )
    );
}

#[test]
fn staged_and_unsaved_helper_changes_are_checked_too() {
    let expected = "test/helpers.ts:1  HIGH  check-removed\n\
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
            "test/support/http.ts:1  HIGH  check-removed\n  removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"
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

// Found in code review.

#[test]
fn files_that_are_not_text_do_not_stop_the_check() {
    let (mut repo, base) = repo_with_helpers();
    // A Latin-1 file in a test folder, changed, and one that the search for importers finds.
    std::fs::write(
        repo.path().join("test/legacy.js"),
        b"import x from './helpers' // caf\xe9\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("test/fixture.js"), b"// caf\xe9\n").unwrap();
    repo.commit("Add legacy files");
    std::fs::write(repo.path().join("test/fixture.js"), b"// caf\xe9 au lait\n").unwrap();
    repo.write("test/helpers.ts", WEAKER);
    repo.commit("Relax expectOk");
    let out = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.contains("removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"),
        "{out}"
    );
}

#[test]
fn a_helper_moved_out_of_test_folders_is_still_followed() {
    let (mut repo, base) = repo_with_helpers();
    repo.remove("test/helpers.ts");
    repo.write("src/testing.ts", STRICT);
    repo.write("test/refund.test.ts", &tests_importing("../src/testing"));
    repo.commit("Move expectOk to src");
    assert_eq!(
        check(&repo, &["--range", &format!("{base}..HEAD")]),
        "no findings\n"
    );
    // Moved and weakened at once: seen.
    let (mut repo, base) = repo_with_helpers();
    repo.remove("test/helpers.ts");
    repo.write("src/testing.ts", WEAKER);
    repo.write("test/refund.test.ts", &tests_importing("../src/testing"));
    repo.commit("Move expectOk to src");
    let out = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.starts_with("src/testing.ts:1  HIGH  check-removed\n  removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"),
        "{out}"
    );
}

#[test]
fn tsconfig_aliases_are_followed() {
    let mut repo = TestRepo::new();
    repo.write(
        "tsconfig.json",
        "{\n  // Aliases.\n  \"extends\": \"./tsconfig.base\",\n}\n",
    );
    repo.write(
        "tsconfig.base.json",
        "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@test/*\": [\"test/support/*\"] } } }",
    );
    repo.write("test/refund.test.ts", &tests_importing("@test/http"));
    repo.write("test/support/http.ts", STRICT);
    let base = repo.commit("Add tests");
    repo.write("test/support/http.ts", WEAKER);
    repo.commit("Relax expectOk");
    let out = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.starts_with("test/support/http.ts:1  HIGH  check-removed\n  removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"),
        "{out}"
    );
}

#[test]
fn switching_to_an_alias_that_can_not_be_resolved_is_not_a_removal() {
    // A Nuxt alias: its tsconfig is generated outside git.
    let (mut repo, base) = repo_with_helpers();
    repo.write("test/refund.test.ts", &tests_importing("~~/test/helpers"));
    repo.commit("Use the alias");
    assert_eq!(
        check(&repo, &["--range", &format!("{base}..HEAD")]),
        "no findings\n"
    );
}

#[test]
fn an_index_file_imported_by_dots_alone_finds_its_tests() {
    let mut repo = TestRepo::new();
    repo.write("test/support/index.ts", STRICT);
    repo.write(
        "test/support/api/refund.test.ts",
        &tests_importing("../index"),
    );
    repo.write("test/support/charge.test.ts", &tests_importing("."));
    let base = repo.commit("Add tests");
    repo.write("test/support/index.ts", WEAKER);
    repo.commit("Relax expectOk");
    let out = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.starts_with("test/support/index.ts:1  HIGH  check-removed\n  removed expect(res.body.ok).toBe(true) from expectOk (in 4 tests)"),
        "{out}"
    );
}

#[test]
fn a_long_chain_of_helper_files_says_where_the_search_stopped() {
    // test/h0.ts passes on test/h1.ts, … , which passes on test/h24.ts with the helper.
    let mut repo = TestRepo::new();
    for n in 0..24 {
        repo.write(
            &format!("test/h{n}.ts"),
            &format!("export * from './h{}'\n", n + 1),
        );
    }
    repo.write("test/h24.ts", STRICT);
    repo.write("test/refund.test.ts", &tests_importing("./h0"));
    let base = repo.commit("Add tests");
    repo.write("test/h24.ts", WEAKER);
    let head = repo.commit("Relax expectOk");
    let out = check(&repo, &["--range", &format!("{base}..HEAD")]);
    assert!(
        out.contains(&format!(
            "note: test/h24.ts ({}): stopped looking for tests that call its helpers, too many helper files pass them on",
            &head[..7]
        )),
        "{out}"
    );
}

#[test]
fn commit_by_commit_each_side_reads_its_own_helpers() {
    // What one commit's "after" read is the next one's "before"; a file the commit touched
    // is read again.
    let (mut repo, base) = repo_with_helpers();
    repo.write("test/helpers.ts", WEAKER);
    let relaxed = repo.commit("Relax expectOk");
    repo.write(
        "test/refund.test.ts",
        &format!(
            "{}it('voids', () => {{ expectOk(voids()) }})\n",
            tests_importing("./helpers")
        ),
    );
    repo.commit("Test voids");
    repo.write("test/helpers.ts", "export function expectOk(res) {}\n");
    let emptied = repo.commit("Empty expectOk");
    assert_eq!(
        check(&repo, &["--range", &format!("{base}..HEAD")]),
        format!(
            "test/helpers.ts:1  HIGH  check-removed\n\
             \x20 removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests) · {}\n\
             test/helpers.ts:1  HIGH  check-removed\n\
             \x20 removed expect(res.status).toBe(200) from expectOk (in 3 tests) · {}\n\
             \n\
             2 findings · warn-only (exit 0)\n",
            &relaxed[..7],
            &emptied[..7]
        )
    );
}

/// A context class whose method checks a response, like immich's `SyncTestContext`.
const CONTEXT: &str = "export class Context {\n  assertResponse(response, records) {\n    const done = response.at(-1);\n    const items = response.slice(0, -1);\n    expect(done).toEqual(expect.objectContaining({ type: 'done' }));\n    expect(items).toEqual(expect.arrayContaining(records));\n    expect(items).toHaveLength(records.length);\n  }\n}\n";

/// Two tests that get a `Context` from a setup function, each with `check` as its last line
/// (`{user}` stands for its record).
fn context_tests(check: &str) -> String {
    let test = |name: &str, user: &str| {
        format!(
            "it('{name}', async () => {{\n  const {{ ctx }} = setup();\n  const response = await sync();\n  {}\n}})\n",
            check.replace("{user}", user)
        )
    };
    format!(
        "import {{ Context }} from './context'\n\nconst setup = () => ({{ ctx: new Context() }})\n\n{}{}",
        test("syncs a user", "{ id: 1 }"),
        test("syncs a partner", "{ id: 2 }")
    )
}

#[test]
fn checks_moved_into_a_method_of_an_imported_class_are_followed() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/sync.test.ts",
        &context_tests(
            "expect(response).toEqual([{user}, expect.objectContaining({ type: 'done' })])",
        ),
    );
    let base = repo.commit("Add tests");
    repo.write("test/context.ts", CONTEXT);
    repo.write(
        "test/sync.test.ts",
        &context_tests("ctx.assertResponse(response, [{user}])"),
    );
    repo.commit("Check responses in Context");
    // The same checks, run by the method the tests call on what `setup` builds.
    assert_eq!(
        check(&repo, &["--range", &format!("{base}..HEAD")]),
        "no findings\n"
    );

    // The method drops its count: one finding there, for both tests.
    let before = repo.git(&["rev-parse", "HEAD"]);
    repo.write(
        "test/context.ts",
        &CONTEXT.replace("    expect(items).toHaveLength(records.length);\n", ""),
    );
    let head = repo.commit("Relax assertResponse");
    assert_eq!(
        check(&repo, &["--range", &format!("{before}..HEAD")]),
        format!(
            "test/context.ts:2  HIGH  check-removed\n  removed expect(items).toHaveLength(1) from assertResponse (in 2 tests) · {}\n\n1 finding · warn-only (exit 0)\n",
            &head[..7]
        )
    );
}

#[test]
fn a_change_to_a_method_that_checks_nothing_loads_nothing() {
    // A page object: `expectOpen` checks, `click` and `API.clear` drive the page, and
    // `h.app.clear()` is the app's own method, not `API.clear`. Which page `expectOpen`
    // clicks isn't a check either.
    let page = |click: &str, import: &str| {
        let click_arg = if click.contains("delay") { 2 } else { 1 };
        format!(
            "{import}\nexport class API {{\n  static clear = () => {{\n    act(() => {{ h.app.clear() }});\n    expect(selected()).toEqual([]);\n  }}\n}}\n\
             export class Page {{\n  click(x) {{ {click} }}\n  expectOpen() {{ this.click({click_arg}); expect(this.dialog).toBeVisible() }}\n}}\n"
        )
    };
    let tests = "import { Page } from './page'\n\nconst page = new Page()\n\nit('opens', () => { page.click(1); page.expectOpen() })\n";
    let mut repo = TestRepo::new();
    repo.write("test/open.test.ts", tests);
    repo.write(
        "test/page.ts",
        &page("fire(x)", "import { act } from 'react'"),
    );
    let base = repo.commit("Add tests");
    repo.write(
        "test/page.ts",
        &page(
            "fire(x, { delay: 1 })",
            "import { act } from 'react-dom/test-utils'",
        )
        .replace("h.app.clear()", "h.app.selection.clear()"),
    );
    let slower = repo.commit("Click slower");
    let run = repo.assertrust(&["pairs", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("no test files changed"),
        "{}",
        run.stdout
    );
    // A method that checks changes: the tests that call it are loaded.
    let current = std::fs::read_to_string(repo.path().join("test/page.ts")).unwrap();
    repo.write(
        "test/page.ts",
        &current.replace("toBeVisible()", "toBeAttached()"),
    );
    repo.commit("Relax expectOpen");
    let run = repo.assertrust(&["pairs", "--range", &format!("{slower}..HEAD")]);
    assert!(
        run.stdout.contains("test/open.test.ts: 1 test paired"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_method_that_turns_static_loses_its_calls() {
    let page = |method: &str| {
        format!("export class Page {{\n  {method}() {{ expect(this.dialog).toBeVisible() }}\n}}\n")
    };
    let mut repo = TestRepo::new();
    repo.write(
        "test/open.test.ts",
        "import { Page } from './page'\n\nconst page = new Page()\n\nit('opens', () => { page.expectOpen() })\n",
    );
    repo.write("test/page.ts", &page("expectOpen"));
    let base = repo.commit("Add tests");
    repo.write("test/page.ts", &page("static expectOpen"));
    let head = repo.commit("Make expectOpen static");
    // `page.expectOpen()` no longer reaches it: only `Page.expectOpen()` would.
    assert_eq!(
        check(&repo, &["--range", &format!("{base}..HEAD")]),
        format!(
            "test/open.test.ts:5  HIGH  check-removed\n  removed page.expectOpen(), which ran 1 check; the test has no checks left · {}\n\n1 finding · warn-only (exit 0)\n",
            &head[..7]
        )
    );
}
