//! The rules on linked files: checks that run through helpers and hooks
//! (`docs/architecture.md`, step 3, "Linked view", and step 5).

use std::collections::HashMap;

use assertrust_ir::{FileIr, Modules, link, parse_file};
use assertrust_rules::{Side, TestFile, check};

/// Helper files other files import, by the import path as written.
#[derive(Default)]
struct Files(HashMap<String, (String, FileIr)>);

impl Files {
    fn with(mut self, specifier: &str, path: &str, source: &str) -> Self {
        let ir = parse_file(path, source).expect("helper file parses");
        self.0.insert(specifier.to_owned(), (path.to_owned(), ir));
        self
    }
}

impl Modules for Files {
    fn module(&self, _from: &str, specifier: &str) -> Option<(&str, &FileIr)> {
        self.0.get(specifier).map(|(path, ir)| (path.as_str(), ir))
    }
}

/// Findings for `test/a.test.ts` before and after, each side linked to its own helper
/// files, as `rule confidence path:line: message`.
fn findings_with(before: &str, after: &str, modules: (&Files, &Files)) -> Vec<String> {
    let path = "test/a.test.ts";
    let before = link(
        path,
        &parse_file(path, before).expect("before parses"),
        modules.0,
    );
    let after = link(
        path,
        &parse_file(path, after).expect("after parses"),
        modules.1,
    );
    check(
        &[TestFile {
            before: Some(Side { path, ir: &before }),
            after: Some(Side { path, ir: &after }),
            renamed_out: None,
        }],
        &[],
    )
    .iter()
    .map(|finding| {
        let line = finding.line.map_or("-".to_owned(), |line| line.to_string());
        format!(
            "{} {} {}:{line}: {}",
            finding.rule, finding.confidence, finding.path, finding.message
        )
    })
    .collect()
}

fn findings(before: &str, after: &str) -> Vec<String> {
    let none = Files::default();
    findings_with(before, after, (&none, &none))
}

const TWO_TESTS: &str = "
it('refunds', () => { expectOk(refund()) })
it('charges', () => { expectOk(charge()) })";

#[test]
fn a_check_removed_from_a_helper_is_one_finding_for_every_test() {
    let before = format!(
        "function expectOk(res) {{\n  expect(res.status).toBe(200)\n  expect(res.body.ok).toBe(true)\n}}{TWO_TESTS}"
    );
    let after = format!("function expectOk(res) {{\n  expect(res.status).toBe(200)\n}}{TWO_TESTS}");
    assert_eq!(
        findings(&before, &after),
        [
            "check-removed high test/a.test.ts:1: removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"
        ]
    );
}

#[test]
fn a_removed_call_says_how_many_checks_it_ran() {
    let helper = "function expectOk(res) {\n  expect(res.status).toBe(200)\n  expect(res.body.ok).toBe(true)\n}\n";
    assert_eq!(
        findings(
            &format!(
                "{helper}it('refunds', () => {{\n  const res = refund()\n  expectOk(res)\n}})"
            ),
            &format!("{helper}it('refunds', () => {{\n  const res = refund()\n}})"),
        ),
        [
            "check-removed high test/a.test.ts:5: removed expectOk(res), which ran 2 checks; the test has no checks left"
        ]
    );
}

#[test]
fn a_call_swapped_for_a_weaker_helper_says_what_is_gone() {
    let helpers = "function expectOk(res) {\n  expect(res.status).toBe(200)\n  expect(res.body.ok).toBe(true)\n}\nfunction expectStatus(res) {\n  expect(res.status).toBe(200)\n}\n";
    assert_eq!(
        findings(
            &format!("{helpers}it('refunds', () => {{ expectOk(refund()) }})"),
            &format!("{helpers}it('refunds', () => {{ expectStatus(refund()) }})"),
        ),
        [
            "check-removed high test/a.test.ts:8: removed expectOk(refund()); 1 of the 2 checks it ran is gone"
        ]
    );
}

