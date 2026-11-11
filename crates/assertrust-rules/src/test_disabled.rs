//! `test-disabled`: a test no longer runs, or no longer runs everywhere.
//!
//! ```text
//! before: it('refunds a payment', …)
//! after:  it.skip('refunds a payment', …)
//! → now skipped (skip)
//! ```
//!
//! Also: a skip decided while the test runs (`test.skip(cond)`, `ctx.skip()`), a committed
//! `.only` (the rest of the file stops running), a test deleted without its checks moving
//! elsewhere, and a test file deleted or renamed to a name the test runner doesn't pick up.

use std::collections::BTreeMap;

use assertrust_ir::{ItemKind, Modifier, Test};
use assertrust_match::DeletedTest;

use crate::{
    Change, Confidence, Finding, RuleId, TestFile, count, item_text, never_runs, short, skipped,
};

pub(crate) fn check(change: &Change, findings: &mut Vec<Finding>) {
    modifiers(change, findings);
    runtime_skips(change, findings);
    for (index, file) in change.files.iter().enumerate() {
        only(file, findings);
        deleted(change, index, file, findings);
    }
}

/// A modifier that stops a test from running (or from failing), as `(how sure, what it
/// does)`: `skipped (skip)`. `only` is handled per file, since it stops the other tests.
fn disabling(modifier: &Modifier) -> Option<(Confidence, String)> {
    let name = modifier.name.as_str();
    let condition = modifier.arg.as_ref().map(|arg| short(&arg.to_string()));
    Some(match (name, condition) {
        ("skip" | "fixme", None) => (Confidence::High, format!("skipped ({name})")),
        ("todo", _) => (
            Confidence::High,
            "marked todo, so it doesn't run".to_owned(),
        ),
        ("fails" | "fail", None) => (
            Confidence::High,
            format!("expected to fail ({name}), so it passes only if it fails"),
        ),
        // Playwright's `test.skip(cond)` in a `describe`, or vitest's `skipIf`.
        ("skip" | "fixme" | "skipIf", Some(condition)) => (
            Confidence::Medium,
            format!("skipped when {condition} ({name})"),
        ),
        ("runIf", Some(condition)) => (
            Confidence::Medium,
            format!("skipped unless {condition} (runIf)"),
        ),
        ("fail", Some(condition)) => (
            Confidence::Medium,
            format!("expected to fail when {condition} (fail)"),
        ),
        _ => return None,
    })
}

/// Tests that gained a disabling modifier. Tests in one file that gained the same one (a
/// `describe.skip`) make one finding.
fn modifiers(change: &Change, findings: &mut Vec<Finding>) {
    let mut groups: BTreeMap<(usize, String), Vec<(&Test, &Test)>> = BTreeMap::new();
    for pair in &change.matching.pairs {
        let (before, after) = (change.before(pair.before), change.after(pair.after));
        // Swapping one skip for another (`skip` → `todo`) stops nothing that ran.
        if never_runs(before) {
            continue;
        }
        for modifier in &after.modifiers {
            if disabling(modifier).is_some() && !before.modifiers.contains(modifier) {
                groups
                    .entry((pair.after.file, modifier.to_string()))
                    .or_default()
                    .push((before, after));
            }
        }
    }
    for ((file, key), tests) in groups {
        let (_, first) = tests[0];
        let modifier = first
            .modifiers
            .iter()
            .find(|modifier| modifier.to_string() == key)
            .expect("grouped by one of its modifiers");
        let (confidence, what) = disabling(modifier).expect("checked above");
        let message = match tests.len() {
            1 => format!("now {what}"),
            n => format!("{} now {what}", count(n, "test")),
        };
        findings.push(Finding {
            rule: RuleId::TestDisabled,
            confidence,
            path: change.after_side(file).path.to_owned(),
            line: Some(first.line),
            test: (tests.len() == 1).then(|| first.id()),
            tests: if tests.len() > 1 {
                tests.iter().map(|(_, after)| after.id()).collect()
            } else {
                Vec::new()
            },
            message,
            before: tests
                .iter()
                .map(|(before, _)| with_modifiers(before))
                .collect(),
            after: tests
                .iter()
                .map(|(_, after)| with_modifiers(after))
                .collect(),
        });
    }
}

/// `billing > refunds [skip]`.
fn with_modifiers(test: &Test) -> String {
    let modifiers: Vec<String> = test.modifiers.iter().map(ToString::to_string).collect();
    format!("{} [{}]", test.id(), modifiers.join(", "))
}

/// Skips decided while a test runs: `test.skip(cond)` or `ctx.skip()` added to its body.
fn runtime_skips(change: &Change, findings: &mut Vec<Finding>) {
    for pair in &change.matching.pairs {
        let (before, after) = (change.before(pair.before), change.after(pair.after));
        // Already not running, or skipped by a modifier, which `modifiers` reports.
        if never_runs(before) || skipped(after) {
            continue;
        }
        let mut old: Vec<&ItemKind> = before
            .body
            .items
            .iter()
            .map(|item| &item.kind)
            .filter(|kind| matches!(kind, ItemKind::RuntimeSkip(_)))
            .collect();
        for item in &after.body.items {
            let ItemKind::RuntimeSkip(condition) = &item.kind else {
                continue;
            };
            // Skips that were already there, one for one.
            if let Some(at) = old.iter().position(|kind| *kind == &item.kind) {
                old.remove(at);
                continue;
            }
            let (confidence, message) = match condition {
                Some(condition) => (
                    Confidence::Medium,
                    format!("now skipped while running when {}", short(condition)),
                ),
                None if !item.context.is_plain() => (
                    Confidence::Medium,
                    format!("now skipped while running ({})", item.context),
                ),
                None => (Confidence::High, "now skipped while running".to_owned()),
            };
            let mut finding = change.at(pair, item.line, RuleId::TestDisabled, confidence);
            finding.message = message;
            finding.after = vec![item_text(item)];
            findings.push(finding);
        }
    }
}

