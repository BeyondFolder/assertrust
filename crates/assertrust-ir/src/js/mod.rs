//! Reading JS/TS test files with oxc (`docs/architecture.md`, step 2).
//!
//! One pass over the syntax tree. Test structure (`describe`, `it`, hooks) is recognized by
//! name; inside a test or hook every `expect(…)` becomes an [`Item`] tagged with where it
//! sits. oxc's tree is freed when [`parse`] returns: only our own types leave this module.

mod calls;
mod consts;
mod facts;
mod values;

use std::collections::HashSet;

use oxc::allocator::Allocator;
use oxc::ast::ast::{
    ArrowFunctionExpression, AwaitExpression, BindingPattern, CallExpression,
    ConditionalExpression, DoWhileStatement, Expression, ForInStatement, ForOfStatement,
    ForStatement, FormalParameters, Function, FunctionBody, IfStatement, LogicalExpression,
    ReturnStatement, Statement, SwitchStatement, TryStatement, WhileStatement,
};
use oxc::ast_visit::{Visit, walk};
use oxc::parser::{ParseOptions, Parser};
use oxc::semantic::SemanticBuilder;
use oxc::span::{GetSpan, SourceType, Span};
use oxc::syntax::scope::ScopeFlags;

use self::calls::{RawArg, RawModifier, Structure};
use self::consts::Consts;
use self::values::{Values, normalize};
use crate::ParseError;
use crate::matcher::MatcherKind;
use crate::model::{
    Assertion, Body, Context, FileIr, Hook, Item, ItemKind, Matcher, Modifier, Test,
};
use crate::value::Value;

pub(crate) fn parse(path: &str, source: &str) -> Result<FileIr, ParseError> {
    let source_type = SourceType::from_path(path).map_err(|_| ParseError::UnsupportedFile)?;
    let allocator = Allocator::default();
    let options = ParseOptions {
        parse_regular_expression: true,
        ..ParseOptions::default()
    };
    let parsed = Parser::new(&allocator, source, source_type)
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

    let program = parsed.program;
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
    };
    collector.visit_program(&program);
    Ok(collector.file)
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
}

/// Names that skip the running test when called: `ctx` in `it('…', (ctx) => ctx.skip())`,
/// or `skip` in `it('…', ({ skip }) => skip())` (vitest).
#[derive(Default, Clone)]
struct SkipNames {
    context: Option<String>,
    skip: Option<String>,
}

struct Collector<'s> {
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
}

impl<'a> Collector<'_> {
    fn line(&self, span: Span) -> u32 {
        self.lines.line(span.start)
    }

    fn push(&mut self, kind: ItemKind, span: Span) {
        let item = Item {
            kind,
            line: self.line(span),
            context: self.context,
        };
        match self.owner {
            Owner::Outside if self.in_helper => {}
            Owner::Outside => self.file.outside.items.push(item),
            Owner::Test(index) => self.file.tests[index].body.items.push(item),
            Owner::Hook(index) => self.file.hooks[index].body.items.push(item),
        }
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
        self.visit_callback_body(&body);

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
            Owner::Test(_) | Owner::Hook(_) => self.context.in_callback = true,
        }
        walk(self);
        (self.context, self.depth, self.in_helper, self.runner_waits) = saved;
    }
}

impl<'a> Visit<'a> for Collector<'_> {
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
        if !matches!(self.owner, Owner::Outside) && self.is_context_skip(call) {
            let condition = call
                .arguments
                .first()
                .and_then(|arg| arg.as_expression())
                .map(normalize);
            self.push(ItemKind::RuntimeSkip(condition), call.span);
            return;
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_await_expression(&mut self, it: &AwaitExpression<'a>) {
        self.mark_awaited(&it.argument);
        walk::walk_await_expression(self, it);
    }

    fn visit_return_statement(&mut self, it: &ReturnStatement<'a>) {
        if let Some(argument) = &it.argument {
            self.returned(argument);
        }
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
        // Without a `catch` (`try { … } finally { … }`), a failure still reaches the test.
        let catches = it.handler.is_some();
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

    fn visit_for_statement(&mut self, it: &ForStatement<'a>) {
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
        self.scoped(
            |context| context.in_loop = true,
            |this| walk::walk_for_of_statement(this, it),
        );
    }

    fn visit_while_statement(&mut self, it: &WhileStatement<'a>) {
        self.scoped(
            |context| context.in_loop = true,
            |this| walk::walk_while_statement(this, it),
        );
    }

    fn visit_do_while_statement(&mut self, it: &DoWhileStatement<'a>) {
        self.scoped(
            |context| context.in_loop = true,
            |this| walk::walk_do_while_statement(this, it),
        );
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        self.nested_function(|this| walk::walk_function(this, it, flags));
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        self.nested_function(|this| walk::walk_arrow_function_expression(this, it));
    }
}

/// A function passed to `describe`, `it`, a hook or `test.step`.
struct Callback<'b, 'a> {
    params: &'b FormalParameters<'a>,
    body: CallbackBody<'b, 'a>,
}

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
            body: match arrow.body.as_function_body() {
                Some(body) => CallbackBody::Block(body),
                None => CallbackBody::Expression(arrow.body.as_expression()?),
            },
        }),
        Expression::FunctionExpression(function) => Some(Callback {
            params: &function.params,
            body: CallbackBody::Block(function.body.as_deref()?),
        }),
        _ => None,
    }
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
