//! Rules (`docs/architecture.md`, step 5): whether a change made a test weaker, how, and how
//! sure we are. Every rule reads what matching (step 4) paired up. None of them needs an AI
//! model or an API key.
//!
//! ```
//! use assertrust_rules::{RuleId, Side, TestFile, check};
//!
//! let before = assertrust_ir::parse_file("a.test.ts", "it('adds', () => { expect(add(1, 2)).toBe(3) })")?;
//! let after = assertrust_ir::parse_file("a.test.ts", "it.skip('adds', () => { expect(add(1, 2)).toBe(3) })")?;
//! let findings = check(&[TestFile {
//!     before: Some(Side { path: "a.test.ts", ir: &before }),
//!     after: Some(Side { path: "a.test.ts", ir: &after }),
//!     renamed_out: None,
//! }]);
//! assert_eq!(findings[0].rule, RuleId::TestDisabled);
//! assert_eq!(findings[0].message, "now skipped (skip)");
//! # Ok::<(), assertrust_ir::ParseError>(())
//! ```

mod check_cant_fail;
mod check_removed;
mod test_disabled;

use std::fmt;

use assertrust_ir::{Assertion, Item, ItemKind, Test};
use assertrust_match::{FilePair, Matching, TestPair, TestRef, match_files};

pub use assertrust_match::Side;

/// A test file before and after a change.
#[derive(Debug, Clone, Copy, Default)]
pub struct TestFile<'a> {
    /// `None` when the file was added, or its old name isn't a test file name (so its tests
    /// didn't run).
    pub before: Option<Side<'a>>,
    /// `None` when the file was deleted, or renamed to a name that isn't a test file name.
    pub after: Option<Side<'a>>,
    /// The new name, when the file was renamed so the test runner no longer picks it up:
    /// `a.test.ts` → `a.test.ts.disabled`. `after` is then `None`.
    pub renamed_out: Option<&'a str>,
}

/// One way a change made a test weaker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: RuleId,
    pub confidence: Confidence,
    /// The file to look at: after the change, or before it when the file is gone.
    pub path: String,
    /// The line after the change. `None` when what the finding is about is gone: a deleted
    /// test or file.
    pub line: Option<u32>,
    /// The test's id, after the change (before it, for a deleted test). `None` when the
    /// finding covers several tests.
    pub test: Option<String>,
    /// One line saying what got weaker.
    pub message: String,
    /// The code before the change, one item per line: removed checks, a test's modifiers, …
    pub before: Vec<String>,
    /// The code after the change, the same way.
    pub after: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleId {
    /// A check disappeared from a test that still exists.
    CheckRemoved,
    /// A test no longer runs, or no longer runs everywhere.
    TestDisabled,
    /// A check is still there but may no longer run or fail.
    CheckCantFail,
}

impl RuleId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckRemoved => "check-removed",
            Self::TestDisabled => "test-disabled",
            Self::CheckCantFail => "check-cant-fail",
        }
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How sure a rule is that the test got weaker. Sorts most sure first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Runs every rule on one change. Findings come most sure first, then by file and line.
pub fn check(files: &[TestFile]) -> Vec<Finding> {
    let pairs: Vec<FilePair> = files
        .iter()
        .map(|file| FilePair {
            before: file.before,
            after: file.after,
        })
        .collect();
    let matching = match_files(&pairs);
    let change = Change {
        files,
        pairs: &pairs,
        matching: &matching,
    };

    let mut findings = Vec::new();
    check_removed::check(&change, &mut findings);
    test_disabled::check(&change, &mut findings);
    check_cant_fail::check(&change, &mut findings);
    findings.sort_by(|a, b| {
        (a.confidence, &a.path, a.line, a.rule).cmp(&(b.confidence, &b.path, b.line, b.rule))
    });
    findings
}

/// One change, as the rules see it.
struct Change<'c, 'a> {
    files: &'c [TestFile<'a>],
    pairs: &'c [FilePair<'a>],
    matching: &'c Matching,
}

impl<'a> Change<'_, 'a> {
    fn before(&self, test_ref: TestRef) -> &'a Test {
        &self.pairs[test_ref.file]
            .before
            .expect("a test before the change has a side before")
            .ir
            .tests[test_ref.test]
    }

    fn after(&self, test_ref: TestRef) -> &'a Test {
        &self.after_side(test_ref.file).ir.tests[test_ref.test]
    }

    fn after_side(&self, file: usize) -> Side<'a> {
        self.pairs[file]
            .after
            .expect("a test after the change has a side after")
    }

    /// A finding about the test after the change in `pair`, at `line`.
    fn at(&self, pair: &TestPair, line: u32, rule: RuleId, confidence: Confidence) -> Finding {
        Finding {
            rule,
            confidence,
            path: self.after_side(pair.after.file).path.to_owned(),
            line: Some(line),
            test: Some(self.after(pair.after).id()),
            message: String::new(),
            before: Vec::new(),
            after: Vec::new(),
        }
    }
}

/// The test is skipped for good by a modifier: `skip`, `todo` or `fixme`, with no condition.
fn skipped(test: &Test) -> bool {
    test.modifiers.iter().any(|modifier| {
        matches!(modifier.name.as_str(), "skip" | "todo" | "fixme") && modifier.arg.is_none()
    })
}

/// The test's checks don't run: it's skipped for good, or it skips itself while running
/// with no condition (`ctx.skip()` outside any `if`). What changes inside such a test
/// doesn't matter, and `test-disabled` reports the skip.
fn never_runs(test: &Test) -> bool {
    skipped(test)
        || test
            .body
            .items
            .iter()
            .any(|item| matches!(item.kind, ItemKind::RuntimeSkip(None)) && item.context.is_plain())
}

/// The check at `item` in a test's body.
fn assertion(test: &Test, item: usize) -> &Assertion {
    match &test.body.items[item].kind {
        ItemKind::Assertion(assertion) => assertion,
        _ => unreachable!("matching pairs only checks"),
    }
}

/// An item as code, with where it sits when that matters: `expect(a).toBe(1) (in if)`.
fn item_text(item: &Item) -> String {
    let text = match &item.kind {
        ItemKind::Assertion(assertion) => assertion.to_string(),
        ItemKind::AssertionCount(Some(n)) => format!("expect.assertions({n})"),
        ItemKind::AssertionCount(None) => "expect.hasAssertions()".to_owned(),
        ItemKind::EarlyReturn => "return".to_owned(),
        ItemKind::RuntimeSkip(Some(condition)) => format!("skip({condition})"),
        ItemKind::RuntimeSkip(None) => "skip()".to_owned(),
    };
    if item.context.is_plain() {
        text
    } else {
        format!("{text} ({})", item.context)
    }
}

/// Code shortened to fit in a one-line message.
fn short(code: &str) -> String {
    const MAX: usize = 60;
    if code.chars().count() <= MAX {
        code.to_owned()
    } else {
        let cut: String = code.chars().take(MAX - 1).collect();
        format!("{cut}…")
    }
}

/// `1 test`, `2 tests`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}
