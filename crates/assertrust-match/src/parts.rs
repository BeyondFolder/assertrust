//! A list check split into checks on parts of the list (`docs/architecture.md`, step 4,
//! "Pairing checks inside a paired test"): a helper that checks a response's last record,
//! then the records before it.
//!
//! ```text
//! before: expect(response).toEqual([user, done])
//! after:  expect(response.at(-1)).toEqual(done)
//!         expect(response.slice(0, -1)).toEqual(expect.arrayContaining([user]))
//!         expect(response.slice(0, -1)).toHaveLength(1)
//! ```
//!
//! Every item the old check listed must be stated again by the new checks. Whether the
//! number and order of the items are still fixed is for the rules: [`split_list`].

use assertrust_ir::{Assertion, Promise, Value};

use crate::check_pairing::{Side, distinctive};
use crate::{CheckMatch, CheckPair};

/// What a list check split into parts still states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitList {
    /// The old check fixed the items' order (`toEqual([…])`), rather than only requiring
    /// them (`toEqual(expect.arrayContaining([…]))`).
    pub ordered: bool,
    /// The old checks fixed how many items there are: an exact list, or a `toHaveLength`
    /// next to `expect.arrayContaining`.
    pub counted: bool,
    /// How many items it listed.
    pub items: usize,
    /// The new checks still fix the number of items.
    pub count_kept: bool,
    /// The new checks still fix their order.
    pub order_kept: bool,
}

/// What the old checks (a list check, and maybe a `toHaveLength` on the same list) and the
/// checks they were split into still state, or `None` when they aren't a list and its
/// parts.
pub fn split_list(olds: &[&Assertion], news: &[&Assertion]) -> Option<SplitList> {
    let (items, exact) = olds.iter().find_map(|old| list(old))?;
    let counted = exact || olds.iter().any(|old| length(old) == Some(items.len()));
    let parts: Vec<Option<(Part, Says)>> = news.iter().map(|new| read(new)).collect();
    let mut split = analyse(items, exact, &parts)?;
    split.list.counted = counted;
    split.used.iter().all(|&used| used).then_some(split.list)
}

/// Step 4 of pairing checks: pairs each list check with the checks on parts of one list
/// that state its items again. The list should be the same one, `response` and
/// `response.at(-1)`. A list under another name (a test that now stores the response
/// first) is accepted when some item is distinctive enough to recognize it by.
pub(crate) fn pair(old: &mut Side, new: &mut Side, pairs: &mut Vec<CheckPair>) {
    for o in old.left.clone() {
        let check = old.assertion(o);
        let Some((items, exact)) = list(check) else {
            continue;
        };
        // The new checks on parts of a list, by the list and the calls they run through.
        let mut lists: Vec<(ListKey, Vec<usize>)> = Vec::new();
        for &n in &new.left {
            let Some((base, _)) = part(&new.assertion(n).subject) else {
                continue;
            };
            let key = (base, new.calls(n));
            match lists.iter_mut().find(|(other, _)| *other == key) {
                Some((_, checks)) => checks.push(n),
                None => lists.push((key, vec![n])),
            }
        }
        let names = same_list_names(check);
        let covered: Vec<(bool, Vec<usize>)> = lists
            .iter()
            .filter_map(|((base, _), checks)| {
                let parts: Vec<Option<(Part, Says)>> =
                    checks.iter().map(|&n| read(new.assertion(n))).collect();
                let split = analyse(items, exact, &parts)?;
                let used = checks
                    .iter()
                    .zip(&split.used)
                    .filter(|(_, used)| **used)
                    .map(|(&n, _)| n)
                    .collect();
                Some((names.iter().any(|name| name == base), used))
            })
            .collect();
        let chosen = match covered.iter().find(|(same, _)| *same) {
            Some((_, used)) => used.clone(),
            None => match covered.as_slice() {
                [(_, used)] if items.iter().any(recognizable) => used.clone(),
                _ => continue,
            },
        };
        // A count of the same list goes with it: `toHaveLength(3)` next to
        // `expect.arrayContaining([a, b, done])`.
        let mut before = vec![o];
        before.extend(old.left.iter().copied().find(|&other| {
            other != o
                && old.assertion(other).subject == check.subject
                && length(old.assertion(other)) == Some(items.len())
        }));
        old.take(&before);
        new.take(&chosen);
        pairs.push(CheckPair {
            before,
            after: chosen,
            how: CheckMatch::Parts,
        });
    }
}

