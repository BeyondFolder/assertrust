//! The test IR (`docs/architecture.md`, step 3): what a file's tests check, in a form that
//! ignores formatting. Nothing here depends on the parser, so another language can produce
//! the same types later.

use std::fmt;

use crate::matcher::MatcherKind;
use crate::value::Value;

/// Everything assertrust found in one file at one revision.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileIr {
    /// Tests in source order.
    pub tests: Vec<Test>,
    /// `beforeEach` / `afterEach` / `beforeAll` / `afterAll`, in source order.
    pub hooks: Vec<Hook>,
    /// Items outside any test or hook: at the top level, or directly inside a `describe`.
    pub outside: Body,
    /// Matchers added with `expect.extend({ … })`. Tracked, never judged.
    pub custom_matchers: Vec<String>,
}

/// One `it(…)` / `test(…)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Test {
    /// Names of the enclosing `describe` blocks, outermost first.
    pub suites: Vec<String>,
    /// The name as written. Template literals keep `${…}`; `.each` keeps its format
    /// string (`'upgrades to %s'`).
    pub name: String,
    /// The test's own modifiers plus the ones inherited from its `describe` blocks
    /// (outermost first), so a test inside `describe.skip` has `skip`.
    pub modifiers: Vec<Modifier>,
    pub line: u32,
    pub body: Body,
    /// The distinct words in the test's source (names, literals, matchers), sorted. Used to
    /// pair a test that was renamed: its body usually stays similar.
    pub fingerprint: Vec<String>,
}

impl Test {
    /// The id used to pair tests before and after a change: `describe` names and the test
    /// name, joined with ` > `.
    pub fn id(&self) -> String {
        let mut parts: Vec<&str> = self.suites.iter().map(String::as_str).collect();
        parts.push(&self.name);
        parts.join(" > ")
    }
}

/// A variant on a test or `describe`: `skip`, `only`, `todo`, `concurrent`, `fails`,
/// `fixme`, `serial`, …, or one that takes a value: `each(table)`, `skipIf(cond)`,
/// `runIf(cond)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Modifier {
    pub name: String,
    /// The `.each` table, or the `skipIf` / `runIf` condition (as an opaque value).
    pub arg: Option<Value>,
}

impl fmt::Display for Modifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.arg {
            Some(arg) => write!(f, "{}({arg})", self.name),
            None => f.write_str(&self.name),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hook {
    pub kind: HookKind,
    /// Names of the enclosing `describe` blocks, outermost first.
    pub suites: Vec<String>,
    pub line: u32,
    pub body: Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    BeforeEach,
    AfterEach,
    BeforeAll,
    AfterAll,
}

impl HookKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeEach => "beforeEach",
            Self::AfterEach => "afterEach",
            Self::BeforeAll => "beforeAll",
            Self::AfterAll => "afterAll",
        }
    }
}

/// What a test, hook or the outside of tests contains, in source order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Body {
    pub items: Vec<Item>,
}

impl Body {
    /// The checks in this body.
    pub fn assertions(&self) -> impl Iterator<Item = &Assertion> {
        self.items.iter().filter_map(|item| match &item.kind {
            ItemKind::Assertion(assertion) => Some(assertion),
            _ => None,
        })
    }
}

/// One thing in a body that decides whether the test can fail.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    /// Used for reporting, never compared.
    pub line: u32,
    pub context: Context,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    /// `expect(…)…`
    Assertion(Assertion),
    /// `expect.assertions(n)` (with `n`) or `expect.hasAssertions()` (without).
    AssertionCount(Option<Value>),
    /// A `return` before the end of the test, so what follows may not run.
    EarlyReturn,
    /// A skip decided while the test runs: `test.skip(cond)`, `ctx.skip()`. Holds the
    /// condition, if any, as normalized source.
    RuntimeSkip(Option<String>),
}

/// One check: `expect(subject)`, then optionally `.not` / `.resolves` / `.rejects`, then a
/// matcher.
#[derive(Debug, Clone, PartialEq)]
pub struct Assertion {
    /// The subject's normalized source, e.g. `res.body`.
    pub subject: String,
    pub expect: ExpectKind,
    pub negated: bool,
    pub promise: Option<Promise>,
    /// `await`ed or `return`ed, so the test waits for it.
    pub awaited: bool,
    /// `None` for a bare `expect(x)`, which checks nothing.
    pub matcher: Option<Matcher>,
    /// What the check states, in a form that doesn't depend on how it's written. Empty for
    /// a bare `expect(x)`, and for a negated check of several things at once.
    pub facts: Vec<Fact>,
}

