//! Reading JS/TS test files with oxc (`docs/architecture.md`, step 2).
//!
//! One pass over the syntax tree. Test structure (`describe`, `it`, hooks) is recognized by
//! name; inside a test or hook every `expect(…)` becomes an [`Item`] tagged with where it
//! sits. oxc's tree is freed when [`parse`] returns: only our own types leave this module.

mod calls;
mod config;
mod consts;
mod facts;
mod values;

pub(crate) use self::config::config;

use std::collections::{HashMap, HashSet};

use oxc::allocator::Allocator;
use oxc::ast::AstKind;
use oxc::ast::ast::{
    ArrayExpressionElement, ArrowFunctionExpression, AwaitExpression, BinaryOperator,
    BindingIdentifier, BindingPattern, BlockStatement, BreakStatement, CallExpression, CatchClause,
    Class, ConditionalExpression, ContinueStatement, DoWhileStatement, ExportAllDeclaration,
    ExportDefaultDeclaration, ExportDefaultDeclarationKind, ExportFromDeclaration, Expression,
    ForInStatement, ForOfStatement, ForStatement, ForStatementInit, FormalParameters, Function,
    FunctionBody, IdentifierReference, IfStatement, ImportDeclaration, ImportDeclarationSpecifier,
    LogicalExpression, MethodDefinition, MethodDefinitionKind, NewExpression, ObjectExpression,
    ObjectProperty, ObjectPropertyKind, Program, PropertyDefinition, ReturnStatement, Statement,
    SwitchStatement, TryStatement, UnaryOperator, VariableDeclarationKind, VariableDeclarator,
    WhileStatement,
};
use oxc::ast_visit::{Visit, walk};
use oxc::parser::{ParseOptions, Parser};
use oxc::semantic::SemanticBuilder;
use oxc::span::{GetSpan, SourceType, Span};
use oxc::syntax::scope::ScopeFlags;
use oxc::syntax::symbol::SymbolId;

use self::calls::{RawArg, RawModifier, Structure};
use self::consts::Consts;
use self::values::{Values, js_str, normalize};
use crate::ParseError;
use crate::matcher::MatcherKind;
use crate::model::{
    Assertion, Body, Call, Context, FileIr, Helper, Hook, Import, Item, ItemKind, Matcher,
    Modifier, Test,
};
use crate::value::Value;

pub(crate) fn parse(path: &str, source: &str) -> Result<FileIr, ParseError> {
    let allocator = Allocator::default();
    let (program, lines) = program(&allocator, path, source)?;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();
    let values = Values {
        source,
        scoping: &scoping,
    };
    let consts = Consts::find(&program, &values);
    let mut collector = Collector {
        values,
        lines,
        file: FileIr::default(),
        suites: vec![Suite::default()],
        owner: Owner::Outside,
        context: Context::default(),
        depth: 0,
        in_helper: false,
        consts,
        awaited: HashSet::new(),
        runner_waits: false,
        last_statement: None,
        skip_names: SkipNames::default(),
        default_export: None,
        locals: Locals::default(),
        class: None,
    };
    collector.visit_program(&program);
    let mut file = collector.file;
    if let Some(name) = collector.default_export {
        for helper in file.helpers.iter_mut().filter(|helper| helper.name == name) {
            helper.default_export = true;
        }
    }
    keep_helper_calls(&mut file);
    Ok(file)
}

/// Drops calls to anything but a possible helper: one defined in the file, or something
/// imported. Helpers can be defined after the tests that call them, so this runs last. The
/// calls kept get their text. Method calls are all kept: the method may be defined in any
/// file the test file imports, which only linking reads.
fn keep_helper_calls(file: &mut FileIr) {
    let known: HashSet<String> = file
        .helpers
        .iter()
        .filter(|helper| helper.owner.is_none())
        .map(|helper| helper.name.clone())
        .chain(
            file.imports
                .iter()
                .filter(|import| !import.reexport)
                .map(|import| import.local.clone()),
        )
        .collect();
    let keep = |body: &mut Body| {
        body.items.retain_mut(|item| match &mut item.kind {
            ItemKind::Call(call) => {
                let known = call.receiver.is_some() || known.contains(&call.name);
                if known {
                    call.text = Call::text_of(call.receiver.as_deref(), &call.name, &call.args);
                }
                known
            }
            _ => true,
        });
    };
    file.tests.iter_mut().for_each(|test| keep(&mut test.body));
    file.hooks.iter_mut().for_each(|hook| keep(&mut hook.body));
    file.helpers
        .iter_mut()
        .for_each(|helper| keep(&mut helper.body));
    keep(&mut file.outside);
}

/// A function's parameters as helper parameters: plain names, `None` for anything else.
fn param_names(params: &FormalParameters) -> Vec<Option<String>> {
    let mut names: Vec<Option<String>> = params
        .items
        .iter()
        .map(|param| match &param.pattern {
            BindingPattern::BindingIdentifier(id) if param.initializer.is_none() => {
                Some(id.name.to_string())
            }
            _ => None,
        })
        .collect();
    if params.rest.is_some() {
        names.push(None);
    }
    names
}

/// Parses one file into oxc's syntax tree, or says why it can't.
fn program<'a>(
    allocator: &'a Allocator,
    path: &str,
    source: &'a str,
) -> Result<(Program<'a>, Lines), ParseError> {
    let source_type = SourceType::from_path(path).map_err(|_| ParseError::UnsupportedFile)?;
    let options = ParseOptions {
        parse_regular_expression: true,
        ..ParseOptions::default()
    };
    let parsed = Parser::new(allocator, source, source_type)
        .with_options(options)
        .parse();
    let lines = Lines::new(source);
    if parsed.fatal_error || parsed.diagnostics.has_errors() {
        let first = parsed.diagnostics.iter().next();
        return Err(ParseError::Syntax {
            line: first
                .and_then(|diagnostic| diagnostic.labels.first())
                .map_or(1, |label| lines.line(label.offset())),
            message: first.map_or_else(|| "syntax error".to_owned(), |d| d.message.to_string()),
        });
    }
    Ok((parsed.program, lines))
}

/// Line numbers from byte offsets.
struct Lines {
    starts: Vec<u32>,
}

impl Lines {
    fn new(source: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(
                source
                    .match_indices('\n')
                    .map(|(i, _)| u32::try_from(i + 1).unwrap_or(u32::MAX)),
            )
            .collect();
        Self { starts }
    }

