//! Turning JS expressions into [`Value`]s and normalized source text.

use std::collections::BTreeMap;

use oxc::ast::ast::{
    ArrayExpressionElement, Expression, ObjectPropertyKind, PropertyKey, RegExpLiteral,
    TemplateElement, TemplateLiteral, UnaryOperator,
};
use oxc::codegen::{Codegen, CodegenOptions};
use oxc::semantic::Scoping;
use oxc::str::JSStr;

use super::calls;
use super::consts::Consts;
use crate::value::Value;

/// Reads expressions from one source file.
pub(super) struct Values<'s> {
    pub(super) source: &'s str,
    pub(super) scoping: &'s Scoping,
}

impl Values<'_> {
    /// An expression as a value. Constants in `consts` are filled in.
    pub(super) fn value(&self, expr: &Expression, consts: &Consts) -> Value {
        match expr.get_inner_expression() {
            Expression::NullLiteral(_) => Value::Null,
            Expression::Identifier(id) if id.name == "undefined" => Value::Undefined,
            Expression::Identifier(id) => consts
                .get(id, self.scoping)
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

    /// A template literal: a plain string when it has no `${…}`, else its source on one line
    /// with each `${…}` normalized, so `${ a }` and `${a}` are equal.
    pub(super) fn template(&self, template: &TemplateLiteral) -> Value {
        if template.expressions.is_empty()
            && let [quasi] = template.quasis.as_slice()
            && let Some(cooked) = quasi.value.cooked
        {
            return Value::String(js_str(cooked));
        }
        let mut source = String::new();
        for (i, quasi) in template.quasis.iter().enumerate() {
            push_template_text(&mut source, quasi);
            if let Some(expr) = template.expressions.get(i) {
                source.push_str("${");
                source.push_str(&normalize(expr));
                source.push('}');
            }
        }
        Value::Template(source)
    }

    /// A tagged `.each` table, one object per row:
    ///
    /// ```text
    /// it.each`
    ///   plan     | price
    ///   ${'pro'} | ${10}
    /// `
    /// ```
    ///
    /// is `[{ plan: 'pro', price: 10 }]`, so lining up the columns differently changes nothing.
    /// Anything that doesn't have that shape is read as a template.
    pub(super) fn table(&self, template: &TemplateLiteral, consts: &Consts) -> Value {
        let text = |quasi: &TemplateElement| quasi.value.cooked.map(js_str);
        let header = template.quasis.first().and_then(text).unwrap_or_default();
        let columns: Vec<&str> = header.split('|').map(str::trim).collect();
        let separators_only = template.quasis[1..].iter().all(|quasi| {
            text(quasi).is_some_and(|text| text.chars().all(|c| c == '|' || c.is_whitespace()))
        });
        if columns.iter().any(|column| column.is_empty())
            || !separators_only
            || !template.expressions.len().is_multiple_of(columns.len())
        {
            return self.template(template);
        }
        let rows = template
            .expressions
            .chunks(columns.len())
            .map(|cells| {
                let fields = columns
                    .iter()
                    .zip(cells)
                    .map(|(column, cell)| ((*column).to_owned(), self.value(cell, consts)))
                    .collect();
                Value::Object(fields)
            })
            .collect();
        Value::Array(rows)
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

/// A template's text between `${…}` parts as it reads (escapes resolved), written back with
/// line breaks escaped so it stays on one line.
fn push_template_text(out: &mut String, quasi: &TemplateElement) {
    let Some(cooked) = quasi.value.cooked else {
        out.push_str(quasi.value.raw.as_str()); // an invalid escape, allowed in tagged templates
        return;
    };
    let text = js_str(cooked);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
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
