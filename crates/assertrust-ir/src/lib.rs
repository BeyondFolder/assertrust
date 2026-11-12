//! Parsing (`docs/architecture.md`, step 2) and the test IR (step 3) of assertrust.
//!
//! [`parse_file`] reads one JS/TS file and returns its tests, hooks and checks, in a form
//! that ignores formatting. It knows nothing about git: callers pass the file's contents.
//!
//! ```
//! let ir = assertrust_ir::parse_file(
//!     "math.test.ts",
//!     "it('adds', () => { expect(add(1, 2)).toBe(3) })",
//! )?;
//! let test = &ir.tests[0];
//! assert_eq!(test.id(), "adds");
//! let check = test.body.assertions().next().unwrap();
//! assert_eq!(check.to_string(), "expect(add(1, 2)).toBe(3)");
//! # Ok::<(), assertrust_ir::ParseError>(())
//! ```

mod gate;
mod js;
mod link;
mod matcher;
mod model;
mod tsconfig;
mod value;

use std::fmt;

pub use crate::gate::{Command, Gate, Job, Package, Script, Setting, TestStep, Workflow};
pub use crate::link::{Modules, link};
pub use crate::matcher::{MatcherKind, close_to};
pub use crate::model::{
    Assertion, Body, Call, Context, Defined, ExpectKind, Fact, FactTest, FileIr, Helper, Hook,
    HookKind, Import, Item, ItemKind, Matcher, Modifier, Promise, Test, Via,
};
pub use crate::tsconfig::{TsConfig, parse_tsconfig};
pub use crate::value::Value;

/// Reads the tests and checks in one file. `path` picks the language from its extension
/// (`.ts`, `.tsx`, `.js`, `.jsx`, `.mts`, `.cts`, `.mjs`, `.cjs`).
pub fn parse_file(path: &str, source: &str) -> Result<FileIr, ParseError> {
    js::parse(path, source)
}

/// Reads a CI or test config file as gate facts (step 3): a GitHub Actions workflow
/// (`.yml`), `package.json`, or a vitest / jest / Playwright config (`.ts`, `.js`, `.json`).
///
/// ```
/// let gate = assertrust_ir::parse_gate(
///     ".github/workflows/ci.yml",
///     "jobs:\n  test:\n    steps:\n      - run: npm test || true\n",
/// )?;
/// let assertrust_ir::Gate::Workflow(workflow) = gate else { unreachable!() };
/// assert!(workflow.jobs[0].steps[0].command.swallows);
/// # Ok::<(), assertrust_ir::ParseError>(())
/// ```
pub fn parse_gate(path: &str, source: &str) -> Result<Gate, ParseError> {
    gate::parse(path, source)
}