    /// 1-based.
    fn line(&self, offset: u32) -> u32 {
        u32::try_from(self.starts.partition_point(|&start| start <= offset)).unwrap_or(u32::MAX)
    }
}

/// A `describe` block being walked. The first entry is the file itself (no name), which
/// holds file-wide skips like a top-level `test.skip(cond)`.
#[derive(Default)]
struct Suite {
    name: Option<String>,
    modifiers: Vec<Modifier>,
    /// The index of its first test in [`FileIr::tests`].
    first_test: usize,
}

/// Whose body the walker is in.
#[derive(Clone, Copy)]
enum Owner {
    Outside,
    Test(usize),
    Hook(usize),
    Helper(usize),
}

impl Owner {
    /// A test or hook: what the runner itself calls.
    fn runs_tests(self) -> bool {
        matches!(self, Self::Test(_) | Self::Hook(_))
    }
}

/// Names that skip the running test when called: `ctx` in `it('…', (ctx) => ctx.skip())`,
/// or `skip` in `it('…', ({ skip }) => skip())` (vitest).
#[derive(Default, Clone)]
struct SkipNames {
    context: Option<String>,
    skip: Option<String>,
}

/// A function declared inside a test, hook or helper, under a name it calls:
/// `const hover = () => {…}` or `function hover() {…}`. It's walked where it's first called,
/// as part of what calls it, so its checks sit where they run: after an early `return`, in
/// the `if` or `try` around the call, and awaited when the call is.
#[derive(Clone, Copy)]
struct LocalFunction<'a> {
    body: CallbackBody<'a, 'a>,
    is_async: bool,
}

/// The local functions of the test, hook or helper being walked.
#[derive(Default)]
struct Locals<'a> {
    /// Found as the walker enters the block or `const` declaring them.
    functions: HashMap<SymbolId, LocalFunction<'a>>,
    /// Walked already, where they were first called (or being walked).
    walked: HashSet<SymbolId>,
    /// Declared, and waiting for a call.
    waiting: Vec<SymbolId>,
}

struct Collector<'s, 'a> {
    values: Values<'s>,
    lines: Lines,
    file: FileIr,
    suites: Vec<Suite>,
    owner: Owner,
    context: Context,
    /// How many functions deep the walker is inside the current test or hook (0 = its body).
    depth: u32,
    /// Inside a function outside any test: a helper. Its checks are linked to tests in
    /// step 2's helper pass, so they're not listed as "outside".
    in_helper: bool,
    consts: Consts,
    /// Calls the test waits for: `await`ed, or returned from a function the runner waits for.
    awaited: HashSet<Span>,
    /// The function being walked is a test, hook or step callback, so the runner waits for
    /// what it returns. Not true for other callbacks: `setTimeout(() => expect(…))` drops it.
    runner_waits: bool,
    /// The last statement of the current test or hook, so a `return` there isn't "early".
    last_statement: Option<Span>,
    skip_names: SkipNames,
    /// `export default expectOk;`: marked on the helper once the file is read.
    default_export: Option<String>,
    locals: Locals<'a>,
    /// The class whose body is being walked, outside tests: its methods are helpers.
    class: Option<String>,
}