/// The count a `toHaveLength(n)` check fixes.
fn length(check: &Assertion) -> Option<usize> {
    let matcher = check.matcher.as_ref()?;
    match matcher.args.as_slice() {
        [Value::Number(count)] if matcher.name == "toHaveLength" && !check.negated => {
            count.parse().ok()
        }
        _ => None,
    }
}

/// An item distinctive enough to recognize a list by: a distinctive value, or a matcher
/// with distinctive fields (`expect.objectContaining({ type: SyncEntityType.AssetEditV1 })`).
fn recognizable(value: &Value) -> bool {
    match value {
        Value::Matcher { name, args } if name.ends_with("Containing") => {
            args.iter().any(recognizable)
        }
        value => distinctive(value),
    }
}

/// A list that checks on parts read: its source, and the lines of the calls the checks run
/// through (two calls to one helper read two lists).
type ListKey<'a> = (&'a str, Vec<u32>);

/// The list an old check states: its items, and whether they're exact.
fn list(check: &Assertion) -> Option<(&[Value], bool)> {
    if check.negated || check.promise == Some(Promise::Rejects) {
        return None;
    }
    let matcher = check.matcher.as_ref()?;
    if !matches!(matcher.name.as_str(), "toEqual" | "toStrictEqual") {
        return None;
    }
    match matcher.args.as_slice() {
        [Value::Array(items)] if !items.is_empty() => Some((items, true)),
        [Value::Matcher { name, args }] if name == "arrayContaining" => match args.as_slice() {
            [Value::Array(items)] if !items.is_empty() => Some((items, false)),
            _ => None,
        },
        _ => None,
    }
}

/// How the list a check reads would be written as the subject of a check on its parts:
/// `response`, or `(await p)` for `expect(p).resolves`.
fn same_list_names(check: &Assertion) -> Vec<String> {
    let subject = &check.subject;
    if check.promise == Some(Promise::Resolves) {
        vec![format!("(await {subject})")]
    } else {
        vec![subject.clone(), format!("({subject})")]
    }
}

/// A part of a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// One item: `at(-1)`, `[0]`.
    At(i64),
    /// The items from one index up to another, or to the end: `slice(0, -1)`, `slice(1)`.
    Slice(i64, Option<i64>),
}

/// `x.at(-1)`, `x[0]`, `x.slice(0, -1)`: the list and the part.
fn part(subject: &str) -> Option<(&str, Part)> {
    if let Some(inner) = subject.strip_suffix(']') {
        let open = inner.rfind('[')?;
        let index = inner[open + 1..].trim().parse().ok()?;
        return Some((&inner[..open], Part::At(index)));
    }
    let inner = subject.strip_suffix(')')?;
    if let Some(open) = inner.rfind(".at(") {
        let index = inner[open + 4..].trim().parse().ok()?;
        return Some((&inner[..open], Part::At(index)));
    }
    let open = inner.rfind(".slice(")?;
    let bounds: Vec<i64> = inner[open + 7..]
        .split(',')
        .map(|arg| arg.trim().parse().ok())
        .collect::<Option<_>>()?;
    let part = match bounds.as_slice() {
        [start] => Part::Slice(*start, None),
        [start, end] => Part::Slice(*start, Some(*end)),
        _ => return None,
    };
    Some((&inner[..open], part))
}

