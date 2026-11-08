//! Pairing checks inside a paired test (`docs/architecture.md`, step 4, "Pairing checks
//! inside a paired test, in order"), and finding checks that moved to another test.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use assertrust_ir::{Assertion, Context, FactTest, ItemKind, MatcherKind, Test, Value};

use crate::similarity::{MARGIN, best_pairs, jaccard, subject_score, subjects_overlap};
use crate::{
    After, CheckMatch, CheckMatching, CheckPair, FilePair, Matching, RemovedCheck, TestRef,
    assertion_items, test,
};

/// The checks of one side of a test pair, and which are still unpaired.
struct Side<'a> {
    test: &'a Test,
    left: Vec<usize>,
}

impl<'a> Side<'a> {
    fn new(test: &'a Test) -> Self {
        Self {
            test,
            left: assertion_items(test).collect(),
        }
    }

    fn assertion(&self, item: usize) -> &'a Assertion {
        match &self.test.body.items[item].kind {
            ItemKind::Assertion(assertion) => assertion,
            _ => unreachable!("only assertion items are tracked"),
        }
    }

    fn context(&self, item: usize) -> Context {
        self.test.body.items[item].context
    }

    fn take(&mut self, items: &[usize]) {
        self.left.retain(|item| !items.contains(item));
    }
}

pub(crate) fn pair_checks(before: &Test, after: &Test) -> CheckMatching {
    let mut old = Side::new(before);
    let mut new = Side::new(after);
    let mut pairs = Vec::new();

    let any = |_: &Assertion, _: &Assertion| true;
    identical(&mut old, &mut new, &mut pairs);
    by_key(
        &mut old,
        &mut new,
        &mut pairs,
        CheckMatch::SameSubjectAndKind,
        any,
        |a| Some(format!("{}\u{0}{:?}", a.subject, kind(a))),
    );
    by_key(
        &mut old,
        &mut new,
        &mut pairs,
        CheckMatch::SameSubject,
        any,
        |a| Some(a.subject.clone()),
    );
    same_fact(&mut old, &mut new, &mut pairs);
    same_expected(&mut old, &mut new, &mut pairs);
    // The same matcher and arguments on a refactored subject, in order:
    // `normalize(x).domains` → `parse(x).domains`.
    let overlap = |a: &Assertion, b: &Assertion| subjects_overlap(&a.subject, &b.subject);
    by_key(
        &mut old,
        &mut new,
        &mut pairs,
        CheckMatch::Refactored,
        overlap,
        |a| {
            let matcher = a.matcher.as_ref()?;
            let args: Vec<String> = matcher.args.iter().map(ToString::to_string).collect();
            Some(format!("{}\u{0}{}", matcher.name, args.join("\u{0}")))
        },
    );
    refactored(&mut old, &mut new, &mut pairs);

    pairs.sort_by_key(|pair: &CheckPair| pair.before.first().copied());
    CheckMatching {
        pairs,
        removed: old
            .left
            .iter()
            .map(|&item| RemovedCheck {
                item,
                moved_to: None,
            })
            .collect(),
        added: new.left,
    }
}

fn kind(assertion: &Assertion) -> Option<MatcherKind> {
    assertion.matcher.as_ref().map(|matcher| matcher.kind)
}

/// Step 1: the same check, first to first. Checks in the same place (both plain, or both
/// in an `if`, …) pair first, so a check that moved into an `if` is the one left to pair
/// with a changed context, which the rules then see.
fn identical(old: &mut Side, new: &mut Side, pairs: &mut Vec<CheckPair>) {
    for same_place in [true, false] {
        for item in old.left.clone() {
            let assertion = old.assertion(item);
            let partner = new.left.iter().copied().find(|&n| {
                new.assertion(n) == assertion
                    && (!same_place || new.context(n) == old.context(item))
            });
            if let Some(partner) = partner {
                pairs.push(CheckPair {
                    before: vec![item],
                    after: vec![partner],
                    how: CheckMatch::Identical,
                });
                old.take(&[item]);
                new.take(&[partner]);
            }
        }
    }
}

