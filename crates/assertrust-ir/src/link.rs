//! Linking (`docs/architecture.md`, step 3, "Linked view"): each test's own checks, plus the
//! ones that run on its behalf from helpers and hooks, each tagged with how it gets there.
//!
//! ```text
//! function expectStatus(res, status) { expect(res.status).toBe(status) }
//! it('refunds', () => { expectStatus(refund(), 200) })
//! → refunds: expect(refund().status).toBe(200), via expectStatus(refund(), 200)
//! ```
//!
//! Methods of classes and objects defined in test files are helpers too, found by their name:
//! `ctx.assertSyncResponse(response, [])` runs the one method called `assertSyncResponse`
//! that checks something, with `this` standing for `ctx`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::model::{
    Call, Context, Defined, Fact, FactTest, FileIr, Helper, Hook, HookKind, Item, ItemKind, Via,
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
/// helper's items, with the call's arguments filled in (in its checks' subjects and expected
/// values), and the hooks around it added:
/// `beforeAll` and `beforeEach` first (outermost `describe` first), then the test's own,
/// then `afterEach` and `afterAll` (innermost first). Each hook starts with a call to it.
/// A call that can't be followed stays, with no [`Defined`](crate::Defined). A method call
/// stays only when it may be a helper's: one that matches no method that checks something is
/// dropped, and one that matches several stays unfollowed.
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
/// // What it checks for this test, and how the helper writes it:
/// assert_eq!(check.subject, "refund().status");
/// assert_eq!(check.to_string(), "expect(res.status).toBe(200)");
/// # Ok::<(), assertrust_ir::ParseError>(())
/// ```
pub fn link(path: &str, file: &FileIr, modules: &dyn Modules) -> FileIr {
    let linker = Linker {
        modules,
        checks: RefCell::new(HashMap::new()),
        working_out: RefCell::new(Vec::new()),
        methods: RefCell::new(HashMap::new()),
        searched: RefCell::new(HashMap::new()),
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
            owner: None,
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

/// For each helper of `file` (at `path`), in the order of [`FileIr::helpers`]: `None` when it
/// runs no check, itself or through the helpers it calls, else the items that decide what it
/// checks. A call that leads to no helper (`act(() => h.app.clearSelection(null))`, from a
/// package) is left out: changing it doesn't change what a test checks, and neither does
/// changing a helper that checks nothing.
pub fn helper_checks(path: &str, file: &FileIr, modules: &dyn Modules) -> Vec<Option<Vec<Item>>> {
    let linker = Linker {
        modules,
        checks: RefCell::new(HashMap::new()),
        working_out: RefCell::new(Vec::new()),
        methods: RefCell::new(HashMap::new()),
        searched: RefCell::new(HashMap::new()),
    };
    file.helpers
        .iter()
        .map(|helper| {
            let place = Place {
                path,
                other: false,
                file,
                suites: &helper.suites,
                owner: helper.owner.as_deref(),
            };
            linker.runs_checks(place, helper).then(|| {
                helper
                    .body
                    .items
                    .iter()
                    .filter(|item| match &item.kind {
                        ItemKind::Call(call) => {
                            !matches!(linker.resolve(place, call), Found::Nothing)
                        }
                        _ => true,
                    })
                    .cloned()
                    .collect()
            })
        })
        .collect()
}

/// A helper's name with its class or object: `Page.expectOpen`.
fn qualified(helper: &Helper) -> String {
    match &helper.owner {
        Some(owner) => format!("{owner}.{}", helper.name),
        None => helper.name.clone(),
    }
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
            receiver: None,
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
    /// The helpers whose answer for `checks` is being worked out, to stop at call cycles.
    working_out: RefCell<Vec<(String, String, u32)>>,
    /// Methods found by their name, by where the call is (file and class) and the name: a
    /// test file can make thousands of calls like `mouse.click(…)`.
    methods: RefCell<MethodCache<'m>>,
    /// The files methods are looked for in, by the file the call is in.
    searched: RefCell<HashMap<String, Rc<Files<'m>>>>,
}

/// Files to look in, each with whether it's another file than the test's.
type Files<'m> = Vec<(&'m str, &'m FileIr, bool)>;

/// By the file a call is in, the class it's in, the name the receiver ends with (`API` for
/// `API.clearSelection()`), and the method's name.
type MethodCache<'m> = HashMap<(String, Option<String>, String, String), Found<'m>>;

/// What a call runs.
#[derive(Clone, Copy)]
enum Found<'m> {
    Helper(Place<'m>, &'m Helper),
    /// A method call that matches several methods that check something: it can't be
    /// followed.
    Ambiguous,
    /// Nothing that can be followed: a package, a file that isn't read, or for a method
    /// call, no method that checks something.
    Nothing,
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
    /// In a method: its class or object, whose other methods `this.…()` calls first.
    owner: Option<&'p str>,
}

/// How the items being expanded run for the test.
struct Expanding<'e> {
    via: &'e [Via],
    /// What every call on the way adds: a helper called in an `if` puts all of its checks
    /// in one.
    context: Context,
    /// The helper's parameters, with the values the call passed, then the constants it
    /// reads from them, and `this` for a method: what the call was made on.
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
            let args: Vec<Value> = call
                .args
                .iter()
                .map(|arg| fill_argument(arg, &from.args))
                .collect();
            let receiver = call
                .receiver
                .as_deref()
                .map(|receiver| fill_source(receiver, &from.args));
            let text = Call::text_of(receiver.as_deref(), &call.name, &args);
            let found = match self.resolve(place, call) {
                Found::Helper(target, helper) => Some((target, helper)),
                Found::Ambiguous => None,
                // A method call that isn't a helper's is like any other line of code.
                Found::Nothing if call.receiver.is_some() => continue,
                Found::Nothing => None,
            };
            // The call stays, so the rules can tell a call removed from a helper that changed,
            // and a call they can't see into (no `defined`) from one that's gone.
            let mut kept = item.clone();
            kept.context = from.context.with(item.context);
            kept.via = from.via.to_vec();
            if let ItemKind::Call(kept) = &mut kept.kind {
                kept.args.clone_from(&args);
                kept.receiver.clone_from(&receiver);
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
            // Two classes in a file may each have a method by one name.
            let key = (target.path.to_owned(), qualified(helper));
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
            let mut bindings: HashMap<String, Value> = helper
                .params
                .iter()
                .zip(args)
                .filter_map(|(param, arg)| Some((param.clone()?, arg)))
                .collect();
            if let Some(receiver) = receiver {
                bindings.insert("this".to_owned(), Value::Opaque(receiver));
            }
            for (name, source) in &helper.locals {
                let value = Value::Opaque(fill_source(source, &bindings));
                bindings.insert(name.clone(), value);
            }
            let inner = Expanding {
                via: &via,
                context,
                args: bindings,
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
        self.runs_checks_in(place, helper).0
    }

    /// Also says whether the answer leaned on a helper still being worked out (a call
    /// cycle): such an answer isn't kept, since it may be wrong once that one is known.
    fn runs_checks_in(&self, place: Place<'m>, helper: &'m Helper) -> (bool, bool) {
        let key = (place.path.to_owned(), helper.name.clone(), helper.line);
        if let Some(&checks) = self.checks.borrow().get(&key) {
            return (checks, false);
        }
        {
            let working_out = self.working_out.borrow();
            if working_out.contains(&key) || working_out.len() >= MAX_DEPTH {
                return (false, true);
            }
        }
        self.working_out.borrow_mut().push(key.clone());
        let mut unsure = false;
        let checks = helper.body.items.iter().any(|item| match &item.kind {
            ItemKind::Assertion(_) | ItemKind::AssertionCount(_) => true,
            ItemKind::Call(call) => match self.resolve(place, call) {
                Found::Helper(target, inner) => {
                    let (checks, cycle) = self.runs_checks_in(target, inner);
                    unsure |= cycle;
                    checks
                }
                Found::Ambiguous | Found::Nothing => false,
            },
            _ => false,
        });
        self.working_out.borrow_mut().pop();
        if checks || !unsure {
            self.checks.borrow_mut().insert(key, checks);
        }
        (checks, unsure && !checks)
    }

    /// What `call`, written at `place`, runs.
    fn resolve(&self, place: Place<'m>, call: &Call) -> Found<'m> {
        let found = match &call.receiver {
            None => self.helper(place, &call.name),
            Some(receiver) => return self.method(place, receiver, &call.name),
        };
        found.map_or(Found::Nothing, |(target, helper)| {
            Found::Helper(target, helper)
        })
    }

    /// The method `name` called on `receiver` from `place`. `this.name()` in a method is its
    /// own class's method, when it has one by that name. Otherwise the receiver's class
    /// isn't known, so the method is found by its name: the one method called that, among
    /// those that check something, in the file and the files it imports (and what they pass
    /// on). A static method or an object's method only counts when it's called on its owner
    /// by name. With none it's no helper's; with several, it can't be followed.
    fn method(&self, place: Place<'m>, receiver: &str, name: &str) -> Found<'m> {
        if receiver == "this"
            && let Some(owner) = place.owner
        {
            let own: Vec<&Helper> = place
                .file
                .helpers
                .iter()
                .filter(|helper| helper.owner.as_deref() == Some(owner) && helper.name == name)
                .collect();
            if let [helper] = own.as_slice() {
                let target = Place {
                    suites: &helper.suites,
                    ..place
                };
                // Its own method surely: a helper only if it checks something.
                return if self.runs_checks_in(target, helper).0 {
                    Found::Helper(target, helper)
                } else {
                    Found::Nothing
                };
            }
        }
        let named = receiver.rsplit('.').next().unwrap_or(receiver);
        let key = (
            place.path.to_owned(),
            place.owner.map(str::to_owned),
            named.to_owned(),
            name.to_owned(),
        );
        if let Some(found) = self.methods.borrow().get(&key) {
            return *found;
        }
        let (found, unsure) = self.method_by_name(place, named, name);
        // An answer that leaned on a helper still being worked out may change.
        if !unsure {
            self.methods.borrow_mut().insert(key, found);
        }
        found
    }

    /// The one method called `name` that checks something, in the file `place` is in and
    /// the files it imports, called on something whose name ends in `named`. An instance
    /// method counts only when its class is built (`new Page()`) or extended in one of those
    /// files: an object of a class nobody builds can't be what the call is made on. Also
    /// says whether the answer leaned on a helper still being worked out.
    fn method_by_name(&self, place: Place<'m>, named: &str, name: &str) -> (Found<'m>, bool) {
        let files = self.search(place);
        let built: HashSet<&str> = files
            .iter()
            .flat_map(|(_, file, _)| file.constructed.iter().map(String::as_str))
            .collect();
        let mut found: Vec<(Place<'m>, &'m Helper)> = Vec::new();
        let mut unsure = false;
        for &(path, file, other) in files.iter() {
            for helper in &file.helpers {
                let Some(owner) = helper.owner.as_deref() else {
                    continue;
                };
                let reachable = if helper.on_owner {
                    owner == named
                } else {
                    built.contains(owner)
                };
                if helper.name != name || !reachable {
                    continue;
                }
                let target = Place {
                    path,
                    other,
                    file,
                    suites: &helper.suites,
                    owner: Some(owner),
                };
                let (checks, cycle) = self.runs_checks_in(target, helper);
                unsure |= cycle;
                if checks {
                    found.push((target, helper));
                }
            }
        }
        let found = match found.as_slice() {
            [] => Found::Nothing,
            [(target, helper)] => Found::Helper(*target, helper),
            _ => Found::Ambiguous,
        };
        (found, unsure)
    }

    /// The files a method called at `place` is looked for in: its own, the files it
    /// imports, and what those pass on or build objects from.
    fn search(&self, place: Place<'m>) -> Rc<Files<'m>> {
        if let Some(files) = self.searched.borrow().get(place.path) {
            return Rc::clone(files);
        }
        let mut files = vec![(place.path, place.file, place.other)];
        self.imported(place.path, place.file, 0, &mut files);
        let files = Rc::new(files);
        self.searched
            .borrow_mut()
            .insert(place.path.to_owned(), Rc::clone(&files));
        files
    }

    /// Adds the files `file` (at `path`) imports, and the files those pass on or build
    /// objects from, to `files`.
    fn imported(
        &self,
        path: &str,
        file: &FileIr,
        depth: usize,
        files: &mut Vec<(&'m str, &'m FileIr, bool)>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        for import in &file.imports {
            // Beyond the file the call is in, only what a file passes on, or builds objects
            // from (`export const page = new Page()` in a fixtures file).
            if depth > 0 && !import.reexport && !file.constructed.contains(&import.local) {
                continue;
            }
            let Some((found, ir)) = self.modules.module(path, &import.from) else {
                continue;
            };
            if files.iter().any(|(known, _, _)| *known == found) {
                continue;
            }
            files.push((found, ir, true));
            self.imported(found, ir, depth + 1, files);
        }
    }

    /// The helper `name` called from `place`: one defined in the same file and visible from
    /// there (the innermost `describe` wins), else one imported from another file.
    fn helper(&self, place: Place<'m>, name: &str) -> Option<(Place<'m>, &'m Helper)> {
        let local = place
            .file
            .helpers
            .iter()
            .filter(|helper| {
                helper.owner.is_none()
                    && helper.name == name
                    && place.suites.starts_with(&helper.suites)
            })
            .max_by_key(|helper| helper.suites.len());
        if let Some(helper) = local {
            let place = Place {
                suites: &helper.suites,
                owner: None,
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
                && helper.owner.is_none()
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
                owner: None,
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

/// An argument a helper passes on, with its own parameters (and `this`) filled in wherever
/// they appear: `await this.syncStream(auth, types)` is
/// `await ctx.syncStream(auth, [SyncRequestType.UsersV1])`. It becomes the next helper's
/// parameter, which may be a check's subject.
fn fill_argument(value: &Value, args: &HashMap<String, Value>) -> Value {
    match fill(value, args) {
        Value::Opaque(source) => Value::Opaque(fill_source(&source, args)),
        filled => filled,
    }
}

/// An expected value with the helper's parameters replaced by what the call passed, where
/// it's a parameter: `toBe(status)` is `toBe(200)`. A parameter inside a longer expression
/// (`schema.safeParse(input).data`) stays as written: a call that passes the same thing
/// under another name (`proto` for `JSON.parse(…)`) mustn't make the outcome look changed.
/// The length of an array that was passed is its number of items: `records.length` is `2`
/// for a call with `[a, b]`.
fn fill(value: &Value, args: &HashMap<String, Value>) -> Value {
    match value {
        Value::Opaque(source) => {
            if let Some(value) = args.get(source) {
                return value.clone();
            }
            if let Some(name) = source.strip_suffix(".length")
                && let Some(Value::Array(items)) = args.get(name)
                && !items
                    .iter()
                    .any(|item| matches!(item, Value::Opaque(item) if item.starts_with("...")))
            {
                return Value::Number(items.len().to_string());
            }
            value.clone()
        }
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

/// Fills a helper's item in for one call: its subject and expected values get the call's
/// arguments, and a check the helper returns is awaited only if the call is.
fn fill_item(item: &mut Item, args: &HashMap<String, Value>, awaited: bool) {
    match &mut item.kind {
        ItemKind::Assertion(assertion) => {
            assertion.awaited &= awaited;
            let subject = fill_source(&assertion.subject, args);
            if subject != assertion.subject {
                let written = std::mem::replace(&mut assertion.subject, subject);
                assertion.written.get_or_insert(written);
            }
            if let Some(matcher) = &mut assertion.matcher {
                for arg in &mut matcher.args {
                    *arg = fill(arg, args);
                }
            }
            for fact in &mut assertion.facts {
                fill_base(fact, args);
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

/// A fact's base with the arguments filled in, read again the way a check written in the
/// test would be, so the two state the same fact: `body` filled with `(await refund()).body`
/// is the base `await refund()` and the path `body`, while `await page.title` stays whole.
fn fill_base(fact: &mut Fact, args: &HashMap<String, Value>) {
    let filled = fill_source(&fact.base, args);
    if filled == fact.base {
        return;
    }
    // A name with properties (`res.body`) splits at its dots; anything else is parsed.
    let plain = |part: &str| {
        part.chars().next().is_some_and(is_identifier_start) && part.chars().all(is_identifier_char)
    };
    let (base, mut path) = if filled.split('.').all(plain) {
        let mut parts = filled.split('.').map(str::to_owned);
        let base = parts.next().unwrap_or_default();
        (base, parts.collect())
    } else {
        crate::js::subject_target(&filled).unwrap_or((filled, Vec::new()))
    };
    path.append(&mut fact.path);
    fact.base = base;
    fact.path = path;
}

/// Normalized source with the names in `args` replaced by their values: a helper's `res.status`
/// is `refund().status` for a call `expectOk(refund())`. Properties (`x.res`), object keys
/// (`{ res: 1 }`) and strings are left alone, a shorthand property keeps its key
/// (`{ res: refund() }`), and a value that isn't a single term is put in parentheses where
/// it's read from: `(await refund()).status`.
pub(crate) fn fill_source(source: &str, args: &HashMap<String, Value>) -> String {
    if args.is_empty() {
        return source.to_owned();
    }
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    // The brackets open where the walk is, to tell an object's shorthand property.
    let mut open: Vec<char> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' | '[' | '{' => open.push(c),
            ')' | ']' | '}' => {
                open.pop();
            }
            _ => {}
        }
        if matches!(c, '\'' | '"' | '`') {
            // A string, copied as written (a template's `${…}` parts too).
            out.push(c);
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == '\\' {
                    if let Some(&next) = chars.get(i + 1) {
                        out.push(next);
                    }
                    i += 2;
                    continue;
                }
                i += 1;
                if chars[i - 1] == c {
                    break;
                }
            }
            continue;
        }
        if c.is_ascii_digit() {
            // A number, with any letters in it (`0x1f`, `1e3`), isn't a name.
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if !is_identifier_start(c) {
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_identifier_char(chars[i]) {
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        let spread = start >= 3 && chars[start - 3..start] == ['.', '.', '.'];
        let property = start > 0 && chars[start - 1] == '.' && !spread;
        let key = chars.get(i) == Some(&':');
        let before = chars[..start].iter().rev().find(|c| !c.is_whitespace());
        let after = chars[i..].iter().find(|c| !c.is_whitespace());
        let shorthand = open.last() == Some(&'{')
            && matches!(before, Some('{' | ','))
            && matches!(after, Some('}' | ','));
        match args.get(&word) {
            Some(value) if shorthand => {
                out.push_str(&word);
                out.push_str(": ");
                out.push_str(&value.to_string());
            }
            Some(value) if !property && !key => {
                let text = value.to_string();
                // On its own (the whole subject, an item, an argument) it needs none.
                let alone = matches!(before, None | Some('(' | '[' | ','))
                    && matches!(after, None | Some(')' | ']' | ','));
                if !alone && !single_term(&text) {
                    out.push('(');
                    out.push_str(&text);
                    out.push(')');
                } else {
                    out.push_str(&text);
                }
            }
            _ => out.push_str(&word),
        }
    }
    out
}

/// Source that reads as one term wherever it's put: a name, a literal, or calls and
/// properties on one. Not `await p`, `a + b` or `!x`.
fn single_term(source: &str) -> bool {
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut chars = source.chars().peekable();
    let mut first = true;
    let mut previous = ' ';
    while let Some(c) = chars.next() {
        let after = previous;
        previous = c;
        if let Some(open) = quote {
            if c == '\\' {
                chars.next();
            } else if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '-' if first => {}
            // `res?.body`, and TypeScript's `res!.body`: still one term.
            '?' if chars.peek() == Some(&'.') => {}
            '!' if (is_identifier_char(after) || matches!(after, ')' | ']'))
                && chars.peek() != Some(&'=') => {}
            c if depth == 0 && (c.is_whitespace() || "+-*/%<>=!&|?:~^,".contains(c)) => {
                return false;
            }
            _ => {}
        }
        first = false;
    }
    !source.starts_with('{')
}

fn is_identifier_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
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

    /// What each check of the first test checks once linked: its subject, then its facts.
    fn checked(source: &str) -> Vec<String> {
        let file = parse_file("a.test.ts", source).expect("parses");
        link("a.test.ts", &file, &()).tests[0]
            .body
            .assertions()
            .map(|check| {
                let facts: Vec<String> = check.facts.iter().map(ToString::to_string).collect();
                format!("{} | {}", check.subject, facts.join(", "))
            })
            .collect()
    }

    #[test]
    fn methods_are_helpers_found_by_their_name() {
        let source = "
class Page {
  expectOpen() { expect(this.dialog.visible).toBe(true) }
  async expectSaved(id) { this.expectOpen(); expect(await db.get(id)).toBeDefined() }
  url() { return '/' }
  expectClosed = () => { expect(this.dialog.visible).toBe(false) }
}
const api = { expectOk(res) { expect(res.ok).toBe(true) } };
class A { check() { expect(1).toBe(1) } }
class B { check() { expect(2).toBe(2) } static reset() { expect(3).toBe(3) } }
class Form { validate() { expect(4).toBe(4) } }
it('works', async () => {
  const page = new Page();
  await page.expectSaved(7);
  page.url();
  page.expectClosed();
  api.expectOk(res);
  thing.check(new A(), new B());
  db.reset(); other.expectOk(res); B.reset();
  schema.validate(input);
});
";
        // `url` checks nothing, so calling it is like any other line. Two methods called
        // `check` check something: which one `thing` has isn't known. A static method, or
        // an object's, is only called on its owner: `db.reset()` isn't `B.reset()`. Nothing
        // builds a `Form`, so `schema` can't be one.
        assert_eq!(
            linked(source, &()),
            [
                "works: 14 call page.expectSaved(7)",
                "works: 4 call page.expectOpen() via page.expectSaved(7)",
                "works: 3 expect(this.dialog.visible).toBe(true) via page.expectSaved(7) > page.expectOpen()",
                "works: 4 expect(await db.get(id)).toBeDefined() via page.expectSaved(7)",
                "works: 16 call page.expectClosed()",
                "works: 6 expect(this.dialog.visible).toBe(false) via page.expectClosed()",
                "works: 17 call api.expectOk(res)",
                "works: 8 expect(res.ok).toBe(true) via api.expectOk(res)",
                "works: 18 call thing.check(new A(), new B()) (not followed)",
                "works: 19 call B.reset()",
                "works: 10 expect(3).toBe(3) via B.reset()",
            ]
        );
        // `this` is what the method was called on.
        assert_eq!(
            checked(source)[..2],
            [
                "page.dialog.visible | page.dialog.visible is truthy",
                "await db.get(7) | await db.get(7) is defined",
            ]
        );
    }

    #[test]
    fn arguments_fill_in_subjects() {
        let source = "
function expectOk(res) {
  const body = res.body;
  const all = load();
  expect(res.status).toBe(200);
  expect(body.ok).toBe(true);
  expect(all.done).toBe(true);
}
it('refunds', async () => { expectOk(await refund()) });
";
        // A constant read from an argument is filled in too; one that isn't stays.
        assert_eq!(
            checked(source),
            [
                "(await refund()).status | await refund().status == 200",
                "(await refund()).body.ok | await refund().body.ok is truthy",
                "all.done | all.done is truthy",
            ]
        );
        // What's filled in reads like the same check written in the test.
        let source = "
function check(v) { expect(v).toBe(1) }
function expectOk(res) { expect(res.ok).toBe(true) }
it('reads', async () => {
  check(await page.title);
  check(a + b.length);
  check(res['body']);
  check(await el.textContent());
  expectOk(data?.body);
});
";
        assert_eq!(
            checked(source),
            [
                "await page.title | await page.title == 1",
                "a + b.length | a + b.length == 1",
                "res['body'] | res.body == 1",
                "await el.textContent() | el.text() == 1",
                "data?.body.ok | data.body.ok is truthy",
            ]
        );
    }

    #[test]
    fn filling_in_leaves_properties_keys_and_strings_alone() {
        let args: HashMap<String, Value> = [
            ("res".to_owned(), Value::Opaque("await refund()".to_owned())),
            ("id".to_owned(), Value::Number("7".to_owned())),
        ]
        .into();
        let fill = |source| fill_source(source, &args);
        assert_eq!(fill("res"), "await refund()");
        assert_eq!(fill("res.body[id]"), "(await refund()).body[7]");
        assert_eq!(fill("cache.res.id"), "cache.res.id");
        assert_eq!(
            fill("find({ id: id, res })"),
            "find({ id: 7, res: await refund() })"
        );
        assert_eq!(fill("[res, id]"), "[await refund(), 7]");
        assert_eq!(fill("get('res', `id ${id}`)"), "get('res', `id ${id}`)");
        assert_eq!(fill("[...res, id2]"), "[...(await refund()), id2]");
        assert_eq!(fill("res?.body"), "(await refund())?.body");
        assert!(single_term("data?.body"));
        assert!(single_term("data!.body"));
        assert!(!single_term("a !== b"));
    }
}
