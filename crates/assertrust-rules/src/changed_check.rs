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
use assertrust_match::{CheckMatch, CheckPair, TestPair};

use crate::looser;
use crate::strength::Canonical;
use crate::{
    Change, Confidence, Finding, RuleId, assertion, count, item_text, never_runs, number, short,
};

pub(crate) fn check(change: &Change, findings: &mut Vec<Finding>) {
    for pair in &change.matching.pairs {
        let (before, after) = (change.before(pair.before), change.after(pair.after));
        if never_runs(after) {
            continue;
        }
        for check in &pair.checks.pairs {
            // The same check, maybe in a new place, which is `check-cant-fail`'s.
            if check.how == CheckMatch::Identical {
                continue;
            }
            let Some((rule, confidence, message)) = verdict(before, after, check) else {
                continue;
            };
            let old = check.before.first().map(|&item| &before.body.items[item]);
            let new = &after.body.items[check.after[0]];
            let mut finding = change.at_item(pair, old, new, rule, confidence);
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
    // Written another way: `toBeNull()` → `toBe(null)`.
    if o == n && old.subject == new.subject && old.promise == new.promise {
        return None;
    }
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
    if o.kind() == MatcherKind::Compare && n.kind() == MatcherKind::Compare {
        return comparison_changed(o, n);
    }
    // Within one family, a different expected value is a different outcome:
    // `toEqual({ preview })` → `toBe('Hi')`.
    if o.kind() == MatcherKind::Equal && n.kind() == MatcherKind::Equal && o.args != n.args {
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

/// One comparison swapped for another. Close to a value → greater or less than one is
/// looser; greater → less is a different outcome; `>` → `>=` is looser when the bound
/// moves outward or stays put.
fn comparison_changed(o: &Canonical, n: &Canonical) -> Option<Verdict> {
    let calls = format!("{} → {}", short(&o.call()), short(&n.call()));
    match (direction(o), direction(n)) {
        ("≈", "≈") => None,
        ("≈", _) => Some((
            RuleId::MatcherLoosened,
            Confidence::Medium,
            format!("{calls}: now {}", n.now_passes()),
        )),
        (_, "≈") => None,
        (old, new) if old != new => Some(outcome(calls)),
        (direction, _) => {
            let (Some(old_bound), Some(new_bound)) = (
                o.args.first().and_then(number),
                n.args.first().and_then(number),
            ) else {
                return Some((
                    RuleId::MatcherSwapped,
                    Confidence::Low,
                    format!("swapped {calls}"),
                ));
            };
            let inclusive = |c: &Canonical| c.name.ends_with("OrEqual");
            let outward = if direction == ">" {
                new_bound < old_bound
            } else {
                new_bound > old_bound
            };
            let same = new_bound == old_bound;
            if outward || same && inclusive(n) && !inclusive(o) {
                let confidence = if outward {
                    Confidence::Medium
                } else {
                    Confidence::Low
                };
                Some((
                    RuleId::ExpectationLoosened,
                    confidence,
                    format!("{calls}: the bound now lets more through"),
                ))
            } else {
                None
            }
        }
    }
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
    // Rows on one side and not the other, one for one, so a dropped duplicate counts.
    let dropped = minus(old, new);
    let added = minus(new, old);
    if dropped.is_empty() {
        return;
    }
    // Rows were edited or reshaped (a column added or removed) as well as dropped: only the
    // number of rows can be compared.
    let (rule, confidence, message) = if new.len() < old.len() && !added.is_empty() {
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

/// The rows of `from` that `to` doesn't have, one for one: `[a, a, b]` minus `[a, b]` is
/// `[a]`.
fn minus(from: &[Value], to: &[Value]) -> Vec<String> {
    let mut left: Vec<&Value> = to.iter().collect();
    let mut missing = Vec::new();
    for row in from {
        match left.iter().position(|other| *other == row) {
            Some(at) => {
                left.remove(at);
            }
            None => missing.push(row.to_string()),
        }
    }
    missing
}
