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
//! }], &[]);
//! assert_eq!(findings[0].rule, RuleId::TestDisabled);
//! assert_eq!(findings[0].message, "now skipped (skip)");
//! # Ok::<(), assertrust_ir::ParseError>(())
//! ```

mod changed_check;
mod check_cant_fail;
mod check_removed;
mod gate_loosened;
mod looser;
mod strength;
mod test_disabled;

use std::fmt;

use assertrust_ir::{Assertion, Gate, Item, ItemKind, Test, Value, Via};
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

/// A CI or test config file before and after a change. A side is `None` when the file was
/// added or deleted.
#[derive(Debug, Clone, Copy, Default)]
pub struct GateFile<'a> {
    pub before: Option<GateSide<'a>>,
    pub after: Option<GateSide<'a>>,
}

#[derive(Debug, Clone, Copy)]
pub struct GateSide<'a> {
    pub path: &'a str,
    pub gate: &'a Gate,
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
    /// The test's id, after the change (before it, for a deleted test), with its file in
    /// front when that isn't `path` (a finding in a helper file). `None` when the finding
    /// covers several tests.
    pub test: Option<String>,
    /// One line saying what got weaker.
    pub message: String,
    /// The code before the change, one item per line: removed checks, a test's modifiers, …
    pub before: Vec<String>,
    /// The code after the change, the same way.
    pub after: Vec<String>,
}

impl Finding {
    /// Points the finding at another file (a helper's): the test then says which file it's
    /// in, `test/a.test.ts: refunds`.
    fn move_to(&mut self, path: &str) {
        if path == self.path {
            return;
        }
        if let Some(test) = &mut self.test {
            *test = format!("{}: {test}", self.path);
        }
        path.clone_into(&mut self.path);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleId {
    /// A check disappeared from a test that still exists.
    CheckRemoved,
    /// A test no longer runs, or no longer runs everywhere.
    TestDisabled,
    /// A check is still there but may no longer run or fail.
    CheckCantFail,
    /// A matcher moved down the strength ladder: `toEqual(x)` → `toBeTruthy()`.
    MatcherLoosened,
    /// A matcher swapped for one at the same level, or any matcher change under `.not`.
    MatcherSwapped,
    /// The same matcher accepts more: fields, alternatives or `.each` rows dropped.
    ExpectationLoosened,
    /// The expected outcome changed, neither looser nor stricter: `toBe(3)` → `toBe(4)`.
    OutcomeChanged,
    /// CI or test config lets more failures through: `|| true`, retries raised, a pattern
    /// added to `exclude`.
    GateLoosened,
}

impl RuleId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckRemoved => "check-removed",
            Self::TestDisabled => "test-disabled",
            Self::CheckCantFail => "check-cant-fail",
            Self::MatcherLoosened => "matcher-loosened",
            Self::MatcherSwapped => "matcher-swapped",
            Self::ExpectationLoosened => "expectation-loosened",
            Self::OutcomeChanged => "outcome-changed",
            Self::GateLoosened => "gate-loosened",
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

/// Runs every rule on one change: its test files and its CI and test config files.
/// Findings come most sure first, then by file and line.
pub fn check(files: &[TestFile], gates: &[GateFile]) -> Vec<Finding> {
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
    changed_check::check(&change, &mut findings);
    gate_loosened::check(gates, &mut findings);
    let mut findings = merge(findings);
    findings.sort_by(|a, b| {
        (a.confidence, &a.path, a.line, a.rule).cmp(&(b.confidence, &b.path, b.line, b.rule))
    });
    findings
}

/// One finding for what several tests share: a check removed from a helper they all call
/// is one finding in the helper, `(in 3 tests)`, not one per test.
fn merge(findings: Vec<Finding>) -> Vec<Finding> {
    let mut merged: Vec<(Finding, Vec<String>)> = Vec::new();
    for finding in findings {
        let same = merged.iter_mut().find(|(other, _)| {
            other.test.is_some()
                && finding.test.is_some()
                && (
                    other.rule,
                    other.confidence,
                    &other.path,
                    other.line,
                    &other.message,
                ) == (
                    finding.rule,
                    finding.confidence,
                    &finding.path,
                    finding.line,
                    &finding.message,
                )
        });
        match same {
            Some((_, tests)) => {
                let test = finding.test.expect("checked above");
                if !tests.contains(&test) {
                    tests.push(test);
                }
            }
            None => {
                let tests = finding.test.iter().cloned().collect();
                merged.push((finding, tests));
            }
        }
    }
    merged
        .into_iter()
        .map(|(mut finding, tests)| {
            if tests.len() > 1 {
                finding.test = None;
                finding
                    .message
                    .push_str(&format!(" (in {} tests)", tests.len()));
            }
            finding
        })
        .collect()
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

    /// A finding about `new`, an item of the test after the change in `pair`, that was `old`.
    ///
    /// An item that runs through a helper (or hook) is reported where it changed: at the
    /// call in the test when the call changed (`expectStatus(res, 200)` → `expectStatus(res,
    /// 500)`, or moved into an `if`), else in the helper, so every test calling it shares
    /// one finding.
    fn at_item(
        &self,
        pair: &TestPair,
        old: Option<&Item>,
        new: &Item,
        rule: RuleId,
        confidence: Confidence,
    ) -> Finding {
        let mut finding = self.at(pair, new.line, rule, confidence);
        match changed_call(old, new) {
            // The same calls all the way: the change is where the item is written.
            None => {
                if let Some(path) = new.path() {
                    finding.move_to(path);
                }
            }
            // The call at `level` changed, in the test (0) or in the helper above it.
            Some(level) => {
                finding.line = Some(new.via[level].line);
                if let Some(path) = level
                    .checked_sub(1)
                    .and_then(|above| new.via[above].path.as_deref())
                {
                    finding.move_to(path);
                }
            }
        }
        finding
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

/// An item as code, with where it sits when that matters and how it runs for the test:
/// `expect(a).toBe(1) (in if) via expectOk(res)`.
fn item_text(item: &Item) -> String {
    let mut text = match &item.kind {
        ItemKind::Assertion(assertion) => assertion.to_string(),
        ItemKind::AssertionCount(Some(n)) => format!("expect.assertions({n})"),
        ItemKind::AssertionCount(None) => "expect.hasAssertions()".to_owned(),
        ItemKind::EarlyReturn => "return".to_owned(),
        ItemKind::RuntimeSkip(Some(condition)) => format!("skip({condition})"),
        ItemKind::RuntimeSkip(None) => "skip()".to_owned(),
        ItemKind::Call(call) => call.text.clone(),
    };
    if !item.context.is_plain() {
        text.push_str(&format!(" ({})", item.context));
    }
    if let Some(via) = item.via_text() {
        text.push(' ');
        text.push_str(&via);
    }
    text
}

/// The same calls, by text, line and file: the same way to an item on one side of a change.
fn same_way(a: &[Via], b: &[Via]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| (&a.call, a.line, &a.path) == (&b.call, b.line, &b.path))
}

/// The first call on the way to `new` that's different from the way to `old`: 0 for the
/// call in the test, 1 for the call inside that helper, and so on. `None` when they're all
/// the same, so whatever changed is where the item is written.
fn changed_call(old: Option<&Item>, new: &Item) -> Option<usize> {
    (0..new.via.len()).find(|&level| {
        let call = &new.via[level];
        old.and_then(|old| old.via.get(level)).is_none_or(|was| {
            (&was.name, &was.args, was.context, was.awaited)
                != (&call.name, &call.args, call.context, call.awaited)
        })
    })
}

/// A number literal's value.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.parse().ok(),
        _ => None,
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
