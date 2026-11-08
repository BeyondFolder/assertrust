//! Comparing a check's expected values before and after (`docs/architecture.md`, step 5,
//! "Which direction is looser"): what the new ones accept that the old ones didn't.

use assertrust_ir::Value;

use crate::strength::Canonical;
use crate::{Confidence, number};

/// How the new expected values compare with the old ones.
#[derive(Debug, Default)]
pub(crate) struct Diff {
    /// Fields or items no longer checked, as paths: `status`, `user.id`.
    pub(crate) dropped: Vec<String>,
    /// Other ways the new values accept more, each with how sure that is:
    /// `now also accepts webhook`.
    pub(crate) looser: Vec<(Confidence, String)>,
    /// They changed in a way that's neither looser nor stricter.
    pub(crate) changed: bool,
}

impl Diff {
    pub(crate) fn is_looser(&self) -> bool {
        !self.dropped.is_empty() || !self.looser.is_empty()
    }

    /// How sure the loosening is: fields, items and alternatives are certain, numbers and
    /// substrings less so.
    pub(crate) fn confidence(&self) -> Confidence {
        let dropped = (!self.dropped.is_empty()).then_some(Confidence::High);
        dropped
            .into_iter()
            .chain(self.looser.iter().map(|(confidence, _)| *confidence))
            .min()
            .unwrap_or(Confidence::Low)
    }

    /// `no longer checks status, domainIds; now accepts any id (expect.any(Number))`.
    pub(crate) fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.dropped.is_empty() {
            parts.push(format!("no longer checks {}", self.dropped.join(", ")));
        }
        parts.extend(self.looser.iter().map(|(_, text)| text.clone()));
        parts.join("; ")
    }

    fn loosen(&mut self, confidence: Confidence, text: String) {
        self.looser.push((confidence, text));
    }
}

/// Compares the expected values of two checks with the same matcher.
pub(crate) fn compare(old: &Canonical, new: &Canonical) -> Diff {
    let mut diff = Diff::default();
    let (a, b) = (old.args.as_slice(), new.args.as_slice());
    match (old.name.as_str(), old.negated) {
        ("toMatchObject", false) => args(a, b, Mode::Subset, &mut diff),
        ("toMatchObject", true) => negated_subset(a, b, &mut diff),
        ("toHaveProperty" | "toHaveAttribute", false) => property(a, b, &mut diff),
        // Always text. `toContain` isn't here: on an array it compares whole items.
        ("toMatch" | "toContainText" | "toHaveTextContent", negated) => {
            text(a, b, negated, &mut diff);
        }
        ("toThrow", false) => match (a, b) {
            ([_, ..], []) => {
                diff.loosen(Confidence::High, "no longer checks which error".to_owned())
            }
            ([], [_, ..]) => {}
            _ => text(a, b, false, &mut diff),
        },
        (
            name @ ("toBeGreaterThan"
            | "toBeGreaterThanOrEqual"
            | "toBeLessThan"
            | "toBeLessThanOrEqual"
            | "toBeCloseTo"),
            false,
        ) => bound(name, a, b, &mut diff),
        // Under `.not`, accepting more makes the check stricter, so any other change is
        // just a change.
        (_, true) => diff.changed = a != b,
        _ => args(a, b, Mode::Exact, &mut diff),
    }
    diff
}

/// Whether an object must match exactly (`toEqual`), may have more fields at every level
/// (`toMatchObject`), or only at its own level (`expect.objectContaining`, whose fields
/// are compared exactly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Exact,
    Subset,
    Contains,
}

fn args(old: &[Value], new: &[Value], mode: Mode, diff: &mut Diff) {
    if old.len() != new.len() {
        diff.changed = true;
        return;
    }
    for (old, new) in old.iter().zip(new) {
        value(old, new, mode, "", diff);
    }
}

