//! Linking (`docs/architecture.md`, step 3, "Linked view"): each test's own checks, plus the
//! ones that run on its behalf from helpers and hooks, each tagged with how it gets there.
//!
//! ```text
//! function expectStatus(res, status) { expect(res.status).toBe(status) }
//! it('refunds', () => { expectStatus(refund(), 200) })
//! → refunds: expect(res.status).toBe(200), via expectStatus(refund(), 200)
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use crate::model::{
    Call, Context, Defined, FactTest, FileIr, Helper, Hook, HookKind, Item, ItemKind, Via,
};
use crate::value::Value;

/// Where linking finds helpers defined in other files.
pub trait Modules {
    /// The file an import in the file `from` points to (`specifier` as written, like
    /// `./utils`), with its path. `None` when it isn't known.
    fn module(&self, from: &str, specifier: &str) -> Option<(&str, &FileIr)>;
}

/// No other files: only helpers defined in the file itself are linked.
impl Modules for () {
    fn module(&self, _: &str, _: &str) -> Option<(&str, &FileIr)> {
        None
    }
}

/// How deep calls from helper to helper are followed, and re-exports too.
const MAX_DEPTH: usize = 8;

/// `file` (at `path`) with each test's body linked: each call to a helper followed by the
/// helper's items, with the call's arguments filled in, and the hooks around it added:
/// `beforeAll` and `beforeEach` first (outermost `describe` first), then the test's own,
/// then `afterEach` and `afterAll` (innermost first). Each hook starts with a call to it.
/// A call that can't be followed stays, with no [`Defined`](crate::Defined).
///
/// ```
/// let file = assertrust_ir::parse_file(
///     "a.test.ts",
///     "function expectStatus(res, status) { expect(res.status).toBe(status) }
///      it('refunds', () => { expectStatus(refund(), 200) })",
/// )?;
/// let linked = assertrust_ir::link("a.test.ts", &file, &());
/// let item = &linked.tests[0].body.items[1];
/// assert_eq!(item.via[0].name, "expectStatus");
/// let assertrust_ir::ItemKind::Assertion(check) = &item.kind else { unreachable!() };
/// assert_eq!(check.to_string(), "expect(res.status).toBe(200)");
/// # Ok::<(), assertrust_ir::ParseError>(())
/// ```
pub fn link(path: &str, file: &FileIr, modules: &dyn Modules) -> FileIr {
    let linker = Linker {
        modules,
        checks: RefCell::new(HashMap::new()),
    };
    let mut linked = file.clone();
    for (index, test) in file.tests.iter().enumerate() {
        let mut items = Vec::new();
        let hooks = |kinds: &[HookKind], innermost_first: bool| {
            let mut hooks: Vec<_> = file
                .hooks
                .iter()
                .filter(|hook| kinds.contains(&hook.kind) && test.suites.starts_with(&hook.suites))
                .collect();
            hooks.sort_by_key(|hook| hook.suites.len());
            if innermost_first {
                hooks.reverse();
            }
            hooks
        };
        let place = |suites| Place {
            path,
            other: false,
            file,
            suites,
        };
        let mut stack = Vec::new();
        for hook in hooks(&[HookKind::BeforeAll, HookKind::BeforeEach], false) {
            linker.hook(place(&hook.suites), hook, &mut stack, &mut items);
        }
        let own = Expanding::new(&[]);
        linker.expand(
            &test.body.items,
            place(&test.suites),
            &own,
            &mut stack,
            &mut items,
        );
        for hook in hooks(&[HookKind::AfterEach, HookKind::AfterAll], true) {
            linker.hook(place(&hook.suites), hook, &mut stack, &mut items);
        }
        linked.tests[index].body.items = items;
    }
    linked
}

fn hook_via(hook: &Hook) -> [Via; 1] {
    [Via {
        name: hook.kind.as_str().to_owned(),
        call: hook.kind.as_str().to_owned(),
        line: hook.line,
        context: Context::default(),
        awaited: true,
        args: Vec::new(),
        path: None,
        defined: hook.line,
    }]
}

