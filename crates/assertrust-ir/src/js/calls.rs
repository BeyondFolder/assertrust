//! Recognizing test-framework calls by their shape (`docs/architecture.md`, step 2).
//!
//! Names are matched, not imports: Playwright projects import `test` from their own
//! fixtures file, so checking imports would miss them.

use oxc::ast::ast::{Argument, CallExpression, Expression, ObjectPropertyKind, TemplateLiteral};

use crate::model::{ExpectKind, HookKind, Promise};

/// A call that declares or changes test structure.
pub(super) enum Structure<'b, 'a> {
    /// `describe(name, fn)` and its variants.
    Suite {
        /// `None` for Playwright's anonymous `test.describe(fn)`.
        name: Option<&'b Expression<'a>>,
        modifiers: Vec<RawModifier<'b, 'a>>,
        callback: Option<&'b Expression<'a>>,
    },
    /// `it(name, fn)` / `test(name, fn)` and their variants.
    Test {
        name: &'b Expression<'a>,
        modifiers: Vec<RawModifier<'b, 'a>>,
        callback: Option<&'b Expression<'a>>,
    },
    Hook {
        kind: HookKind,
        callback: Option<&'b Expression<'a>>,
    },
    /// `test.skip()`, `test.skip(cond, reason)`, `test.fixme(cond)`, `test.fail(cond)`:
    /// Playwright's skips decided while the tests run.
    Skip {
        name: &'b str,
        condition: Option<&'b Expression<'a>>,
    },
    /// Playwright's `test.step(name, fn)`: runs right away, as part of the test.
    Step {
        callback: Option<&'b Expression<'a>>,
    },
}

/// A modifier before its argument is turned into a value.
pub(super) struct RawModifier<'b, 'a> {
    pub(super) name: &'b str,
    pub(super) arg: RawArg<'b, 'a>,
}

#[derive(Clone, Copy)]
pub(super) enum RawArg<'b, 'a> {
    None,
    /// `it.each([…])`, `it.skipIf(cond)`: the first argument.
    Expr(&'b Expression<'a>),
    /// ``it.each`a | b` `` (a tagged template table).
    Template(&'b TemplateLiteral<'a>),
}

/// Modifiers a test or `describe` can carry.
const MODIFIERS: &[&str] = &[
    "skip",
    "only",
    "todo",
    "each",
    "for",
    "concurrent",
    "sequential",
    "fails",
    "fail",
    "fixme",
    "serial",
    "parallel",
    "shuffle",
    "skipIf",
    "runIf",
];

/// Modifiers that, called without a test, skip at run time in Playwright.
const RUNTIME_SKIPS: &[&str] = &["skip", "fixme", "fail"];

pub(super) fn structure<'b, 'a>(call: &'b CallExpression<'a>) -> Option<Structure<'b, 'a>> {
    let (base, segments) = chain(&call.callee)?;
    let args = &call.arguments;

    if let Some(kind) = hook_kind(base) {
        return segments.is_empty().then(|| Structure::Hook {
            kind,
            callback: last_function(args),
        });
    }

    let (implied, is_suite_base) = match base {
        "describe" | "suite" => (None, true),
        "xdescribe" => (Some("skip"), true),
        "fdescribe" => (Some("only"), true),
        "it" | "test" => (None, false),
        "xit" | "xtest" => (Some("skip"), false),
        "fit" => (Some("only"), false),
        _ => return None,
    };

    // Playwright hangs everything off `test`: `test.beforeEach`, `test.describe`, `test.step`.
    if let [only] = segments.as_slice()
        && let Some(kind) = hook_kind(only.name)
        && !is_suite_base
    {
        return Some(Structure::Hook {
            kind,
            callback: last_function(args),
        });
    }
    if let [only] = segments.as_slice()
        && only.name == "step"
    {
        return Some(Structure::Step {
            callback: last_function(args),
        });
    }

    let is_suite = is_suite_base || segments.first().is_some_and(|s| s.name == "describe");
    let modifier_segments = if is_suite && !is_suite_base {
        &segments[1..]
    } else {
        &segments[..]
    };
    if !modifier_segments
        .iter()
        .all(|segment| MODIFIERS.contains(&segment.name))
    {
        return None; // `test.use(…)`, `test.describe.configure(…)`, `test.extend(…)`, …
    }
    let mut modifiers: Vec<RawModifier> = implied
        .map(|name| RawModifier {
            name,
            arg: RawArg::None,
        })
        .into_iter()
        .collect();
    modifiers.extend(modifier_segments.iter().map(|segment| RawModifier {
        name: segment.name,
        arg: segment.arg,
    }));

    let first = args.first().and_then(Argument::as_expression);
    if is_suite {
        let anonymous = first.is_some_and(is_function);
        return Some(Structure::Suite {
            name: if anonymous { None } else { first },
            modifiers,
            callback: last_function(args),
        });
    }

    // `test.skip(cond)` versus `test.skip('name', fn)`: a test has a name first (not a
    // function, which would be Playwright's `test.skip(({ browserName }) => …)`), then a
    // function to run, or just a name literal (`it.todo('name')`).
    let declares_test = first.is_some_and(|first| !is_function(first))
        && (first.is_some_and(is_name_literal) || last_function(&args[1..]).is_some());
    if !declares_test {
        if let [segment] = modifier_segments
            && implied.is_none()
            && RUNTIME_SKIPS.contains(&segment.name)
        {
            return Some(Structure::Skip {
                name: segment.name,
                condition: first,
            });
        }
        return None;
    }
    Some(Structure::Test {
        name: first?,
        modifiers,
        callback: last_function(args),
    })
}