/// Pairs checks with the same key. When both sides have the same number of checks with a
/// key, they're paired in order. When one side has one and the other several, the closest
/// check is picked, if it's clearly closest. Otherwise none are paired.
fn by_key(
    old: &mut Side,
    new: &mut Side,
    pairs: &mut Vec<CheckPair>,
    how: CheckMatch,
    allowed: impl Fn(&Assertion, &Assertion) -> bool,
    key: impl Fn(&Assertion) -> Option<String>,
) {
    let mut groups: BTreeMap<String, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for &item in &old.left {
        if let Some(key) = key(old.assertion(item)) {
            groups.entry(key).or_default().0.push(item);
        }
    }
    for &item in &new.left {
        if let Some(key) = key(new.assertion(item)) {
            groups.entry(key).or_default().1.push(item);
        }
    }
    for (olds, news) in groups.into_values() {
        let found: Vec<(usize, usize)> = if olds.len() == news.len() {
            olds.into_iter().zip(news).collect()
        } else if let ([one], many) = (olds.as_slice(), news.as_slice()) {
            closest(old.assertion(*one), many, new)
                .map(|n| vec![(*one, n)])
                .unwrap_or_default()
        } else if let (many, [one]) = (olds.as_slice(), news.as_slice()) {
            closest(new.assertion(*one), many, old)
                .map(|o| vec![(o, *one)])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for (o, n) in found {
            if !allowed(old.assertion(o), new.assertion(n)) {
                continue;
            }
            pairs.push(CheckPair {
                before: vec![o],
                after: vec![n],
                how,
            });
            old.take(&[o]);
            new.take(&[n]);
        }
    }
}

/// The candidate most like `assertion`, if no other comes close: by the words in the subject,
/// the expected value, then the same `.not` and the same matcher. So `toBe(200)` goes to
/// `toBe(201)` rather than `.not.toBe(500)` when all three check `res.status`.
fn closest(assertion: &Assertion, candidates: &[usize], side: &Side) -> Option<usize> {
    let alike = |other: &Assertion| {
        let same_matcher =
            assertion.matcher.as_ref().map(|m| &m.name) == other.matcher.as_ref().map(|m| &m.name);
        subject_score(&assertion.subject, &other.subject)
            + expected_score(assertion, other)
            + if assertion.negated == other.negated {
                0.25
            } else {
                0.0
            }
            + if same_matcher { 0.25 } else { 0.0 }
    };
    let mut scored: Vec<(f64, usize)> = candidates
        .iter()
        .map(|&item| (alike(side.assertion(item)), item))
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    match scored.as_slice() {
        [(best, item), (second, _), ..] if best - second >= MARGIN => Some(*item),
        _ => None,
    }
}

/// Step 4: checks written differently that state a fact in common. A check can share facts
/// with several on the other side, so this pairs groups: everything connected by a shared
/// fact.
fn same_fact(old: &mut Side, new: &mut Side, pairs: &mut Vec<CheckPair>) {
    let olds = old.left.clone();
    let news = new.left.clone();
    // Union-find over olds (0..n) then news (n..).
    let mut parent: Vec<usize> = (0..olds.len() + news.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (i, &o) in olds.iter().enumerate() {
        let facts = &old.assertion(o).facts;
        for (j, &n) in news.iter().enumerate() {
            if new
                .assertion(n)
                .facts
                .iter()
                .any(|fact| facts.contains(fact))
            {
                let (a, b) = (root(&mut parent, i), root(&mut parent, olds.len() + j));
                parent[a] = b;
            }
        }
    }
    let mut groups: BTreeMap<usize, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, &o) in olds.iter().enumerate() {
        groups.entry(root(&mut parent, i)).or_default().0.push(o);
    }
    for (j, &n) in news.iter().enumerate() {
        groups
            .entry(root(&mut parent, olds.len() + j))
            .or_default()
            .1
            .push(n);
    }
    for (before, after) in groups.into_values() {
        if before.is_empty() || after.is_empty() {
            continue;
        }
        old.take(&before);
        new.take(&after);
        pairs.push(CheckPair {
            before,
            after,
            how: CheckMatch::SameFact,
        });
    }
}

/// Step 5: the subject was refactored, but a distinctive expected value is still there:
/// `expect(fields.text).toBe('Hi there')` → `expect(textOf(html)).toBe('Hi there')`, or
/// `expect(parse(x)).toEqual({ text: 'Hi there', preview: … })` → `expect(textOf(x)).toBe('Hi there')`.
/// Pairs one to one. A check that could pair with several goes to the one with the closest
/// subject, if it's clearly closest.
fn same_expected(old: &mut Side, new: &mut Side, pairs: &mut Vec<CheckPair>) {
    let values = |side: &Side, item: usize| expected_values(side.assertion(item));
    let olds: Vec<(usize, Vec<String>)> = old.left.iter().map(|&o| (o, values(old, o))).collect();
    let news: Vec<(usize, Vec<String>)> = new.left.iter().map(|&n| (n, values(new, n))).collect();
    let shares = |a: &[String], b: &[String]| a.iter().any(|value| b.contains(value));
    let candidates = |values: &[String], others: &[(usize, Vec<String>)]| -> Vec<usize> {
        others
            .iter()
            .filter(|(_, theirs)| shares(values, theirs))
            .map(|(item, _)| *item)
            .collect()
    };

    let mut found: Vec<(usize, usize)> = Vec::new();
    for (o, values) in &olds {
        let for_old = candidates(values, &news);
        let chosen = match for_old.as_slice() {
            [] => continue,
            [n] => Some(*n),
            many => closest(old.assertion(*o), many, new),
        };
        let Some(n) = chosen else { continue };
        let n_values = &news
            .iter()
            .find(|(item, _)| *item == n)
            .expect("a candidate")
            .1;
        let for_new = candidates(n_values, &olds);
        let back = match for_new.as_slice() {
            [single] => Some(*single),
            many => closest(new.assertion(n), many, old),
        };
        if back == Some(*o) && !found.iter().any(|&(_, taken)| taken == n) {
            found.push((*o, n));
        }
    }
    for (o, n) in found {
        old.take(&[o]);
        new.take(&[n]);
        pairs.push(CheckPair {
            before: vec![o],
            after: vec![n],
            how: CheckMatch::SameExpected,
        });
    }
}

/// Step 7: a refactored subject with the same matcher kind, scored by how alike the
/// subjects are plus how alike the expected values are (0 to 2). A pair needs subjects that
/// share a word, and either a mostly matching expected value or a mostly matching subject:
/// `expect(rows.map(toId)).toEqual({ a: 1, b: 0 })` → `expect(tracked('a')).toEqual({ a: 1, b: 0, c: 0 })`.
fn refactored(old: &mut Side, new: &mut Side, pairs: &mut Vec<CheckPair>) {
    let scores: Vec<Vec<f64>> = old
        .left
        .iter()
        .map(|&o| {
            let before = old.assertion(o);
            new.left
                .iter()
                .map(|&n| {
                    let after = new.assertion(n);
                    if kind(before) != kind(after)
                        || !subjects_overlap(&before.subject, &after.subject)
                    {
                        return 0.0;
                    }
                    let subjects = subject_score(&before.subject, &after.subject);
                    let expected = expected_score(before, after);
                    if subjects >= 0.5 || expected >= 0.5 {
                        subjects + expected
                    } else {
                        0.0
                    }
                })
                .collect()
        })
        .collect();
    let found: Vec<(usize, usize)> = best_pairs(&scores, 0.5)
        .into_iter()
        .map(|(o, n, _)| (old.left[o], new.left[n]))
        .collect();
    for (o, n) in found {
        old.take(&[o]);
        new.take(&[n]);
        pairs.push(CheckPair {
            before: vec![o],
            after: vec![n],
            how: CheckMatch::Refactored,
        });
    }
}

/// How alike two checks' expected values are, from 0 to 1, comparing objects field by
/// field. Two checks without arguments (`toBeNull()`) count as alike.
fn expected_score(before: &Assertion, after: &Assertion) -> f64 {
    let (a, b) = (expected_leaves(before), expected_leaves(after));
    if a.is_empty() && b.is_empty() {
        1.0
    } else {
        jaccard(&a, &b)
    }
}

/// The matcher's arguments as `path=value` strings, one per field of an object.
fn expected_leaves(assertion: &Assertion) -> BTreeSet<String> {
    fn flatten(path: String, value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(fields) if !fields.is_empty() => {
                for (key, value) in fields {
                    flatten(format!("{path}.{key}"), value, out);
                }
            }
            _ => {
                out.insert(format!("{path}={value}"));
            }
        }
    }
    let mut out = BTreeSet::new();
    if let Some(matcher) = &assertion.matcher {
        for (i, arg) in matcher.args.iter().enumerate() {
            flatten(i.to_string(), arg, &mut out);
        }
    }
    out
}

