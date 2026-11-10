//! Linking (`docs/architecture.md`, step 3, "Linked view"): each test's own checks, plus the
//! ones that run on its behalf from helpers and hooks, each tagged with how it gets there.
//!
//! ```text
//! function expectStatus(res, status) { expect(res.status).toBe(status) }
//! it('refunds', () => { expectStatus(refund(), 200) })
//! → refunds: expect(res.status).toBe(200), via expectStatus(refund(), 200)
//! ```

use std::collections::HashMap;

use crate::model::{FactTest, FileIr, Helper, HookKind, Item, ItemKind, Via};
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
/// then `afterEach` and `afterAll` (innermost first).
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
    let linker = Linker { modules };
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
            let via = hook_via(hook.kind, hook.line);
            let from = Expanding::new(&via);
            linker.expand(
                &hook.body.items,
                place(&hook.suites),
                &from,
                &mut stack,
                &mut items,
            );
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
            let via = hook_via(hook.kind, hook.line);
            let from = Expanding::new(&via);
            linker.expand(
                &hook.body.items,
                place(&hook.suites),
                &from,
                &mut stack,
                &mut items,
            );
        }
        linked.tests[index].body.items = items;
    }
    linked
}

fn hook_via(kind: HookKind, line: u32) -> [Via; 1] {
    [Via {
        name: kind.as_str().to_owned(),
        call: kind.as_str().to_owned(),
        line,
        context: crate::model::Context::default(),
        awaited: true,
        args: Vec::new(),
        path: None,
        defined: line,
    }]
}

struct Linker<'m> {
    modules: &'m dyn Modules,
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
    context: crate::model::Context,
    /// The helper's parameters, with the values the call passed.
    args: HashMap<String, Value>,
    /// The call is awaited, so the test waits for what the helper returns.
    awaited: bool,
}

impl<'e> Expanding<'e> {
    fn new(via: &'e [Via]) -> Self {
        Self {
            via,
            context: crate::model::Context::default(),
            args: HashMap::new(),
            awaited: true,
        }
    }
}

impl<'m> Linker<'m> {
    fn expand(
        &self,
        items: &[Item],
        place: Place<'m>,
        from: &Expanding,
        stack: &mut Vec<(String, String)>,
        out: &mut Vec<Item>,
    ) {
        for item in items {
            let ItemKind::Call(call) = &item.kind else {
                // A `return` in a hook or helper ends only that function.
                if matches!(item.kind, ItemKind::EarlyReturn) && !from.via.is_empty() {
                    continue;
                }
                let mut item = item.clone();
                item.context = from.context.with(item.context);
                item.via = from.via.to_vec();
                fill_item(&mut item, &from.args, from.awaited);
                out.push(item);
                continue;
            };
            let Some((target, helper)) = self.helper(place, &call.name) else {
                continue;
            };
            let key = (target.path.to_owned(), helper.name.clone());
            if stack.contains(&key) || stack.len() >= MAX_DEPTH {
                continue;
            }
            let args: Vec<Value> = call.args.iter().map(|arg| fill(arg, &from.args)).collect();
            let text = format!(
                "{}({})",
                call.name,
                args.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            // The call stays, so the rules can tell a call removed from a helper that changed.
            let mut kept = item.clone();
            kept.context = from.context.with(item.context);
            kept.via = from.via.to_vec();
            if let ItemKind::Call(kept) = &mut kept.kind {
                kept.args.clone_from(&args);
                kept.text.clone_from(&text);
            }
            out.push(kept);
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
                        ItemKind::Call(call) => format!("call {}", call.text),
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
                "refunds: 7 expect(db.isClean()).toBe(true) via beforeEach",
                "refunds: 11 call expectOk(res) (in if)",
                "refunds: 5 call expectStatus(res, 200) (in if) via expectOk(res)",
                "refunds: 3 expect(res.status).toBe(200) (in if) via expectOk(res) > expectStatus(res, 200)",
                "refunds: 5 expect(res.body.error).toBeUndefined() (in if) via expectOk(res)",
                "refunds: 12 expect(res.id).toBeDefined()",
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
function a() { expect(1).toBe(1); b() }
function b() { a() }
it('circles', () => { a(); setup(); });
";
        assert_eq!(
            linked(source, &()),
            [
                "circles: 4 call a()",
                "circles: 2 expect(1).toBe(1) via a()",
                "circles: 2 call b() via a()",
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
