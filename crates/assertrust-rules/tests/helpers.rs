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
            "check-removed high test/a.test.ts:3: removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"
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
        ["check-removed high test/a.test.ts:5: removed expectOk(res), which ran 2 checks"]
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
            "check-removed high test/helpers.ts:3: removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)"
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
