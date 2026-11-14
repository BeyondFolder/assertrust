//! What a check states, as [`Fact`]s that don't depend on how it's written
//! (`docs/architecture.md`, step 4, "Rewritten checks").
//!
//! The same check can be written many ways, and refactors and linters move between them:
//!
//! ```text
//! expect(reply.text).toBe('hi')                 reply.text == 'hi'
//! expect(reply).toMatchObject({ text: 'hi' })   reply.text == 'hi'
//! expect(reply).toHaveProperty('text', 'hi')    reply.text == 'hi'
//! expect(list.length).toBe(2)                   list.length == 2
//! expect(list).toHaveLength(2)                  list.length == 2
//! expect(list.includes('a')).toBe(true)         list toContain('a')
//! expect(await page.title()).toBe('Inbox')      page.title == 'Inbox'
//! await expect(page).toHaveTitle('Inbox')       page.title == 'Inbox'
//! expect(arrow.y).toBeCloseTo(239, 1)           arrow.y == 239
//! ```
//!
//! Matching pairs two checks that state a fact in common, so a rewrite isn't read as one
//! check removed and another added.

use oxc::allocator::Allocator;
use oxc::ast::ast::{Argument, BinaryOperator, ChainElement, Expression, UnaryOperator};
use oxc::parser::Parser;
use oxc::span::SourceType;

use super::calls::ExpectCall;
use super::consts::Consts;
use super::values::{Values, js_str, normalize};
use crate::matcher::close_to;
use crate::model::{Fact, FactTest, Promise};
use crate::value::Value;

/// Playwright reads a page or locator with a method, or checks it with a matcher. Both map to
/// the same step in the path: `await locator.textContent()` and `toHaveText(…)` are
/// `.text()`. The `()` keeps it apart from a property: `store.count` isn't `store.count()`.
const READERS: &[(&str, &str)] = &[
    ("textContent", "text()"),
    ("innerText", "text()"),
    ("title", "title()"),
    ("url", "url()"),
    ("count", "count()"),
    ("inputValue", "value()"),
    ("isVisible", "visible()"),
    ("isHidden", "hidden()"),
    ("isChecked", "checked()"),
    ("isEnabled", "enabled()"),
    ("isDisabled", "disabled()"),
    ("isEditable", "editable()"),
    ("ok", "ok()"),
];

/// Playwright matchers that check one of those for a value.
const VALUE_MATCHERS: &[(&str, &str)] = &[
    ("toHaveText", "text()"),
    ("toHaveTitle", "title()"),
    ("toHaveURL", "url()"),
    ("toHaveCount", "count()"),
    ("toHaveValue", "value()"),
];

/// Playwright matchers that check one of those is true.
const STATE_MATCHERS: &[(&str, &str)] = &[
    ("toBeVisible", "visible()"),
    ("toBeHidden", "hidden()"),
    ("toBeChecked", "checked()"),
    ("toBeEnabled", "enabled()"),
    ("toBeDisabled", "disabled()"),
    ("toBeEditable", "editable()"),
    ("toBeOK", "ok()"),
];

pub(super) fn facts(found: &ExpectCall, values: &Values, consts: &Consts) -> Vec<Fact> {
    let (Some(subject), Some((matcher, raw_args))) = (found.subject, found.matcher) else {
        return Vec::new();
    };
    if found.promise == Some(Promise::Rejects) {
        return Vec::new(); // what a rejection holds isn't a value of the subject
    }
    let args: Vec<Value> = raw_args
        .iter()
        .map(|arg| match arg.as_expression() {
            Some(expr) => values.value(expr, consts),
            None => Value::Opaque("...".to_owned()),
        })
        .collect();

    // `expect(a === b).toBe(true)` states `a == b`.
    let mut negated = found.negated;
    let (subject, leaves) = match truth(matcher, &args)
        .and_then(|truth| boolean(subject, values, consts).map(|found| (truth, found)))
    {
        Some((truth, (inner, test, flipped))) => {
            negated ^= !truth ^ flipped;
            (inner, vec![(Vec::new(), test)])
        }
        None => (subject, leaves(matcher, raw_args, &args, found.negated)),
    };
    let (subject, leaves) = typeof_check(subject, leaves);
    if leaves.is_empty() || (negated && leaves.len() > 1) {
        // `.not.toMatchObject({ a: 1, b: 2 })` states "not both", which isn't a fact per key.
        return Vec::new();
    }

    let target = if found.promise == Some(Promise::Resolves) {
        Target {
            base: format!("await {}", normalize(subject)),
            path: Vec::new(),
        }
    } else {
        target(subject, matcher)
    };
    leaves
        .into_iter()
        .map(|(path, test)| {
            let mut full = target.path.clone();
            full.extend(path);
            Fact {
                base: target.base.clone(),
                path: full,
                negated,
                test,
            }
        })
        .collect()
}