/// A hook, as a call the runner makes for the test: dropping the hook drops it.
fn hook_call(hook: &Hook) -> Item {
    Item {
        kind: ItemKind::Call(Call {
            name: hook.kind.as_str().to_owned(),
            args: Vec::new(),
            text: hook.kind.as_str().to_owned(),
            awaited: true,
            defined: Some(Defined {
                path: None,
                line: hook.line,
            }),
        }),
        line: hook.line,
        context: Context::default(),
        via: Vec::new(),
    }
}

struct Linker<'m> {
    modules: &'m dyn Modules,
    /// Whether each helper (by file, name and line) runs a check, itself or through the
    /// helpers it calls. One that doesn't (a factory, a mock) isn't expanded: its call is
    /// all the rules need.
    checks: RefCell<HashMap<(String, String, u32), bool>>,
}

/// Where items are written: which file, and in which `describe` blocks.
#[derive(Clone, Copy)]
struct Place<'p> {
    /// The file's path, to find what its imports point to.
    path: &'p str,
    /// Not the test's own file.
    other: bool,
    file: &'p FileIr,
    suites: &'p [String],
}

/// How the items being expanded run for the test.
struct Expanding<'e> {
    via: &'e [Via],
    /// What every call on the way adds: a helper called in an `if` puts all of its checks
    /// in one.
    context: Context,
    /// The helper's parameters, with the values the call passed.
    args: HashMap<String, Value>,
    /// The call is awaited, so the test waits for what the helper returns.
    awaited: bool,
}

impl<'e> Expanding<'e> {
    fn new(via: &'e [Via]) -> Self {
        Self {
            via,
            context: Context::default(),
            args: HashMap::new(),
            awaited: true,
        }
    }
}

impl<'m> Linker<'m> {
    /// A hook that runs for the test: a call to it, then its items.
    fn hook(
        &self,
        place: Place<'m>,
        hook: &Hook,
        stack: &mut Vec<(String, String)>,
        out: &mut Vec<Item>,
    ) {
        out.push(hook_call(hook));
        let via = hook_via(hook);
        self.expand(&hook.body.items, place, &Expanding::new(&via), stack, out);
    }

    fn expand(
        &self,
        items: &[Item],
        place: Place<'m>,
        from: &Expanding,
        stack: &mut Vec<(String, String)>,
        out: &mut Vec<Item>,
    ) {
        for item in items {
            // A `return` in a hook or helper stays too, tagged like the rest: the rules read
            // it as ending only that function's items.
            let ItemKind::Call(call) = &item.kind else {
                let mut item = item.clone();
                item.context = from.context.with(item.context);
                item.via = from.via.to_vec();
                fill_item(&mut item, &from.args, from.awaited);
                out.push(item);
                continue;
            };
            let args: Vec<Value> = call.args.iter().map(|arg| fill(arg, &from.args)).collect();
            let text = Call::text_of(&call.name, &args);
            let found = self.helper(place, &call.name);
            // The call stays, so the rules can tell a call removed from a helper that changed,
            // and a call they can't see into (no `defined`) from one that's gone.
            let mut kept = item.clone();
            kept.context = from.context.with(item.context);
            kept.via = from.via.to_vec();
            if let ItemKind::Call(kept) = &mut kept.kind {
                kept.args.clone_from(&args);
                kept.text.clone_from(&text);
                kept.defined = found.map(|(target, helper)| Defined {
                    path: target.other.then(|| target.path.to_owned()),
                    line: helper.line,
                });
            }
            out.push(kept);
            let Some((target, helper)) = found else {
                continue;
            };
            let key = (target.path.to_owned(), helper.name.clone());
            if stack.contains(&key) || stack.len() >= MAX_DEPTH || !self.runs_checks(target, helper)
            {
                continue;
            }
            let mut via = from.via.to_vec();
            via.push(Via {
                name: call.name.clone(),
                call: text,
                line: item.line,
                context: item.context,
                awaited: call.awaited,
                args: args.clone(),
                path: target.other.then(|| target.path.to_owned()),
                defined: helper.line,
            });
            let mut context = from.context.with(item.context);
            // An async helper the caller doesn't wait for may finish after the test.
            let awaited = call.awaited && from.awaited;
            context.not_awaited |= helper.is_async && !awaited;
            // Inside an async helper its own `await`s decide; whether the test waits for the
            // helper is `not_awaited` above. A plain helper hands what it returns back.
            let awaited = helper.is_async || awaited;
            let inner = Expanding {
                via: &via,
                context,
                args: helper
                    .params
                    .iter()
                    .zip(args)
                    .filter_map(|(param, arg)| Some((param.clone()?, arg)))
                    .collect(),
                awaited,
            };
            stack.push(key);
            self.expand(&helper.body.items, target, &inner, stack, out);
            stack.pop();
        }
    }

