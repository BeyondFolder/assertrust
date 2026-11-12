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
    /// Named functions outside tests and hooks, which tests may call:
    /// `function expectOk(res) { … }`, `const expectOk = (res) => { … }`.
    pub helpers: Vec<Helper>,
    /// What this file imports (`import { expectOk } from './utils'`) or passes on
    /// (`export * from './api'`). Imports from packages aren't followed.
    pub imports: Vec<Import>,
}

/// A named function outside tests and hooks. Its checks run for the tests that call it,
/// once [`link`](crate::link) has put them there.
#[derive(Debug, Clone, PartialEq)]
pub struct Helper {
    pub name: String,
    /// The `describe` blocks it's defined in, outermost first: it's visible to tests in them.
    pub suites: Vec<String>,
    /// Its parameters in order: a name, or `None` for one that isn't a plain name
    /// (`{ a, b }`, `x = 1`, `...rest`). A call's arguments are filled in for the names.
    pub params: Vec<Option<String>>,
    /// `async`: a call that isn't awaited may finish after the test.
    pub is_async: bool,
    /// `export default`.
    pub default_export: bool,
    pub line: u32,
    /// Its last line: an allow comment anywhere from `line - 1` to here is about it.
    pub end_line: u32,
    pub body: Body,
}

/// One name imported, or passed on, from another file or a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The name in this file. `*` for `export * from './api'`.
    pub local: String,
    /// The name where it's defined: `default` for a default import, `*` for `export *`.
    pub imported: String,
    /// As written: `./utils`.
    pub from: String,
    /// `export … from`: the file passes it on rather than using it.
    pub reexport: bool,
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
    /// Its last line: an allow comment anywhere from `line - 1` to here is about it.
    pub end_line: u32,
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
    /// Its last line.
    pub end_line: u32,
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
    /// Used for reporting, never compared. In the file the item is written in: a helper's
    /// file for one that runs through a helper.
    pub line: u32,
    pub context: Context,
    /// The helpers (or hook) it runs through for the test, outermost first. Empty for an
    /// item written in the test itself.
    pub via: Vec<Via>,
}

impl Item {
    /// The file the item is written in: its helper's, or `None` for the test's own file.
    pub fn path(&self) -> Option<&str> {
        self.via.last().and_then(|via| via.path.as_deref())
    }

    /// How it runs for the test, as `via expectOk(res) > expectStatus(res, 200)`. `None`
    /// for an item written in the test itself.
    pub fn via_text(&self) -> Option<String> {
        let calls: Vec<&str> = self.via.iter().map(|via| via.call.as_str()).collect();
        (!calls.is_empty()).then(|| format!("via {}", calls.join(" > ")))
    }
}

/// One step from a test to a check that runs on its behalf: a call to a helper, or a hook.
#[derive(Debug, Clone, PartialEq)]
pub struct Via {
    /// The helper or hook: `expectOk`, `beforeEach`.
    pub name: String,
    /// The call as written (`expectOk(res)`), or the hook's name.
    pub call: String,
    /// The line of the call (or the hook), in the file it's written in.
    pub line: u32,
    /// Where the call sits.
    pub context: Context,
    /// The call is `await`ed (or returned). Always true for a hook.
    pub awaited: bool,
    /// The call's arguments, filled into the helper's checks.
    pub args: Vec<Value>,
    /// The file the helper is defined in, when it isn't the test's.
    pub path: Option<String>,
    /// The helper's (or hook's) line, in that file.
    pub defined: u32,
}

/// A call to a function by its name, which may be a helper: `expectOk(res)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub args: Vec<Value>,
    /// The call as normalized source, for messages: [`Call::text_of`].
    pub text: String,
    /// `await`ed or returned, so the caller waits for it.
    pub awaited: bool,
    /// Where linking found the helper it runs. `None` before linking, and for a call
    /// linking couldn't follow: to a package, through an alias it can't resolve, or into a
    /// file it doesn't read.
    pub defined: Option<Defined>,
}

impl Call {
    /// A call's text from its name and arguments: `expectStatus(res, 200)`.
    pub fn text_of(name: &str, args: &[Value]) -> String {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        format!("{name}({})", args.join(", "))
    }
}

/// Where a helper (or hook) is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defined {
    /// Its file, when it isn't the test's.
    pub path: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    /// `expect(…)…`
    Assertion(Assertion),
    /// `expect.assertions(n)` (with `n`) or `expect.hasAssertions()` (without).
    AssertionCount(Option<Value>),
    /// A `return` before the end of the test, hook or helper, so what follows in it may not
    /// run.
    EarlyReturn,
    /// A skip decided while the test runs: `test.skip(cond)`, `ctx.skip()`. Holds the
    /// condition, if any, as normalized source.
    RuntimeSkip(Option<String>),
    /// A call to a helper defined in the file, or to something imported. Linking follows it
    /// with the helper's items. In a linked test, each hook it runs is a call too.
    Call(Call),
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
            ExpectKind::Element => "expect.element",
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

/// Which `expect`: plain, `expect.soft` (doesn't stop the test), `expect.poll` (retries), or
/// vitest browser mode's `expect.element` (retries until the element matches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectKind {
    Plain,
    Soft,
    Poll,
    Element,
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
    /// Inside a loop that may run zero times: not `for (const x of [a, b])`, a `for` with
    /// literal bounds, `while (true)` or `do … while`, up to a `break` or `continue` in them.
    pub in_loop: bool,
    /// Inside a `try` block that has a `catch` (a failure may be caught).
    pub in_try: bool,
    /// Inside a `catch` block (only runs if something threw).
    pub in_catch: bool,
    /// Inside a function nested in the test, e.g. a `.forEach` callback. Not a `test.step`
    /// or an awaited `vi.waitFor`, which run as part of the test, nor a function declared in
    /// the test, which is read where it's called (`const hover = () => {…}; hover()`).
    pub in_callback: bool,
    /// On the right of `&&`, `||` or `??`.
    pub short_circuit: bool,
    /// In an async helper the caller doesn't `await`: it may finish after the test.
    pub not_awaited: bool,
}

impl Context {
    pub fn is_plain(self) -> bool {
        self == Self::default()
    }

    /// Everything either context has: a check in an `if` in a helper called from a loop is
    /// in an `if` and in a loop.
    pub fn with(self, other: Self) -> Self {
        Self {
            in_if: self.in_if || other.in_if,
            in_loop: self.in_loop || other.in_loop,
            in_try: self.in_try || other.in_try,
            in_catch: self.in_catch || other.in_catch,
            in_callback: self.in_callback || other.in_callback,
            short_circuit: self.short_circuit || other.short_circuit,
            not_awaited: self.not_awaited || other.not_awaited,
        }
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
            (self.not_awaited, "not awaited"),
        ];
        let names: Vec<&str> = flags
            .iter()
            .filter(|(set, _)| *set)
            .map(|(_, name)| *name)
            .collect();
        f.write_str(&names.join(", "))
    }
}