/// What a subject given as source reads, the way a check written in a test is read: its base
/// and the properties read from it (`(await refund()).body.ok` → `await refund()` and
/// `[body, ok]`). For a helper's check once linking has filled in a call's arguments. `None`
/// when it doesn't parse.
pub(crate) fn subject_target(source: &str) -> Option<(String, Vec<String>)> {
    let allocator = Allocator::default();
    let expr = Parser::new(&allocator, source, SourceType::ts().with_module(true))
        .parse_expression()
        .ok()?;
    let target = target(&expr, "");
    Some((target.base, target.path))
}

/// What a check reads: a base value and the properties read from it.
struct Target {
    base: String,
    path: Vec<String>,
}

/// `res.body.status` → `res` and `[body, status]`. Playwright readers become a step:
/// `await page.title()` → `page` and `[title()]`. For `toThrow`, `() => obj.parse()` is
/// `obj.parse`, so it's `obj` and `[parse]` like the direct form.
fn target(subject: &Expression, matcher: &str) -> Target {
    let mut path = Vec::new();
    let mut expr = subject.get_inner_expression();
    if matcher.starts_with("toThrow")
        && let Some(callee) = thunk(expr)
    {
        expr = callee;
    }
    loop {
        if let Some((object, key)) = member(expr) {
            path.push(key);
            expr = object.get_inner_expression();
        } else if let Some((object, key)) = reader(expr) {
            path.push(key);
            expr = object.get_inner_expression();
        } else {
            break;
        }
    }
    path.reverse();
    Target {
        base: normalize(expr),
        path,
    }
}

/// `a.b`, `a?.b`, `a['b']`, `a[0]`: the object and the key.
fn member<'b, 'a>(expr: &'b Expression<'a>) -> Option<(&'b Expression<'a>, String)> {
    let member = match expr {
        Expression::ChainExpression(chain) => chain.expression.member_expression()?,
        _ => expr.as_member_expression()?,
    };
    let key = match member.static_property_name() {
        Some(name) => js_str(name),
        None => match member {
            oxc::ast::ast::MemberExpression::ComputedMemberExpression(computed) => {
                match computed.expression.get_inner_expression() {
                    Expression::NumericLiteral(n) => format!("{}", n.value),
                    _ => return None,
                }
            }
            _ => return None,
        },
    };
    Some((member.object(), key))
}

/// `await locator.textContent()`, `page.url()`, `await locator.getAttribute('href')`: the
/// page or locator and the property read.
fn reader<'b, 'a>(expr: &'b Expression<'a>) -> Option<(&'b Expression<'a>, String)> {
    let expr = match expr {
        Expression::AwaitExpression(await_expr) => await_expr.argument.get_inner_expression(),
        _ => expr,
    };
    let call = match expr {
        Expression::CallExpression(call) => call,
        Expression::ChainExpression(chain) => match &chain.expression {
            ChainElement::CallExpression(call) => call,
            _ => return None,
        },
        _ => return None,
    };
    let callee = call.callee.get_inner_expression().as_member_expression()?;
    let method = callee.static_property_name()?;
    let method = method.as_str()?;
    if method == "getAttribute"
        && let [Argument::StringLiteral(name)] = call.arguments.as_slice()
    {
        return Some((
            callee.object(),
            format!("attribute({})", Value::String(js_str(name.value))),
        ));
    }
    let (_, property) = READERS.iter().find(|(name, _)| *name == method)?;
    call.arguments
        .is_empty()
        .then(|| (callee.object(), (*property).to_owned()))
}

