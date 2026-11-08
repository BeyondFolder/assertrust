//! Constants whose value can be filled in where they're used: `const PLAN = 'pro'`, then
//! `expect(plan).toBe(PLAN)` is read as `toBe('pro')`.
//!
//! Names are resolved by oxc's scope analysis, so a parameter or an inner variable with the
//! same name (`it.each(rows)('…', (PLAN) => …)`) is never mixed up with the constant.
//!
//! A `const` array or object can still change (`expected.push(x)`), so one is only filled in
//! when every use of it just reads it: as a matcher argument, an `expect(…)` subject or an
//! `.each` table. Anything else (`expected.push(x)`, `build(expected)`) keeps it as written.

use std::collections::{HashMap, HashSet};

use oxc::ast::ast::{
    ArrayExpressionElement, BindingPattern, CallExpression, Expression, IdentifierReference,
    ObjectPropertyKind, Program, VariableDeclaration, VariableDeclarationKind,
};
use oxc::ast_visit::{Visit, walk};
use oxc::semantic::Scoping;
use oxc::syntax::reference::ReferenceId;
use oxc::syntax::symbol::SymbolId;

use super::calls::{self, RawArg, Structure};
use super::values::Values;
use crate::value::Value;

/// Constants with a literal value, by the variable they declare.
#[derive(Default)]
pub(super) struct Consts {
    values: HashMap<SymbolId, Value>,
}

impl Consts {
    /// The value of the constant `id` refers to, if it's one that can be filled in.
    pub(super) fn get(&self, id: &IdentifierReference, scoping: &Scoping) -> Option<&Value> {
        let symbol = scoping.get_reference(id.reference_id.get()?).symbol_id()?;
        self.values.get(&symbol)
    }

    /// Finds them in a whole file.
    pub(super) fn find(program: &Program, values: &Values) -> Self {
        let mut uses = Uses {
            scoping: values.scoping,
            reads: HashSet::new(),
            others: HashSet::new(),
        };
        uses.visit_program(program);

        let mut declarations = Declarations {
            values,
            unsafe_containers: &uses.others,
            consts: Self::default(),
        };
        declarations.visit_program(program);
        declarations.consts
    }
}

/// Every `const` with a literal value, in source order, so one can use an earlier one.
struct Declarations<'v, 's> {
    values: &'v Values<'s>,
    /// Variables used in a way that might change them.
    unsafe_containers: &'v HashSet<SymbolId>,
    consts: Consts,
}

impl<'a> Visit<'a> for Declarations<'_, '_> {
    fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'a>) {
        if it.kind == VariableDeclarationKind::Const {
            for declarator in &it.declarations {
                if let BindingPattern::BindingIdentifier(id) = &declarator.id
                    && let Some(init) = &declarator.init
                    && let Some(symbol) = id.symbol_id.get()
                {
                    let value = self.values.value(init, &self.consts);
                    let container = matches!(value, Value::Array(_) | Value::Object(_));
                    if is_literal(&value)
                        && !(container && self.unsafe_containers.contains(&symbol))
                    {
                        self.consts.values.insert(symbol, value);
                    }
                }
            }
        }
        walk::walk_variable_declaration(self, it);
    }
}

/// A value made only of literals (no variables, calls or `${…}`).
fn is_literal(value: &Value) -> bool {
    match value {
        Value::Opaque(_) | Value::Template(_) | Value::Matcher { .. } => false,
        Value::Array(items) => items.iter().all(is_literal),
        Value::Object(fields) => fields.values().all(is_literal),
        _ => true,
    }
}

/// Sorts every use of a variable into "only read" or "anything else".
struct Uses<'s> {
    scoping: &'s Scoping,
    /// Uses that only read the value: they're skipped when seen.
    reads: HashSet<ReferenceId>,
    /// Variables with at least one other use.
    others: HashSet<SymbolId>,
}

impl Uses<'_> {
    /// Marks the variables in `expr` as only read, looking inside array and object literals.
    fn read(&mut self, expr: &Expression) {
        match expr.get_inner_expression() {
            Expression::Identifier(id) => {
                if let Some(reference) = id.reference_id.get() {
                    self.reads.insert(reference);
                }
            }
            Expression::ArrayExpression(array) => {
                for element in &array.elements {
                    match element {
                        ArrayExpressionElement::SpreadElement(spread) => {
                            self.read(&spread.argument)
                        }
                        element => {
                            if let Some(expr) = element.as_expression() {
                                self.read(expr);
                            }
                        }
                    }
                }
            }
            Expression::ObjectExpression(object) => {
                for property in &object.properties {
                    match property {
                        ObjectPropertyKind::ObjectProperty(property) => self.read(&property.value),
                        ObjectPropertyKind::SpreadProperty(spread) => self.read(&spread.argument),
                    }
                }
            }
            _ => {}
        }
    }
}

impl<'a> Visit<'a> for Uses<'_> {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Some(found) = calls::expect_call(call) {
            if let Some(subject) = found.subject {
                self.read(subject);
            }
            if let Some((_, args)) = found.matcher {
                args.iter()
                    .filter_map(|arg| arg.as_expression())
                    .for_each(|arg| self.read(arg));
            }
        } else if calls::value_matcher_name(call).is_some() {
            call.arguments
                .iter()
                .filter_map(|arg| arg.as_expression())
                .for_each(|arg| self.read(arg));
        } else if let Some(Structure::Test { modifiers, .. } | Structure::Suite { modifiers, .. }) =
            calls::structure(call)
        {
            for modifier in modifiers
                .iter()
                .filter(|m| matches!(m.name, "each" | "for"))
            {
                match modifier.arg {
                    RawArg::Expr(table) => self.read(table),
                    RawArg::Template(table) => table.expressions.iter().for_each(|e| self.read(e)),
                    RawArg::None => {}
                }
            }
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_identifier_reference(&mut self, id: &IdentifierReference<'a>) {
        if let Some(reference) = id.reference_id.get()
            && !self.reads.contains(&reference)
            && let Some(symbol) = self.scoping.get_reference(reference).symbol_id()
        {
            self.others.insert(symbol);
        }
    }
}