#[test]
fn a_changed_argument_is_reported_at_the_call() {
    let helper = "function expectFast(res, limit) {\n  expect(res.ms).toBeLessThan(limit)\n}\n";
    assert_eq!(
        findings(
            &format!("{helper}it('loads', () => {{\n  expectFast(load(), 100)\n}})"),
            &format!("{helper}it('loads', () => {{\n  expectFast(load(), 500)\n}})"),
        ),
        [
            "expectation-loosened medium test/a.test.ts:5: toBeLessThan upper bound raised from 100 to 500"
        ]
    );
}

#[test]
fn a_call_moved_into_an_if_is_one_finding() {
    let helper = "function expectOk(res) {\n  expect(res.status).toBe(200)\n  expect(res.body.ok).toBe(true)\n}\n";
    assert_eq!(
        findings(
            &format!(
                "{helper}it('refunds', () => {{\n  const res = refund()\n  expectOk(res)\n}})"
            ),
            &format!(
                "{helper}it('refunds', () => {{\n  const res = refund()\n  if (res) expectOk(res)\n}})"
            ),
        ),
        [
            "check-cant-fail medium test/a.test.ts:7: expectOk(res) is now inside an if, with its 2 checks: it may not run"
        ]
    );
}

#[test]
fn a_check_moved_into_an_if_inside_a_helper_is_reported_in_the_helper() {
    let before =
        format!("function expectOk(res) {{\n  expect(res.status).toBe(200)\n}}{TWO_TESTS}");
    let after = format!(
        "function expectOk(res) {{\n  if (res) expect(res.status).toBe(200)\n}}{TWO_TESTS}"
    );
    assert_eq!(
        findings(&before, &after),
        [
            "check-cant-fail medium test/a.test.ts:2: expect(res.status).toBe(200) in expectOk is now inside an if: it may not run (in 2 tests)"
        ]
    );
}

#[test]
fn an_async_helper_call_that_lost_its_await_is_flagged() {
    let helper = "async function expectSaved(id) {\n  expect(await db.get(id)).toBeDefined()\n}\n";
    assert_eq!(
        findings(
            &format!("{helper}it('saves', async () => {{\n  await expectSaved(1)\n}})"),
            &format!("{helper}it('saves', async () => {{\n  expectSaved(1)\n}})"),
        ),
        [
            "check-cant-fail medium test/a.test.ts:5: expectSaved(1) is now not awaited, with its check: it may run after the test ends"
        ]
    );
    // A helper that isn't async runs its checks before it returns.
    let helper = "function expectSaved(id) {\n  expect(db.get(id)).toBeDefined()\n}\n";
    assert_eq!(
        findings(
            &format!("{helper}it('saves', async () => {{\n  await expectSaved(1)\n}})"),
            &format!("{helper}it('saves', async () => {{\n  expectSaved(1)\n}})"),
        ),
        Vec::<String>::new()
    );
}

#[test]
fn checks_removed_from_any_hook_are_flagged() {
    for hook in ["beforeAll", "beforeEach", "afterEach", "afterAll"] {
        let tests = "\n  it('refunds', () => { expect(refund()).toBe(true) })\n  it('charges', () => { expect(charge()).toBe(true) })\n})";
        assert_eq!(
            findings(
                &format!(
                    "describe('billing', () => {{\n  {hook}(() => {{ expect(db.open).toBe(true) }}){tests}"
                ),
                &format!("describe('billing', () => {{\n  {hook}(() => {{ connect() }}){tests}"),
            ),
            [format!(
                "check-removed high test/a.test.ts:2: removed expect(db.open).toBe(true) from {hook} (in 2 tests)"
            )],
            "{hook}"
        );
    }
}

#[test]
fn an_early_return_does_not_stop_after_each() {
    let hook = "afterEach(() => { expect(db.closed).toBe(true) })\n";
    assert_eq!(
        findings(
            &format!("{hook}it('refunds', () => {{\n  expect(refund()).toBe(true)\n}})"),
            &format!("{hook}it('refunds', () => {{\n  return\n  expect(refund()).toBe(true)\n}})"),
        ),
        ["check-cant-fail high test/a.test.ts:3: a return now comes before 1 check: it never runs"]
    );
}