/// A `.only` committed in a file that had none: the file's other tests stop running. A new
/// file is left alone, since none of its tests ran before.
fn only(file: &TestFile, findings: &mut Vec<Finding>) {
    let (Some(before), Some(after)) = (file.before, file.after) else {
        return;
    };
    let has_only = |test: &&Test| test.modifiers.iter().any(|m| m.name == "only");
    if before.ir.tests.iter().any(|test| has_only(&test)) {
        return;
    }
    let focused: Vec<&Test> = after.ir.tests.iter().filter(has_only).collect();
    let others = after.ir.tests.len() - focused.len();
    let Some(first) = focused.first() else {
        return;
    };
    if others == 0 {
        return;
    }
    let message = if others == 1 {
        "only added: the other test in this file won't run".to_owned()
    } else {
        format!("only added: the other {others} tests in this file won't run")
    };
    findings.push(Finding {
        rule: RuleId::TestDisabled,
        confidence: Confidence::High,
        path: after.path.to_owned(),
        line: Some(first.line),
        test: (focused.len() == 1).then(|| first.id()),
        tests: Vec::new(),
        message,
        before: Vec::new(),
        after: focused.iter().map(|test| with_modifiers(test)).collect(),
    });
}

/// How many tests deleted from one file in one change make one finding instead of one each.
const GROUP_DELETED: usize = 4;

/// Tests deleted from a file without all their checks reappearing elsewhere. A file that's
/// gone (deleted, or renamed out of the suite) makes one finding, and so do 4 or more tests
/// deleted from one file; otherwise each test does.
fn deleted(change: &Change, index: usize, file: &TestFile, findings: &mut Vec<Finding>) {
    let Some(before) = file.before else {
        return;
    };
    let test = |deleted: &DeletedTest| &before.ir.tests[deleted.test.test];
    // A test that didn't run (`it.todo('…')`) stops nothing when it goes.
    let lost: Vec<&DeletedTest> = change
        .matching
        .deleted
        .iter()
        .filter(|deleted| {
            deleted.test.file == index && !deleted.all_moved() && !never_runs(test(deleted))
        })
        .collect();
    if lost.is_empty() {
        return;
    }

    let Some(after) = file.after else {
        let tests = match lost.len() {
            1 => "its test no longer runs".to_owned(),
            n => format!("its {n} tests no longer run"),
        };
        let message = match file.renamed_out {
            Some(path) => format!("renamed to {path}, which isn't a test file name: {tests}"),
            None => format!("file deleted: {tests}"),
        };
        findings.push(Finding {
            rule: RuleId::TestDisabled,
            confidence: Confidence::High,
            path: before.path.to_owned(),
            line: None,
            test: None,
            tests: lost.iter().map(|deleted| test(deleted).id()).collect(),
            message,
            before: lost.iter().map(|deleted| test(deleted).id()).collect(),
            after: Vec::new(),
        });
        return;
    };

    // A file that also gained tests matching couldn't pair may have had this one rewritten
    // under a new name and body.
    let gained_tests = change
        .matching
        .added
        .iter()
        .any(|added| added.file == index);
    let confidence = if gained_tests {
        Confidence::Medium
    } else {
        Confidence::High
    };
    // Many tests deleted at once (a feature removed with its tests) are one finding.
    if lost.len() >= GROUP_DELETED {
        let first = test(lost[0]).id();
        findings.push(Finding {
            rule: RuleId::TestDisabled,
            confidence,
            path: after.path.to_owned(),
            line: None,
            test: None,
            tests: lost.iter().map(|deleted| test(deleted).id()).collect(),
            message: format!("deleted {} tests, including {}", lost.len(), short(&first)),
            before: lost.iter().map(|deleted| test(deleted).id()).collect(),
            after: Vec::new(),
        });
        return;
    }
    for deleted in lost {
        let id = test(deleted).id();
        let moved = deleted
            .checks
            .iter()
            .filter(|check| check.moved_to.is_some())
            .count();
        let message = if moved == 0 {
            format!("deleted: {id}")
        } else {
            format!(
                "deleted: {id} (only {moved} of its {} moved to other tests)",
                count(deleted.checks.len(), "check")
            )
        };
        findings.push(Finding {
            rule: RuleId::TestDisabled,
            confidence,
            path: after.path.to_owned(),
            line: None,
            test: Some(id),
            tests: Vec::new(),
            message,
            before: deleted
                .checks
                .iter()
                .filter(|check| check.moved_to.is_none())
                .map(|check| item_text(&test(deleted).body.items[check.item]))
                .collect(),
            after: Vec::new(),
        });
    }
}
