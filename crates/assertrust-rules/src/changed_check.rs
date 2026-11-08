//! The rules that compare a check with what it became: `matcher-loosened`,
//! `matcher-swapped`, `expectation-loosened` and `outcome-changed`. Each check pair gets at
//! most one of them, so they live together.
//!
//! ```text
//! before: expect(source).not.toMatch(/support@|resend|inboxes\.net/)
//! after:  expect(source).not.toMatch(/support@|inboxes\.net/)
//! → expectation-loosened: .not.toMatch no longer rules out resend
//! ```

use assertrust_ir::{Assertion, Fact, MatcherKind, Test, Value};
use assertrust_match::{CheckPair, TestPair};

use crate::looser;
use crate::strength::Canonical;
use crate::{Change, Confidence, Finding, RuleId, assertion, count, item_text, never_runs, short};

pub(crate) fn check(change: &Change, findings: &mut Vec<Finding>) {
    for pair in &change.matching.pairs {
        let (before, after) = (change.before(pair.before), change.after(pair.after));
        if never_runs(after) {
            continue;
        }
        for check in &pair.checks.pairs {
            let Some((rule, confidence, message)) = verdict(before, after, check) else {
                continue;
            };
            let line = after.body.items[check.after[0]].line;
            let mut finding = change.at(pair, line, rule, confidence);
            finding.message = message;
            finding.before = check
                .before
                .iter()
                .map(|&item| item_text(&before.body.items[item]))
                .collect();
            finding.after = check
                .after
                .iter()
                .map(|&item| item_text(&after.body.items[item]))
                .collect();
            findings.push(finding);
        }
        each_rows(change, pair, before, after, findings);
    }
}

type Verdict = (RuleId, Confidence, String);

fn verdict(before: &Test, after: &Test, check: &CheckPair) -> Option<Verdict> {
    let olds: Vec<&Assertion> = check
        .before
        .iter()
        .map(|&item| assertion(before, item))
        .collect();
    let news: Vec<&Assertion> = check
        .after
        .iter()
        .map(|&item| assertion(after, item))
        .collect();
    match (olds.as_slice(), news.as_slice()) {
        ([old], [new]) => one(old, new),
        _ => folded(&olds, &news),
    }
}

/// One check and what it became.
fn one(old: &Assertion, new: &Assertion) -> Option<Verdict> {
    // A bare `expect(x)` is `check-cant-fail`'s.
    let (o, n) = (Canonical::of(old)?, Canonical::of(new)?);
    // Part of the check moved into the subject or out of it, `expect(r.text).toBe(v)` →
    // `expect(r).toMatchObject({ text: v })`, `await expect(p).resolves.toBe(v)` →
    // `expect(await p).toBe(v)`: what it states is what counts.
    if old.subject != new.subject {
        let missing: Vec<&Fact> = old
            .facts
            .iter()
            .filter(|fact| !new.facts.contains(fact))
            .collect();
        let kept = missing.len() < old.facts.len();
        if !old.facts.is_empty() && missing.is_empty() {
            return None;
        }
        // Some of it is still stated, the rest isn't: `toMatchObject({ a: 1, b: 2 })` on `r`
        // → `toBe(1)` on `r.a`.
        if kept {
            let missing: Vec<String> = missing.iter().map(ToString::to_string).collect();
            return Some(no_longer_checks(&missing));
        }
    }
    if o.negated != n.negated {
        let what = if n.negated {
            ".not added"
        } else {
            ".not removed"
        };
        return Some(outcome(format!("{what}: {} → {}", o.call(), n.call())));
    }
    if old.promise != new.promise {
        return Some(outcome(format!("{old} → {new}")));
    }
    if o.name != n.name {
        return matcher_changed(&o, &n);
    }
    let diff = looser::compare(&o, &n);
    if diff.is_looser() {
        Some((
            RuleId::ExpectationLoosened,
            diff.confidence(),
            format!("{} {}", o.label(), diff.describe()),
        ))
    } else if diff.changed {
        Some(outcome(format!(
            "{} → {}",
            short(&o.call()),
            short(&n.call())
        )))
    } else {
        None
    }
}

