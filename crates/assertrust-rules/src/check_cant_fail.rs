//! `check-cant-fail`: a check is still there, but may no longer run or fail.
//!
//! ```text
//! before: expect(res.status).toBe(200)
//! after:  if (res) { expect(res.status).toBe(200) }
//! → expect(res.status).toBe(200) is now inside an if: it may not run
//! ```
//!
//! Also: an early `return` added before checks, `await` dropped from a check that returns a
//! promise, a matcher dropped (`expect(x)` checks nothing), and `expect.assertions(n)`
//! removed or lowered.

use std::collections::BTreeMap;

use assertrust_ir::{Assertion, Context, ExpectKind, Item, ItemKind, Test, Value, Via};
use assertrust_match::TestPair;

use crate::{
    Change, Confidence, Finding, RuleId, assertion, count, item_text, never_runs, number, short,
};

pub(crate) fn check(change: &Change, findings: &mut Vec<Finding>) {
    for pair in &change.matching.pairs {
        let (before, after) = (change.before(pair.before), change.after(pair.after));
        if never_runs(after) {
            continue;
        }
        let mut found = Found {
            change,
            pair,
            findings: &mut *findings,
            moved_calls: Vec::new(),
        };
        for check in &pair.checks.pairs {
            for &item in &check.after {
                found.context(before, after, &check.before, item);
                found.matcher_dropped(before, after, &check.before, item);
            }
            if let ([old], [new]) = (check.before.as_slice(), check.after.as_slice()) {
                found.await_dropped(&before.body.items[*old], after, *new);
            }
        }
        found.moved_calls();
        found.early_return(before, after);
        found.assertion_count(before, after);
    }
}

/// Adds findings about one test pair.
struct Found<'f, 'c, 'a> {
    change: &'f Change<'c, 'a>,
    pair: &'f TestPair,
    findings: &'f mut Vec<Finding>,
    /// Calls to helpers that moved somewhere new (into an `if`, a loop, …), with the checks
    /// they run: one finding each, not one per check.
    moved_calls: Vec<MovedCall<'a>>,
}

struct MovedCall<'a> {
    /// The call's line and text in the test.
    at: (u32, &'a str),
    /// Where the call now sits that it didn't.
    places: Context,
    /// The first check it runs, before and after.
    old: Option<&'a Item>,
    new: &'a Item,
    before: Vec<String>,
    after: Vec<String>,
}

impl<'a> Found<'_, '_, 'a> {
    fn push(
        &mut self,
        line: u32,
        confidence: Confidence,
        message: String,
        before: Vec<String>,
        after: Vec<String>,
    ) {
        let mut finding = self
            .change
            .at(self.pair, line, RuleId::CheckCantFail, confidence);
        finding.message = message;
        finding.before = before;
        finding.after = after;
        self.findings.push(finding);
    }

    /// A finding about the check `new` (that was `old`), where it changed: in the test, or
    /// in the helper it runs through.
    fn push_item(
        &mut self,
        old: Option<&Item>,
        new: &Item,
        confidence: Confidence,
        message: String,
        before: Vec<String>,
    ) {
        let mut finding =
            self.change
                .at_item(self.pair, old, new, RuleId::CheckCantFail, confidence);
        finding.message = message;
        finding.before = before;
        finding.after = vec![item_text(new)];
        self.findings.push(finding);
    }