#[test]
fn a_check_moved_into_a_helper_is_not_removed() {
    assert_eq!(
        findings(
            "it('refunds', () => { const res = refund(); expect(res.status).toBe(200) })",
            "function expectOk(res) { expect(res.status).toBe(200) }
             it('refunds', () => { const res = refund(); expectOk(res) })",
        ),
        Vec::<String>::new()
    );
}

#[test]
fn arguments_fill_in_expected_values() {
    // Inlining `expectStatus(res, 200)` gives the same check.
    assert_eq!(
        findings(
            "it('refunds', () => { expect(res.status).toBe(200) })",
            "function expectStatus(res, status) { expect(res.status).toBe(status) }
             it('refunds', () => { expectStatus(res, 200) })",
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_change_in_an_imported_helper_is_reported_in_its_file() {
    let test = "import { expectOk } from './helpers'
it('refunds', () => { expectOk(refund()) })
it('charges', () => { expectOk(charge()) })";
    let before = Files::default().with(
        "./helpers",
        "test/helpers.ts",
        "export function expectOk(res) {\n  expect(res.status).toBe(200)\n  expect(res.body.ok).toBe(true)\n}",
    );
    let after = Files::default().with(
        "./helpers",
        "test/helpers.ts",
        "export function expectOk(res) {\n  expect(res.status).toBe(200)\n}",
    );
    assert_eq!(
        findings_with(test, test, (&before, &after)),
        [
            "check-removed high test/helpers.ts:1: removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"
        ]
    );
}

#[test]
fn helpers_passed_on_by_another_file_are_followed() {
    let test = "import { expectOk } from './support'
it('refunds', () => { expectOk(refund()) })";
    let files = |body: &str| {
        Files::default()
            .with(
                "./support",
                "test/support/index.ts",
                "export * from './http'",
            )
            .with("./http", "test/support/http.ts", body)
    };
    assert_eq!(
        findings_with(
            test,
            test,
            (
                &files("export function expectOk(res) {\n  expect(res.ok).toBe(true)\n}"),
                &files("export function expectOk(res) {\n  expect(res.ok).toBeDefined()\n}"),
            )
        ),
        [
            "matcher-loosened high test/support/http.ts:2: toBe(true) → toBeDefined(): now any defined value passes"
        ]
    );
}

// Found in code review.

#[test]
fn a_hook_count_does_not_stand_in_for_the_test_s_own() {
    let hook = "beforeEach(() => { expect.hasAssertions() })\n";
    assert_eq!(
        findings(
            &format!("{hook}it('a', () => {{\n  expect.assertions(2)\n  expect(a).toBe(1)\n}})"),
            &format!("{hook}it('a', () => {{\n  expect(a).toBe(1)\n}})"),
        ),
        ["check-cant-fail medium test/a.test.ts:2: expect.assertions(2) removed"]
    );
}

#[test]
fn a_return_added_in_a_helper_stops_only_its_checks() {
    let tests = "\nit('refunds', () => { expectOk(refund()); expect(done).toBe(true) })\nit('charges', () => { expectOk(charge()) })";
    assert_eq!(
        findings(
            &format!(
                "function expectOk(res) {{\n  log(res)\n  expect(res.ok).toBe(true)\n}}{tests}"
            ),
            &format!(
                "function expectOk(res) {{\n  log(res)\n  return\n  expect(res.ok).toBe(true)\n}}{tests}"
            ),
        ),
        [
            "check-cant-fail high test/a.test.ts:3: a return in expectOk now comes before 1 check: it never runs (in 2 tests)"
        ]
    );
}

#[test]
fn a_return_added_in_a_hook_is_flagged() {
    let tests = "\n  it('a', () => { expect(a).toBe(1) })\n})";
    assert_eq!(
        findings(
            &format!(
                "describe('db', () => {{\n  beforeEach(() => {{ expect(db.open).toBe(true) }}){tests}"
            ),
            &format!(
                "describe('db', () => {{\n  beforeEach(() => {{ return; expect(db.open).toBe(true) }}){tests}"
            ),
        ),
        [
            "check-cant-fail high test/a.test.ts:2: a return in beforeEach now comes before 1 check: it never runs"
        ]
    );
}

#[test]
fn a_guard_in_a_helper_called_from_an_if_still_stops_its_checks() {
    // The `return` is unconditional inside the helper: the `if` around the call doesn't make
    // it any less sure to stop the helper's checks.
    let helper = |body: &str| {
        format!(
            "function expectOk(res) {{\n{body}  expect(res.ok).toBe(true)\n}}\nit('a', () => {{ if (x) expectOk(res) }})"
        )
    };
    assert_eq!(
        findings(&helper(""), &helper("  return\n")),
        [
            "check-cant-fail high test/a.test.ts:2: a return in expectOk now comes before 1 check: it never runs"
        ]
    );
}

#[test]
fn a_check_removed_from_a_helper_points_at_the_helper_as_it_is_now() {
    // Four lines added above the helper: the check was on line 3, the helper now starts on 5.
    let tests = "\nit('refunds', () => { expectOk(refund()) })";
    assert_eq!(
        findings(
            &format!(
                "function expectOk(res) {{\n  expect(res.status).toBe(200)\n  expect(res.ok).toBe(true)\n}}{tests}"
            ),
            &format!(
                "// Shared checks,\n// used by every\n// test.\n\nfunction expectOk(res) {{\n  expect(res.status).toBe(200)\n}}{tests}"
            ),
        ),
        ["check-removed high test/a.test.ts:5: removed expect(res.ok).toBe(true) from expectOk"]
    );
}

#[test]
fn an_await_dropped_from_an_async_helper_call_is_one_finding() {
    let helper = "async function check(p) {\n  await expect(p).resolves.toBe(1)\n}\n";
    assert_eq!(
        findings(
            &format!("{helper}it('a', async () => {{\n  await check(p)\n}})"),
            &format!("{helper}it('a', async () => {{\n  check(p)\n}})"),
        ),
        [
            "check-cant-fail medium test/a.test.ts:5: check(p) is now not awaited, with its check: it may run after the test ends"
        ]
    );
}

#[test]
fn a_call_removed_from_a_helper_is_reported_in_that_helper() {
    let inner = "function b(r) {\n  expect(r.ok).toBe(true)\n}\n";
    let test = "\nit('a', () => { a(res) })";
    assert_eq!(
        findings(
            &format!("{inner}function a(r) {{\n  expect(r.s).toBe(1)\n  b(r)\n}}{test}"),
            &format!("{inner}function a(r) {{\n  expect(r.s).toBe(1)\n}}{test}"),
        ),
        ["check-removed high test/a.test.ts:4: removed the call to b from a, which ran 1 check"]
    );
    // An argument changed in `a`'s call to `b` is reported at that call, in `a`.
    let inner = "function b(r, limit) {\n  expect(r.ms).toBeLessThan(limit)\n}\n";
    assert_eq!(
        findings(
            &format!("{inner}function a(r) {{\n  b(r, 100)\n}}{test}"),
            &format!("{inner}function a(r) {{\n  b(r, 500)\n}}{test}"),
        ),
        [
            "expectation-loosened medium test/a.test.ts:5: toBeLessThan upper bound raised from 100 to 500"
        ]
    );
}

#[test]
fn a_call_that_can_no_longer_be_followed_is_not_removed() {
    // `./helpers` became an alias nothing resolves: the call is still there, what it checks
    // can't be seen.
    let helpers = Files::default().with(
        "./helpers",
        "test/helpers.ts",
        "export function expectOk(res) {\n  expect(res.ok).toBe(true)\n}",
    );
    assert_eq!(
        findings_with(
            "import { expectOk } from './helpers'\nit('a', () => { expectOk(res) })",
            "import { expectOk } from '@/test/helpers'\nit('a', () => { expectOk(res) })",
            (&helpers, &Files::default()),
        ),
        Vec::<String>::new()
    );
    // Dropping the call still counts.
    assert_eq!(
        findings_with(
            "import { expectOk } from './helpers'\nit('a', () => { expectOk(res) })",
            "import { expectOk } from '@/test/helpers'\nit('a', () => {})",
            (&helpers, &Files::default()),
        ),
        [
            "check-removed high test/a.test.ts:2: removed expectOk(res), which ran 1 check; the test has no checks left"
        ]
    );
}

#[test]
fn a_try_added_inside_a_helper_is_one_finding_there() {
    // The calls changed too (an argument dropped), but the `try` is the helper's.
    let tests = |args: &str| {
        format!(
            "\nit('a', async () => {{ await expectOk(res{args}) }})\nit('b', async () => {{ await expectOk(res{args}) }})"
        )
    };
    assert_eq!(
        findings(
            &format!(
                "async function expectOk(res, strict) {{\n  expect(res.ok).toBe(true)\n}}{}",
                tests(", true")
            ),
            &format!(
                "async function expectOk(res) {{\n  try {{\n    expect(res.ok).toBe(true)\n  }} catch {{}}\n}}{}",
                tests("")
            ),
        ),
        [
            "check-cant-fail medium test/a.test.ts:3: expect(res.ok).toBe(true) in expectOk is now inside a try with a catch: its failure may be caught (in 2 tests)"
        ]
    );
}

#[test]
fn a_catch_that_throws_again_lets_the_failure_through() {
    assert_eq!(
        findings(
            "it('a', () => { expect(a).toBe(1) })",
            "it('a', () => { try { expect(a).toBe(1) } catch (err) { tidy(err); throw err } })",
        ),
        Vec::<String>::new()
    );
    // One that may return instead still swallows it.
    assert_eq!(
        findings(
            "it('a', () => { expect(a).toBe(1) })",
            "it('a', () => { try { expect(a).toBe(1) } catch (err) { if (flaky) return; throw err } })",
        ),
        [
            "check-cant-fail medium test/a.test.ts:1: expect(a).toBe(1) is now inside a try with a catch: its failure may be caught"
        ]
    );
}

#[test]
fn methods_on_an_object_a_helper_builds_are_not_its_checks() {
    // `testContext()` hands back methods; their checks run when a test calls them, which
    // isn't followed. They mustn't stand in for the check the test dropped.
    let helper = "function testContext() {\n  return { serverAborted(call) { expect(call.aborted).toBe(true) } }\n}\n";
    assert_eq!(
        findings(
            &format!(
                "{helper}it('stops', () => {{\n  const t = testContext()\n  expect(older.aborted).toBe(false)\n}})"
            ),
            &format!("{helper}it('stops', () => {{\n  const t = testContext()\n}})"),
        ),
        [
            "check-removed high test/a.test.ts:4: removed expect(older.aborted).toBe(false); the test has no checks left"
        ]
    );
}

/// immich's `SyncTestContext.assertSyncResponse`: the last record, then the ones before it
/// in any order, and how many there are.
const SYNC_CONTEXT: &str = "
export class SyncContext {
  assertSyncResponse(response, records) {
    const completeAck = response.at(-1);
    const items = response.slice(0, -1);
    expect(completeAck).toEqual(expect.objectContaining({ type: 'SyncCompleteV1' }));
    expect(items).toEqual(expect.arrayContaining(records));
    expect(items).toHaveLength(records.length);
  }
  async assertSyncIsComplete(auth) {
    this.assertSyncResponse(await this.syncStream(auth), []);
  }
}
";

/// A test file using `SyncContext`, built in a setup function, with `body` as the test.
fn sync_test(body: &str) -> String {
    format!(
        "import {{ SyncContext }} from './context';\n\
         const setup = () => ({{ ctx: new SyncContext() }});\n\
         it('syncs', async () => {{\n  const {{ ctx }} = setup();\n  const response = await ctx.syncStream(auth);\n{body}\n}})\n"
    )
}

#[test]
fn checks_moved_into_a_method_are_still_checked() {
    let files = Files::default().with("./context", "test/context.ts", SYNC_CONTEXT);
    let complete = "expect.objectContaining({ type: 'SyncCompleteV1' })";
    // One record: the same check, written as parts of the list.
    assert_eq!(
        findings_with(
            &sync_test(&format!(
                "  expect(response).toEqual([{{ id: 'user-1', type: 'UserV1' }}, {complete}]);"
            )),
            &sync_test("  ctx.assertSyncResponse(response, [{ id: 'user-1', type: 'UserV1' }]);"),
            (&files, &files),
        ),
        Vec::<String>::new()
    );
    // A list that only had to contain them, with its length: nothing lost either.
    assert_eq!(
        findings_with(
            &sync_test(&format!(
                "  expect(response).toHaveLength(3);\n  expect(response).toEqual(expect.arrayContaining([{{ id: 'user-1' }}, {{ id: 'user-2' }}, {complete}]));"
            )),
            &sync_test("  ctx.assertSyncResponse(response, [{ id: 'user-1' }, { id: 'user-2' }]);"),
            (&files, &files),
        ),
        Vec::<String>::new()
    );
    // `this.…()` in the method, with the argument it passes on: `await expect(p).resolves`
    // is the list `(await p)`.
    assert_eq!(
        findings_with(
            &sync_test(&format!(
                "  await expect(ctx.syncStream(auth)).resolves.toEqual([{complete}]);"
            )),
            &sync_test("  await ctx.assertSyncIsComplete(auth);"),
            (&files, &files),
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_list_checked_in_parts_may_lose_its_order_or_count() {
    let files = Files::default().with("./context", "test/context.ts", SYNC_CONTEXT);
    let complete = "expect.objectContaining({ type: 'SyncCompleteV1' })";
    let before = sync_test(&format!(
        "  expect(response).toEqual([{{ id: 'user-1' }}, {{ id: 'user-2' }}, {complete}]);"
    ));
    assert_eq!(
        findings_with(
            &before,
            &sync_test("  ctx.assertSyncResponse(response, [{ id: 'user-1' }, { id: 'user-2' }]);"),
            (&files, &files),
        ),
        [
            "expectation-loosened low test/a.test.ts:6: expect(response).toEqual([{ id: 'user-1' }, { id: 'user-2' … is now checked in parts, with its 3 items in any order"
        ]
    );
    // Without the length check, more records would pass too.
    let lenient = Files::default().with(
        "./context",
        "test/context.ts",
        &SYNC_CONTEXT.replace("    expect(items).toHaveLength(records.length);\n", ""),
    );
    assert_eq!(
        findings_with(
            &before,
            &sync_test("  ctx.assertSyncResponse(response, [{ id: 'user-1' }, { id: 'user-2' }]);"),
            (&files, &lenient),
        ),
        [
            "expectation-loosened medium test/a.test.ts:6: expect(response).toEqual([{ id: 'user-1' }, { id: 'user-2' … is now checked in parts, which pass with more than its 3 items"
        ]
    );
}

#[test]
fn a_check_removed_from_a_method_is_one_finding_for_every_test() {
    let tests = "import { Api } from './api';\nconst api = new Api();\n\
                 it('refunds', () => { api.expectOk(refund()) })\n\
                 it('charges', () => { api.expectOk(charge()) })\n";
    let strict = "export class Api {\n  expectOk(res) {\n    expect(res.status).toBe(200);\n    expect(res.body.ok).toBe(true);\n  }\n  url() { return '/api' }\n}\n";
    let weaker = strict.replace("    expect(res.body.ok).toBe(true);\n", "");
    let before = Files::default().with("./api", "test/api.ts", strict);
    let after = Files::default().with("./api", "test/api.ts", &weaker);
    assert_eq!(
        findings_with(tests, tests, (&before, &after)),
        [
            "check-removed high test/api.ts:2: removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"
        ]
    );
}
