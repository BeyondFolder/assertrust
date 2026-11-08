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

mod js;
mod matcher;
mod model;
mod value;

use std::fmt;

pub use crate::matcher::MatcherKind;
pub use crate::model::{
    Assertion, Body, Context, ExpectKind, FileIr, Hook, HookKind, Item, ItemKind, Matcher,
    Modifier, Promise, Test,
};
pub use crate::value::Value;

/// Reads the tests and checks in one file. `path` picks the language from its extension
/// (`.ts`, `.tsx`, `.js`, `.jsx`, `.mts`, `.cts`, `.mjs`, `.cjs`).
pub fn parse_file(path: &str, source: &str) -> Result<FileIr, ParseError> {
    js::parse(path, source)
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
