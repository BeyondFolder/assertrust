//! Gate facts (`docs/architecture.md`, step 3, "Gate facts"): what a CI workflow, a test
//! config or `package.json` says about which test failures get through.

mod command;
mod workflow;

use std::collections::BTreeMap;
use std::fmt;

use crate::ParseError;
use crate::value::Value;

pub(crate) use self::command::{Shell, is_test_script};

/// One CI or test config file, as the facts that decide whether a failing test fails the
/// build.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    /// A GitHub Actions workflow.
    Workflow(Workflow),
    /// A vitest, jest or Playwright config: its literal settings.
    Config(Vec<Setting>),
    /// `package.json`: its test scripts, and its `jest` settings if any.
    Package(Package),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Workflow {
    /// Every job, in order. Most only care about the ones with test steps.
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub line: u32,
    /// `if:`, as written.
    pub condition: Option<String>,
    /// `continue-on-error:`, as written: `true`, `${{ matrix.experimental }}`.
    pub continue_on_error: Option<String>,
    /// The steps that run tests, in order.
    pub steps: Vec<TestStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestStep {
    pub name: Option<String>,
    /// The `run:` script, trimmed.
    pub run: String,
    pub line: u32,
    pub condition: Option<String>,
    pub continue_on_error: Option<String>,
    pub command: Command,
}

/// A shell command that runs tests, and what it does to their result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Command {
    /// How the tests are run: `npm test`, `npx vitest`, `pnpm exec playwright test`.
    pub runner: String,
    /// A failure doesn't fail the command: `|| true`, `|| echo …`, `; exit 0`, `set +e`.
    pub swallows: bool,
    /// `--passWithNoTests`.
    pub pass_with_no_tests: bool,
    /// `--retry=2`, `--retries 2`.
    pub retries: Option<u32>,
    /// What narrows which tests run: file filters, `-t name`, `--grep x`, `--project p`.
    pub filters: Vec<String>,
    /// What skips tests: `--exclude x`, `--testPathIgnorePatterns x`, `--grep-invert x`.
    pub excludes: Vec<String>,
}

/// A literal config setting, by its path: `test.coverage.thresholds.lines = 80`. An array
/// of objects with a name is keyed by it: `projects[chromium].retries`.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub path: String,
    pub value: Value,
    pub line: u32,
}

impl fmt::Display for Setting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = {}", self.path, self.value)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Package {
    /// The scripts that run tests (`test`, `test:unit`, `e2e`), in file order.
    pub scripts: Vec<Script>,
    /// The `jest` key's settings, prefixed `jest.`.
    pub jest: Vec<Setting>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub name: String,
    /// The script as written.
    pub run: String,
    pub line: u32,
    /// What it does, when it calls a test runner itself.
    pub command: Option<Command>,
    /// It only prints or passes: `echo skip`, `exit 0`, `true`.
    pub noop: bool,
}

/// Reads one gate file. `path` picks how: a workflow (`.yml` / `.yaml`), `package.json`,
/// a JSON config, or a JS / TS config.
pub(crate) fn parse(path: &str, source: &str) -> Result<Gate, ParseError> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".yml") || name.ends_with(".yaml") {
        workflow::parse(source).map(Gate::Workflow)
    } else if name == "package.json" {
        package(source).map(Gate::Package)
    } else if name.ends_with(".json") {
        let json = json(source)?;
        let mut settings = Vec::new();
        flatten_json("", &json, source, &mut settings);
        Ok(Gate::Config(settings))
    } else {
        crate::js::config(path, source).map(Gate::Config)
    }
}

fn json(source: &str) -> Result<serde_json::Value, ParseError> {
    serde_json::from_str(source).map_err(|err| ParseError::Syntax {
        line: u32::try_from(err.line()).unwrap_or(1),
        message: err.to_string(),
    })
}