/// `() => f()` → `f`: the same function passed to `toThrow` directly.
fn thunk<'b, 'a>(expr: &'b Expression<'a>) -> Option<&'b Expression<'a>> {
    let Expression::ArrowFunctionExpression(arrow) = expr else {
        return None;
    };
    if !arrow.params.items.is_empty() || arrow.params.rest.is_some() {
        return None;
    }
    match arrow.get_expression()?.get_inner_expression() {
        Expression::CallExpression(call) if call.arguments.is_empty() => {
            Some(call.callee.get_inner_expression())
        }
        _ => None,
    }
}

/// `expect(typeof x).toBe('string')` states `x toBeTypeOf('string')`.
fn typeof_check<'b, 'a>(
    subject: &'b Expression<'a>,
    leaves: Vec<(Vec<String>, FactTest)>,
) -> (&'b Expression<'a>, Vec<(Vec<String>, FactTest)>) {
    if let Expression::UnaryExpression(unary) = subject.get_inner_expression()
        && unary.operator == UnaryOperator::Typeof
        && let [(path, FactTest::Equals(value))] = leaves.as_slice()
        && path.is_empty()
    {
        let test = FactTest::Other {
            matcher: "toBeTypeOf".to_owned(),
            args: vec![value.clone()],
        };
        return (&unary.argument, vec![(Vec::new(), test)]);
    }
    (subject, leaves)
}

/// Whether the matcher states its subject is true (`Some(true)`) or false: `toBe(true)`,
/// `toBeTruthy()`, `toBe(false)`, `toBeFalsy()`.
fn truth(matcher: &str, args: &[Value]) -> Option<bool> {
    match (matcher, args) {
        ("toBe" | "toEqual" | "toStrictEqual", [Value::Bool(value)]) => Some(*value),
        ("toBeTruthy", []) => Some(true),
        ("toBeFalsy", []) => Some(false),
        _ => None,
    }
}

/// A subject that is itself a check written as a boolean: `a === b`, `a !== b`, `a > 1`,
/// `x instanceof Y`, `list.includes(v)`, `!a`. Returns what it checks, the test, and
/// whether it's negated.
fn boolean<'b, 'a>(
    expr: &'b Expression<'a>,
    values: &Values,
    consts: &Consts,
) -> Option<(&'b Expression<'a>, FactTest, bool)> {
    match expr.get_inner_expression() {
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
            match boolean(&unary.argument, values, consts) {
                Some((inner, test, negated)) => Some((inner, test, !negated)),
                None => Some((&unary.argument, FactTest::Falsy, false)),
            }
        }
        Expression::BinaryExpression(binary) => {
            let right = values.value(&binary.right, consts);
            let other = |matcher: &str| FactTest::Other {
                matcher: matcher.to_owned(),
                args: vec![right.clone()],
            };
            let (test, negated) = match binary.operator {
                BinaryOperator::StrictEquality | BinaryOperator::Equality => (equals(right), false),
                BinaryOperator::StrictInequality | BinaryOperator::Inequality => {
                    (equals(right), true)
                }
                BinaryOperator::GreaterThan => (other("toBeGreaterThan"), false),
                BinaryOperator::GreaterEqualThan => (other("toBeGreaterThanOrEqual"), false),
                BinaryOperator::LessThan => (other("toBeLessThan"), false),
                BinaryOperator::LessEqualThan => (other("toBeLessThanOrEqual"), false),
                BinaryOperator::Instanceof => (other("toBeInstanceOf"), false),
                _ => return None,
            };
            Some((&binary.left, test, negated))
        }
        Expression::CallExpression(call) => {
            let callee = call.callee.get_inner_expression().as_member_expression()?;
            let [arg] = call.arguments.as_slice() else {
                return None;
            };
            if callee.static_property_name()?.as_str()? != "includes" {
                return None;
            }
            let arg = values.value(arg.as_expression()?, consts);
            let test = FactTest::Other {
                matcher: "toContain".to_owned(),
                args: vec![arg],
            };
            Some((callee.object(), test, false))
        }
        _ => None,
    }
}