/// Why a file couldn't be read. A check skips such a file with a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Not a JS or TS file.
    UnsupportedFile,
    /// The file doesn't parse. Work-in-progress commits often don't, and guessing from a
    /// half-parsed file would cause false alarms.
    Syntax { line: u32, message: String },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFile => f.write_str("not a JS or TS file"),
            Self::Syntax { line, message } => write!(f, "doesn't parse (line {line}: {message})"),
        }
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test's id with its items as text, without line numbers.
    fn shape(source: &str) -> Vec<(String, Vec<String>)> {
        let ir = parse_file("a.test.ts", source).expect("parses");
        ir.tests
            .iter()
            .map(|test| {
                let items = test
                    .body
                    .items
                    .iter()
                    .map(|item| format!("{:?} {}", item.kind, item.context))
                    .collect();
                (test.id(), items)
            })
            .collect()
    }

    fn only_assertion(source: &str) -> Assertion {
        let ir = parse_file("a.test.ts", source).expect("parses");
        let mut assertions = ir.tests[0].body.assertions();
        let assertion = assertions.next().expect("one assertion").clone();
        assert!(assertions.next().is_none(), "more than one assertion");
        assertion
    }

    #[test]
    fn formatting_never_counts() {
        let before = r#"
it("saves", async () => {
  expect(res.body).toMatchObject({ status: "pending", ids: [1, 2,], });
  await expect(save()).resolves.toBe(true)
})
"#;
        let after = "\n\n\nit('saves', async () => { expect( res.body ).toMatchObject({\n    ids: [ 1, 2 ],\n    status: 'pending'\n  }); await expect(save()).resolves.toBe(true); });\n";
        assert_eq!(shape(before), shape(after));
    }

    #[test]
    fn regex_alternatives_are_split_at_the_top_level() {
        let assertion =
            only_assertion(r"it('x', () => { expect(s).not.toMatch(/a|b(c|d)|\|e|[|]f/i) })");
        let matcher = assertion.matcher.expect("has a matcher");
        let Value::Regex {
            alternatives,
            flags,
            ..
        } = &matcher.args[0]
        else {
            panic!("not a regex: {:?}", matcher.args[0]);
        };
        assert_eq!(alternatives, &["a", "b(c|d)", r"\|e", "[|]f"]);
        assert_eq!(flags, "i");
        assert!(assertion.negated);
    }

    #[test]
    fn simple_constants_are_filled_in() {
        let source = "
const PLAN = 'pro';
const LIMITS = { seats: 3 };
const computed = makePlan();
let changing = 1;
it('x', () => {
  const local = 42;
  expect(plan).toEqual([PLAN, LIMITS, local, computed, changing]);
});
";
        let assertion = only_assertion(source);
        assert_eq!(
            assertion.to_string(),
            "expect(plan).toEqual(['pro', { seats: 3 }, 42, computed, changing])"
        );
    }

    /// The first test's items as text, followed by their context if any.
    fn first_test_items(source: &str) -> Vec<String> {
        let ir = parse_file("a.test.ts", source).expect("parses");
        ir.tests[0]
            .body
            .items
            .iter()
            .map(|item| {
                let text = match &item.kind {
                    ItemKind::Assertion(assertion) => assertion.to_string(),
                    other => format!("{other:?}"),
                };
                format!("{text} {}", item.context).trim_end().to_owned()
            })
            .collect()
    }

    /// Each test as `id [modifiers]`.
    fn tests_with_modifiers(path: &str, source: &str) -> Vec<String> {
        let ir = parse_file(path, source).expect("parses");
        ir.tests
            .iter()
            .map(|test| {
                let modifiers: Vec<String> =
                    test.modifiers.iter().map(ToString::to_string).collect();
                format!("{} [{}]", test.id(), modifiers.join(", "))
            })
            .collect()
    }

    #[test]
    fn constants_are_not_mixed_up_with_variables_of_the_same_name() {
        let source = "
const PLAN = 'free';
const ROLE = 'admin';
describe('d', () => {
  const ROLE = 'user';
  it.each(['pro'])('upgrades %s', (PLAN) => {
    expect(role).toBe(ROLE);
    expect(plan).toBe(PLAN);
    for (const ROLE of roles) expect(role).toBe(ROLE);
  });
});
";
        assert_eq!(
            first_test_items(source),
            [
                "expect(role).toBe('user')",
                "expect(plan).toBe(PLAN)",
                "expect(role).toBe(ROLE) in loop",
            ]
        );
    }

    #[test]
    fn arrays_and_objects_that_may_change_are_left_as_written() {
        let source = "
const built = [];
const sent = { id: 1 };
const rows = [['pro', 10]];
const wanted = { id: 1 };
it.each(rows)('x', () => {
  built.push(1);
  send(sent);
  expect(a).toEqual([built, sent, wanted, expect.objectContaining(wanted)]);
});
";
        let ir = parse_file("a.test.ts", source).expect("parses");
        let test = &ir.tests[0];
        assert_eq!(test.modifiers[0].to_string(), "each([['pro', 10]])");
        let assertion = test.body.assertions().next().expect("one assertion");
        assert_eq!(
            assertion.to_string(),
            "expect(a).toEqual([built, sent, { id: 1 }, expect.objectContaining({ id: 1 })])"
        );
    }

    #[test]
    fn templates_ignore_formatting_but_not_content() {
        let names = |source: &str| -> Vec<String> {
            let ir = parse_file("a.test.ts", source).expect("parses");
            ir.tests.iter().map(Test::id).collect()
        };
        assert_eq!(
            names("it(`saves ${ plan.name }`, () => {})"),
            names("it(`saves ${plan.name}`, () => {})")
        );
        assert_ne!(
            names("it(`a\nb ${x}`, () => {})"),
            names("it(`a b ${x}`, () => {})")
        );

        let aligned = "it.each`\n  plan     | price\n  ${'pro'} | ${10}\n`('t', () => {})";
        let squashed = "it.each`plan|price\n${'pro'}|${10}`('t', () => {})";
        assert_eq!(
            tests_with_modifiers("a.test.ts", aligned),
            ["t [each([{ plan: 'pro', price: 10 }])]"]
        );
        assert_eq!(
            tests_with_modifiers("a.test.ts", aligned),
            tests_with_modifiers("a.test.ts", squashed)
        );
    }

    #[test]
    fn playwright_skips_cover_the_whole_describe_but_not_helpers() {
        let source = "
function skipWebkit(browserName) { test.skip(browserName === 'webkit'); }
test.describe('g', () => {
  test('above', async () => {});
  test.skip(isMobile);
  test.describe.only('inner', () => { test('nested', async () => {}); });
});
test('outside', async () => {});
";
        assert_eq!(
            tests_with_modifiers("a.spec.ts", source),
            [
                "g > above [skip(isMobile)]",
                "g > inner > nested [skip(isMobile), only]",
                "outside []",
            ]
        );
    }

    #[test]
    fn returning_from_a_step_is_not_an_early_return() {
        let source = "
it('x', async () => {
  await test.step('s', async () => { return 1; });
  expect(a).toBe(1);
});
";
        assert_eq!(first_test_items(source), ["expect(a).toBe(1)"]);
    }

    #[test]
    fn returned_checks_are_awaited_only_when_the_runner_waits() {
        let source = "
it('a', () => expect(p).resolves.toBe(1));
it('b', async () => { await test.step('s', () => expect(p).resolves.toBe(1)); });
it('c', () => { setTimeout(() => expect(p).resolves.toBe(1)); });
it('d', () => { items.forEach((p) => { return expect(p).resolves.toBe(1); }); });
";
        let ir = parse_file("a.test.ts", source).expect("parses");
        let awaited: Vec<bool> = ir
            .tests
            .iter()
            .map(|test| test.body.assertions().next().expect("a check").awaited)
            .collect();
        assert_eq!(awaited, [true, true, false, false]);
    }

    #[test]
    fn an_awaited_retry_runs_as_part_of_the_test() {
        let source = "
it('x', async () => {
  await vi.waitFor(() => { expect(a).toBe(1); });
  await waitFor(() => expect(b).toBe(2));
  vi.waitFor(() => { expect(c).toBe(3); });
  await vi.waitUntil(() => { if (!d) return false; expect(d).toBe(4); return true; });
});
";
        assert_eq!(
            first_test_items(source),
            [
                "expect(a).toBe(1)",
                // `waitFor` waits for what its function returns.
                "await expect(b).toBe(2)",
                // Not awaited: the test may end before it fails.
                "expect(c).toBe(3) in callback",
                "expect(d).toBe(4)",
            ]
        );
    }

    #[test]
    fn a_try_swallows_a_failure_only_with_a_catch_or_a_finally_that_returns() {
        let source = "
it('x', () => {
  try { expect(a).toBe(1); } catch {}
  try { expect(b).toBe(2); } finally { cleanup(); }
  try { expect(c).toBe(3); } finally { for (const x of xs) { if (x) break; } }
  for (const r of rows) { try { expect(r).toBe(4); } finally { continue; } }
  try { expect(e).toBe(5); } finally { return; }
});
";
        // A `finally` that returns (or `continue`s out of it) drops the failure too.
        assert_eq!(
            first_test_items(source),
            [
                "expect(a).toBe(1) in try",
                "expect(b).toBe(2)",
                "expect(c).toBe(3)",
                "expect(r).toBe(4) in loop, in try",
                "expect(e).toBe(5) in try",
                "EarlyReturn",
            ]
        );
    }

    #[test]
    fn checks_in_a_switch_case_are_conditional() {
        let source = "it('x', () => { switch (k) { case 1: expect(a).toBe(1); } });";
        assert_eq!(first_test_items(source), ["expect(a).toBe(1) in if"]);
    }

    #[test]
    fn a_bare_soft_or_poll_expect_is_still_listed() {
        let source = "it('x', () => { expect.soft(a); expect.poll(() => b); });";
        let ir = parse_file("a.spec.ts", source).expect("parses");
        let checks: Vec<String> = ir.tests[0]
            .body
            .assertions()
            .map(ToString::to_string)
            .collect();
        assert_eq!(checks, ["expect.soft(a)", "expect.poll(() => b)"]);
    }

    #[test]
    fn calls_that_only_look_like_tests_are_ignored() {
        let source = "
test.use({ locale: 'en' });
test.describe.configure({ mode: 'serial' });
test.extend({ page: async () => {} });
";
        let ir = parse_file("a.spec.ts", source).expect("parses");
        assert!(ir.tests.is_empty());
        assert!(ir.outside.items.is_empty());
    }

    #[test]
    fn only_js_and_ts_files_are_parsed() {
        assert_eq!(
            parse_file("notes.md", "# hi"),
            Err(ParseError::UnsupportedFile)
        );
    }
}