/// Compares one value, at `path` inside the expected value (empty at the top).
fn value(old: &Value, new: &Value, mode: Mode, path: &str, diff: &mut Diff) {
    if old == new {
        return;
    }
    if let Some(confidence) = wider(old, new) {
        let what = if path.is_empty() {
            "value".to_owned()
        } else {
            path.to_owned()
        };
        diff.loosen(confidence, format!("now accepts any {what} ({new})"));
        return;
    }
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            let inner = if mode == Mode::Contains {
                Mode::Exact
            } else {
                mode
            };
            // New keys a dropped one was renamed to.
            let mut renamed_to: Vec<&String> = Vec::new();
            for (key, old_value) in old {
                match new.get(key) {
                    Some(new_value) => value(old_value, new_value, inner, &join(path, key), diff),
                    None if mode != Mode::Exact => {
                        // The same value under a new key that looks alike (`statusCode: 422`
                        // → `status: 422`): renamed along with the code, not dropped.
                        let renamed = new.iter().find(|(other, new_value)| {
                            !old.contains_key(*other)
                                && !renamed_to.contains(other)
                                && *new_value == old_value
                                && alike(key, other)
                        });
                        match renamed {
                            Some((other, _)) => {
                                renamed_to.push(other);
                                diff.changed = true;
                            }
                            None => diff.dropped.push(join(path, key)),
                        }
                    }
                    None => diff.changed = true,
                }
            }
            // Extra fields make a subset stricter, but an exact object different.
            if mode == Mode::Exact && new.keys().any(|key| !old.contains_key(key)) {
                diff.changed = true;
            }
        }
        (Value::Array(old), Value::Array(new)) if old.len() == new.len() => {
            for (i, (old, new)) in old.iter().zip(new).enumerate() {
                value(old, new, mode, &join(path, &i.to_string()), diff);
            }
        }
        // `{ a: 1 }` → `expect.objectContaining({ a: 1 })`
        (Value::Object(_), Value::Matcher { name, args })
            if mode == Mode::Exact && name == "objectContaining" && args.len() == 1 =>
        {
            let where_ = if path.is_empty() {
                String::new()
            } else {
                format!(" in {path}")
            };
            diff.loosen(Confidence::High, format!("now allows extra fields{where_}"));
            value(old, &args[0], Mode::Contains, path, diff);
        }
        (
            Value::Matcher {
                name: old_name,
                args: old_args,
            },
            Value::Matcher {
                name: new_name,
                args: new_args,
            },
        ) if old_name == new_name && old_args.len() == 1 && new_args.len() == 1 => {
            let (old, new) = (&old_args[0], &new_args[0]);
            match old_name.as_str() {
                "objectContaining" => value(old, new, Mode::Contains, path, diff),
                "arrayContaining" => items(old, new, path, diff),
                "stringContaining" => substring(old, new, false, diff),
                "stringMatching" => pattern(old, new, false, diff),
                _ => diff.changed = true,
            }
        }
        _ => diff.changed = true,
    }
}

/// `expect.arrayContaining([a, b])` → `expect.arrayContaining([a])`.
fn items(old: &Value, new: &Value, path: &str, diff: &mut Diff) {
    let (Value::Array(old), Value::Array(new)) = (old, new) else {
        diff.changed = true;
        return;
    };
    for item in old {
        if !new.contains(item) {
            let item = item.to_string();
            diff.dropped.push(if path.is_empty() {
                item
            } else {
                format!("{item} in {path}")
            });
        }
    }
}

/// `.not.toMatchObject({ a: 1 })` fails only if every field matches, so adding a field
/// lets more through.
fn negated_subset(old: &[Value], new: &[Value], diff: &mut Diff) {
    if let ([Value::Object(old)], [Value::Object(new)]) = (old, new)
        && old.iter().all(|(key, value)| new.get(key) == Some(value))
    {
        let added: Vec<&str> = new
            .keys()
            .filter(|key| !old.contains_key(*key))
            .map(String::as_str)
            .collect();
        if added.is_empty() {
            return;
        }
        diff.loosen(
            Confidence::High,
            format!("now fails only if {} match too", added.join(", ")),
        );
        return;
    }
    diff.changed = true;
}