    /// Whether `helper` (defined at `place`) runs a check, itself or through the helpers it
    /// calls.
    fn runs_checks(&self, place: Place<'m>, helper: &'m Helper) -> bool {
        let mut working_out = Vec::new();
        self.runs_checks_in(place, helper, &mut working_out).0
    }

    /// Also says whether the answer leaned on a helper still being worked out (a call
    /// cycle): such an answer isn't kept, since it may be wrong once that one is known.
    fn runs_checks_in(
        &self,
        place: Place<'m>,
        helper: &'m Helper,
        working_out: &mut Vec<(String, String, u32)>,
    ) -> (bool, bool) {
        let key = (place.path.to_owned(), helper.name.clone(), helper.line);
        if let Some(&checks) = self.checks.borrow().get(&key) {
            return (checks, false);
        }
        if working_out.contains(&key) || working_out.len() >= MAX_DEPTH {
            return (false, true);
        }
        working_out.push(key.clone());
        let mut unsure = false;
        let checks = helper.body.items.iter().any(|item| match &item.kind {
            ItemKind::Assertion(_) | ItemKind::AssertionCount(_) => true,
            ItemKind::Call(call) => {
                self.helper(place, &call.name)
                    .is_some_and(|(target, inner)| {
                        let (checks, cycle) = self.runs_checks_in(target, inner, working_out);
                        unsure |= cycle;
                        checks
                    })
            }
            _ => false,
        });
        working_out.pop();
        if checks || !unsure {
            self.checks.borrow_mut().insert(key, checks);
        }
        (checks, unsure && !checks)
    }

    /// The helper `name` called from `place`: one defined in the same file and visible from
    /// there (the innermost `describe` wins), else one imported from another file.
    fn helper(&self, place: Place<'m>, name: &str) -> Option<(Place<'m>, &'m Helper)> {
        let local = place
            .file
            .helpers
            .iter()
            .filter(|helper| helper.name == name && place.suites.starts_with(&helper.suites))
            .max_by_key(|helper| helper.suites.len());
        if let Some(helper) = local {
            let place = Place {
                suites: &helper.suites,
                ..place
            };
            return Some((place, helper));
        }
        let import = place
            .file
            .imports
            .iter()
            .find(|import| !import.reexport && import.local == name)?;
        self.exported(place.path, &import.from, &import.imported, 0)
    }

    /// The helper the module `specifier` (imported from `from`) exports as `name`, following
    /// `export … from` and `export * from`.
    fn exported(
        &self,
        from: &str,
        specifier: &str,
        name: &str,
        depth: usize,
    ) -> Option<(Place<'m>, &'m Helper)> {
        if depth > MAX_DEPTH {
            return None;
        }
        let (path, file) = self.modules.module(from, specifier)?;
        let helper = file.helpers.iter().find(|helper| {
            helper.suites.is_empty()
                && if name == "default" {
                    helper.default_export
                } else {
                    helper.name == name
                }
        });
        if let Some(helper) = helper {
            let place = Place {
                path,
                other: true,
                file,
                suites: &helper.suites,
            };
            return Some((place, helper));
        }
        file.imports
            .iter()
            .filter(|import| import.reexport)
            .find_map(|import| {
                if import.local == name {
                    self.exported(path, &import.from, &import.imported, depth + 1)
                } else if import.local == "*" && name != "default" {
                    self.exported(path, &import.from, name, depth + 1)
                } else {
                    None
                }
            })
    }
}

