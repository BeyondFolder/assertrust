//! Matching (`docs/architecture.md`, step 4): which test after a change is the same test as
//! before it, and which check inside it is the same check, even when the test was renamed,
//! moved or refactored.
//!
//! Matching only pairs. Whether a pair got weaker is up to the rules (step 5).
//!
//! ```
//! use assertrust_match::{FilePair, Side, TestMatch, match_files};
//!
//! let before = assertrust_ir::parse_file("a.test.ts", "it('adds', () => { expect(add(1, 2)).toBe(3) })")?;
//! let after = assertrust_ir::parse_file("a.test.ts", "it('adds two numbers', () => { expect(add(1, 2)).toBe(3) })")?;
//! let matching = match_files(&[FilePair {
//!     before: Some(Side { path: "a.test.ts", ir: &before }),
//!     after: Some(Side { path: "a.test.ts", ir: &after }),
//! }]);
//! assert!(matches!(matching.pairs[0].how, TestMatch::Similar(_)));
//! # Ok::<(), assertrust_ir::ParseError>(())
//! ```

mod check_pairing;
mod similarity;
mod test_pairing;

use assertrust_ir::{FileIr, ItemKind, Test};

/// One version of a test file.
#[derive(Debug, Clone, Copy)]
pub struct Side<'a> {
    pub path: &'a str,
    pub ir: &'a FileIr,
}

/// A test file before and after a change. A side is `None` when the file was added or
/// deleted, or doesn't parse on that side.
#[derive(Debug, Clone, Copy, Default)]
pub struct FilePair<'a> {
    pub before: Option<Side<'a>>,
    pub after: Option<Side<'a>>,
}

/// A test, by the index of its file pair and its index in that side's [`FileIr::tests`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TestRef {
    pub file: usize,
    pub test: usize,
}

/// What matching found in one change.
#[derive(Debug, Default)]
pub struct Matching {
    /// Tests that exist before and after, in the order of the tests before.
    pub pairs: Vec<TestPair>,
    /// Tests before with no partner after.
    pub deleted: Vec<DeletedTest>,
    /// Tests after with no partner before. Adding a test is never a problem.
    pub added: Vec<TestRef>,
}

#[derive(Debug)]
pub struct TestPair {
    pub before: TestRef,
    pub after: TestRef,
    pub how: TestMatch,
    pub checks: CheckMatching,
}

/// Why two tests were paired. A pair across two files is a test moved to another file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TestMatch {
    /// Same `describe` path and name.
    SameId,
    /// Same name under a different `describe` path (one was renamed, or the test moved).
    SameName,
    /// Names and bodies alike enough, with the score (0 to 1).
    Similar(f64),
}

/// How the checks of a test pair line up. Checks are indices into the test's
/// [`Body::items`](assertrust_ir::Body::items).
#[derive(Debug, Default)]
pub struct CheckMatching {
    pub pairs: Vec<CheckPair>,
    pub removed: Vec<RemovedCheck>,
    pub added: Vec<usize>,
}

/// Checks before and the checks they became. Usually one each; a rewrite can fold several
/// checks into one (`expect(r.a).toBe(1); expect(r.b).toBe(2)` →
/// `expect(r).toMatchObject({ a: 1, b: 2 })`) or split one into several.
#[derive(Debug)]
pub struct CheckPair {
    pub before: Vec<usize>,
    pub after: Vec<usize>,
    pub how: CheckMatch,
}

/// Why two checks were paired, in the order the steps run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMatch {
    /// The same check.
    Identical,
    /// Same subject and matcher kind: `toMatchObject({ a, b })` → `toMatchObject({ a })`.
    SameSubjectAndKind,
    /// Same subject, another matcher: `toEqual({ id: 1 })` → `toBeTruthy()`.
    SameSubject,
    /// Written differently, but states a fact in common:
    /// `expect(r.text).toBe(v)` → `expect(r).toMatchObject({ text: v })`.
    SameFact,
    /// A distinctive expected value in common, with the subject refactored:
    /// `expect(fields.text).toBe('Hi there')` → `expect(textOf(html)).toBe('Hi there')`.
    SameExpected,
    /// Same matcher, a subject that shares words with the old one, and the same or a similar
    /// expected value: `expect(hasEnforcement(result)).toBe(false)` →
    /// `expect(result.enforced).toBe(false)`.
    Refactored,
}

/// A check before with no partner after. It may have moved to another test in the same
/// change, which isn't a removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovedCheck {
    pub item: usize,
    /// The test after the change that has the same check, if any.
    pub moved_to: Option<TestRef>,
}

/// A test before with no partner after. If every check in it moved elsewhere (the test was
/// split, or merged into another), it isn't a deletion.
#[derive(Debug)]
pub struct DeletedTest {
    pub test: TestRef,
    pub checks: Vec<RemovedCheck>,
}

impl DeletedTest {
    /// True when every check reappears in other tests.
    pub fn all_moved(&self) -> bool {
        self.checks.iter().all(|check| check.moved_to.is_some())
    }
}

/// Pairs the tests and checks in one change.
pub fn match_files(files: &[FilePair]) -> Matching {
    let (pairs, deleted, added) = test_pairing::pair_tests(files);

    let mut matching = Matching::default();
    for (before, after, how) in pairs {
        let checks =
            check_pairing::pair_checks(test(files, before, Before), test(files, after, After));
        matching.pairs.push(TestPair {
            before,
            after,
            how,
            checks,
        });
    }
    matching.pairs.sort_by_key(|pair| pair.before);
    matching.added = added;
    matching.deleted = deleted
        .into_iter()
        .map(|test_ref| DeletedTest {
            test: test_ref,
            checks: assertion_items(test(files, test_ref, Before))
                .map(|item| RemovedCheck {
                    item,
                    moved_to: None,
                })
                .collect(),
        })
        .collect();
    check_pairing::find_moves(files, &mut matching);
    matching
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum When {
    Before,
    After,
}
use When::{After, Before};

/// The test `test_ref` points to, on one side.
pub(crate) fn test<'a>(files: &[FilePair<'a>], test_ref: TestRef, when: When) -> &'a Test {
    let side = match when {
        Before => files[test_ref.file].before,
        After => files[test_ref.file].after,
    };
    &side
        .expect("a test ref points to an existing side")
        .ir
        .tests[test_ref.test]
}

/// The indices of a test's checks in its body.
pub(crate) fn assertion_items(test: &Test) -> impl Iterator<Item = usize> + '_ {
    test.body
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Assertion(_)))
        .map(|(index, _)| index)
}
