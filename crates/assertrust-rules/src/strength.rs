//! The matcher strength ladder (`docs/architecture.md`, step 5): how much a matcher lets
//! through, from exact (1) to presence (5).

use assertrust_ir::{Assertion, MatcherKind, Value};

/// A check's matcher in one spelling: aliases folded (`toBeCalled` → `toHaveBeenCalled`),
/// and checks that behave the same written the same (`toBeNull()` → `toBe(null)`,
/// `.not.toBeUndefined()` → `toBeDefined()`, `toEqual(5)` → `toBe(5)`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Canonical {
    pub(crate) name: String,
    pub(crate) args: Vec<Value>,
    pub(crate) negated: bool,
}

impl Canonical {
    pub(crate) fn of(assertion: &Assertion) -> Option<Self> {
        let matcher = assertion.matcher.as_ref()?;
        let mut name = alias(&matcher.name);
        let mut args = matcher.args.clone();
        let mut negated = assertion.negated;
        if negated {
            let flipped = match name {
                "toBeTruthy" => Some("toBeFalsy"),
                "toBeFalsy" => Some("toBeTruthy"),
                "toBeUndefined" => Some("toBeDefined"),
                "toBeDefined" => Some("toBeUndefined"),
                _ => None,
            };
            if let Some(flipped) = flipped {
                name = flipped;
                negated = false;
            }
        }
        match name {
            "toBeNull" => (name, args) = ("toBe", vec![Value::Null]),
            "toBeUndefined" => (name, args) = ("toBe", vec![Value::Undefined]),
            "toEqual" | "toStrictEqual" if args.len() == 1 && is_primitive(&args[0]) => {
                name = "toBe";
            }
            _ => {}
        }
        Some(Self {
            name: name.to_owned(),
            args,
            negated,
        })
    }

    pub(crate) fn kind(&self) -> MatcherKind {
        MatcherKind::of(&self.name)
    }

    /// Where the matcher sits on the ladder, from 1 (exact) to 5 (presence). `None` for
    /// matchers that aren't ranked: custom ones, and anything unknown.
    pub(crate) fn level(&self) -> Option<u8> {
        let with_value = |n: usize| self.args.len() >= n;
        Some(match self.kind() {
            MatcherKind::Equal | MatcherKind::Snapshot => 1,
            MatcherKind::Throws if with_value(1) => 1,
            MatcherKind::Throws => 3,
            MatcherKind::Subset if self.name == "toHaveProperty" && !with_value(2) => 5,
            MatcherKind::Subset | MatcherKind::Size => 2,
            MatcherKind::Called if self.name.ends_with("With") => 2,
            MatcherKind::Called if self.name.ends_with("Times") => 3,
            MatcherKind::Called => 5,
            MatcherKind::Contains | MatcherKind::Pattern | MatcherKind::Compare => 3,
            MatcherKind::Type => 4,
            MatcherKind::Truthiness if self.name == "toBeNaN" => 1,
            MatcherKind::Truthiness => 5,
            MatcherKind::UiState => ui_level(&self.name, with_value(2))?,
            MatcherKind::Custom => return None,
        })
    }

    /// What passes now that didn't, for a matcher further down the ladder:
    /// `any truthy value passes`.
    pub(crate) fn now_passes(&self) -> &'static str {
        match (self.level(), self.kind(), self.name.as_str()) {
            (_, _, "toBeTruthy") => "any truthy value passes",
            (_, _, "toBeFalsy") => "any falsy value passes",
            (_, _, "toBeDefined") => "any defined value passes",
            (_, _, "toHaveProperty") => "any value passes as long as the key exists",
            (_, _, "toHaveAttribute") => "any value passes as long as the attribute exists",
            (Some(5), MatcherKind::Called, _) => "any call passes",
            (Some(5), _, _) => "only the element's state is checked",
            (Some(4), _, _) => "any value of the right type passes",
            (_, MatcherKind::Compare, _) => "a range of values passes",
            (_, MatcherKind::Throws, _) => "any error passes",
            (_, MatcherKind::Called, _) => "calls with any arguments pass",
            (_, MatcherKind::Subset, _) => "extra fields pass",
            (_, MatcherKind::Size, _) => "only the length is checked",
            _ => "a partial match passes",
        }
    }

    /// `toEqual({ id: 1 })`, with `.not.` in front when negated.
    pub(crate) fn call(&self) -> String {
        let args: Vec<String> = self.args.iter().map(ToString::to_string).collect();
        let not = if self.negated { ".not." } else { "" };
        format!("{not}{}({})", self.name, args.join(", "))
    }

    /// The matcher's name, with `.not.` in front when negated: `.not.toMatch`.
    pub(crate) fn label(&self) -> String {
        if self.negated {
            format!(".not.{}", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The usual name of a matcher that has several.
fn alias(name: &str) -> &str {
    match name {
        "toThrowError" => "toThrow",
        "toBeCalled" => "toHaveBeenCalled",
        "toBeCalledWith" => "toHaveBeenCalledWith",
        "toBeCalledTimes" => "toHaveBeenCalledTimes",
        "lastCalledWith" | "toHaveBeenLastCalledWith" => "toHaveBeenLastCalledWith",
        "nthCalledWith" => "toHaveBeenNthCalledWith",
        "toReturn" => "toHaveReturned",
        "toReturnWith" => "toHaveReturnedWith",
        "toReturnTimes" => "toHaveReturnedTimes",
        "lastReturnedWith" => "toHaveLastReturnedWith",
        "nthReturnedWith" => "toHaveNthReturnedWith",
        other => other,
    }
}

/// A value `toBe` and `toEqual` treat the same.
fn is_primitive(value: &Value) -> bool {
    matches!(
        value,
        Value::Null
            | Value::Undefined
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Template(_)
    )
}

/// Playwright and jest-dom matchers: exact text or values, partial text, or a state.
fn ui_level(name: &str, with_value: bool) -> Option<u8> {
    Some(match name {
        "toHaveAttribute" if with_value => 1,
        "toHaveAttribute" => 5,
        "toHaveText"
        | "toHaveValue"
        | "toHaveValues"
        | "toHaveURL"
        | "toHaveTitle"
        | "toHaveCount"
        | "toHaveId"
        | "toHaveJSProperty"
        | "toHaveCSS"
        | "toHaveClass"
        | "toHaveAccessibleName"
        | "toHaveAccessibleDescription"
        | "toHaveAccessibleErrorMessage"
        | "toHaveRole"
        | "toHaveDisplayValue"
        | "toHaveFormValues"
        | "toHaveStyle" => 1,
        "toContainText" | "toContainClass" | "toHaveTextContent" => 3,
        "toBeVisible" | "toBeHidden" | "toBeAttached" | "toBeChecked" | "toBeDisabled"
        | "toBeEnabled" | "toBeEditable" | "toBeEmpty" | "toBeFocused" | "toBeInViewport"
        | "toBeOK" | "toBeInTheDocument" | "toHaveFocus" | "toBeRequired" | "toBeInvalid"
        | "toBeValid" => 5,
        _ => return None,
    })
}