impl<'a> Collector<'_, 'a> {
    fn line(&self, span: Span) -> u32 {
        self.lines.line(span.start)
    }

    fn push(&mut self, kind: ItemKind, span: Span) {
        let item = Item {
            kind,
            line: self.line(span),
            context: self.context,
            via: Vec::new(),
        };
        match self.owner {
            Owner::Outside if self.in_helper => {}
            Owner::Outside => self.file.outside.items.push(item),
            Owner::Test(index) => self.file.tests[index].body.items.push(item),
            Owner::Hook(index) => self.file.hooks[index].body.items.push(item),
            Owner::Helper(index) => self.file.helpers[index].body.items.push(item),
        }
    }

    /// The body of a loop that runs at least once. Its statements surely run up to the
    /// first one that may `break` or `continue`; from there on they're `in_loop`.
    fn first_pass(&mut self, body: &Statement<'a>) {
        let statements = match body {
            Statement::BlockStatement(block) => block.body.as_slice(),
            other => std::slice::from_ref(other),
        };
        let cut = statements
            .iter()
            .position(may_leave_loop)
            .unwrap_or(statements.len());
        let (surely, rest) = statements.split_at(cut);
        for statement in surely {
            self.visit_statement(statement);
        }
        self.scoped(
            |context| context.in_loop = true,
            |this| {
                for statement in rest {
                    this.visit_statement(statement);
                }
            },
        );
    }

    fn symbol(&self, id: &IdentifierReference) -> Option<SymbolId> {
        self.values
            .scoping
            .get_reference(id.reference_id.get()?)
            .symbol_id()
    }

    /// Notes the local functions a block or `const` declares, as the walker enters it: a
    /// `const` function, or a function declaration (which can be called before it). A `let`
    /// can be given another function before the call, so it isn't one.
    fn note_locals(&mut self, kind: AstKind<'a>) {
        if matches!(self.owner, Owner::Outside) {
            return;
        }
        let mut note = |id: &BindingIdentifier, function: Option<LocalFunction<'a>>| {
            if let (Some(symbol), Some(function)) = (id.symbol_id.get(), function) {
                self.locals.functions.insert(symbol, function);
            }
        };
        match kind {
            AstKind::VariableDeclaration(declaration)
                if declaration.kind == VariableDeclarationKind::Const =>
            {
                for declarator in &declaration.declarations {
                    if let BindingPattern::BindingIdentifier(id) = &declarator.id
                        && let Some(init) = &declarator.init
                    {
                        note(id, local_expression(init));
                    }
                }
            }
            AstKind::FunctionBody(FunctionBody { statements, .. })
            | AstKind::BlockStatement(BlockStatement {
                body: statements, ..
            }) => {
                for statement in statements {
                    if let Statement::FunctionDeclaration(function) = statement
                        && let Some(id) = &function.id
                    {
                        note(id, local_declaration(function));
                    }
                }
            }
            _ => {}
        }
    }

    /// A local function's declaration: walked later, where it's called.
    fn declared_local(&mut self, symbol: Option<SymbolId>) -> bool {
        let Some(symbol) = symbol.filter(|symbol| self.locals.functions.contains_key(symbol))
        else {
            return false;
        };
        if !self.locals.walked.contains(&symbol) && !self.locals.waiting.contains(&symbol) {
            self.locals.waiting.push(symbol);
        }
        true
    }

    /// Walks a local function where it's called, the first time it is: the call's place
    /// (an `if`, a `try`, a callback) is its checks' place, and the call's `await` is what
    /// it returns. An async one the test doesn't wait for may finish after the test.
    fn called_local(&mut self, call: &CallExpression<'a>) {
        let Expression::Identifier(callee) = call.callee.get_inner_expression() else {
            return;
        };
        let Some(symbol) = self.symbol(callee) else {
            return;
        };
        if self.locals.walked.contains(&symbol) {
            return;
        }
        let Some(&function) = self.locals.functions.get(&symbol) else {
            return;
        };
        self.locals.walked.insert(symbol);
        let awaited = self.awaited.contains(&call.span);
        let saved = (self.context, self.depth, self.runner_waits);
        self.depth += 1;
        self.context.in_callback |= function.is_async && !awaited;
        // A plain function hands what it returns to the call; an async one's own `await`s
        // decide inside it.
        self.runner_waits = function.is_async || awaited;
        self.visit_callback_body(&function.body);
        (self.context, self.depth, self.runner_waits) = saved;
    }

    /// Once a test, hook or helper is walked: a local function whose call wasn't reached
    /// (say, it's inside an `expect(…)`) is walked as a callback.
    fn settle_locals(&mut self) {
        for symbol in std::mem::take(&mut self.locals.waiting) {
            if self.locals.walked.insert(symbol)
                && let Some(&function) = self.locals.functions.get(&symbol)
            {
                self.nested_function(|this| this.visit_callback_body(&function.body));
            }
        }
    }

    /// A helper can be defined here: outside tests, hooks and other functions.
    fn can_define_helper(&self) -> bool {
        matches!(self.owner, Owner::Outside) && !self.in_helper
    }

    /// Walks a named function outside tests as a helper, or a method: its owner, and whether
    /// it's called only on the owner by name.
    fn helper(
        &mut self,
        name: String,
        method: Option<(String, bool)>,
        params: &FormalParameters<'a>,
        is_async: bool,
        span: Span,
        body: &CallbackBody<'_, 'a>,
    ) {
        let params = param_names(params);
        let locals = self.argument_locals(&params, method.is_some(), body);
        let (owner, on_owner) = match method {
            Some((owner, on_owner)) => (Some(owner), on_owner),
            None => (None, false),
        };
        self.file.helpers.push(Helper {
            name,
            owner,
            on_owner,
            suites: self.suite_names(),
            params,
            locals,
            is_async,
            default_export: false,
            line: self.line(span),
            end_line: self.lines.line(span.end),
            body: Body::default(),
        });
        let saved = (
            self.owner,
            self.context,
            self.depth,
            self.runner_waits,
            self.last_statement,
        );
        self.owner = Owner::Helper(self.file.helpers.len() - 1);
        self.context = Context::default();
        self.depth = 0;
        // What a helper returns, its caller gets: a check returned from it is awaited when
        // the call is.
        self.runner_waits = true;
        self.last_statement = body.statements().last().map(GetSpan::span);
        let locals = std::mem::take(&mut self.locals);
        self.visit_callback_body(body);
        self.settle_locals();
        self.locals = locals;
        (
            self.owner,
            self.context,
            self.depth,
            self.runner_waits,
            self.last_statement,
        ) = saved;
    }

    /// The constants a helper's body declares first thing from its arguments (or, in a
    /// method, from `this`): `const items = response.slice(0, -1)`. Each may read the ones
    /// before it.
    fn argument_locals(
        &self,
        params: &[Option<String>],
        method: bool,
        body: &CallbackBody<'_, 'a>,
    ) -> Vec<(String, String)> {
        let mut known: Vec<String> = params.iter().flatten().cloned().collect();
        let mut locals = Vec::new();
        for statement in body.statements() {
            let Statement::VariableDeclaration(declaration) = statement else {
                continue;
            };
            if declaration.kind != VariableDeclarationKind::Const {
                continue;
            }
            for declarator in &declaration.declarations {
                if let BindingPattern::BindingIdentifier(id) = &declarator.id
                    && let Some(init) = &declarator.init
                    && root_is(init, &known, method)
                {
                    known.push(id.name.to_string());
                    locals.push((id.name.to_string(), normalize(init)));
                }
            }
        }
        locals
    }

    /// A call to a function by its name, or to a method, which may be a helper: kept if one
    /// turns up.
    fn call(&mut self, call: &CallExpression<'a>) {
        if matches!(self.owner, Owner::Outside) {
            return;
        }
        let (name, receiver) = match call.callee.get_inner_expression() {
            Expression::Identifier(callee) => (callee.name.to_string(), None),
            Expression::StaticMemberExpression(member) if is_receiver(&member.object) => (
                member.property.name.to_string(),
                Some(normalize(&member.object)),
            ),
            _ => return,
        };
        let args: Vec<Value> = call
            .arguments
            .iter()
            .map(|arg| match arg.as_expression() {
                Some(expr) => self.values.value(expr, &self.consts),
                None => Value::Opaque("...".to_owned()),
            })
            .collect();
        let awaited = self.awaited.contains(&call.span);
        // The text is written once the call is known to be kept.
        self.push(
            ItemKind::Call(Call {
                name,
                receiver,
                args,
                text: String::new(),
                awaited,
                defined: None,
            }),
            call.span,
        );
    }

    fn suite_names(&self) -> Vec<String> {
        self.suites
            .iter()
            .filter_map(|suite| suite.name.clone())
            .collect()
    }

    /// Runs `walk` with the context changed by `set`, then restores it.
    fn scoped(&mut self, set: impl FnOnce(&mut Context), walk: impl FnOnce(&mut Self)) {
        let saved = self.context;
        set(&mut self.context);
        walk(self);
        self.context = saved;
    }

    fn modifiers(&self, raw: &[RawModifier]) -> Vec<Modifier> {
        raw.iter()
            .map(|modifier| Modifier {
                name: modifier.name.to_owned(),
                arg: match modifier.arg {
                    RawArg::None => None,
                    RawArg::Expr(expr) if matches!(modifier.name, "each" | "for") => {
                        Some(self.values.value(expr, &self.consts))
                    }
                    RawArg::Expr(expr) => Some(Value::Opaque(normalize(expr))),
                    RawArg::Template(template) if matches!(modifier.name, "each" | "for") => {
                        Some(self.values.table(template, &self.consts))
                    }
                    RawArg::Template(template) => Some(self.values.template(template)),
                },
            })
            .collect()
    }

    /// A test or `describe` name as written: string literals as-is, template literals with
    /// each `${…}` normalized, anything else as normalized source.
    fn name(&self, expr: &Expression) -> String {
        match self.values.value(expr, &Consts::default()) {
            Value::String(name) | Value::Template(name) => name,
            other => other.to_string(),
        }
    }

    /// Handles `describe`, `it`, hooks, steps and Playwright skips. Returns false for any
    /// other call.
    fn structure(&mut self, call: &CallExpression<'a>) -> bool {
        let Some(found) = calls::structure(call) else {
            return false;
        };
        match found {
            Structure::Suite {
                name,
                modifiers,
                callback,
            } => {
                let suite = Suite {
                    name: name.map(|name| self.name(name)),
                    modifiers: self.modifiers(&modifiers),
                    first_test: self.file.tests.len(),
                };
                self.suites.push(suite);
                if let Some(callback) = callback.and_then(as_callback) {
                    self.visit_callback_body(&callback.body);
                }
                self.suites.pop();
            }
            Structure::Test {
                name,
                modifiers,
                callback,
            } => {
                let own = self.modifiers(&modifiers);
                let mut all: Vec<Modifier> = self
                    .suites
                    .iter()
                    .flat_map(|suite| suite.modifiers.iter().cloned())
                    .collect();
                let has_table = own
                    .iter()
                    .any(|m| matches!(m.name.as_str(), "each" | "for"));
                all.extend(own);
                self.file.tests.push(Test {
                    suites: self.suite_names(),
                    name: self.name(name),
                    modifiers: all,
                    line: self.line(call.span),
                    end_line: self.lines.line(call.span.end),
                    body: Body::default(),
                    fingerprint: callback.map_or_else(Vec::new, |callback| {
                        fingerprint(self.values.source, callback.span())
                    }),
                });
                let owner = Owner::Test(self.file.tests.len() - 1);
                self.enter(owner, callback, !has_table);
            }
            Structure::Hook { kind, callback } => {
                self.file.hooks.push(Hook {
                    kind,
                    suites: self.suite_names(),
                    line: self.line(call.span),
                    end_line: self.lines.line(call.span.end),
                    body: Body::default(),
                });
                let owner = Owner::Hook(self.file.hooks.len() - 1);
                self.enter(owner, callback, false);
            }
            Structure::Skip { name, condition } => {
                let condition = condition.map(normalize);
                match self.owner {
                    // In a function outside tests, which may never be called. Helpers are
                    // linked to the tests that call them in a later step.
                    Owner::Outside if self.in_helper => {}
                    Owner::Helper(_) => {}
                    Owner::Outside => self.skip_suite(Modifier {
                        name: name.to_owned(),
                        arg: condition.map(Value::Opaque),
                    }),
                    Owner::Test(_) | Owner::Hook(_) => {
                        self.push(ItemKind::RuntimeSkip(condition), call.span);
                    }
                }
            }
            Structure::Step { callback } => {
                if let Some(callback) = callback {
                    self.part_of_the_test(callback);
                }
            }
        }
        true
    }

    /// Walks a function that runs as part of the test: a `test.step`, or an awaited
    /// `vi.waitFor`. Its checks aren't "in a callback", and the runner waits for what it
    /// returns, but a `return` in it only ends the function, so it counts as one deeper.
    fn part_of_the_test(&mut self, callback: &Expression<'a>) {
        if let Some(callback) = as_callback(callback) {
            let saved = (self.depth, self.runner_waits);
            self.depth += 1;
            self.runner_waits = true;
            self.visit_callback_body(&callback.body);
            (self.depth, self.runner_waits) = saved;
        }
    }

    /// Playwright's `test.skip(cond)` in a `describe` (or at the top of a file) skips every
    /// test in it, including the ones above it.
    fn skip_suite(&mut self, modifier: Modifier) {
        // A test's modifiers start with those of each enclosing suite, outermost first, so
        // this suite's go right after the ones it already has.
        let at: usize = self.suites.iter().map(|suite| suite.modifiers.len()).sum();
        let suite = self.suites.last_mut().expect("the file is always there");
        for test in &mut self.file.tests[suite.first_test..] {
            test.modifiers.insert(at, modifier.clone());
        }
        suite.modifiers.push(modifier);
    }

    /// Walks a test or hook body as `owner`.
    fn enter(&mut self, owner: Owner, callback: Option<&Expression<'a>>, read_params: bool) {
        let Some(Callback { params, body }) = callback.and_then(as_callback) else {
            return;
        };
        let saved = (
            self.owner,
            self.context,
            self.depth,
            self.in_helper,
            self.runner_waits,
            self.last_statement,
            self.skip_names.clone(),
        );
        self.owner = owner;
        self.context = Context::default();
        self.depth = 0;
        self.in_helper = false;
        self.runner_waits = true;
        self.last_statement = body.statements().last().map(GetSpan::span);
        self.skip_names = if read_params {
            skip_names(params)
        } else {
            SkipNames::default()
        };
        let locals = std::mem::take(&mut self.locals);
        self.visit_callback_body(&body);
        self.settle_locals();
        self.locals = locals;

        (
            self.owner,
            self.context,
            self.depth,
            self.in_helper,
            self.runner_waits,
            self.last_statement,
            self.skip_names,
        ) = saved;
    }

    fn assertion(&mut self, call: &CallExpression<'a>, found: calls::ExpectCall) {
        let matcher = found.matcher.map(|(name, args)| Matcher {
            name: name.to_owned(),
            kind: MatcherKind::of(name),
            args: args
                .iter()
                .map(|arg| match arg.as_expression() {
                    Some(expr) => self.values.value(expr, &self.consts),
                    None => Value::Opaque("...".to_owned()),
                })
                .collect(),
        });
        let facts = facts::facts(&found, &self.values, &self.consts);
        let assertion = Assertion {
            subject: found.subject.map(normalize).unwrap_or_default(),
            written: None,
            expect: found.kind,
            negated: found.negated,
            promise: found.promise,
            awaited: self.awaited.contains(&call.span),
            matcher,
            facts,
        };
        self.push(ItemKind::Assertion(assertion), call.span);
    }

    /// `ctx.skip()` or `skip()` with the names from the test function's parameters.
    fn is_context_skip(&self, call: &CallExpression) -> bool {
        match call.callee.get_inner_expression() {
            Expression::Identifier(id) => self.skip_names.skip.as_deref() == Some(id.name.as_str()),
            Expression::StaticMemberExpression(member) => {
                member.property.name == "skip"
                    && matches!(
                        member.object.get_inner_expression(),
                        Expression::Identifier(id)
                            if self.skip_names.context.as_deref() == Some(id.name.as_str())
                    )
            }
            _ => false,
        }
    }

    /// Walks a callback's body in place: not as a nested function, so it isn't "in callback".
    fn visit_callback_body(&mut self, body: &CallbackBody<'_, 'a>) {
        match body {
            CallbackBody::Block(body) => self.visit_function_body(body),
            CallbackBody::Expression(expr) => {
                self.returned(expr);
                self.visit_expression(expr);
            }
        }
    }

    fn mark_awaited(&mut self, expr: &Expression) {
        if let Expression::CallExpression(call) = expr.get_inner_expression() {
            self.awaited.insert(call.span);
        }
    }

    /// `expr` is returned from the function being walked, explicitly or as an arrow's body.
    fn returned(&mut self, expr: &Expression) {
        if self.runner_waits {
            self.mark_awaited(expr);
        }
    }

    /// A function inside a test is a callback: its checks may run late or not at all.
    /// Outside tests it's a helper.
    fn nested_function(&mut self, walk: impl FnOnce(&mut Self)) {
        let saved = (self.context, self.depth, self.in_helper, self.runner_waits);
        self.depth += 1;
        self.runner_waits = false;
        match self.owner {
            Owner::Outside => self.in_helper = true,
            Owner::Test(_) | Owner::Hook(_) | Owner::Helper(_) => {
                self.context.in_callback = true;
            }
        }
        walk(self);
        (self.context, self.depth, self.in_helper, self.runner_waits) = saved;
    }

    /// Notes a name the file builds objects from or extends.
    fn constructed(&mut self, name: &str) {
        if !self.file.constructed.iter().any(|known| known == name) {
            self.file.constructed.push(name.to_owned());
        }
    }

    /// The functions of an object defined outside tests, as its methods: `expectOk(res) { … }`
    /// and `expectOk: (res) => { … }`. Anything else in it is walked as usual.
    fn object_methods(&mut self, owner: &str, object: &ObjectExpression<'a>) {
        for property in &object.properties {
            match property {
                ObjectPropertyKind::ObjectProperty(property) => {
                    if let Some(name) = property.key.static_name()
                        && let Some(name) = name.as_str().map(str::to_owned)
                        && !property.computed
                        && let Some(callback) = as_callback(&property.value)
                    {
                        let is_async = is_async(&property.value);
                        self.helper(
                            name,
                            Some((owner.to_owned(), true)),
                            callback.params,
                            is_async,
                            property.span,
                            &callback.body,
                        );
                    } else {
                        self.visit_object_property(property);
                    }
                }
                ObjectPropertyKind::SpreadProperty(spread) => self.visit_spread_element(spread),
            }
        }
    }
}