/// One step in a callee chain: `.skip`, or `.each(table)`.
#[derive(Clone, Copy)]
struct Segment<'b, 'a> {
    name: &'b str,
    arg: RawArg<'b, 'a>,
}

/// Splits `it.concurrent.each(table)` into `("it", [concurrent, each(table)])`.
fn chain<'b, 'a>(callee: &'b Expression<'a>) -> Option<(&'b str, Vec<Segment<'b, 'a>>)> {
    match callee.get_inner_expression() {
        Expression::Identifier(id) => Some((id.name.as_str(), Vec::new())),
        Expression::StaticMemberExpression(member) => {
            let (base, mut segments) = chain(&member.object)?;
            segments.push(Segment {
                name: member.property.name.as_str(),
                arg: RawArg::None,
            });
            Some((base, segments))
        }
        // `it.each(table)` used as a callee: the table belongs to the last segment.
        Expression::CallExpression(inner) => {
            let (base, mut segments) = chain(&inner.callee)?;
            let last = segments.last_mut()?;
            if !matches!(last.arg, RawArg::None) {
                return None;
            }
            last.arg = match inner.arguments.first().and_then(Argument::as_expression) {
                Some(arg) => RawArg::Expr(arg),
                None => RawArg::None,
            };
            Some((base, segments))
        }
        Expression::TaggedTemplateExpression(tagged) => {
            let (base, mut segments) = chain(&tagged.tag)?;
            let last = segments.last_mut()?;
            last.arg = RawArg::Template(&tagged.quasi);
            Some((base, segments))
        }
        _ => None,
    }
}

fn hook_kind(name: &str) -> Option<HookKind> {
    match name {
        "beforeEach" => Some(HookKind::BeforeEach),
        "afterEach" => Some(HookKind::AfterEach),
        "beforeAll" => Some(HookKind::BeforeAll),
        "afterAll" => Some(HookKind::AfterAll),
        _ => None,
    }
}

fn last_function<'b, 'a>(args: &'b [Argument<'a>]) -> Option<&'b Expression<'a>> {
    args.iter()
        .filter_map(Argument::as_expression)
        .rfind(|arg| is_function(arg))
}

fn is_function(expr: &Expression) -> bool {
    matches!(
        expr.get_inner_expression(),
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
    )
}

fn is_name_literal(expr: &Expression) -> bool {
    matches!(
        expr.get_inner_expression(),
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_)
    )
}

/// The function passed to `vi.waitFor(fn)`, `vi.waitUntil(fn)` or Testing Library's
/// `waitFor(fn)`. They call `fn` until it passes and reject if it never does, so when the
/// test awaits them, a check in `fn` fails the test like any other.
pub(super) fn retry_callback<'b, 'a>(call: &'b CallExpression<'a>) -> Option<&'b Expression<'a>> {
    let (base, segments) = chain(&call.callee)?;
    let retries = match (base, segments.as_slice()) {
        ("waitFor", []) => true,
        ("vi" | "vitest", [segment]) => {
            matches!(segment.name, "waitFor" | "waitUntil") && matches!(segment.arg, RawArg::None)
        }
        _ => false,
    };
    let callback = call.arguments.first().and_then(Argument::as_expression)?;
    (retries && is_function(callback)).then_some(callback)
}