fn package(source: &str) -> Result<Package, ParseError> {
    let json = json(source)?;
    let mut package = Package::default();
    if let Some(scripts) = json.get("scripts").and_then(serde_json::Value::as_object) {
        for (name, run) in scripts {
            let Some(run) = run.as_str() else { continue };
            if !is_test_script(name) {
                continue;
            }
            package.scripts.push(Script {
                name: name.clone(),
                run: run.to_owned(),
                line: line_of(source, &format!("\"{name}\"")),
                command: command::analyze(run, Shell::KeepGoing),
                noop: command::does_nothing(run),
            });
        }
    }
    if let Some(jest) = json.get("jest") {
        flatten_json("jest", jest, source, &mut package.jest);
    }
    Ok(package)
}

/// A JSON config as settings, the way `js::config` reads a JS one.
fn flatten_json(prefix: &str, json: &serde_json::Value, source: &str, out: &mut Vec<Setting>) {
    let join = |key: &str| {
        if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        }
    };
    match json {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if value.is_object() {
                    flatten_json(&join(key), value, source, out);
                } else {
                    out.push(Setting {
                        path: join(key),
                        value: json_value(value),
                        line: line_of(source, &format!("\"{key}\"")),
                    });
                }
            }
        }
        other => out.push(Setting {
            path: prefix.to_owned(),
            value: json_value(other),
            line: 1,
        }),
    }
}

fn json_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Bool(*value),
        serde_json::Value::Number(n) => Value::Number(n.to_string()),
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => Value::Array(items.iter().map(json_value).collect()),
        serde_json::Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), json_value(value)))
                .collect::<BTreeMap<_, _>>(),
        ),
    }
}

/// The first line from `from` (1-based) on where `key` starts a YAML mapping entry:
/// `test:` but not `run: npm run test:unit` or `test:unit:`.
fn key_line(source: &str, key: &str, from: u32) -> u32 {
    source
        .lines()
        .enumerate()
        .skip(from.saturating_sub(1) as usize)
        .find(|(_, line)| {
            let line = line.trim_start();
            [
                format!("{key}:"),
                format!("\"{key}\":"),
                format!("'{key}':"),
            ]
            .iter()
            .any(|entry| {
                line.strip_prefix(entry.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', '#']))
            })
        })
        .map_or(from.max(1), |(i, _)| {
            u32::try_from(i + 1).unwrap_or(u32::MAX)
        })
}

/// The first line containing `text`, for reporting. 1 if there's none.
fn line_of(source: &str, text: &str) -> u32 {
    line_after(source, text, 0)
}

/// The first line from `from` (1-based, inclusive) on that contains `text`.
fn line_after(source: &str, text: &str, from: u32) -> u32 {
    source
        .lines()
        .enumerate()
        .skip(from.saturating_sub(1) as usize)
        .find(|(_, line)| line.contains(text))
        .map_or(from.max(1), |(i, _)| {
            u32::try_from(i + 1).unwrap_or(u32::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_json_test_scripts_and_jest_settings() {
        let source = r#"{
  "scripts": {
    "build": "nuxt build",
    "test": "vitest run",
    "test:e2e": "playwright test --retries 1",
    "test:build": "nuxt build --envName test"
  },
  "jest": { "coverageThreshold": { "global": { "lines": 80 } }, "testPathIgnorePatterns": ["/e2e/"] }
}"#;
        let Gate::Package(package) = parse("package.json", source).expect("parses") else {
            panic!("not a package");
        };
        let scripts: Vec<(String, u32, Option<u32>)> = package
            .scripts
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.line,
                    s.command.as_ref().and_then(|c| c.retries),
                )
            })
            .collect();
        assert_eq!(
            scripts,
            [
                ("test".to_owned(), 4, None),
                ("test:e2e".to_owned(), 5, Some(1))
            ]
        );
        let jest: Vec<String> = package.jest.iter().map(ToString::to_string).collect();
        assert_eq!(
            jest,
            [
                "jest.coverageThreshold.global.lines = 80",
                "jest.testPathIgnorePatterns = ['/e2e/']"
            ]
        );
    }
}