/// A different matcher: down the ladder is looser, across it is a swap, up it is fine.
fn matcher_changed(o: &Canonical, n: &Canonical) -> Option<Verdict> {
    let swapped = || {
        Some((
            RuleId::MatcherSwapped,
            Confidence::Low,
            format!("swapped {} for {}", short(&o.call()), short(&n.call())),
        ))
    };
    // Under `.not`, a looser matcher makes the check stricter and the other way round, so
    // changes there aren't ranked.
    if o.negated {
        return swapped();
    }
    // Within one family, a different expected value is a different outcome:
    // `toEqual({ preview })` → `toBe('Hi')`, `toBeGreaterThan(t)` → `toBeLessThanOrEqual(t)`.
    let different = match (o.kind(), n.kind()) {
        (MatcherKind::Equal, MatcherKind::Equal) => o.args != n.args,
        (MatcherKind::Compare, MatcherKind::Compare) => direction(o) != direction(n),
        _ => false,
    };
    if different {
        return Some(outcome(format!(
            "{} → {}",
            short(&o.call()),
            short(&n.call())
        )));
    }
    match (o.level(), n.level()) {
        (Some(old_level), Some(new_level)) if new_level > old_level => Some((
            RuleId::MatcherLoosened,
            if new_level == 5 {
                Confidence::High
            } else {
                Confidence::Medium
            },
            format!(
                "{} → {}: now {}",
                short(&o.call()),
                short(&n.call()),
                n.now_passes()
            ),
        )),
        (Some(old_level), Some(new_level)) if new_level < old_level => None,
        // Deep equality made strict.
        _ if o.kind() == MatcherKind::Equal && n.name == "toStrictEqual" => None,
        _ => swapped(),
    }
}

/// Several checks folded into one, or one split into several: what they state together.
fn folded(olds: &[&Assertion], news: &[&Assertion]) -> Option<Verdict> {
    let new_facts: Vec<&Fact> = news.iter().flat_map(|new| &new.facts).collect();
    let missing: Vec<String> = olds
        .iter()
        .flat_map(|old| &old.facts)
        .filter(|fact| !new_facts.contains(fact))
        .map(ToString::to_string)
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(no_longer_checks(&missing))
}

/// Facts the old checks stated that the new ones don't.
fn no_longer_checks(missing: &[String]) -> Verdict {
    (
        RuleId::ExpectationLoosened,
        Confidence::High,
        format!("no longer checks {}", short(&missing.join(", "))),
    )
}

/// Which way a comparison goes: `>`/`>=`, `<`/`<=`, or close to.
fn direction(matcher: &Canonical) -> &'static str {
    if matcher.name.starts_with("toBeGreaterThan") {
        ">"
    } else if matcher.name.starts_with("toBeLessThan") {
        "<"
    } else {
        "≈"
    }
}

fn outcome(message: String) -> Verdict {
    (RuleId::OutcomeChanged, Confidence::Low, message)
}

/// `.each` rows dropped: the test runs for fewer cases.
fn each_rows(
    change: &Change,
    pair: &TestPair,
    before: &Test,
    after: &Test,
    findings: &mut Vec<Finding>,
) {
    let (Some((name, Value::Array(old))), Some((_, Value::Array(new)))) =
        (each_table(before), each_table(after))
    else {
        return;
    };
    let dropped: Vec<String> = old
        .iter()
        .filter(|row| !new.contains(row))
        .map(ToString::to_string)
        .collect();
    if dropped.is_empty() {
        return;
    }
    // No old row is left as it was: the table was reshaped (a column added or removed), so
    // only the number of rows can be compared.
    let reshaped = dropped.len() == old.len();
    let (rule, confidence, message) = if new.len() < old.len() && reshaped {
        (
            RuleId::ExpectationLoosened,
            Confidence::Medium,
            format!(
                ".{name} now runs {} rows instead of {}",
                new.len(),
                old.len()
            ),
        )
    } else if new.len() < old.len() {
        (
            RuleId::ExpectationLoosened,
            Confidence::High,
            format!(
                ".{name} no longer runs {}: {}",
                count(dropped.len(), "row"),
                short(&dropped.join(", "))
            ),
        )
    } else {
        outcome(format!(
            ".{name} rows changed: {}",
            short(&dropped.join(", "))
        ))
    };
    let mut finding = change.at(pair, after.line, rule, confidence);
    finding.message = message;
    finding.before = old.iter().map(ToString::to_string).collect();
    finding.after = new.iter().map(ToString::to_string).collect();
    findings.push(finding);
}

/// The test's `.each` / `.for` modifier name and table.
fn each_table(test: &Test) -> Option<(&str, &Value)> {
    test.modifiers
        .iter()
        .find(|modifier| matches!(modifier.name.as_str(), "each" | "for"))
        .and_then(|modifier| Some((modifier.name.as_str(), modifier.arg.as_ref()?)))
}