/// `toHaveProperty(path, value)` or `toHaveAttribute(name, value)`: dropping the value.
fn property(old: &[Value], new: &[Value], diff: &mut Diff) {
    let (Some(old_path), Some(new_path)) = (old.first(), new.first()) else {
        diff.changed = true;
        return;
    };
    let path = old_path.to_string();
    if old_path != new_path {
        diff.changed = true;
        return;
    }
    match (old.get(1), new.get(1)) {
        (Some(_), None) => diff.loosen(
            Confidence::High,
            format!("no longer checks the value of {path}"),
        ),
        (Some(old), Some(new)) => value(old, new, Mode::Exact, "", diff),
        _ => {}
    }
}

/// Matchers that take text: a string to find, or a regex to match.
fn text(old: &[Value], new: &[Value], negated: bool, diff: &mut Diff) {
    match (old, new) {
        ([old @ Value::String(_)], [new @ Value::String(_)]) => substring(old, new, negated, diff),
        ([old @ Value::Regex { .. }], [new @ Value::Regex { .. }]) => {
            pattern(old, new, negated, diff);
        }
        _ if negated => {
            if old != new {
                diff.changed = true;
            }
        }
        _ => args(old, new, Mode::Exact, diff),
    }
}

/// A substring check: needing less of the text is looser. Under `.not`, ruling out a longer
/// text is.
fn substring(old: &Value, new: &Value, negated: bool, diff: &mut Diff) {
    let (Value::String(old_text), Value::String(new_text)) = (old, new) else {
        diff.changed = old != new;
        return;
    };
    if old_text == new_text {
        return;
    }
    // The new text is part of the old one, or the old one is part of the new one.
    let part = old_text.contains(new_text.as_str());
    let whole = new_text.contains(old_text.as_str());
    match (negated, part, whole) {
        (false, true, _) => diff.loosen(Confidence::Medium, format!("now only needs {new}")),
        (true, _, true) => diff.loosen(Confidence::Medium, format!("now only rules out {new}")),
        (false, _, true) | (true, true, _) => {} // stricter
        _ => diff.changed = true,
    }
}

/// A regex: an alternative added lets more through; under `.not`, one removed does. A
/// regex rewritten from scratch (no alternative kept) is just different.
fn pattern(old: &Value, new: &Value, negated: bool, diff: &mut Diff) {
    let (
        Value::Regex {
            flags: old_flags,
            alternatives: old_alternatives,
            ..
        },
        Value::Regex {
            flags: new_flags,
            alternatives: new_alternatives,
            ..
        },
    ) = (old, new)
    else {
        diff.changed = old != new;
        return;
    };
    let kept = old_alternatives
        .iter()
        .any(|alternative| new_alternatives.contains(alternative));
    let Some(flags) = flags(old_flags, new_flags, negated) else {
        diff.changed = true;
        return;
    };
    if !kept {
        diff.changed = true;
        return;
    }
    if let Some(flags) = flags {
        diff.loosen(Confidence::High, flags);
    }
    let missing = |from: &[String], to: &[String]| -> Vec<String> {
        from.iter()
            .filter(|alternative| !to.contains(alternative))
            .cloned()
            .collect()
    };
    let added = missing(new_alternatives, old_alternatives);
    let removed = missing(old_alternatives, new_alternatives);
    if !negated && !added.is_empty() {
        diff.loosen(
            Confidence::High,
            format!("now also accepts {}", added.join(", ")),
        );
    } else if negated && !removed.is_empty() {
        diff.loosen(
            Confidence::High,
            format!("no longer rules out {}", removed.join(", ")),
        );
    }
}