/// The expected values in a check that are distinctive enough to recognize it by.
pub(crate) fn expected_values(assertion: &Assertion) -> Vec<String> {
    let mut values: Vec<&Value> = Vec::new();
    for fact in &assertion.facts {
        match &fact.test {
            FactTest::Equals(value) => values.push(value),
            FactTest::Other { args, .. } => values.extend(args),
            FactTest::Defined | FactTest::Truthy | FactTest::Falsy => {}
        }
    }
    if assertion.facts.is_empty()
        && let Some(matcher) = &assertion.matcher
    {
        values.extend(&matcher.args);
    }
    values
        .into_iter()
        .filter(|value| distinctive(value))
        .map(ToString::to_string)
        .collect()
}

/// `'Hi there'`, `/err(or)?/`, `1234`, `user.email`: likely to mean the same thing wherever
/// it appears. `200`, `true`, `null` or `'ok'` could be anything.
fn distinctive(value: &Value) -> bool {
    match value {
        Value::String(text) | Value::Template(text) => text.trim().chars().count() >= 4,
        Value::Regex { .. } => true,
        Value::Number(number) => number.len() >= 4,
        Value::Opaque(source) => source.len() >= 4,
        Value::Array(items) => items.iter().any(distinctive),
        Value::Object(fields) => fields.values().any(distinctive),
        Value::Null | Value::Undefined | Value::Bool(_) | Value::Matcher { .. } => false,
    }
}