    /// The check at `item` sits somewhere new that may stop it from running or failing.
    fn context(&mut self, before: &'a Test, after: &'a Test, old: &[usize], item: usize) {
        // What every old check had: one plain check folded into one in an `if` is now in
        // an `if`.
        let was = old
            .iter()
            .map(|&old| before.body.items[old].context)
            .reduce(both)
            .unwrap_or_default();
        let new_item = &after.body.items[item];
        let first = old.first().map(|&old| &before.body.items[old]);
        let added = minus(new_item.context, was);
        // The call to the helper it runs through moved: reported once for the call.
        if let Some(call) = new_item.via.first()
            && !added.is_plain()
            && minus(added, call_context(call)).is_plain()
            && first.and_then(|old| old.via.first()).is_none_or(|was| {
                (&was.call, was.context, was.awaited) != (&call.call, call.context, call.awaited)
            })
        {
            let texts = |items: &mut dyn Iterator<Item = &Item>| items.map(item_text).collect();
            let at = (call.line, call.call.as_str());
            match self.moved_calls.iter_mut().find(|moved| moved.at == at) {
                Some(moved) => {
                    moved.places = moved.places.with(added);
                    moved
                        .before
                        .extend(old.iter().map(|&old| item_text(&before.body.items[old])));
                    moved.after.push(item_text(new_item));
                }
                None => self.moved_calls.push(MovedCall {
                    at,
                    places: added,
                    old: first,
                    new: new_item,
                    before: texts(&mut old.iter().map(|&old| &before.body.items[old])),
                    after: vec![item_text(new_item)],
                }),
            }
            return;
        }
        let places = places(added);
        let Some(&(_, consequence, _)) = places.first() else {
            return;
        };
        let confidence = places
            .iter()
            .map(|&(_, _, confidence)| confidence)
            .min()
            .expect("at least one place");
        let wheres: Vec<&str> = places.iter().map(|&(place, _, _)| place).collect();
        let message = format!(
            "{}{} is now {}: {consequence}",
            short(&assertion(after, item).to_string()),
            in_helper(new_item),
            wheres.join(" and ")
        );
        let before = old
            .iter()
            .map(|&old| item_text(&before.body.items[old]))
            .collect();
        self.push_item(first, new_item, confidence, message, before);
    }

    /// One finding per call to a helper that moved somewhere new: `expectOk(res) is now
    /// inside an if, with its 2 checks: it may not run`.
    fn moved_calls(&mut self) {
        for moved in std::mem::take(&mut self.moved_calls) {
            let places = places(moved.places);
            let Some(&(_, consequence, _)) = places.first() else {
                continue;
            };
            let confidence = places
                .iter()
                .map(|&(_, _, confidence)| confidence)
                .min()
                .expect("at least one place");
            // The call itself isn't awaited, rather than sitting in a helper that isn't.
            let wheres: Vec<&str> = places
                .iter()
                .map(|&(place, _, _)| {
                    if place == NOT_AWAITED {
                        "not awaited"
                    } else {
                        place
                    }
                })
                .collect();
            let checks = if moved.after.len() == 1 {
                "its check".to_owned()
            } else {
                format!("its {}", count(moved.after.len(), "check"))
            };
            let message = format!(
                "{} is now {}, with {checks}: {consequence}",
                short(moved.at.1),
                wheres.join(" and ")
            );
            let mut finding = self.change.at_item(
                self.pair,
                moved.old,
                moved.new,
                RuleId::CheckCantFail,
                confidence,
            );
            finding.message = message;
            finding.before = moved.before;
            finding.after = moved.after;
            self.findings.push(finding);
        }
    }

    /// `expect(x).toBe(1)` → `expect(x)`: a bare `expect` checks nothing.
    fn matcher_dropped(&mut self, before: &Test, after: &Test, old: &[usize], item: usize) {
        let new = assertion(after, item);
        let had_matcher = old
            .iter()
            .any(|&old| assertion(before, old).matcher.is_some());
        if new.matcher.is_some() || !had_matcher {
            return;
        }
        let new_item = &after.body.items[item];
        let first = old.first().map(|&old| &before.body.items[old]);
        let before = old
            .iter()
            .map(|&old| item_text(&before.body.items[old]))
            .collect();
        self.push_item(
            first,
            new_item,
            Confidence::High,
            format!(
                "{}{} has no matcher: it checks nothing",
                short(&new.to_string()),
                in_helper(new_item)
            ),
            before,
        );
    }

    /// `await expect(p).resolves.toBe(1)` → `expect(p).resolves.toBe(1)`: the test may end
    /// before the check fails.
    fn await_dropped(&mut self, old_item: &Item, after: &Test, item: usize) {
        let ItemKind::Assertion(old) = &old_item.kind else {
            return;
        };
        let new = assertion(after, item);
        if !old.awaited || new.awaited || !returns_promise(new) {
            return;
        }
        let new_item = &after.body.items[item];
        self.push_item(
            Some(old_item),
            new_item,
            Confidence::High,
            format!(
                "await dropped from {}{}: the test may end before it fails",
                short(&new.to_string()),
                in_helper(new_item)
            ),
            vec![item_text(old_item)],
        );
    }

