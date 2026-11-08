//! vitest, jest and Playwright config files: their literal settings, for the gate facts
//! (`docs/architecture.md`, step 3).
//!
//! ```text
//! export default defineConfig({ test: { retry: 2, coverage: { thresholds: { lines: 80 } } } })
//! → test.retry = 2, test.coverage.thresholds.lines = 80
//! ```

use oxc::allocator::Allocator;
use oxc::ast::ast::{
    ArrayExpression, ArrayExpressionElement, AssignmentTarget, BindingPattern, Expression,
    ObjectExpression, ObjectPropertyKind, Program, Statement,
};
use oxc::semantic::SemanticBuilder;

use super::consts::Consts;
use super::values::{Values, js_str};
use super::{Lines, program};
use crate::ParseError;
use crate::gate::Setting;
use crate::value::Value;

pub(crate) fn config(path: &str, source: &str) -> Result<Vec<Setting>, ParseError> {
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
    let mut settings = Vec::new();
    let reader = Reader {
        values: &values,
        consts: &consts,
        lines: &lines,
    };
    match exported(&program) {
        Some(Exported::Object(object)) => reader.object("", object, &mut settings),
        // A vitest workspace: `defineWorkspace(['packages/*', { test: { name: 'api' } }])`.
        Some(Exported::Array(array)) => {
            if let Some(entries) = entries(array, true) {
                reader.entries("projects", entries, &mut settings);
            }
        }
        None => {}
    }
    Ok(settings)
}

/// What a config file exports: its config object, or a vitest workspace's list of projects.
enum Exported<'b, 'a> {
    Object(&'b ObjectExpression<'a>),
    Array(&'b ArrayExpression<'a>),
}

/// The config: what `export default` or `module.exports =` holds, through
/// `defineConfig(…)`, `mergeConfig(…, …)`, a function returning it, or a `const`.
fn exported<'b, 'a>(program: &'b Program<'a>) -> Option<Exported<'b, 'a>> {
    program.body.iter().find_map(|statement| {
        let expr = match statement {
            Statement::ExportDefaultDeclaration(export) => export.declaration.as_expression()?,
            Statement::ExpressionStatement(statement) => match &statement.expression {
                Expression::AssignmentExpression(assign) if is_module_exports(&assign.left) => {
                    &assign.right
                }
                _ => return None,
            },
            _ => return None,
        };
        config_of(expr, program, 0)
    })
}

fn is_module_exports(target: &AssignmentTarget) -> bool {
    matches!(
        target,
        AssignmentTarget::StaticMemberExpression(member)
            if member.property.name == "exports"
                && matches!(&member.object, Expression::Identifier(id) if id.name == "module")
    )
}

fn config_of<'b, 'a>(
    expr: &'b Expression<'a>,
    program: &'b Program<'a>,
    depth: u32,
) -> Option<Exported<'b, 'a>> {
    if depth > 8 {
        return None;
    }
    match expr.get_inner_expression() {
        Expression::ObjectExpression(object) => Some(Exported::Object(object)),
        Expression::ArrayExpression(array) => Some(Exported::Array(array)),
        // `defineConfig({…})`, `mergeConfig(base, defineConfig({…}))`: the last config wins.
        Expression::CallExpression(call) => call
            .arguments
            .iter()
            .rev()
            .filter_map(|arg| arg.as_expression())
            .find_map(|arg| config_of(arg, program, depth + 1)),
        Expression::ArrowFunctionExpression(arrow) => match arrow.body.as_expression() {
            Some(body) => config_of(body, program, depth + 1),
            None => returned(arrow.body.as_function_body()?.statements.as_slice())
                .and_then(|expr| config_of(expr, program, depth + 1)),
        },
        Expression::FunctionExpression(function) => {
            returned(function.body.as_ref()?.statements.as_slice())
                .and_then(|expr| config_of(expr, program, depth + 1))
        }
        // `const config = {…}; export default config;`
        Expression::Identifier(id) => program.body.iter().find_map(|statement| {
            let Statement::VariableDeclaration(declaration) = statement else {
                return None;
            };
            declaration.declarations.iter().find_map(|declarator| {
                let BindingPattern::BindingIdentifier(binding) = &declarator.id else {
                    return None;
                };
                if binding.name != id.name {
                    return None;
                }
                config_of(declarator.init.as_ref()?, program, depth + 1)
            })
        }),
        _ => None,
    }
}

/// What a function body's first `return` returns.
fn returned<'b, 'a>(statements: &'b [Statement<'a>]) -> Option<&'b Expression<'a>> {
    statements.iter().find_map(|statement| match statement {
        Statement::ReturnStatement(ret) => ret.argument.as_ref(),
        _ => None,
    })
}

struct Reader<'r, 's> {
    values: &'r Values<'s>,
    consts: &'r Consts,
    lines: &'r Lines,
}

