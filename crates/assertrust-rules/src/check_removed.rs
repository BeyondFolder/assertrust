//! `check-removed`: a check disappeared from a test that still exists, and didn't reappear
//! in another test in the same change.
//!
//! ```text
//! before: it('puts the reply into the text or preview', () => { …; expect(fields.preview).toContain('Hi') })
//! after:  it('puts the reply into the text', () => { … })
//! → removed expect(fields.preview).toContain('Hi'); the test name dropped "or preview"
//! ```
//!
//! One finding per test, listing what it lost.

use assertrust_match::dropped_words;

use crate::{Change, Confidence, Finding, RuleId, assertion, count, item_text, never_runs, short};

pub(crate) fn check(change: &Change, findings: &mut Vec<Finding>) {
    for pair in &change.matching.pairs {
        let (before, after) = (change.before(pair.before), change.after(pair.after));
        if never_runs(after) {
            continue;
        }
        let removed: Vec<usize> = pair
            .checks
            .removed
            .iter()
            .filter(|check| check.moved_to.is_none())
            .map(|check| check.item)
            .collect();
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