/// The parts of an `expect(…)…` call, before they're turned into IR.
pub(super) struct ExpectCall<'b, 'a> {
    pub(super) subject: Option<&'b Expression<'a>>,
    pub(super) kind: ExpectKind,
    pub(super) negated: bool,
    pub(super) promise: Option<Promise>,
    /// The matcher's name and arguments. `None` for a bare `expect(x)`.
    pub(super) matcher: Option<(&'b str, &'b [Argument<'a>])>,
}

/// `expect(subject)`, optionally `.not` / `.resolves` / `.rejects`, then `.toX(args)`.
/// Also a bare `expect(subject)`, which checks nothing.
pub(super) fn expect_call<'b, 'a>(call: &'b CallExpression<'a>) -> Option<ExpectCall<'b, 'a>> {
    if let Expression::StaticMemberExpression(member) = call.callee.get_inner_expression() {
        let mut negated = false;
        let mut promise = None;
        let mut object = &member.object;
        loop {
            match object.get_inner_expression() {
                Expression::StaticMemberExpression(inner) => {
                    match inner.property.name.as_str() {
                        "not" => negated = true,
                        "resolves" => promise = Some(Promise::Resolves),
                        "rejects" => promise = Some(Promise::Rejects),
                        _ => return None,
                    }
                    object = &inner.object;
                }
                Expression::CallExpression(root) => {
                    let kind = expect_kind(root)?;
                    return Some(ExpectCall {
                        subject: root.arguments.first().and_then(Argument::as_expression),
                        kind,
                        negated,
                        promise,
                        matcher: Some((member.property.name.as_str(), &call.arguments)),
                    });
                }
                // `expect.soft(x)` itself, with no matcher: checked below.
                _ => break,
            }
        }
    }
    let kind = expect_kind(call)?;
    Some(ExpectCall {
        subject: call.arguments.first().and_then(Argument::as_expression),
        kind,
        negated: false,
        promise: None,
        matcher: None,
    })
}

/// `expect(…)`, `expect.soft(…)`, `expect.poll(…)` or vitest browser mode's
/// `expect.element(…)`.
fn expect_kind(call: &CallExpression) -> Option<ExpectKind> {
    match call.callee.get_inner_expression() {
        Expression::Identifier(id) if id.name == "expect" => Some(ExpectKind::Plain),
        Expression::StaticMemberExpression(member) if is_expect(&member.object) => {
            match member.property.name.as_str() {
                "soft" => Some(ExpectKind::Soft),
                "poll" => Some(ExpectKind::Poll),
                "element" => Some(ExpectKind::Element),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `expect.assertions(n)` (`Some(n)`) or `expect.hasAssertions()` (`None`).
pub(super) fn assertion_count<'b, 'a>(
    call: &'b CallExpression<'a>,
) -> Option<Option<&'b Expression<'a>>> {
    let Expression::StaticMemberExpression(member) = call.callee.get_inner_expression() else {
        return None;
    };
    if !is_expect(&member.object) {
        return None;
    }
    match member.property.name.as_str() {
        "assertions" => Some(call.arguments.first().and_then(Argument::as_expression)),
        "hasAssertions" => Some(None),
        _ => None,
    }
}

/// The matcher names in `expect.extend({ toBeWithinRange() {…} })`.
pub(super) fn extended_matchers(call: &CallExpression) -> Option<Vec<String>> {
    let Expression::StaticMemberExpression(member) = call.callee.get_inner_expression() else {
        return None;
    };
    if !is_expect(&member.object) || member.property.name != "extend" {
        return None;
    }
    let Some(Expression::ObjectExpression(object)) = call
        .arguments
        .first()
        .and_then(Argument::as_expression)
        .map(Expression::get_inner_expression)
    else {
        return Some(Vec::new());
    };
    Some(
        object
            .properties
            .iter()
            .filter_map(|property| match property {
                ObjectPropertyKind::ObjectProperty(property) => property.key.static_name(),
                ObjectPropertyKind::SpreadProperty(_) => None,
            })
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect(),
    )
}

/// `expect.objectContaining(…)` and friends used as a value: the name after `expect.`
/// (`not.objectContaining` for `expect.not.objectContaining`).
pub(super) fn value_matcher_name(call: &CallExpression) -> Option<String> {
    let Expression::StaticMemberExpression(member) = call.callee.get_inner_expression() else {
        return None;
    };
    let name = member.property.name.as_str();
    match member.object.get_inner_expression() {
        object if is_expect(object) => Some(name.to_owned()),
        Expression::StaticMemberExpression(not)
            if not.property.name == "not" && is_expect(&not.object) =>
        {
            Some(format!("not.{name}"))
        }
        _ => None,
    }
}

fn is_expect(expr: &Expression) -> bool {
    matches!(expr.get_inner_expression(), Expression::Identifier(id) if id.name == "expect")
}
