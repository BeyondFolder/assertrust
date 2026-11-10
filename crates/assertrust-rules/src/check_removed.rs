//! `check-removed`: a check disappeared from a test that still exists, and didn't reappear
//! in another test in the same change.
//!
//! ```text
//! before: it('puts the reply into the text or preview', () => { …; expect(fields.preview).toContain('Hi') })
//! after:  it('puts the reply into the text', () => { … })
//! → removed expect(fields.preview).toContain('Hi'); the test name dropped "or preview"
//! ```
//!
//! One finding per test, listing what it lost. A check that ran through a helper is
//! reported as the call removed from the test (`removed expectOk(res), which ran 2
//! checks`), or as the check removed from the helper, once for every test that calls it.

use assertrust_ir::{Item, ItemKind, Test};
use assertrust_match::{TestPair, dropped_words};

use crate::{Change, Confidence, Finding, RuleId, assertion, count, item_text, never_runs, short};

pub(crate) fn check(change: &Change, findings: &mut Vec<Finding>) {
    for pair in &change.matching.pairs {
        let (before, after) = (change.before(pair.before), change.after(pair.after));
        if never_runs(after) {
            continue;
        }
        let (removed, linked): (Vec<usize>, Vec<usize>) = pair
            .checks
            .removed
            .iter()
            .filter(|check| check.moved_to.is_none())
            .map(|check| check.item)
            .partition(|&item| before.body.items[item].via.is_empty());
        through_helpers(change, pair, before, after, &linked, findings);
        let Some(&first) = removed.first() else {
            continue;
        };

        let first = short(&assertion(before, first).to_string());
        let mut message = match removed.len() {
            1 => format!("removed {first}"),
            n => format!("removed {}, including {first}", count(n, "check")),
        };
        if after.body.assertions().next().is_none() {
            message.push_str("; the test has no checks left");
        }
        if before.name != after.name {
            let dropped = dropped_words(&before.name, &after.name);
            if dropped.is_empty() {
                message.push_str("; the test was renamed");
            } else {
                message.push_str(&format!(
                    "; the test name dropped \"{}\"",
                    dropped.join(" ")
                ));
            }
        }

        // A test that also gained checks matching couldn't pair was rewritten: the new
        // checks may cover the old ones in a way matching can't see.
        let confidence = if pair.checks.added.is_empty() {
            Confidence::High
        } else {
            Confidence::Medium
        };
        let mut finding = change.at(pair, after.line, RuleId::CheckRemoved, confidence);
        finding.message = message;
        finding.before = removed
            .iter()
            .map(|&item| item_text(&before.body.items[item]))
            .collect();
        finding.after = pair
            .checks
            .added
            .iter()
            .map(|&item| item_text(&after.body.items[item]))
            .collect();
        findings.push(finding);
    }
}

/// Checks that ran through a helper (or hook) and are gone: the test stopped calling the
/// helper, or the helper lost them.
fn through_helpers(
    change: &Change,
    pair: &TestPair,
    before: &Test,
    after: &Test,
    removed: &[usize],
    findings: &mut Vec<Finding>,
) {
    // By the call in the test they ran through.
    let mut by_call: Vec<(&assertrust_ir::Via, Vec<&Item>)> = Vec::new();
    for &item in removed {
        let item = &before.body.items[item];
        let call = &item.via[0];
        match by_call
            .iter_mut()
            .find(|(other, _)| (&other.call, other.line) == (&call.call, call.line))
        {
            Some((_, items)) => items.push(item),
            None => by_call.push((call, vec![item])),
        }
    }
    for (call, items) in by_call {
        if calls(after, &call.name) < calls(before, &call.name) {
            let mut finding = change.at(pair, after.line, RuleId::CheckRemoved, Confidence::High);
            // The checks it ran, including ones that have a counterpart after the change.
            let ran = before
                .body
                .items
                .iter()
                .filter(|item| {
                    matches!(item.kind, ItemKind::Assertion(_))
                        && item
                            .via
                            .first()
                            .is_some_and(|via| (&via.call, via.line) == (&call.call, call.line))
                })
                .count();
            finding.message = if ran == items.len() {
                format!(
                    "removed {}, which ran {}",
                    short(&call.call),
                    count(items.len(), "check")
                )
            } else {
                format!(
                    "removed {}; {} of the {ran} checks it ran {} gone",
                    short(&call.call),
                    items.len(),
                    if items.len() == 1 { "is" } else { "are" }
                )
            };
            finding.before = items.iter().map(|item| item_text(item)).collect();
            findings.push(finding);
            continue;
        }
        for item in items {
            let helper = &item.via.last().expect("linked").name;
            let mut finding = change.at(pair, item.line, RuleId::CheckRemoved, Confidence::High);
            if let Some(path) = item.path() {
                finding.path = path.to_owned();
            }
            let check = match &item.kind {
                ItemKind::Assertion(assertion) => assertion.to_string(),
                _ => item_text(item),
            };
            finding.message = format!("removed {} from {helper}", short(&check));
            finding.before = vec![item_text(item)];
            findings.push(finding);
        }
    }
}

/// How many times the test calls the helper `name` itself (hooks run once per test).
fn calls(test: &Test, name: &str) -> usize {
    test.body
        .items
        .iter()
        .filter(|item| {
            item.via.is_empty() && matches!(&item.kind, ItemKind::Call(call) if call.name == name)
        })
        .count()
}