/// What the matcher states, as (path below the subject, test) pairs.
///
/// Equality is split into one fact per field (`toEqual({ a: 1, b: 2 })` states `a == 1`
/// and `b == 2`), but not under `.not`: `.not.toEqual({ a: 1 })` passes for `{ a: 1, b: 2 }`,
/// so it doesn't state `not a == 1`. A negated subset check does: `.not.toMatchObject({ a: 1 })`.
fn leaves(
    matcher: &str,
    raw: &[Argument],
    args: &[Value],
    negated: bool,
) -> Vec<(Vec<String>, FactTest)> {
    let first = args.first();
    let one = |path: &[&str], test: FactTest| -> Vec<(Vec<String>, FactTest)> {
        vec![(path.iter().map(|&key| key.to_owned()).collect(), test)]
    };
    let equal_to = |path: Vec<String>, value: &Value| match value {
        Value::Matcher { name, .. } if name == "objectContaining" => expand(path, value),
        _ if negated => vec![(path, equals(value.clone()))],
        _ => expand(path, value),
    };
    match matcher {
        "toBe" | "toEqual" | "toStrictEqual" => match first {
            Some(value) => equal_to(Vec::new(), value),
            None => Vec::new(),
        },
        "toMatchObject" => match first {
            Some(value) => expand(Vec::new(), value),
            None => Vec::new(),
        },
        "toHaveProperty" => {
            let Some(keys) = first.and_then(property_path) else {
                return Vec::new();
            };
            match args.get(1) {
                Some(value) => equal_to(keys, value),
                None => vec![(keys, FactTest::Defined)],
            }
        }
        "toHaveLength" => match first {
            Some(n) => one(&["length"], equals(n.clone())),
            None => Vec::new(),
        },
        // Close enough states the number, like `toBe(239)` written for a computed value.
        "toBeCloseTo" => match close_to(args) {
            Some(number) => one(&[], equals(number.clone())),
            None => one(
                &[],
                FactTest::Other {
                    matcher: matcher.to_owned(),
                    args: args.to_vec(),
                },
            ),
        },
        "toBeNull" => one(&[], FactTest::Equals(Value::Null)),
        "toBeUndefined" => one(&[], FactTest::Equals(Value::Undefined)),
        "toBeDefined" => one(&[], FactTest::Defined),
        "toBeTruthy" => one(&[], FactTest::Truthy),
        "toBeFalsy" => one(&[], FactTest::Falsy),
        "toHaveBeenCalledTimes" | "toBeCalledTimes" => match first {
            Some(n) => one(&["mock", "calls", "length"], equals(n.clone())),
            None => Vec::new(),
        },
        "toHaveBeenNthCalledWith" | "nthCalledWith" => match first.and_then(index_from_one) {
            Some(index) => one(
                &["mock", "calls", index.as_str()],
                FactTest::Equals(Value::Array(args[1..].to_vec())),
            ),
            None => Vec::new(),
        },
        "toContainText" => one(
            &["text()"],
            FactTest::Other {
                matcher: "toContain".to_owned(),
                args: args.to_vec(),
            },
        ),
        "toHaveAttribute" => match (raw.first(), args.get(1)) {
            (Some(Argument::StringLiteral(name)), value) => {
                let key = format!("attribute({})", Value::String(js_str(name.value)));
                match value {
                    Some(value) => one(&[key.as_str()], equals(value.clone())),
                    None => one(&[key.as_str()], FactTest::Defined),
                }
            }
            _ => Vec::new(),
        },
        _ => {
            if let Some((_, property)) = VALUE_MATCHERS.iter().find(|(name, _)| *name == matcher)
                && let Some(value) = first
            {
                return one(&[*property], equals(value.clone()));
            }
            if let Some((_, property)) = STATE_MATCHERS.iter().find(|(name, _)| *name == matcher) {
                return one(&[*property], FactTest::Truthy);
            }
            one(
                &[],
                FactTest::Other {
                    matcher: matcher.to_owned(),
                    args: args.to_vec(),
                },
            )
        }
    }
}

