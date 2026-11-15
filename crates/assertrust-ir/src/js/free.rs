//! The names an expression reads from outside it: the ones a helper's arguments can be
//! filled in for (`link`).

use oxc::allocator::Allocator;
use oxc::ast::ast::{
    ArrowFunctionExpression, BindingIdentifier, Function, IdentifierReference, ThisExpression,
};
use oxc::ast_visit::{Visit, walk};
use oxc::parser::Parser;
use oxc::span::SourceType;
use oxc::syntax::scope::ScopeFlags;

/// Where the names `source` reads from outside it start, in bytes: `res` in
/// `items.filter((x) => x.id === res.id)`, but not `x`, which the arrow function binds,
/// nor a word in a regex. `this` counts too, except inside a `function`, which has its
/// own. `None` when it doesn't parse.
pub(crate) fn free_names(source: &str) -> Option<Vec<u32>> {
    let allocator = Allocator::default();
    let expr = Parser::new(&allocator, source, SourceType::ts().with_module(true))
        .parse_expression()
        .ok()?;
    let mut free = Free::default();
    free.visit_expression(&expr);
    Some(free.found)
}

#[derive(Default)]
struct Free {
    /// The names each function on the way binds: its parameters and what it declares.
    bound: Vec<Vec<String>>,
    /// How many `function`s deep the walk is: `this` in one is its own.
    functions: usize,
    found: Vec<u32>,
}

impl Free {
    fn is_bound(&self, name: &str) -> bool {
        self.bound.iter().flatten().any(|bound| bound == name)
    }
}

impl<'a> Visit<'a> for Free {
    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        let mut names = Bindings::default();
        names.visit_arrow_function_expression(it);
        self.bound.push(names.0);
        walk::walk_arrow_function_expression(self, it);
        self.bound.pop();
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        let mut names = Bindings::default();
        names.visit_function(it, flags);
        self.bound.push(names.0);
        self.functions += 1;
        walk::walk_function(self, it, flags);
        self.functions -= 1;
        self.bound.pop();
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        if !self.is_bound(&it.name) {
            self.found.push(it.span.start);
        }
    }

    fn visit_this_expression(&mut self, it: &ThisExpression) {
        if self.functions == 0 {
            self.found.push(it.span.start);
        }
    }
}

/// Every name bound inside a function, nested ones included.
#[derive(Default)]
struct Bindings(Vec<String>);

impl<'a> Visit<'a> for Bindings {
    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.0.push(it.name.to_string());
    }
}
