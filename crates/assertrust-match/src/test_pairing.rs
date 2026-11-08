//! Pairing tests (`docs/architecture.md`, step 4, "Pairing tests, in order").

use std::collections::BTreeMap;

use crate::similarity::{MIN_SCORE, best_pairs, test_score};
use crate::{After, Before, FilePair, TestMatch, TestRef, test};

type Pairs = Vec<(TestRef, TestRef, TestMatch)>;

/// Pairs tests before and after: within each file first, then across files. Returns the
/// pairs, the tests left before (deleted) and the tests left after (added).
pub(crate) fn pair_tests(files: &[FilePair]) -> (Pairs, Vec<TestRef>, Vec<TestRef>) {
    let mut pairs = Vec::new();
    let mut left_before = Vec::new();
    let mut left_after = Vec::new();

    for (file, pair) in files.iter().enumerate() {
        let refs = |count: usize| (0..count).map(move |test| TestRef { file, test });
        let mut before: Vec<TestRef> =
            refs(pair.before.map_or(0, |side| side.ir.tests.len())).collect();
        let mut after: Vec<TestRef> =
            refs(pair.after.map_or(0, |side| side.ir.tests.len())).collect();
        run_passes(files, &mut before, &mut after, true, &mut pairs);
        left_before.extend(before);
        left_after.extend(after);
    }

    // Tests moved to another file. A test can't move within the file it's in: that was
    // handled above.
    run_passes(files, &mut left_before, &mut left_after, false, &mut pairs);
    (pairs, left_before, left_after)
}

/// The three passes, each on what the previous ones left. Within a file, duplicate ids are
/// paired in order; across files, an id must be unique on each side.
fn run_passes(
    files: &[FilePair],
    before: &mut Vec<TestRef>,
    after: &mut Vec<TestRef>,
    within_file: bool,
    pairs: &mut Pairs,
) {
    let id = |when| move |test_ref: TestRef| test(files, test_ref, when).id();
    let name = |when| move |test_ref: TestRef| test(files, test_ref, when).name.clone();

    pair_by_key(
        before,
        after,
        id(Before),
        id(After),
        TestMatch::SameId,
        within_file,
        pairs,
    );
    pair_by_key(
        before,
        after,
        name(Before),
        name(After),
        TestMatch::SameName,
        false,
        pairs,
    );

    let scores: Vec<Vec<f64>> = before
        .iter()
        .map(|&b| {
            after
                .iter()
                .map(|&a| test_score(test(files, b, Before), test(files, a, After)))
                .collect()
        })
        .collect();
    let found = best_pairs(&scores, MIN_SCORE);
    let (mut used_before, mut used_after) = (Vec::new(), Vec::new());
    for (b, a, score) in found {
        pairs.push((before[b], after[a], TestMatch::Similar(score)));
        used_before.push(before[b]);
        used_after.push(after[a]);
    }
    before.retain(|test_ref| !used_before.contains(test_ref));
    after.retain(|test_ref| !used_after.contains(test_ref));
}

/// Pairs tests with the same key. With `in_order`, several tests with the same key are
/// paired first to first, second to second (duplicate names in a file); without it, only
/// a key held by exactly one test on each side counts.
fn pair_by_key(
    before: &mut Vec<TestRef>,
    after: &mut Vec<TestRef>,
    key_before: impl Fn(TestRef) -> String,
    key_after: impl Fn(TestRef) -> String,
    how: TestMatch,
    in_order: bool,
    pairs: &mut Pairs,
) {
    let mut groups: BTreeMap<String, (Vec<TestRef>, Vec<TestRef>)> = BTreeMap::new();
    for &test_ref in before.iter() {
        groups
            .entry(key_before(test_ref))
            .or_default()
            .0
            .push(test_ref);
    }
    for &test_ref in after.iter() {
        groups
            .entry(key_after(test_ref))
            .or_default()
            .1
            .push(test_ref);
    }
    let mut found = Vec::new();
    for (olds, news) in groups.values() {
        if in_order || (olds.len() == 1 && news.len() == 1) {
            found.extend(olds.iter().zip(news).map(|(&b, &a)| (b, a)));
        }
    }
    before.retain(|test_ref| !found.iter().any(|(b, _)| b == test_ref));
    after.retain(|test_ref| !found.iter().any(|(_, a)| a == test_ref));
    pairs.extend(found.into_iter().map(|(b, a)| (b, a, how)));
}