/// An expected value as facts: one per field of an object (nested objects too, and
/// `expect.objectContaining(…)`), or the value itself.
fn expand(path: Vec<String>, value: &Value) -> Vec<(Vec<String>, FactTest)> {
    match value {
        Value::Object(fields) if !fields.is_empty() => fields
            .iter()
            .filter(|(key, _)| !key.starts_with("..."))
            .flat_map(|(key, value)| {
                let mut path = path.clone();
                path.push(key.clone());
                expand(path, value)
            })
            .collect(),
        Value::Matcher { name, args } if name == "objectContaining" => match args.as_slice() {
            [object] => expand(path, object),
            _ => vec![(path, FactTest::Equals(value.clone()))],
        },
        _ => vec![(path, equals(value.clone()))],
    }
}

/// `true` and `false` state the same as `toBeTruthy` / `toBeFalsy` for pairing.
fn equals(value: Value) -> FactTest {
    match value {
        Value::Bool(true) => FactTest::Truthy,
        Value::Bool(false) => FactTest::Falsy,
        value => FactTest::Equals(value),
    }
}

/// `'a.b'` or `['a', 'b']` → `[a, b]`.
fn property_path(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(path) => Some(path.split('.').map(str::to_owned).collect()),
        Value::Array(keys) => keys
            .iter()
            .map(|key| match key {
                Value::String(key) | Value::Number(key) => Some(key.clone()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// `toHaveBeenNthCalledWith(1, …)` is the call at index `0`.
fn index_from_one(value: &Value) -> Option<String> {
    let Value::Number(n) = value else {
        return None;
    };
    let n: u32 = n.parse().ok()?;
    Some(n.checked_sub(1)?.to_string())
}

#[cfg(test)]
mod tests {
    use crate::{Fact, parse_file};

    fn facts(check: &str) -> Vec<Fact> {
        let source = format!("it('t', async () => {{ {check}; }});");
        let ir = parse_file("a.spec.ts", &source).expect("parses");
        let assertion = ir.tests[0].body.assertions().next().expect("a check");
        assertion.facts.clone()
    }

    fn shared(a: &str, b: &str) -> bool {
        let first = facts(a);
        facts(b).iter().any(|fact| first.contains(fact))
    }

    /// Each group is one fact written several ways (`docs/architecture.md`, step 4).
    #[test]
    fn the_same_fact_written_differently_is_equal() {
        let groups: &[&[&str]] = &[
            &[
                "expect(reply.text).toBe('hi')",
                "expect(reply).toMatchObject({ text: 'hi', html: null })",
                "expect(reply).toHaveProperty('text', 'hi')",
                "expect(reply).toEqual(expect.objectContaining({ text: 'hi' }))",
                "expect(reply?.text).toEqual('hi')",
                "expect(reply['text']).toStrictEqual('hi')",
            ],
            &[
                "expect(res.body.user.id).toBe(7)",
                "expect(res.body).toMatchObject({ user: { id: 7 } })",
                "expect(res).toHaveProperty('body.user.id', 7)",
                "expect(res).toHaveProperty(['body', 'user', 'id'], 7)",
            ],
            &[
                "expect(list.length).toBe(2)",
                "expect(list).toHaveLength(2)",
            ],
            &[
                "expect(list).toContain('a')",
                "expect(list.includes('a')).toBe(true)",
                "expect(list.includes('a')).toBeTruthy()",
            ],
            &[
                "expect(a).toBe(b)",
                "expect(a === b).toBe(true)",
                "expect(a == b).toBeTruthy()",
            ],
            &[
                "expect(a).not.toBe(b)",
                "expect(a !== b).toBe(true)",
                "expect(a === b).toBe(false)",
                "expect(a === b).not.toBe(true)",
            ],
            &["expect(n).toBeGreaterThan(5)", "expect(n > 5).toBe(true)"],
            &[
                "expect(x).toBeTypeOf('string')",
                "expect(typeof x).toBe('string')",
                "expect(typeof x === 'string').toBe(true)",
            ],
            &[
                "expect(e).toBeInstanceOf(TypeError)",
                "expect(e instanceof TypeError).toBe(true)",
            ],
            &[
                "expect(ok).toBeFalsy()",
                "expect(ok).toBe(false)",
                "expect(!ok).toBe(true)",
            ],
            &["expect(x).toBeNull()", "expect(x).toBe(null)"],
            &["expect(x).toBeUndefined()", "expect(x).toEqual(undefined)"],
            &[
                "await expect(load()).resolves.toEqual({ id: 1 })",
                "expect(await load()).toEqual({ id: 1 })",
                "expect((await load()).id).toBe(1)",
            ],
            &[
                "await expect(page).toHaveTitle('Inbox')",
                "expect(await page.title()).toBe('Inbox')",
            ],
            &[
                "await expect(row).toHaveText('Paid')",
                "expect(await row.textContent()).toBe('Paid')",
            ],
            &[
                "await expect(row).toContainText('Paid')",
                "expect(await row.innerText()).toContain('Paid')",
            ],
            &[
                "await expect(rows).toHaveCount(3)",
                "expect(await rows.count()).toBe(3)",
            ],
            &[
                "await expect(page).toHaveURL('/inbox')",
                "expect(page.url()).toBe('/inbox')",
            ],
            &[
                "await expect(button).toBeVisible()",
                "expect(await button.isVisible()).toBe(true)",
            ],
            &[
                "await expect(input).toHaveValue('x')",
                "expect(await input.inputValue()).toBe('x')",
            ],
            &[
                "await expect(link).toHaveAttribute('href', '/a')",
                "expect(await link.getAttribute('href')).toBe('/a')",
            ],
            &["await expect(res).toBeOK()", "expect(res.ok()).toBe(true)"],
            &[
                "expect(fn).toHaveBeenCalledTimes(2)",
                "expect(fn.mock.calls.length).toBe(2)",
            ],
            &[
                "expect(fn).toHaveBeenNthCalledWith(1, 'a', 1)",
                "expect(fn.mock.calls[0]).toEqual(['a', 1])",
            ],
            &[
                "expect(parse).toThrow('bad')",
                "expect(() => parse()).toThrow('bad')",
            ],
            &[
                "expect(obj.parse).toThrow('bad')",
                "expect(() => obj.parse()).toThrow('bad')",
            ],
            &[
                "expect(r).not.toMatchObject({ status: 'ok' })",
                "expect(r.status).not.toBe('ok')",
            ],
        ];
        for group in groups {
            for other in &group[1..] {
                assert!(
                    shared(group[0], other),
                    "{} and {other} should share a fact: {:?} vs {:?}",
                    group[0],
                    facts(group[0]),
                    facts(other)
                );
            }
        }
    }

    #[test]
    fn different_facts_stay_apart() {
        let pairs = [
            (
                "expect(reply.text).toBe('hi')",
                "expect(reply.title).toBe('hi')",
            ),
            (
                "expect(reply.text).toBe('hi')",
                "expect(reply.text).toBe('bye')",
            ),
            ("expect(x).toBe(1)", "expect(x).not.toBe(1)"),
            (
                "expect(p.id).resolves.toBe(1)",
                "expect((await p).id).toBe(1)",
            ),
            (
                "expect(() => parse(1)).toThrow()",
                "expect(parse).toThrow()",
            ),
            // A method call isn't the property of the same name.
            (
                "expect(store.count()).toBe(0)",
                "expect(store.count).toBe(0)",
            ),
            // `.not.toEqual({ status: 'ok' })` passes for `{ status: 'ok', extra: 1 }`.
            (
                "expect(r).not.toEqual({ status: 'ok' })",
                "expect(r.status).not.toBe('ok')",
            ),
            (
                "expect(r).not.toHaveProperty('a', { b: 1 })",
                "expect(r.a.b).not.toBe(1)",
            ),
        ];
        for (a, b) in pairs {
            assert!(!shared(a, b), "{a} and {b} shouldn't share a fact");
        }
    }

    #[test]
    fn checks_that_arent_one_fact_per_field_have_none() {
        assert!(facts("expect(a).not.toMatchObject({ x: 1, y: 2 })").is_empty());
        assert!(facts("await expect(p).rejects.toThrow('nope')").is_empty());
        assert!(facts("expect(a)").is_empty());
    }

    #[test]
    fn facts_read_like_the_check() {
        let text: Vec<String> =
            facts("expect(res).toMatchObject({ status: 'pending', body: { id: 1 } })")
                .iter()
                .map(ToString::to_string)
                .collect();
        assert_eq!(text, ["res.body.id == 1", "res.status == 'pending'"]);
        let text: Vec<String> = facts("expect(a).not.toContain('x')")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(text, ["not a toContain('x')"]);
    }
}