impl fmt::Display for Assertion {
    /// Normalized source, e.g. `await expect(p).resolves.toEqual({ a: 1 })`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.awaited {
            f.write_str("await ")?;
        }
        let expect = match self.expect {
            ExpectKind::Plain => "expect",
            ExpectKind::Soft => "expect.soft",
            ExpectKind::Poll => "expect.poll",
        };
        write!(f, "{expect}({})", self.subject)?;
        if self.negated {
            f.write_str(".not")?;
        }
        match self.promise {
            Some(Promise::Resolves) => f.write_str(".resolves")?,
            Some(Promise::Rejects) => f.write_str(".rejects")?,
            None => {}
        }
        if let Some(matcher) = &self.matcher {
            write!(f, ".{}(", matcher.name)?;
            for (i, arg) in matcher.args.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{arg}")?;
            }
            f.write_str(")")?;
        }
        Ok(())
    }
}

/// One thing a check states about a value. The same fact written two ways is equal:
/// `expect(reply.text).toBe('hi')`, `expect(reply).toMatchObject({ text: 'hi' })` and
/// `expect(reply).toHaveProperty('text', 'hi')` all state `reply.text == 'hi'`.
///
/// `docs/architecture.md` (step 4) lists the forms that are recognized.
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    /// The value checked, without the properties read from it, as normalized source: `reply`
    /// for `reply.text`. A resolved promise is `await p`.
    pub base: String,
    /// The properties read from the base, e.g. `["text"]`. Some forms add a name of their
    /// own: `["length"]` for `toHaveLength`, `["text"]` for Playwright's `toHaveText`.
    pub path: Vec<String>,
    pub negated: bool,
    pub test: FactTest,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FactTest {
    /// Equal to the value (`toBe`, `toEqual`, a field in `toMatchObject`, `toBeNull`, …).
    Equals(Value),
    /// Present and not `undefined`: `toBeDefined()`, `toHaveProperty('key')`.
    Defined,
    /// `toBeTruthy()`, and `toBe(true)`, which states the same for pairing.
    Truthy,
    /// `toBeFalsy()`, and `toBe(false)`.
    Falsy,
    /// Any other matcher, with its arguments: `toContain('x')`, `toBeGreaterThan(1)`.
    Other { matcher: String, args: Vec<Value> },
}

impl fmt::Display for Fact {
    /// e.g. `reply.text == 'hi'`, `not res.status == 500`, `items toContain('x')`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negated {
            f.write_str("not ")?;
        }
        f.write_str(&self.base)?;
        for key in &self.path {
            write!(f, ".{key}")?;
        }
        match &self.test {
            FactTest::Equals(value) => write!(f, " == {value}"),
            FactTest::Defined => f.write_str(" is defined"),
            FactTest::Truthy => f.write_str(" is truthy"),
            FactTest::Falsy => f.write_str(" is falsy"),
            FactTest::Other { matcher, args } => {
                write!(f, " {matcher}(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                f.write_str(")")
            }
        }
    }
}

/// Which `expect`: plain, `expect.soft` (doesn't stop the test) or `expect.poll` (retries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectKind {
    Plain,
    Soft,
    Poll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promise {
    Resolves,
    Rejects,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Matcher {
    /// As written, e.g. `toMatchObject`.
    pub name: String,
    pub kind: MatcherKind,
    pub args: Vec<Value>,
}

/// Where an item sits, when that means it might not run or might not fail the test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Context {
    /// Inside an `if` or a `? :`.
    pub in_if: bool,
    /// Inside a `for` / `while` loop.
    pub in_loop: bool,
    /// Inside a `try` block that has a `catch` (a failure may be caught).
    pub in_try: bool,
    /// Inside a `catch` block (only runs if something threw).
    pub in_catch: bool,
    /// Inside a function nested in the test, e.g. a `.forEach` callback. Not a `test.step`
    /// or an awaited `vi.waitFor`, which run as part of the test.
    pub in_callback: bool,
    /// On the right of `&&`, `||` or `??`.
    pub short_circuit: bool,
}

impl Context {
    pub fn is_plain(self) -> bool {
        self == Self::default()
    }
}

impl fmt::Display for Context {
    /// e.g. `in if, in callback`. Empty when nothing applies.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flags = [
            (self.in_if, "in if"),
            (self.in_loop, "in loop"),
            (self.in_try, "in try"),
            (self.in_catch, "in catch"),
            (self.in_callback, "in callback"),
            (self.short_circuit, "short-circuit"),
        ];
        let names: Vec<&str> = flags
            .iter()
            .filter(|(set, _)| *set)
            .map(|(_, name)| *name)
            .collect();
        f.write_str(&names.join(", "))
    }
}