/// What a check on a part states.
#[derive(Debug, Clone, Copy)]
enum Says<'a> {
    /// The item is this value.
    Item(&'a Value),
    /// These items, in this order (`true`) or in any order.
    Items(&'a [Value], bool),
    /// This many items.
    Count(usize),
}

/// A check on a part of a list, and what it states.
fn read(check: &Assertion) -> Option<(Part, Says<'_>)> {
    if check.negated || check.promise.is_some() {
        return None;
    }
    let (_, part) = part(&check.subject)?;
    let matcher = check.matcher.as_ref()?;
    let [arg] = matcher.args.as_slice() else {
        return None;
    };
    let says = match (part, matcher.name.as_str(), arg) {
        (Part::At(_), "toEqual" | "toStrictEqual" | "toBe", value) => Says::Item(value),
        (Part::Slice(..), "toEqual" | "toStrictEqual", Value::Array(items)) => {
            Says::Items(items, true)
        }
        (Part::Slice(..), "toEqual" | "toStrictEqual", Value::Matcher { name, args })
            if name == "arrayContaining" =>
        {
            match args.as_slice() {
                [Value::Array(items)] => Says::Items(items, false),
                _ => return None,
            }
        }
        (Part::Slice(..), "toHaveLength", Value::Number(count)) => Says::Count(count.parse().ok()?),
        _ => return None,
    };
    Some((part, says))
}

struct Analysis {
    list: SplitList,
    /// Which of the checks on parts state something about the list's items.
    used: Vec<bool>,
}

/// Whether the checks on parts state every item of `items` again, and what they still
/// fix. An item is matched once. When the old list was exact, a part must state the items
/// at its place: `at(i)` the one at `i`, a slice the ones in its range.
///
/// The number of items is still fixed only by one slice whose count is known (an exact list,
/// or a `toHaveLength` on the same slice) with single items around it that cover the rest:
/// `slice(0, -1)` and `at(-1)`, or `[0]` and `slice(1)`. Single items alone never fix it.
fn analyse(items: &[Value], exact: bool, parts: &[Option<(Part, Says)>]) -> Option<Analysis> {
    let count = items.len();
    let len = i64::try_from(count).ok()?;
    let index = |at: i64| usize::try_from(if at < 0 { len + at } else { at }).ok();
    let bound = |at: i64| {
        usize::try_from(if at < 0 {
            (len + at).max(0)
        } else {
            at.min(len)
        })
    };
    let mut claimed = vec![false; count];
    let mut used = vec![false; parts.len()];
    let mut singles: Vec<i64> = Vec::new();
    let mut slices: Vec<(Part, Option<usize>)> = Vec::new();
    let mut order_kept = true;
    for (i, found) in parts.iter().enumerate() {
        match found {
            Some((Part::At(at), Says::Item(value))) => {
                let matched = if exact {
                    index(*at).filter(|&j| j < count && !claimed[j] && items[j] == **value)
                } else {
                    (0..count).find(|&j| !claimed[j] && items[j] == **value)
                };
                if let Some(j) = matched {
                    claimed[j] = true;
                    used[i] = true;
                    singles.push(*at);
                }
            }
            Some((part @ Part::Slice(start, end), Says::Items(values, ordered))) => {
                let range = if exact {
                    let start = bound(*start).ok()?;
                    let end = end.map_or(Ok(count), bound).ok()?;
                    start..end.max(start)
                } else {
                    0..count
                };
                if exact && *ordered {
                    // The same items in the same places.
                    if values.len() != range.len()
                        || range
                            .clone()
                            .zip(*values)
                            .any(|(j, value)| claimed[j] || items[j] != *value)
                    {
                        return None;
                    }
                    range.for_each(|j| claimed[j] = true);
                } else {
                    for value in *values {
                        let j = range.clone().find(|&j| !claimed[j] && items[j] == *value)?;
                        claimed[j] = true;
                    }
                    // Items that are all alike have no order to lose.
                    if values.iter().any(|value| *value != values[0]) {
                        order_kept &= *ordered;
                    }
                }
                used[i] = true;
                slices.push((*part, ordered.then_some(values.len())));
            }
            _ => {}
        }
    }
    if !claimed.iter().all(|&claimed| claimed) {
        return None;
    }
    // A count only counts for the slice whose items were stated.
    for (i, found) in parts.iter().enumerate() {
        if let Some((part @ Part::Slice(..), Says::Count(n))) = found
            && let Some((_, known)) = slices.iter_mut().find(|(other, _)| other == part)
        {
            used[i] = true;
            known.get_or_insert(*n);
        }
    }
    let count_kept = match slices.as_slice() {
        [(Part::Slice(start, end), Some(sliced))]
            if *start >= 0 && end.is_none_or(|end| end < 0) =>
        {
            let mut around: Vec<i64> = (0..*start).chain(end.unwrap_or(0)..0).collect();
            let mut written = singles.clone();
            around.sort_unstable();
            written.sort_unstable();
            around == written && sliced + singles.len() == count
        }
        _ => false,
    };
    Some(Analysis {
        list: SplitList {
            ordered: exact,
            counted: exact,
            items: count,
            count_kept,
            order_kept,
        },
        used,
    })
}