/// A check removed from one test that shows up, unchanged and in the same place (not newly
/// inside an `if` or a callback), in another test of the same change moved there. Each
/// added check can account for one removed check.
pub(crate) fn find_moves(files: &[FilePair], matching: &mut Matching) {
    // Checks added anywhere, by their text and place, in order.
    let key = |test: &Test, item: usize| {
        let item = &test.body.items[item];
        match &item.kind {
            ItemKind::Assertion(assertion) => Some(format!("{assertion}\u{0}{:?}", item.context)),
            _ => None,
        }
    };
    let mut added: HashMap<String, VecDeque<TestRef>> = HashMap::new();
    let mut offer = |test_ref: TestRef, items: &mut dyn Iterator<Item = usize>| {
        let test = test(files, test_ref, After);
        for item in items {
            if let Some(key) = key(test, item) {
                added.entry(key).or_default().push_back(test_ref);
            }
        }
    };
    for pair in &matching.pairs {
        offer(pair.after, &mut pair.checks.added.iter().copied());
    }
    for &test_ref in &matching.added {
        offer(test_ref, &mut assertion_items(test(files, test_ref, After)));
    }

    let mut claim = |test: &Test, removed: &mut RemovedCheck| {
        if let Some(key) = key(test, removed.item) {
            removed.moved_to = added.get_mut(&key).and_then(VecDeque::pop_front);
        }
    };
    for pair in &mut matching.pairs {
        let before = test(files, pair.before, crate::Before);
        for removed in &mut pair.checks.removed {
            claim(before, removed);
        }
    }
    for deleted in &mut matching.deleted {
        let before = test(files, deleted.test, crate::Before);
        for removed in &mut deleted.checks {
            claim(before, removed);
        }
    }
}