impl Reader<'_, '_> {
    /// Each property of `object` as settings under `prefix`, going into nested objects.
    fn object(&self, prefix: &str, object: &ObjectExpression, out: &mut Vec<Setting>) {
        for property in &object.properties {
            let ObjectPropertyKind::ObjectProperty(property) = property else {
                continue;
            };
            if property.method {
                continue;
            }
            let key = self.values.key(&property.key, property.computed);
            // vitest's `projects` (or older `workspace`) may also list folders and globs.
            let projects = key == "projects" || key == "workspace";
            let path = if prefix.is_empty() {
                key
            } else {
                format!("{prefix}.{key}")
            };
            let value = property.value.get_inner_expression();
            if let Expression::ObjectExpression(inner) = value {
                self.object(&path, inner, out);
            } else if let Expression::ArrayExpression(array) = value
                && let Some(entries) = entries(array, projects)
            {
                self.entries(&path, entries, out);
            } else {
                out.push(Setting {
                    path,
                    value: self.values.value(value, self.consts),
                    line: self.lines.line(property.span.start),
                });
            }
        }
    }

    /// Each entry of a list of projects under `path[name]`.
    fn entries(&self, path: &str, entries: Vec<(String, Entry)>, out: &mut Vec<Setting>) {
        for (name, entry) in entries {
            let path = format!("{path}[{name}]");
            match entry {
                Entry::Object(object) => self.object(&path, object, out),
                Entry::Path(at) => out.push(Setting {
                    path,
                    value: Value::String(name),
                    line: self.lines.line(at),
                }),
            }
        }
    }
}

/// One entry of a list of projects.
enum Entry<'b, 'a> {
    /// An inline project, read like a config.
    Object(&'b ObjectExpression<'a>),
    /// A project given by its folder, config file or glob (`'packages/*'`), at this offset.
    Path(u32),
}

/// A list of named objects, like Playwright's or vitest's `projects`, by each object's
/// name: `name`, or vitest's `test.name`. Unnamed ones go by position. With `paths`, a
/// string entry (`'packages/*'`) counts too, named by itself. `None` for any other array.
fn entries<'b, 'a>(
    array: &'b ArrayExpression<'a>,
    paths: bool,
) -> Option<Vec<(String, Entry<'b, 'a>)>> {
    let mut entries = Vec::new();
    for (i, element) in array.elements.iter().enumerate() {
        let entry = match element {
            ArrayExpressionElement::ObjectExpression(object) => {
                let name = name_of(object)
                    .or_else(|| match property(object, "test")? {
                        Expression::ObjectExpression(test) => name_of(test),
                        _ => None,
                    })
                    .unwrap_or_else(|| i.to_string());
                (name, Entry::Object(object))
            }
            ArrayExpressionElement::StringLiteral(path) if paths => {
                (js_str(path.value), Entry::Path(path.span.start))
            }
            _ => return None,
        };
        entries.push(entry);
    }
    (!entries.is_empty()).then_some(entries)
}

fn name_of(object: &ObjectExpression) -> Option<String> {
    match property(object, "name")? {
        Expression::StringLiteral(name) => Some(js_str(name.value)),
        _ => None,
    }
}

fn property<'b, 'a>(object: &'b ObjectExpression<'a>, key: &str) -> Option<&'b Expression<'a>> {
    object
        .properties
        .iter()
        .find_map(|property| match property {
            ObjectPropertyKind::ObjectProperty(property)
                if !property.computed
                    && property
                        .key
                        .static_name()
                        .is_some_and(|name| name.as_str() == Some(key)) =>
            {
                Some(property.value.get_inner_expression())
            }
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(path: &str, source: &str) -> Vec<String> {
        config(path, source)
            .expect("parses")
            .iter()
            .map(|setting| format!("{} {setting}", setting.line))
            .collect()
    }

    #[test]
    fn a_vitest_config_with_projects() {
        let source = "
import { defineConfig } from 'vitest/config';
export default defineConfig({
  test: {
    retry: 2,
    coverage: { thresholds: { lines: 80 } },
    projects: [
      { test: { name: 'unit', include: ['test/unit/**/*.test.ts'] } },
      { test: { name: 'api', include: ['test/api/**/*.test.ts'], isolate: false } },
    ],
  },
});
";
        assert_eq!(
            settings("vitest.config.ts", source),
            [
                "5 test.retry = 2",
                "6 test.coverage.thresholds.lines = 80",
                "8 test.projects[unit].test.name = 'unit'",
                "8 test.projects[unit].test.include = ['test/unit/**/*.test.ts']",
                "9 test.projects[api].test.name = 'api'",
                "9 test.projects[api].test.include = ['test/api/**/*.test.ts']",
                "9 test.projects[api].test.isolate = false",
            ]
        );
    }

    #[test]
    fn the_config_object_however_it_is_exported() {
        let cases = [
            (
                "playwright.config.ts",
                "export default defineConfig({ retries: process.env.CI ? 2 : 0 });",
            ),
            ("jest.config.js", "module.exports = { retries: 1 };"),
            (
                "jest.config.ts",
                "const config: Config = { retries: 1 };\nexport default config;",
            ),
            (
                "vitest.config.ts",
                "export default mergeConfig(viteConfig, defineConfig({ retries: 1 }));",
            ),
            (
                "vitest.config.ts",
                "export default defineConfig(({ mode }) => ({ retries: 1 }));",
            ),
            (
                "vitest.config.mts",
                "export default defineConfig(() => { return { retries: 1 }; });",
            ),
        ];
        for (path, source) in cases {
            let found = settings(path, source);
            assert_eq!(found.len(), 1, "{source}: {found:?}");
            assert!(found[0].contains("retries = "), "{source}: {found:?}");
        }
    }
}
