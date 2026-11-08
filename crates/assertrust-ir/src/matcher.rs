//! Matcher kinds (`docs/architecture.md`, step 3, "Matcher kinds").
//!
//! The IR only labels the kind. Which kind is stricter than which is up to the rules
//! (step 5), so a pytest `assert a == b` could map to [`MatcherKind::Equal`] too.

/// What kind of check a matcher makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatcherKind {
    /// `toBe`, `toEqual`, `toStrictEqual`
    Equal,
    /// `toMatchObject`, `toHaveProperty`
    Subset,
    /// `toContain`, `toContainEqual`
    Contains,
    /// `toMatch`
    Pattern,
    /// `toBeTruthy`, `toBeDefined`, `toBeNull`, …
    Truthiness,
    /// `toBeInstanceOf`, `toBeTypeOf`
    Type,
    /// `toBeGreaterThan`, `toBeCloseTo`, …
    Compare,
    /// `toHaveLength`
    Size,
    /// `toThrow`, `toThrowError`
    Throws,
    /// `toHaveBeenCalled`, `toHaveBeenCalledWith`, …
    Called,
    /// `toMatchSnapshot`, `toMatchInlineSnapshot`, `toHaveScreenshot`, …
    Snapshot,
    /// Playwright and testing-library UI checks: `toBeVisible`, `toHaveText`, …
    UiState,
    /// From `expect.extend`, or unknown.
    Custom,
}

impl MatcherKind {
    /// The kind of a matcher, from its name.
    pub fn of(name: &str) -> Self {
        match name {
            "toBe" | "toEqual" | "toStrictEqual" => Self::Equal,
            "toMatchObject" | "toHaveProperty" => Self::Subset,
            "toContain" | "toContainEqual" => Self::Contains,
            "toMatch" => Self::Pattern,
            "toBeTruthy" | "toBeFalsy" | "toBeDefined" | "toBeUndefined" | "toBeNull"
            | "toBeNaN" => Self::Truthiness,
            "toBeInstanceOf" | "toBeTypeOf" => Self::Type,
            "toBeGreaterThan"
            | "toBeGreaterThanOrEqual"
            | "toBeLessThan"
            | "toBeLessThanOrEqual"
            | "toBeCloseTo" => Self::Compare,
            "toHaveLength" => Self::Size,
            "toThrow" | "toThrowError" => Self::Throws,
            "toMatchSnapshot"
            | "toMatchInlineSnapshot"
            | "toMatchFileSnapshot"
            | "toThrowErrorMatchingSnapshot"
            | "toThrowErrorMatchingInlineSnapshot"
            | "toHaveScreenshot"
            | "toMatchAriaSnapshot" => Self::Snapshot,
            name if is_called(name) => Self::Called,
            name if is_ui_state(name) => Self::UiState,
            _ => Self::Custom,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Subset => "subset",
            Self::Contains => "contains",
            Self::Pattern => "pattern",
            Self::Truthiness => "truthiness",
            Self::Type => "type",
            Self::Compare => "compare",
            Self::Size => "size",
            Self::Throws => "throws",
            Self::Called => "called",
            Self::Snapshot => "snapshot",
            Self::UiState => "ui-state",
            Self::Custom => "custom",
        }
    }
}

/// Mock checks: `toHaveBeenCalled…`, `toBeCalled…`, `toHaveReturned…`, `toHaveResolved…`,
/// and jest's old aliases `lastCalledWith` / `nthCalledWith`.
fn is_called(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "toHaveBeenCalled",
        "toHaveBeenLastCalled",
        "toHaveBeenNthCalled",
        "toBeCalled",
        "toHaveReturned",
        "toHaveLastReturned",
        "toHaveNthReturned",
        "toReturn",
        "toHaveResolved",
        "toHaveLastResolved",
        "toHaveNthResolved",
        "lastCalledWith",
        "nthCalledWith",
        "lastReturnedWith",
        "nthReturnedWith",
    ];
    PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// Playwright's web-first assertions, plus the common testing-library (jest-dom) ones.
fn is_ui_state(name: &str) -> bool {
    matches!(
        name,
        "toBeAttached"
            | "toBeChecked"
            | "toBeDisabled"
            | "toBeEditable"
            | "toBeEmpty"
            | "toBeEnabled"
            | "toBeFocused"
            | "toBeHidden"
            | "toBeInViewport"
            | "toBeVisible"
            | "toBeOK"
            | "toContainClass"
            | "toContainText"
            | "toHaveAccessibleDescription"
            | "toHaveAccessibleErrorMessage"
            | "toHaveAccessibleName"
            | "toHaveAttribute"
            | "toHaveCSS"
            | "toHaveClass"
            | "toHaveCount"
            | "toHaveId"
            | "toHaveJSProperty"
            | "toHaveRole"
            | "toHaveText"
            | "toHaveTitle"
            | "toHaveURL"
            | "toHaveValue"
            | "toHaveValues"
            | "toBeInTheDocument"
            | "toHaveTextContent"
            | "toHaveStyle"
            | "toHaveFocus"
            | "toBeRequired"
            | "toBeInvalid"
            | "toBeValid"
            | "toHaveDisplayValue"
            | "toHaveFormValues"
            | "toBeEmptyDOMElement"
            | "toBePartiallyChecked"
            | "toHaveErrorMessage"
            | "toHaveDescription"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_matchers_get_their_kind() {
        let cases = [
            ("toBe", MatcherKind::Equal),
            ("toMatchObject", MatcherKind::Subset),
            ("toHaveProperty", MatcherKind::Subset),
            ("toContain", MatcherKind::Contains),
            ("toMatch", MatcherKind::Pattern),
            ("toBeDefined", MatcherKind::Truthiness),
            ("toBeInstanceOf", MatcherKind::Type),
            ("toBeCloseTo", MatcherKind::Compare),
            ("toHaveLength", MatcherKind::Size),
            ("toThrow", MatcherKind::Throws),
            ("toHaveBeenCalledWith", MatcherKind::Called),
            ("toHaveBeenCalledTimes", MatcherKind::Called),
            ("toHaveBeenNthCalledWith", MatcherKind::Called),
            ("toHaveReturnedWith", MatcherKind::Called),
            ("toMatchInlineSnapshot", MatcherKind::Snapshot),
            ("toHaveScreenshot", MatcherKind::Snapshot),
            ("toBeVisible", MatcherKind::UiState),
            ("toHaveText", MatcherKind::UiState),
        ];
        for (name, kind) in cases {
            assert_eq!(MatcherKind::of(name), kind, "{name}");
        }
    }

    #[test]
    fn unknown_matchers_are_custom() {
        assert_eq!(MatcherKind::of("toBeWithinRange"), MatcherKind::Custom);
    }
}