/// Regex flags changed: `Some(None)` when they don't let more through, `Some(Some(how))` when
/// they do (`i` added: any case matches), `None` when they changed some other way. Under
/// `.not`, removing such a flag is what lets more through.
fn flags(old: &str, new: &str, negated: bool) -> Option<Option<String>> {
    let added: String = new.chars().filter(|flag| !old.contains(*flag)).collect();
    let removed: String = old.chars().filter(|flag| !new.contains(*flag)).collect();
    // The flags that make a regex match more: any case, `.` matching newlines, `^` and `$`
    // at every line.
    let wider = |flags: &str| flags.chars().all(|flag| "ism".contains(flag));
    match (added.is_empty(), removed.is_empty()) {
        (true, true) => Some(None),
        (false, true) if wider(&added) && !negated => {
            Some(Some(format!("now matches more with the {added} flag")))
        }
        (true, false) if wider(&removed) && negated => Some(Some(format!(
            "no longer rules out what only the {removed} flag matched"
        ))),
        // A wider flag removed, or added under `.not`: stricter.
        (false, true) if wider(&added) => Some(None),
        (true, false) if wider(&removed) => Some(None),
        _ => None,
    }
}

/// `toBeGreaterThan(5)` → `toBeGreaterThan(3)`, `toBeCloseTo(x, 3)` → `toBeCloseTo(x, 1)`.
fn bound(name: &str, old: &[Value], new: &[Value], diff: &mut Diff) {
    if name == "toBeCloseTo" {
        if old.first() != new.first() {
            diff.changed = true;
            return;
        }
        // Vitest and jest check 2 digits by default.
        let digits = |args: &[Value]| args.get(1).map_or(Some(2.0), number);
        if let (Some(old_digits), Some(new_digits)) = (digits(old), digits(new))
            && new_digits < old_digits
        {
            diff.loosen(
                Confidence::Medium,
                format!("precision lowered from {old_digits} to {new_digits} digits"),
            );
        }
        return;
    }
    let (Some(old_bound), Some(new_bound)) = (old.first(), new.first()) else {
        diff.changed = old != new;
        return;
    };
    let (Some(o), Some(n)) = (number(old_bound), number(new_bound)) else {
        diff.changed = old_bound != new_bound;
        return;
    };
    let lower = name.starts_with("toBeGreaterThan");
    if lower && n < o {
        diff.loosen(
            Confidence::Medium,
            format!("lower bound lowered from {old_bound} to {new_bound}"),
        );
    } else if !lower && n > o {
        diff.loosen(
            Confidence::Medium,
            format!("upper bound raised from {old_bound} to {new_bound}"),
        );
    }
}

/// `new` is `expect.anything()` or `expect.any(T)` where `old` was a value it accepts, so it
/// lets more through: with how sure that is. `None` when it rejects the old value
/// (`expect.anything()` rejects `null`, `expect.any(String)` a number): that's a different
/// outcome, not a looser one.
fn wider(old: &Value, new: &Value) -> Option<Confidence> {
    let Value::Matcher { name, args } = new else {
        return None;
    };
    match (name.as_str(), old) {
        ("anything", Value::Null | Value::Undefined) => None,
        ("anything", Value::Matcher { name, .. }) => (name == "any").then_some(Confidence::High),
        // A variable could hold `null`.
        ("anything", Value::Opaque(_)) => Some(Confidence::Medium),
        ("anything", _) => Some(Confidence::High),
        ("any", _) => {
            let Some(Value::Opaque(class)) = args.first() else {
                return None;
            };
            match (class.as_str(), old) {
                ("Number", Value::Number(_))
                | ("String", Value::String(_) | Value::Template(_))
                | ("Boolean", Value::Bool(_))
                | ("Array", Value::Array(_))
                | ("Object", Value::Object(_) | Value::Array(_)) => Some(Confidence::High),
                ("String", Value::Matcher { name, .. })
                    if name == "stringContaining" || name == "stringMatching" =>
                {
                    Some(Confidence::High)
                }
                (_, Value::Opaque(_)) => Some(Confidence::Medium),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether two keys look like one renamed: one contains the other (`statusCode`,
/// `status`), or they start the same for at least 4 letters (`statusMessage`,
/// `statusText`). Case doesn't matter.
fn alike(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let shared = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    a.contains(&b) || b.contains(&a) || shared >= 4
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}
