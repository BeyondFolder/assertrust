//! Turning JS expressions into [`Value`]s and normalized source text.

use std::collections::{BTreeMap, HashMap};

use oxc::ast::ast::{
    ArrayExpressionElement, Expression, ObjectPropertyKind, PropertyKey, RegExpLiteral, Statement,
    TemplateLiteral, UnaryOperator, VariableDeclarationKind,
};
use oxc::ast::ast::{BindingPattern, Declaration};
use oxc::codegen::{Codegen, CodegenOptions};
use oxc::str::JSStr;

use super::calls;
use crate::value::Value;

/// Constants with literal values, by name: `const PLAN = 'pro'`. Innermost scope last.
#[derive(Default)]
pub(super) struct Consts {
    scopes: Vec<HashMap<String, Value>>,
}

impl Consts {
    pub(super) fn push(&mut self, scope: HashMap<String, Value>) {
        self.scopes.push(scope);
    }

    pub(super) fn pop(&mut self) {
        self.scopes.pop();
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }
}

/// Reads expressions from one source file.
pub(super) struct Values<'s> {
    pub(super) source: &'s str,
}

impl Values<'_> {
    /// An expression as a value. Constants in `consts` are filled in.
    pub(super) fn value(&self, expr: &Expression, consts: &Consts) -> Value {
        match expr.get_inner_expression() {
            Expression::NullLiteral(_) => Value::Null,
            Expression::Identifier(id) if id.name == "undefined" => Value::Undefined,
            Expression::Identifier(id) => consts
                .get(id.name.as_str())
                .cloned()
                .unwrap_or_else(|| Value::Opaque(id.name.to_string())),
            Expression::BooleanLiteral(lit) => Value::Bool(lit.value),
            Expression::NumericLiteral(lit) => Value::Number(number(lit.value)),
            Expression::UnaryExpression(unary) => match (unary.operator, &unary.argument) {
                (UnaryOperator::UnaryNegation, Expression::NumericLiteral(lit)) => {
                    Value::Number(number(-lit.value))
                }
                _ => Value::Opaque(normalize(expr)),
            },
            Expression::StringLiteral(lit) => Value::String(js_str(lit.value)),
            Expression::TemplateLiteral(template) => self.template(template),
            Expression::RegExpLiteral(regex) => self.regex(regex),
            Expression::ArrayExpression(array) => Value::Array(
                array
                    .elements
                    .iter()
                    .map(|element| match element {
                        ArrayExpressionElement::SpreadElement(spread) => {
                            Value::Opaque(format!("...{}", normalize(&spread.argument)))
                        }
                        ArrayExpressionElement::Elision(_) => Value::Undefined,
                        _ => match element.as_expression() {
                            Some(expr) => self.value(expr, consts),
                            None => Value::Undefined,
                        },
                    })
                    .collect(),
            ),
            Expression::ObjectExpression(object) => {
                let mut fields = BTreeMap::new();
                for property in &object.properties {
                    match property {
                        ObjectPropertyKind::ObjectProperty(property) => {
                            let key = self.key(&property.key, property.computed);
                            let value = if property.method {
                                Value::Opaque(normalize(&property.value))
                            } else {
                                self.value(&property.value, consts)
                            };
                            fields.insert(key, value);
                        }
                        ObjectPropertyKind::SpreadProperty(spread) => {
                            let source = normalize(&spread.argument);
                            fields.insert(format!("...{source}"), Value::Opaque(source));
                        }
                    }
                }
                Value::Object(fields)
            }
            Expression::CallExpression(call) => match calls::value_matcher_name(call) {
                Some(name) => Value::Matcher {
                    name,
                    args: call
                        .arguments
                        .iter()
                        .map(|arg| match arg.as_expression() {
                            Some(expr) => self.value(expr, consts),
                            None => Value::Opaque("...".to_owned()),
                        })
                        .collect(),
                },
                None => Value::Opaque(normalize(expr)),
            },
            _ => Value::Opaque(normalize(expr)),
        }
    }

    /// A template literal: a plain string when it has no `${…}`, else its normalized source.
    pub(super) fn template(&self, template: &TemplateLiteral) -> Value {
        if template.expressions.is_empty()
            && let [quasi] = template.quasis.as_slice()
            && let Some(cooked) = quasi.value.cooked
        {
            return Value::String(js_str(cooked));
        }
        let source = &self.source[template.span.start as usize + 1..template.span.end as usize - 1];
        Value::Template(collapse(source))
    }

    fn regex(&self, regex: &RegExpLiteral) -> Value {
        let source = regex.regex.pattern.text.to_string();
        let alternatives = match &regex.regex.pattern.pattern {
            Some(pattern) => pattern
                .body
                .body
                .iter()
                .map(|alternative| {
                    let span = alternative.span;
                    self.source[span.start as usize..span.end as usize].to_owned()
                })
                .collect(),
            None => vec![source.clone()],
        };
        Value::Regex {
            source,
            flags: regex.regex.flags.to_string(),
            alternatives,
        }
    }

    fn key(&self, key: &PropertyKey, computed: bool) -> String {
        if !computed && let Some(name) = key.static_name() {
            return name.as_str().map_or_else(String::new, str::to_owned);
        }
        match key.as_expression() {
            Some(expr) => format!("[{}]", normalize(expr)),
            None => String::new(),
        }
    }

    /// Constants with a literal value declared directly in `statements`, e.g.
    /// `const PLAN = 'pro'`. Anything computed is left out.
    pub(super) fn consts_in(
        &self,
        statements: &[Statement],
        outer: &Consts,
    ) -> HashMap<String, Value> {
        let mut found = HashMap::new();
        for statement in statements {
            let declaration = match statement {
                Statement::VariableDeclaration(declaration) => declaration,
                Statement::ExportDeclaration(export) => match &export.declaration {
                    Declaration::VariableDeclaration(declaration) => declaration,
                    _ => continue,
                },
                _ => continue,
            };
            if declaration.kind != VariableDeclarationKind::Const {
                continue;
            }
            for declarator in &declaration.declarations {
                if let BindingPattern::BindingIdentifier(id) = &declarator.id
                    && let Some(init) = &declarator.init
                {
                    let value = self.value(init, outer);
                    if is_literal(&value) {
                        found.insert(id.name.to_string(), value);
                    }
                }
            }
        }
        found
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

/// An expression's source with formatting removed: oxc prints it back in one canonical
/// style (single quotes, no trailing commas), then line breaks are folded into spaces.
pub(super) fn normalize(expr: &Expression) -> String {
    let options = CodegenOptions {
        single_quote: true,
        ..CodegenOptions::default()
    };
    let mut codegen = Codegen::new().with_options(options);
    codegen.print_expression(expr);
    collapse(&codegen.into_source_text())
}

/// Folds each line break and the indentation after it into one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' || c == '\r' {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out.trim_end().to_owned()
}

/// `1`, `1.5`, `-3`, `16` (for `0x10`), `1000` (for `1e3`).
fn number(value: f64) -> String {
    format!("{value}")
}

pub(super) fn js_str(value: JSStr) -> String {
    value.as_str().map_or_else(
        || String::from_utf8_lossy(value.as_bytes()).into_owned(),
        str::to_owned,
    )
}
