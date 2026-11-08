//! Values in a check's arguments, compared as structure rather than text.

use std::collections::BTreeMap;
use std::fmt;

/// An argument to a matcher (or an `.each` table), e.g. `{ status: 'pending' }`.
///
/// Formatting doesn't survive: quotes, whitespace and trailing commas are gone, and object
/// keys are sorted, so `{ b: 1, a: 2 }` and `{a:2,b:1}` are equal.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Undefined,
    Bool(bool),
    /// Normalized, so `0x10` and `16` are equal.
    Number(String),
    String(String),
    /// A template literal with `${…}` parts, as normalized source (without backticks).
    Template(String),
    Regex {
        source: String,
        flags: String,
        /// The top-level `|` alternatives, e.g. `["a", "b(c|d)"]` for `/a|b(c|d)/`.
        alternatives: Vec<String>,
    },
    /// Keys sorted. A spread `...rest` is stored under the key `...rest`.
    Object(BTreeMap<String, Value>),
    Array(Vec<Value>),
    /// A matcher used as a value, e.g. `expect.objectContaining({ id: 1 })` is
    /// `{ name: "objectContaining", args: [{ id: 1 }] }`. `expect.not.…` names start with
    /// `not.`.
    Matcher {
        name: String,
        args: Vec<Value>,
    },
    /// Anything else (a variable, a call, …), as normalized source.
    Opaque(String),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Undefined => f.write_str("undefined"),
            Self::Bool(value) => write!(f, "{value}"),
            Self::Number(value) => f.write_str(value),
            Self::String(value) => write_quoted(f, value),
            Self::Template(source) => write!(f, "`{source}`"),
            Self::Regex { source, flags, .. } => write!(f, "/{source}/{flags}"),
            Self::Object(fields) if fields.is_empty() => f.write_str("{}"),
            Self::Object(fields) => {
                f.write_str("{ ")?;
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    let shorthand = matches!(value, Self::Opaque(source) if source == key);
                    if key.starts_with("...") || shorthand {
                        f.write_str(key)?;
                    } else if is_identifier(key) {
                        write!(f, "{key}: {value}")?;
                    } else {
                        write_quoted(f, key)?;
                        write!(f, ": {value}")?;
                    }
                }
                f.write_str(" }")
            }
            Self::Array(items) => {
                f.write_str("[")?;
                write_list(f, items)?;
                f.write_str("]")
            }
            Self::Matcher { name, args } => {
                write!(f, "expect.{name}(")?;
                write_list(f, args)?;
                f.write_str(")")
            }
            Self::Opaque(source) => f.write_str(source),
        }
    }
}

fn write_list(f: &mut fmt::Formatter<'_>, values: &[Value]) -> fmt::Result {
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{value}")?;
    }
    Ok(())
}

/// Writes a JS string literal in single quotes.
fn write_quoted(f: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    f.write_str("'")?;
    for c in value.chars() {
        match c {
            '\'' => f.write_str("\\'")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            c => write!(f, "{c}")?,
        }
    }
    f.write_str("'")
}

fn is_identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_print_with_sorted_keys() {
        let value = Value::Object(BTreeMap::from([
            ("status".to_owned(), Value::String("pending".to_owned())),
            ("domainIds".to_owned(), Value::Null),
            ("content-type".to_owned(), Value::Bool(true)),
            ("...rest".to_owned(), Value::Opaque("rest".to_owned())),
        ]));
        assert_eq!(
            value.to_string(),
            "{ ...rest, 'content-type': true, domainIds: null, status: 'pending' }"
        );
    }

    #[test]
    fn strings_are_escaped() {
        let value = Value::String("it's\na \\ test".to_owned());
        assert_eq!(value.to_string(), r"'it\'s\na \\ test'");
    }

    #[test]
    fn nested_matchers_print_as_written() {
        let value = Value::Matcher {
            name: "arrayContaining".to_owned(),
            args: vec![Value::Array(vec![
                Value::Number("1".to_owned()),
                Value::Number("2".to_owned()),
            ])],
        };
        assert_eq!(value.to_string(), "expect.arrayContaining([1, 2])");
    }
}
