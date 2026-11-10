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
//! checks`) or from a helper, or as the check removed from the helper, once for every test
//! that calls it.

use assertrust_ir::{Call, Item, ItemKind, Test, Via};
use assertrust_match::{TestPair, dropped_words};

use crate::{
    Change, Confidence, Finding, RuleId, assertion, count, item_text, never_runs, same_way, short,
};

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

/// Checks that ran through a helper (or hook) and are gone: a call on the way to them was
/// removed (from the test, or from a helper), or the helper lost them. Checks behind a call
/// that's still there but can't be followed (an alias, a package) aren't reported: what it
/// checks now can't be seen.
fn through_helpers(
    change: &Change,
    pair: &TestPair,
    before: &Test,
    after: &Test,
    removed: &[usize],
    findings: &mut Vec<Finding>,
) {
    // A test that also gained checks matching couldn't pair was rewritten, as for its own
    // checks.
    let confidence = if pair.checks.added.is_empty() {
        Confidence::High
    } else {
        Confidence::Medium
    };
    // Checks under a removed call, by the call: its level and the way to it.
    let mut by_call: Vec<(usize, &[Via], Vec<&Item>)> = Vec::new();
    for &item in removed {
        let item = &before.body.items[item];
        match gone(before, after, item) {
            Gone::Unseen => {}
            Gone::Call(level) => {
                let way = &item.via[..=level];
                match by_call
                    .iter_mut()
                    .find(|(other, other_way, _)| *other == level && same_way(other_way, way))
                {
                    Some((_, _, items)) => items.push(item),
                    None => by_call.push((level, way, vec![item])),
                }
            }
            Gone::FromHelper => {
                let helper = item.via.len() - 1;
                let (path, line) = defined_after(after, &item.via, helper);
                let mut finding = change.at(pair, line, RuleId::CheckRemoved, confidence);
                if let Some(path) = path {
                    finding.move_to(&path);
                }
                let check = match &item.kind {
                    ItemKind::Assertion(assertion) => assertion.to_string(),
                    _ => item_text(item),
                };
                finding.message =
                    format!("removed {} from {}", short(&check), item.via[helper].name);
                finding.before = vec![item_text(item)];
                findings.push(finding);
            }
        }
    }
    for (level, way, items) in by_call {
        let call = &way[level];
        // The checks it ran, including ones that have a counterpart after the change.
        let ran = before
            .body
            .items
            .iter()
            .filter(|item| {
                matches!(item.kind, ItemKind::Assertion(_))
                    && item.via.len() > level
                    && same_way(&item.via[..=level], way)
            })
            .count();
        let checks = if ran == items.len() {
            format!(", which ran {}", count(ran, "check"))
        } else {
            format!(
                "; {} of the {ran} checks it ran {} gone",
                items.len(),
                if items.len() == 1 { "is" } else { "are" }
            )
        };
        let mut finding = if level == 0 {
            let mut finding = change.at(pair, after.line, RuleId::CheckRemoved, confidence);
            finding.message = format!("removed {}{checks}", short(&call.call));
            if after.body.assertions().next().is_none() {
                finding.message.push_str("; the test has no checks left");
            }
            finding
        } else {
            // Removed from the helper above it: reported there.
            let (path, line) = defined_after(after, way, level - 1);
            let mut finding = change.at(pair, line, RuleId::CheckRemoved, confidence);
            if let Some(path) = path {
                finding.move_to(&path);
            }
            // By name: the arguments differ from test to test, and one finding covers them.
            finding.message = format!(
                "removed the call to {} from {}{checks}",
                call.name,
                way[level - 1].name
            );
            finding
        };
        finding.before = items.iter().map(|item| item_text(item)).collect();
        findings.push(finding);
    }
}

/// Why a check that ran through helpers is gone.
enum Gone {
    /// The call at this level on the way to it was removed.
    Call(usize),
    /// Every call on the way is still there: its helper lost it.
    FromHelper,
    /// A call on the way is still there but can't be followed any more.
    Unseen,
}

fn gone(before: &Test, after: &Test, item: &Item) -> Gone {
    for level in 0..item.via.len() {
        let was = calls(before, &item.via[..level], &item.via[level].name);
        let now = calls(after, &item.via[..level], &item.via[level].name);
        if now.len() < was.len() {
            return Gone::Call(level);
        }
        if now.iter().any(|call| call.defined.is_none()) {
            return Gone::Unseen;
        }
    }
    Gone::FromHelper
}

/// The calls to `name` in `test` made on the way `way` (by the helpers' names): the test's
/// own for an empty way. Hooks count as calls the test makes.
fn calls<'t>(test: &'t Test, way: &[Via], name: &str) -> Vec<&'t Call> {
    test.body
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Call(call)
                if call.name == name
                    && item.via.len() == way.len()
                    && item.via.iter().zip(way).all(|(a, b)| a.name == b.name) =>
            {
                Some(call)
            }
            _ => None,
        })
        .collect()
}

/// Where the helper at `level` on the way `way` is defined after the change, so a finding
/// about it points at the code as it is now: `(file, line)`, with no file for the test's
/// own. Falls back to where it was.
fn defined_after(after: &Test, way: &[Via], level: usize) -> (Option<String>, u32) {
    calls(after, &way[..level], &way[level].name)
        .into_iter()
        .find_map(|call| call.defined.clone())
        .map_or_else(
            || (way[level].path.clone(), way[level].defined),
            |defined| (defined.path, defined.line),
        )
}