impl<'a> Visit<'a> for Collector<'_, 'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        self.note_locals(kind);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if self.structure(call) {
            return;
        }
        if let Some(found) = calls::expect_call(call) {
            self.assertion(call, found);
            return;
        }
        if let Some(count) = calls::assertion_count(call) {
            let count = count.map(|n| self.values.value(n, &self.consts));
            self.push(ItemKind::AssertionCount(count), call.span);
            return;
        }
        if let Some(names) = calls::extended_matchers(call) {
            self.file.custom_matchers.extend(names);
            return;
        }
        if let Some(callback) = calls::retry_callback(call)
            && self.awaited.contains(&call.span)
        {
            self.part_of_the_test(callback);
            for argument in call.arguments.iter().skip(1) {
                self.visit_argument(argument);
            }
            return;
        }
        if self.owner.runs_tests() && self.is_context_skip(call) {
            let condition = call
                .arguments
                .first()
                .and_then(|arg| arg.as_expression())
                .map(normalize);
            self.push(ItemKind::RuntimeSkip(condition), call.span);
            return;
        }
        self.call(call);
        walk::walk_call_expression(self, call);
        // Its arguments first, then what it runs.
        self.called_local(call);
    }

    fn visit_await_expression(&mut self, it: &AwaitExpression<'a>) {
        self.mark_awaited(&it.argument);
        walk::walk_await_expression(self, it);
    }

    fn visit_return_statement(&mut self, it: &ReturnStatement<'a>) {
        if let Some(argument) = &it.argument {
            self.returned(argument);
        }
        // A `return` in a helper ends the helper: linking keeps it to the helper's own items.
        let in_body = !matches!(self.owner, Owner::Outside) && self.depth == 0;
        if in_body && self.last_statement != Some(it.span) {
            self.push(ItemKind::EarlyReturn, it.span);
        }
        walk::walk_return_statement(self, it);
    }

    fn visit_if_statement(&mut self, it: &IfStatement<'a>) {
        self.visit_expression(&it.test);
        self.scoped(
            |context| context.in_if = true,
            |this| {
                this.visit_statement(&it.consequent);
                if let Some(alternate) = &it.alternate {
                    this.visit_statement(alternate);
                }
            },
        );
    }

    fn visit_switch_statement(&mut self, it: &SwitchStatement<'a>) {
        self.visit_expression(&it.discriminant);
        self.scoped(
            |context| context.in_if = true,
            |this| this.visit_switch_cases(&it.cases),
        );
    }

    fn visit_conditional_expression(&mut self, it: &ConditionalExpression<'a>) {
        self.visit_expression(&it.test);
        self.scoped(
            |context| context.in_if = true,
            |this| {
                this.visit_expression(&it.consequent);
                this.visit_expression(&it.alternate);
            },
        );
    }

    fn visit_logical_expression(&mut self, it: &LogicalExpression<'a>) {
        self.visit_expression(&it.left);
        self.scoped(
            |context| context.short_circuit = true,
            |this| this.visit_expression(&it.right),
        );
    }

    fn visit_try_statement(&mut self, it: &TryStatement<'a>) {
        // Without a `catch` (`try { … } finally { … }`), a failure still reaches the test,
        // unless the `finally` block returns, which drops it. A `catch` that throws again
        // (`catch (err) { tidy(err); throw err }`) lets it through too.
        let rethrows = |handler: &CatchClause| {
            handler
                .body
                .body
                .iter()
                .any(|statement| matches!(statement, Statement::ThrowStatement(_)))
                && !leaves_early(&handler.body)
        };
        let catches = it
            .handler
            .as_deref()
            .is_some_and(|handler| !rethrows(handler))
            || it.finalizer.as_deref().is_some_and(leaves_early);
        self.scoped(
            |context| context.in_try |= catches,
            |this| this.visit_block_statement(&it.block),
        );
        if let Some(handler) = &it.handler {
            self.scoped(
                |context| context.in_catch = true,
                |this| this.visit_catch_clause(handler),
            );
        }
        if let Some(finalizer) = &it.finalizer {
            self.visit_block_statement(finalizer);
        }
    }

    /// A loop puts its checks `in_loop`, unless its body always runs at least once: a `for`
    /// with literal bounds or no condition, a `for…of` over a written-out array, a
    /// `while (true)`, a `do { … } while (…)`. Then see [`Self::first_pass`].
    fn visit_for_statement(&mut self, it: &ForStatement<'a>) {
        if runs_at_least_once(it) {
            if let Some(init) = &it.init {
                self.visit_for_statement_init(init);
            }
            if let Some(test) = &it.test {
                self.visit_expression(test);
            }
            if let Some(update) = &it.update {
                self.visit_expression(update);
            }
            self.first_pass(&it.body);
            return;
        }
        self.scoped(
            |context| context.in_loop = true,
            |this| walk::walk_for_statement(this, it),
        );
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        self.scoped(
            |context| context.in_loop = true,
            |this| walk::walk_for_in_statement(this, it),
        );
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        if has_items(&it.right) {
            self.visit_for_statement_left(&it.left);
            self.visit_expression(&it.right);
            self.first_pass(&it.body);
            return;
        }
        self.scoped(
            |context| context.in_loop = true,
            |this| walk::walk_for_of_statement(this, it),
        );
    }

    fn visit_while_statement(&mut self, it: &WhileStatement<'a>) {
        if is_true(&it.test) {
            self.visit_expression(&it.test);
            self.first_pass(&it.body);
            return;
        }
        self.scoped(
            |context| context.in_loop = true,
            |this| walk::walk_while_statement(this, it),
        );
    }

    fn visit_do_while_statement(&mut self, it: &DoWhileStatement<'a>) {
        self.first_pass(&it.body);
        self.visit_expression(&it.test);
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        if self.can_define_helper()
            && let (Some(id), Some(body)) = (&it.id, &it.body)
        {
            let body = CallbackBody::Block(body);
            self.helper(
                id.name.to_string(),
                None,
                &it.params,
                it.r#async,
                it.span,
                &body,
            );
            return;
        }
        if it.is_declaration()
            && self.declared_local(it.id.as_ref().and_then(|id| id.symbol_id.get()))
        {
            return;
        }
        self.nested_function(|this| walk::walk_function(this, it, flags));
    }

    /// `const expectOk = (res) => { … }` outside tests is a helper too.
    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if self.can_define_helper()
            && let BindingPattern::BindingIdentifier(id) = &it.id
            && let Some(init) = &it.init
            && let Some(callback) = as_callback(init)
        {
            let is_async = is_async(init);
            let name = id.name.to_string();
            self.helper(
                name,
                None,
                callback.params,
                is_async,
                it.span,
                &callback.body,
            );
            return;
        }
        // `const app = { expectOk(res) { … } }` outside tests: its methods are helpers.
        if self.can_define_helper()
            && let BindingPattern::BindingIdentifier(id) = &it.id
            && let Some(Expression::ObjectExpression(object)) =
                it.init.as_ref().map(Expression::get_inner_expression)
        {
            self.object_methods(id.name.as_ref(), object);
            return;
        }
        // `const hover = () => {…}` inside a test: walked where it's called.
        if let BindingPattern::BindingIdentifier(id) = &it.id
            && self.declared_local(id.symbol_id.get())
        {
            return;
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        let from = js_str(it.source.value);
        if it.import_kind.is_type() {
            return;
        }
        for specifier in it.specifiers.iter().flatten() {
            let (local, imported) = match specifier {
                ImportDeclarationSpecifier::ImportSpecifier(specifier) => {
                    if specifier.import_kind.is_type() {
                        continue;
                    }
                    (
                        specifier.local.name.to_string(),
                        specifier.imported.name().to_string(),
                    )
                }
                ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) => {
                    (specifier.local.name.to_string(), "default".to_owned())
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => continue,
            };
            self.file.imports.push(Import {
                local,
                imported,
                from: from.clone(),
                reexport: false,
            });
        }
    }

    /// `export { expectOk } from './api'`
    fn visit_export_from_declaration(&mut self, it: &ExportFromDeclaration<'a>) {
        let from = js_str(it.source.value);
        if it.export_kind.is_type() {
            return;
        }
        for specifier in &it.specifiers {
            self.file.imports.push(Import {
                local: specifier.exported.name().to_string(),
                imported: specifier.local.name().to_string(),
                from: from.clone(),
                reexport: true,
            });
        }
    }

    /// `export * from './api'`
    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        let from = js_str(it.source.value);
        if it.exported.is_none() && !it.export_kind.is_type() {
            self.file.imports.push(Import {
                local: "*".to_owned(),
                imported: "*".to_owned(),
                from,
                reexport: true,
            });
        }
    }

    /// `export default function expectOk() {}`, `export default expectOk;`
    fn visit_export_default_declaration(&mut self, it: &ExportDefaultDeclaration<'a>) {
        let helpers = self.file.helpers.len();
        walk::walk_export_default_declaration(self, it);
        match &it.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(_)
                if self.file.helpers.len() > helpers =>
            {
                self.file.helpers[helpers].default_export = true;
            }
            ExportDefaultDeclarationKind::Identifier(id) => {
                self.default_export = Some(id.name.to_string());
            }
            _ => {}
        }
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        self.nested_function(|this| walk::walk_arrow_function_expression(this, it));
    }

    /// A class outside tests: its methods are helpers, called on the objects it builds.
    fn visit_class(&mut self, it: &Class<'a>) {
        if let Some(Expression::Identifier(base)) = it
            .heritage
            .as_ref()
            .map(|heritage| heritage.expression.get_inner_expression())
        {
            self.constructed(base.name.as_str());
        }
        let name = it
            .id
            .as_ref()
            .filter(|_| self.can_define_helper())
            .map(|id| id.name.to_string());
        let saved = std::mem::replace(&mut self.class, name);
        walk::walk_class(self, it);
        self.class = saved;
    }

    /// `assertSyncResponse(response, records) { … }` in a class outside tests. Constructors,
    /// getters and setters aren't helpers: nothing calls them by name.
    fn visit_method_definition(&mut self, it: &MethodDefinition<'a>) {
        if let Some(owner) = self.class.clone()
            && self.can_define_helper()
            && it.kind == MethodDefinitionKind::Method
            && let Some(name) = it.key.static_name()
            && let Some(name) = name.as_str().map(str::to_owned)
            && let Some(body) = it.value.body.as_deref()
        {
            let body = CallbackBody::Block(body);
            let function = &it.value;
            self.helper(
                name,
                Some((owner, it.r#static)),
                &function.params,
                function.r#async,
                it.span,
                &body,
            );
            return;
        }
        walk::walk_method_definition(self, it);
    }

    /// `expectVisible = async () => { … }` in a class outside tests is a method too.
    fn visit_property_definition(&mut self, it: &PropertyDefinition<'a>) {
        if let Some(owner) = self.class.clone()
            && self.can_define_helper()
            && let Some(name) = it.key.static_name()
            && let Some(name) = name.as_str().map(str::to_owned)
            && let Some(value) = &it.value
            && let Some(callback) = as_callback(value)
        {
            let is_async = is_async(value);
            self.helper(
                name,
                Some((owner, it.r#static)),
                callback.params,
                is_async,
                it.span,
                &callback.body,
            );
            return;
        }
        walk::walk_property_definition(self, it);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        if let Expression::Identifier(callee) = it.callee.get_inner_expression() {
            self.constructed(callee.name.as_str());
        }
        walk::walk_new_expression(self, it);
    }

    /// A method on an object a helper builds (`return { serverAborted() { expect(…) } }`)
    /// runs when it's called, not with the helper: its checks aren't the helper's.
    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        let is_function = matches!(
            it.value.get_inner_expression(),
            Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
        );
        if is_function && matches!(self.owner, Owner::Helper(_)) {
            return;
        }
        walk::walk_object_property(self, it);
    }
}

/// A function value that's `async`.
fn is_async(expr: &Expression) -> bool {
    match expr.get_inner_expression() {
        Expression::ArrowFunctionExpression(arrow) => arrow.r#async,
        Expression::FunctionExpression(function) => function.r#async,
        _ => false,
    }
}

/// Built-ins and test-framework objects, whose methods are never helpers.
const NOT_RECEIVERS: &[&str] = &[
    "expect",
    "vi",
    "jest",
    "console",
    "Object",
    "JSON",
    "Math",
    "Promise",
    "Array",
    "Date",
    "Number",
    "String",
    "Reflect",
    "Symbol",
    "Buffer",
    "process",
    "globalThis",
    "window",
    "document",
];

/// What a method that may be a helper is called on: a name, `this`, or properties read from
/// one (`ctx`, `this`, `app.album`), not a built-in.
fn is_receiver(expr: &Expression) -> bool {
    match expr.get_inner_expression() {
        Expression::Identifier(id) => !NOT_RECEIVERS.contains(&id.name.as_str()),
        Expression::ThisExpression(_) => true,
        Expression::StaticMemberExpression(member) => is_receiver(&member.object),
        _ => false,
    }
}

/// Whether `expr` reads from one of `names` (or `this`, in a method): the name, then
/// properties, method calls and `await` on it (`response.slice(0, -1)`, `this.page`,
/// `(await res.json()).items`).
fn root_is(expr: &Expression, names: &[String], method: bool) -> bool {
    match expr.get_inner_expression() {
        Expression::Identifier(id) => names.iter().any(|name| name == id.name.as_str()),
        Expression::ThisExpression(_) => method,
        Expression::AwaitExpression(inner) => root_is(&inner.argument, names, method),
        Expression::CallExpression(call) => match call.callee.get_inner_expression() {
            Expression::Identifier(_) => false,
            callee => root_is(callee, names, method),
        },
        Expression::ChainExpression(chain) => match &chain.expression {
            oxc::ast::ast::ChainElement::CallExpression(call) => {
                root_is(&call.callee, names, method)
            }
            element => element
                .as_member_expression()
                .is_some_and(|member| root_is(member.object(), names, method)),
        },
        expr => expr
            .as_member_expression()
            .is_some_and(|member| root_is(member.object(), names, method)),
    }
}

/// A `for` loop whose body runs at least once: one without a condition (`for (;;)`), with
/// `true`, or with a counter that starts inside its literal bound (`for (let i = 0; i < 2;
/// i++)`).
fn runs_at_least_once(it: &ForStatement) -> bool {
    let Some(test) = &it.test else {
        return true;
    };
    if is_true(test) {
        return true;
    }
    let Some(ForStatementInit::VariableDeclaration(init)) = &it.init else {
        return false;
    };
    let [declarator] = init.declarations.as_slice() else {
        return false;
    };
    let (BindingPattern::BindingIdentifier(counter), Some(start)) =
        (&declarator.id, &declarator.init)
    else {
        return false;
    };
    let Expression::BinaryExpression(test) = test.get_inner_expression() else {
        return false;
    };
    let (Expression::Identifier(left), Some(start), Some(bound)) = (
        test.left.get_inner_expression(),
        literal_number(start),
        literal_number(&test.right),
    ) else {
        return false;
    };
    left.name == counter.name
        && match test.operator {
            BinaryOperator::LessThan => start < bound,
            BinaryOperator::LessEqualThan => start <= bound,
            BinaryOperator::GreaterThan => start > bound,
            BinaryOperator::GreaterEqualThan => start >= bound,
            _ => false,
        }
}

/// `2`, `-1`.
fn literal_number(expr: &Expression) -> Option<f64> {
    match expr.get_inner_expression() {
        Expression::NumericLiteral(lit) => Some(lit.value),
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::UnaryNegation => {
            literal_number(&unary.argument).map(|n| -n)
        }
        _ => None,
    }
}

fn is_true(expr: &Expression) -> bool {
    matches!(expr.get_inner_expression(), Expression::BooleanLiteral(lit) if lit.value)
}

/// An array written out with at least one item that isn't a spread, so a `for…of` over it
/// runs: `for (const path of ['/a', '/b'])`.
fn has_items(expr: &Expression) -> bool {
    let Expression::ArrayExpression(array) = expr.get_inner_expression() else {
        return false;
    };
    array
        .elements
        .iter()
        .any(|element| !matches!(element, ArrayExpressionElement::SpreadElement(_)))
}

/// The block has a `return`, or a `break` / `continue` that leaves it. In a `finally`
/// block, that drops the error being thrown.
fn leaves_early(block: &BlockStatement) -> bool {
    let mut finder = Leaves {
        returns: true,
        ..Leaves::default()
    };
    finder.visit_block_statement(block);
    finder.found
}

/// The statement, in a loop's body, may `break` out of the loop or `continue` to its next
/// pass, skipping what follows it.
fn may_leave_loop(statement: &Statement) -> bool {
    let mut finder = Leaves::default();
    finder.visit_statement(statement);
    finder.found
}

/// Finds a `break` or `continue` that leaves where it starts, and a `return` if asked.
#[derive(Default)]
struct Leaves {
    returns: bool,
    found: bool,
    /// Loops inside, which an unlabelled `break` or `continue` stays in.
    loops: u32,
    /// `switch`es inside, which an unlabelled `break` stays in, but not a `continue`.
    switches: u32,
}

impl Leaves {
    fn inside(&mut self, walk: impl FnOnce(&mut Self)) {
        self.loops += 1;
        walk(self);
        self.loops -= 1;
    }
}

impl<'a> Visit<'a> for Leaves {
    fn visit_return_statement(&mut self, _: &ReturnStatement<'a>) {
        self.found |= self.returns;
    }
    fn visit_break_statement(&mut self, it: &BreakStatement<'a>) {
        self.found |= self.loops + self.switches == 0 || it.label.is_some();
    }
    fn visit_continue_statement(&mut self, it: &ContinueStatement<'a>) {
        self.found |= self.loops == 0 || it.label.is_some();
    }
    fn visit_for_statement(&mut self, it: &ForStatement<'a>) {
        self.inside(|this| walk::walk_for_statement(this, it));
    }
    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        self.inside(|this| walk::walk_for_in_statement(this, it));
    }
    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        self.inside(|this| walk::walk_for_of_statement(this, it));
    }
    fn visit_while_statement(&mut self, it: &WhileStatement<'a>) {
        self.inside(|this| walk::walk_while_statement(this, it));
    }
    fn visit_do_while_statement(&mut self, it: &DoWhileStatement<'a>) {
        self.inside(|this| walk::walk_do_while_statement(this, it));
    }
    fn visit_switch_statement(&mut self, it: &SwitchStatement<'a>) {
        self.switches += 1;
        walk::walk_switch_statement(self, it);
        self.switches -= 1;
    }
    // A `return` in a nested function returns from that function.
    fn visit_function(&mut self, _: &Function<'a>, _: ScopeFlags) {}
    fn visit_arrow_function_expression(&mut self, _: &ArrowFunctionExpression<'a>) {}
}

/// A function passed to `describe`, `it`, a hook or `test.step`.
struct Callback<'b, 'a> {
    params: &'b FormalParameters<'a>,
    body: CallbackBody<'b, 'a>,
}

#[derive(Clone, Copy)]
enum CallbackBody<'b, 'a> {
    /// `() => { … }` or `function () { … }`
    Block(&'b FunctionBody<'a>),
    /// `() => expect(x).toBe(1)`
    Expression(&'b Expression<'a>),
}

impl<'a> CallbackBody<'_, 'a> {
    fn statements(&self) -> &[Statement<'a>] {
        match self {
            Self::Block(body) => &body.statements,
            Self::Expression(_) => &[],
        }
    }
}

fn as_callback<'b, 'a>(expr: &'b Expression<'a>) -> Option<Callback<'b, 'a>> {
    match expr.get_inner_expression() {
        Expression::ArrowFunctionExpression(arrow) => Some(Callback {
            params: &arrow.params,
            body: arrow_body(arrow)?,
        }),
        Expression::FunctionExpression(function) => Some(Callback {
            params: &function.params,
            body: CallbackBody::Block(function.body.as_deref()?),
        }),
        _ => None,
    }
}

/// A `const`'s value as a local function: `() => {…}`, `async function () {…}`.
fn local_expression<'a>(init: &'a Expression<'a>) -> Option<LocalFunction<'a>> {
    match init.get_inner_expression() {
        Expression::ArrowFunctionExpression(arrow) => Some(LocalFunction {
            body: arrow_body(arrow)?,
            is_async: arrow.r#async,
        }),
        Expression::FunctionExpression(function) => local_declaration(function),
        _ => None,
    }
}

fn local_declaration<'a>(function: &'a Function<'a>) -> Option<LocalFunction<'a>> {
    Some(LocalFunction {
        body: CallbackBody::Block(function.body.as_deref()?),
        is_async: function.r#async,
    })
}

/// An arrow function's body: a block, or the one expression it returns.
fn arrow_body<'b, 'a>(arrow: &'b ArrowFunctionExpression<'a>) -> Option<CallbackBody<'b, 'a>> {
    Some(match arrow.body.as_function_body() {
        Some(body) => CallbackBody::Block(body),
        None => CallbackBody::Expression(arrow.body.as_expression()?),
    })
}

/// Words that say nothing about what a test does, left out of its fingerprint.
const COMMON_WORDS: &[&str] = &[
    "async",
    "await",
    "const",
    "let",
    "var",
    "function",
    "return",
    "true",
    "false",
    "null",
    "undefined",
    "new",
    "expect",
    "toBe",
    "toEqual",
    "typeof",
    "this",
];

/// The distinct words (identifiers, numbers, words in strings) in a test's source, sorted.
fn fingerprint(source: &str, span: Span) -> Vec<String> {
    let text = &source[span.start as usize..span.end as usize];
    let words: std::collections::BTreeSet<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|word| word.len() >= 3 && !COMMON_WORDS.contains(word))
        .collect();
    words.into_iter().map(str::to_owned).collect()
}

fn skip_names(params: &FormalParameters) -> SkipNames {
    let mut names = SkipNames::default();
    match params.items.first().map(|param| &param.pattern) {
        Some(BindingPattern::BindingIdentifier(id)) => names.context = Some(id.name.to_string()),
        Some(BindingPattern::ObjectPattern(object)) => {
            for property in &object.properties {
                if property
                    .key
                    .static_name()
                    .is_some_and(|name| name == "skip")
                    && let BindingPattern::BindingIdentifier(id) = &property.value
                {
                    names.skip = Some(id.name.to_string());
                }
            }
        }
        _ => {}
    }
    names
}