    /// A `return` added before checks that used to run, or one that used to be inside an
    /// `if` and no longer is.
    fn early_return(&mut self, before: &Test, after: &Test) {
        // The checks now behind a `return`, by the `return` they're behind.
        let mut behind: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for check in &self.pair.checks.pairs {
            let was = check
                .before
                .iter()
                .map(|&old| blocked(before, return_before(before, old)))
                .min()
                .unwrap_or(Blocked::No);
            for &item in &check.after {
                let at = return_before(after, item);
                if let Some(at) = at
                    && blocked(after, Some(at)) > was
                {
                    behind.entry(at).or_default().push(item);
                }
            }
        }
        for (at, items) in behind {
            let ret = &after.body.items[at];
            let checks = count(items.len(), "check");
            let (confidence, message) = if ret.context.is_plain() {
                let them = if items.len() == 1 {
                    "it never runs"
                } else {
                    "they never run"
                };
                (
                    Confidence::High,
                    format!("a return now comes before {checks}: {them}"),
                )
            } else {
                let them = if items.len() == 1 { "it" } else { "they" };
                (
                    Confidence::Medium,
                    format!(
                        "a return ({}) now comes before {checks}: {them} may not run",
                        ret.context
                    ),
                )
            };
            let after_lines = std::iter::once(item_text(ret))
                .chain(items.iter().map(|&item| item_text(&after.body.items[item])))
                .collect();
            self.push(ret.line, confidence, message, Vec::new(), after_lines);
        }
    }

    /// `expect.assertions(n)` removed, lowered, or swapped for `expect.hasAssertions()`.
    fn assertion_count(&mut self, before: &Test, after: &Test) {
        let Some((old_item, old)) = assertion_count(before) else {
            return;
        };
        let new = assertion_count(after);
        let message = match (old, new) {
            (_, None) => format!("{} removed", item_text(old_item)),
            (Some(old), Some((_, None))) => {
                format!("expect.assertions({old}) replaced with expect.hasAssertions()")
            }
            (Some(old), Some((_, Some(new)))) => match (number(old), number(new)) {
                (Some(o), Some(n)) if n < o => {
                    format!("expect.assertions lowered from {old} to {new}")
                }
                _ => return,
            },
            (None, Some(_)) => return,
        };
        let line = new.map_or(after.line, |(item, _)| item.line);
        let after_lines = new.map(|(item, _)| item_text(item)).into_iter().collect();
        self.push(
            line,
            Confidence::Medium,
            message,
            vec![item_text(old_item)],
            after_lines,
        );
    }
}

const NOT_AWAITED: &str = "in an async helper that isn't awaited";

/// Where a check can sit that stops it from running or failing, as `(where, what that
/// means, how sure)`, most serious first.
fn places(context: Context) -> Vec<(&'static str, &'static str, Confidence)> {
    let all = [
        (
            context.not_awaited,
            NOT_AWAITED,
            "it may run after the test ends",
            Confidence::Medium,
        ),
        (
            context.in_callback,
            "inside a callback",
            "it may run after the test ends, or not at all",
            Confidence::Medium,
        ),
        (
            context.in_catch,
            "inside a catch",
            "it runs only if something throws",
            Confidence::Medium,
        ),
        (
            context.in_try,
            "inside a try with a catch",
            "its failure may be caught",
            Confidence::Medium,
        ),
        (
            context.in_if,
            "inside an if",
            "it may not run",
            Confidence::Medium,
        ),
        (
            context.short_circuit,
            "after &&, || or ??",
            "it may not run",
            Confidence::Medium,
        ),
        (
            context.in_loop,
            "inside a loop",
            "it doesn't run if the loop runs zero times",
            Confidence::Low,
        ),
    ];
    all.into_iter()
        .filter(|&(set, ..)| set)
        .map(|(_, place, consequence, confidence)| (place, consequence, confidence))
        .collect()
}