/// `value` with the helper's parameters replaced by what the call passed.
fn fill(value: &Value, args: &HashMap<String, Value>) -> Value {
    match value {
        Value::Opaque(name) => args.get(name).cloned().unwrap_or_else(|| value.clone()),
        Value::Array(items) => Value::Array(items.iter().map(|item| fill(item, args)).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), fill(value, args)))
                .collect(),
        ),
        Value::Matcher { name, args: inner } => Value::Matcher {
            name: name.clone(),
            args: inner.iter().map(|arg| fill(arg, args)).collect(),
        },
        other => other.clone(),
    }
}

/// Fills a helper's item in for one call: its expected values get the call's arguments, and
/// a check the helper returns is awaited only if the call is.
fn fill_item(item: &mut Item, args: &HashMap<String, Value>, awaited: bool) {
    match &mut item.kind {
        ItemKind::Assertion(assertion) => {
            assertion.awaited &= awaited;
            if let Some(matcher) = &mut assertion.matcher {
                for arg in &mut matcher.args {
                    *arg = fill(arg, args);
                }
            }
            for fact in &mut assertion.facts {
                match &mut fact.test {
                    FactTest::Equals(value) => *value = fill(value, args),
                    FactTest::Other { args: values, .. } => {
                        for value in values {
                            *value = fill(value, args);
                        }
                    }
                    FactTest::Defined | FactTest::Truthy | FactTest::Falsy => {}
                }
            }
        }
        ItemKind::AssertionCount(Some(count)) => *count = fill(count, args),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_file;

    /// Each linked item of each test: `line text (context) via a > b`.
    fn linked(source: &str, modules: &dyn Modules) -> Vec<String> {
        let file = parse_file("a.test.ts", source).expect("parses");
        link("a.test.ts", &file, modules)
            .tests
            .iter()
            .flat_map(|test| {
                test.body.items.iter().map(move |item| {
                    let text = match &item.kind {
                        ItemKind::Assertion(assertion) => assertion.to_string(),
                        ItemKind::Call(call) if call.defined.is_some() => {
                            format!("call {}", call.text)
                        }
                        ItemKind::Call(call) => format!("call {} (not followed)", call.text),
                        other => format!("{other:?}"),
                    };
                    let context = if item.context.is_plain() {
                        String::new()
                    } else {
                        format!(" ({})", item.context)
                    };
                    let via: Vec<&str> = item.via.iter().map(|via| via.call.as_str()).collect();
                    let via = if via.is_empty() {
                        String::new()
                    } else {
                        format!(" via {}", via.join(" > "))
                    };
                    format!("{}: {} {text}{context}{via}", test.name, item.line)
                })
            })
            .collect()
    }

    #[test]
    fn helpers_and_hooks_run_for_the_test() {
        let source = "
function expectStatus(res, status) {
  expect(res.status).toBe(status);
}
const expectOk = (res) => { expectStatus(res, 200); expect(res.body.error).toBeUndefined() };
describe('refunds', () => {
  beforeEach(() => { expect(db.isClean()).toBe(true) });
  afterAll(() => { expect(leaks()).toEqual([]) });
  it('refunds', () => {
    const res = refund();
    if (res) expectOk(res);
    expect(res.id).toBeDefined();
  });
});
it('charges', () => { expectStatus(charge(), 201) });
";
        assert_eq!(
            linked(source, &()),
            [
                "refunds: 7 call beforeEach",
                "refunds: 7 expect(db.isClean()).toBe(true) via beforeEach",
                "refunds: 11 call expectOk(res) (in if)",
                "refunds: 5 call expectStatus(res, 200) (in if) via expectOk(res)",
                "refunds: 3 expect(res.status).toBe(200) (in if) via expectOk(res) > expectStatus(res, 200)",
                "refunds: 5 expect(res.body.error).toBeUndefined() (in if) via expectOk(res)",
                "refunds: 12 expect(res.id).toBeDefined()",
                "refunds: 8 call afterAll",
                "refunds: 8 expect(leaks()).toEqual([]) via afterAll",
                "charges: 15 call expectStatus(charge(), 201)",
                "charges: 3 expect(res.status).toBe(201) via expectStatus(charge(), 201)",
            ]
        );
    }

    #[test]
    fn an_async_helper_that_is_not_awaited() {
        let source = "
async function expectSaved(id) { expect(await db.get(id)).toBeDefined() }
it('awaits', async () => { await expectSaved(1) });
it('forgets', async () => { expectSaved(2) });
";
        assert_eq!(
            linked(source, &()),
            [
                "awaits: 3 call expectSaved(1)",
                "awaits: 2 expect(await db.get(id)).toBeDefined() via expectSaved(1)",
                "forgets: 4 call expectSaved(2)",
                "forgets: 2 expect(await db.get(id)).toBeDefined() (not awaited) via expectSaved(2)",
            ]
        );
    }

    #[test]
    fn calls_that_go_nowhere_or_in_circles() {
        let source = "
import { expectOk } from '@/test/helpers';
function a() { expect(1).toBe(1); b() }
function b() { a() }
it('circles', () => { a(); setup(); expectOk(res) });
";
        // `setup` isn't a helper or an import; `expectOk` is, but can't be followed.
        assert_eq!(
            linked(source, &()),
            [
                "circles: 5 call a()",
                "circles: 3 expect(1).toBe(1) via a()",
                "circles: 3 call b() via a()",
                "circles: 4 call a() via a() > b()",
                "circles: 5 call expectOk(res) (not followed)",
            ]
        );
    }

    #[test]
    fn a_return_in_a_helper_or_hook_stays_with_it() {
        let source = "
function expectOk(res) {
  if (!res) return;
  expect(res.ok).toBe(true);
  return res;
}
beforeEach(() => { return; expect(db.open).toBe(true) });
it('refunds', () => { expectOk(refund()) });
";
        assert_eq!(
            linked(source, &()),
            [
                "refunds: 7 call beforeEach",
                "refunds: 7 EarlyReturn via beforeEach",
                "refunds: 7 expect(db.open).toBe(true) via beforeEach",
                "refunds: 8 call expectOk(refund())",
                "refunds: 3 EarlyReturn (in if) via expectOk(refund())",
                "refunds: 4 expect(res.ok).toBe(true) via expectOk(refund())",
            ]
        );
    }

    #[test]
    fn an_async_helper_keeps_its_own_awaits() {
        // Not awaiting the call is `not awaited`; the check inside is still awaited.
        let source = "
async function check(p) { await expect(p).resolves.toBe(1) }
it('forgets', async () => { check(p) });
";
        assert_eq!(
            linked(source, &()),
            [
                "forgets: 3 call check(p)",
                "forgets: 2 await expect(p).resolves.toBe(1) (not awaited) via check(p)",
            ]
        );
    }

    struct Files(Vec<(&'static str, FileIr)>);

    impl Modules for Files {
        fn module(&self, _: &str, specifier: &str) -> Option<(&str, &FileIr)> {
            let name = specifier.trim_start_matches("./");
            self.0
                .iter()
                .find(|(path, _)| path.trim_end_matches(".ts") == name)
                .map(|(path, file)| (*path, file))
        }
    }

    #[test]
    fn helpers_imported_from_other_files() {
        let parse = |path, source| parse_file(path, source).expect("parses");
        let files = Files(vec![
            ("helpers.ts", parse("helpers.ts", "export * from './api';")),
            (
                "api.ts",
                parse(
                    "api.ts",
                    "export function expectOk(res) {\n  expect(res.ok).toBe(true)\n}\nexport default function check(x) {\n  expect(x).toBeTruthy()\n}",
                ),
            ),
        ]);
        let source = "
import { expectOk as ok } from './helpers';
import check from './api';
it('works', () => { ok(res); check(value) });
";
        let file = parse_file("a.test.ts", source).expect("parses");
        let test = &link("a.test.ts", &file, &files).tests[0];
        let found: Vec<(Option<&str>, u32, String)> = test
            .body
            .items
            .iter()
            .filter(|item| !matches!(item.kind, ItemKind::Call(_)))
            .map(|item| {
                let via: Vec<&str> = item.via.iter().map(|via| via.call.as_str()).collect();
                (item.path(), item.line, via.join(" > "))
            })
            .collect();
        assert_eq!(
            found,
            [
                (Some("api.ts"), 2, "ok(res)".to_owned()),
                (Some("api.ts"), 5, "check(value)".to_owned()),
            ]
        );
    }
}
