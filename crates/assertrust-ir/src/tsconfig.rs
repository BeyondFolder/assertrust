//! The parts of a `tsconfig.json` that say where imports point: `extends`, and
//! `compilerOptions.baseUrl` and `paths` (`docs/architecture.md`, step 2, "Helper
//! functions").
//!
//! ```text
//! { "compilerOptions": { "paths": { "@/*": ["./src/*"] } } }
//! → import { expectOk } from '@/test/helpers'  is  ./src/test/helpers
//! ```

use serde_json::Value as Json;

/// What a tsconfig file says about imports, as written. Paths in it are relative to its
/// folder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TsConfig {
    /// The configs it builds on (`extends`), in order: one or several.
    pub extends: Vec<String>,
    /// `compilerOptions.baseUrl`: where imports that aren't relative are looked up.
    pub base_url: Option<String>,
    /// `compilerOptions.paths`: each pattern (`@/*`) with the paths it stands for
    /// (`./src/*`). `None` when the file doesn't set it, so it comes from `extends`.
    pub paths: Option<Vec<(String, Vec<String>)>>,
}

/// Reads a tsconfig file, which may have comments and trailing commas. `None` when it
/// isn't JSON even then.
///
/// ```
/// let config = assertrust_ir::parse_tsconfig(
///     r#"{
///       // Aliases for tests.
///       "extends": "./tsconfig.base.json",
///       "compilerOptions": { "paths": { "@/*": ["./src/*"], }, },
///     }"#,
/// ).unwrap();
/// assert_eq!(config.extends, ["./tsconfig.base.json"]);
/// assert_eq!(config.paths, Some(vec![("@/*".to_owned(), vec!["./src/*".to_owned()])]));
/// ```
pub fn parse_tsconfig(source: &str) -> Option<TsConfig> {
    let json: Json = serde_json::from_str(&plain_json(source)).ok()?;
    let extends = match json.get("extends") {
        Some(Json::String(one)) => vec![one.clone()],
        Some(Json::Array(several)) => several
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    let options = json.get("compilerOptions");
    let base_url = options
        .and_then(|options| options.get("baseUrl"))
        .and_then(Json::as_str)
        .map(str::to_owned);
    let paths = options
        .and_then(|options| options.get("paths"))
        .and_then(Json::as_object)
        .map(|paths| {
            paths
                .iter()
                .map(|(pattern, targets)| {
                    let targets = targets
                        .as_array()
                        .map(|targets| {
                            targets
                                .iter()
                                .filter_map(|target| target.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    (pattern.clone(), targets)
                })
                .collect()
        });
    Some(TsConfig {
        extends,
        base_url,
        paths,
    })
}

/// JSON with comments and trailing commas → plain JSON. Strings are left alone.
fn plain_json(source: &str) -> String {
    // Comments out first, then trailing commas, so a comment between a comma and `}` is
    // already gone.
    let mut without_comments = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            without_comments.push(c);
            match c {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        without_comments.push(escaped);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                without_comments.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        without_comments.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
                without_comments.push(' ');
            }
            _ => without_comments.push(c),
        }
    }

    let chars: Vec<char> = without_comments.chars().collect();
    let mut plain = String::with_capacity(chars.len());
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            plain.push(c);
            if c == '\\' {
                if let Some(&escaped) = chars.get(i + 1) {
                    plain.push(escaped);
                }
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            plain.push(c);
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                plain.push(c);
            }
        } else {
            plain.push(c);
        }
        i += 1;
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_extends_base_url_and_paths() {
        let config = parse_tsconfig(
            r##"{
              /* Several bases. */
              "extends": ["./a.json", "@tsconfig/node20/tsconfig.json"],
              "compilerOptions": {
                "baseUrl": ".", // the repo
                "paths": { "~/*": ["./app/*", "./lib/*"], "#shared": ["./shared/index.ts"] }
              }
            }"##,
        )
        .unwrap();
        assert_eq!(
            config.extends,
            ["./a.json", "@tsconfig/node20/tsconfig.json"]
        );
        assert_eq!(config.base_url.as_deref(), Some("."));
        assert_eq!(
            config.paths.unwrap(),
            [
                ("#shared".to_owned(), vec!["./shared/index.ts".to_owned()]),
                (
                    "~/*".to_owned(),
                    vec!["./app/*".to_owned(), "./lib/*".to_owned()]
                ),
            ]
        );
    }

    #[test]
    fn leaves_comment_like_text_in_strings_alone() {
        let config =
            parse_tsconfig(r#"{ "compilerOptions": { "baseUrl": "./src//*x*/,", } }"#).unwrap();
        assert_eq!(config.base_url.as_deref(), Some("./src//*x*/,"));
        assert_eq!(config.paths, None);
    }

    #[test]
    fn not_json_is_none() {
        assert_eq!(parse_tsconfig("{ compilerOptions: "), None);
    }
}