/// What `a` and `b` both have.
fn both(a: Context, b: Context) -> Context {
    Context {
        in_if: a.in_if && b.in_if,
        in_loop: a.in_loop && b.in_loop,
        in_try: a.in_try && b.in_try,
        in_catch: a.in_catch && b.in_catch,
        in_callback: a.in_callback && b.in_callback,
        short_circuit: a.short_circuit && b.short_circuit,
        not_awaited: a.not_awaited && b.not_awaited,
    }
}

/// What `a` has that `b` doesn't.
fn minus(a: Context, b: Context) -> Context {
    Context {
        in_if: a.in_if && !b.in_if,
        in_loop: a.in_loop && !b.in_loop,
        in_try: a.in_try && !b.in_try,
        in_catch: a.in_catch && !b.in_catch,
        in_callback: a.in_callback && !b.in_callback,
        short_circuit: a.short_circuit && !b.short_circuit,
        not_awaited: a.not_awaited && !b.not_awaited,
    }
}

/// Where a call to a helper sits, as its checks see it: a call that isn't awaited leaves
/// them not awaited.
fn call_context(call: &Via) -> Context {
    Context {
        not_awaited: !call.awaited,
        ..call.context
    }
}

/// ` in expectOk` for a check written in a helper (or ` in beforeEach`), empty for one
/// written in the test.
fn in_helper(item: &Item) -> String {
    item.via
        .last()
        .map_or_else(String::new, |via| format!(" in {}", via.name))
}

/// The early `return` before `item` in the test's body that's most sure to stop it: the
/// first unconditional one, else the first one inside an `if`.
fn return_before(test: &Test, item: usize) -> Option<usize> {
    // `afterEach` and `afterAll` run however the test ends.
    if test.body.items[item]
        .via
        .first()
        .is_some_and(|via| matches!(via.name.as_str(), "afterEach" | "afterAll"))
    {
        return None;
    }
    let mut returns = test.body.items[..item]
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item.kind, ItemKind::EarlyReturn));
    let first = returns.next()?;
    let unconditional = std::iter::once(first)
        .chain(returns)
        .find(|(_, item)| item.context.is_plain());
    Some(unconditional.unwrap_or(first).0)
}

/// Whether a `return` stops a check: not at all, sometimes, or always.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Blocked {
    No,
    Sometimes,
    Always,
}

fn blocked(test: &Test, at: Option<usize>) -> Blocked {
    match at {
        None => Blocked::No,
        Some(at) if test.body.items[at].context.is_plain() => Blocked::Always,
        Some(_) => Blocked::Sometimes,
    }
}

/// The test's first `expect.assertions(n)` / `expect.hasAssertions()`, with `n`.
fn assertion_count(test: &Test) -> Option<(&assertrust_ir::Item, Option<&Value>)> {
    test.body.items.iter().find_map(|item| match &item.kind {
        ItemKind::AssertionCount(n) => Some((item, n.as_ref())),
        _ => None,
    })
}

/// Playwright's matchers that wait and return a promise: without `await`, they never fail
/// the test.
const ASYNC_MATCHERS: &[&str] = &[
    "toBeAttached",
    "toBeChecked",
    "toBeDisabled",
    "toBeEditable",
    "toBeEmpty",
    "toBeEnabled",
    "toBeFocused",
    "toBeHidden",
    "toBeInViewport",
    "toBeOK",
    "toBeVisible",
    "toContainClass",
    "toContainText",
    "toHaveAccessibleDescription",
    "toHaveAccessibleErrorMessage",
    "toHaveAccessibleName",
    "toHaveAttribute",
    "toHaveClass",
    "toHaveCount",
    "toHaveCSS",
    "toHaveId",
    "toHaveJSProperty",
    "toHaveRole",
    "toHaveScreenshot",
    "toHaveText",
    "toHaveTitle",
    "toHaveURL",
    "toHaveValue",
    "toHaveValues",
    "toMatchAriaSnapshot",
    "toPass",
];

/// The check returns a promise, so it only fails the test when awaited.
fn returns_promise(assertion: &Assertion) -> bool {
    assertion.promise.is_some()
        || assertion.expect == ExpectKind::Poll
        || assertion
            .matcher
            .as_ref()
            .is_some_and(|matcher| ASYNC_MATCHERS.contains(&matcher.name.as_str()))
}
