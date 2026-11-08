//! `assertrust tests`: shows the tests and checks assertrust finds in one file, without
//! checking anything. It prints what parsing (steps 2–3) sees, for debugging. For a CI or
//! test config file it shows the gate facts instead.
//!
//! ```text
//! test/forwarding.test.ts: 2 tests
//!    3  test forwarding > stores the pending forward
//!    4        equal      expect(res.status).toBe(200)
//!    5        subset     expect(row).toMatchObject({ domainIds: null, status: 'pending' })
//!    8  test forwarding > retries  [skip]
//! ```

use std::fmt::Write as _;
use std::fs;

use assertrust_git::{Classifier, FileKind, Repo, Rev};
use assertrust_ir::{Body, Command, FileIr, Gate, ItemKind, ParseError};

use crate::cli::TestsArgs;
use crate::output::{count, line};

pub fn run(args: &TestsArgs) -> Result<(), String> {
    let file = read(args)?;
    if Classifier::default().classify(&file.path) == FileKind::Gate {
        let output = match assertrust_ir::parse_gate(&file.path, &file.source) {
            Ok(gate) => render_gate(&file.label, &gate),
            Err(err) => format!("{}: skipped, {err}\n", file.label),
        };
        return crate::print_stdout(&output);
    }
    let output = match assertrust_ir::parse_file(&file.path, &file.source) {
        Ok(ir) => render(&file.label, &ir),
        Err(ParseError::UnsupportedFile) => {
            return Err(format!("`{}` isn't a JS or TS file", file.path));
        }
        Err(err @ ParseError::Syntax { .. }) => format!("{}: skipped, {err}\n", file.label),
    };
    crate::print_stdout(&output)
}

struct File {
    /// The path in the repository when it's in one, for picking the language and telling a
    /// CI or test config file (step 1's Gate kind) from a test file.
    path: String,
    /// The path plus where it was read from, e.g. `test/a.test.ts (HEAD~1)`.
    label: String,
    source: String,
}

/// Reads the file from disk, or from git with `--rev` / `--staged`.
fn read(args: &TestsArgs) -> Result<File, String> {
    if args.rev.is_none() && !args.staged {
        let label = args.file.display().to_string();
        let source = fs::read_to_string(&args.file)
            .map_err(|err| format!("couldn't read `{label}`: {err}"))?;
        let path = std::env::current_dir()
            .ok()
            .and_then(|dir| Repo::open(&dir).ok())
            .and_then(|repo| repo.path_in_repo(&args.file).ok())
            .unwrap_or_else(|| label.trim_start_matches("./").to_owned());
        return Ok(File {
            label,
            path,
            source,
        });
    }

    let dir = std::env::current_dir().map_err(|err| format!("can't read current dir: {err}"))?;
    let repo = Repo::open(&dir).map_err(|err| err.to_string())?;
    let path = repo
        .path_in_repo(&args.file)
        .map_err(|err| err.to_string())?;
    let (rev, at) = match &args.rev {
        Some(rev) => (
            Rev::Commit(repo.resolve(rev).map_err(|err| err.to_string())?),
            rev.as_str(),
        ),
        None => (Rev::Index, "staged"),
    };
    let source = repo
        .read_file(&rev, &path)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| match &args.rev {
            Some(rev) => format!("`{path}` doesn't exist at {rev}"),
            None => format!("`{path}` isn't staged"),
        })?;
    Ok(File {
        label: format!("{path} ({at})"),
        path,
        source,
    })
}

fn render(label: &str, ir: &FileIr) -> String {
    let mut out = String::new();
    let mut summary = vec![count(ir.tests.len(), "test")];
    if !ir.hooks.is_empty() {
        summary.push(count(ir.hooks.len(), "hook"));
    }
    if !ir.custom_matchers.is_empty() {
        summary.push(format!(
            "custom matchers: {}",
            ir.custom_matchers.join(", ")
        ));
    }
    line(&mut out, &format!("{label}: {}", summary.join(", ")));

    if !ir.outside.items.is_empty() {
        line(&mut out, "      outside tests");
        body(&mut out, &ir.outside);
    }

    // Tests and hooks in source order.
    let mut blocks: Vec<(u32, String, &Body)> = Vec::new();
    for test in &ir.tests {
        let mut heading = format!("test {}", test.id());
        if !test.modifiers.is_empty() {
            let modifiers: Vec<String> = test.modifiers.iter().map(ToString::to_string).collect();
            write!(heading, "  [{}]", modifiers.join(", ")).expect("writing to a String");
        }
        blocks.push((test.line, heading, &test.body));
    }
    for hook in &ir.hooks {
        let mut heading = format!("hook {}", hook.kind.as_str());
        if !hook.suites.is_empty() {
            write!(heading, "  ({})", hook.suites.join(" > ")).expect("writing to a String");
        }
        blocks.push((hook.line, heading, &hook.body));
    }
    blocks.sort_by_key(|(line, _, _)| *line);
    for (number, heading, block) in blocks {
        line(&mut out, &format!("{number:>4}  {heading}"));
        body(&mut out, block);
    }
    out
}

fn body(out: &mut String, body: &Body) {
    for item in &body.items {
        let (label, text) = match &item.kind {
            ItemKind::Assertion(assertion) => (
                assertion
                    .matcher
                    .as_ref()
                    .map_or("no-matcher", |matcher| matcher.kind.as_str()),
                assertion.to_string(),
            ),
            ItemKind::AssertionCount(Some(n)) => ("count", format!("expect.assertions({n})")),
            ItemKind::AssertionCount(None) => ("count", "expect.hasAssertions()".to_owned()),
            ItemKind::EarlyReturn => ("exit", "early return".to_owned()),
            ItemKind::RuntimeSkip(condition) => (
                "skip",
                format!("skip({})", condition.as_deref().unwrap_or_default()),
            ),
        };
        let mut text = format!("{:>4}        {label:<10} {text}", item.line);
        if !item.context.is_plain() {
            write!(text, "  ({})", item.context).expect("writing to a String");
        }
        line(out, &text);
    }
}

/// ```text
/// .github/workflows/ci.yml: 1 job runs tests
///   28  job test
///   48    npm test
///   51    npm test -- --retry 2  (retries 2)
/// ```
fn render_gate(label: &str, gate: &Gate) -> String {
    let mut out = String::new();
    match gate {
        Gate::Workflow(workflow) => {
            let jobs: Vec<_> = workflow
                .jobs
                .iter()
                .filter(|job| !job.steps.is_empty())
                .collect();
            let run = if jobs.len() == 1 { "runs" } else { "run" };
            line(
                &mut out,
                &format!("{label}: {} {run} tests", count(jobs.len(), "job")),
            );
            for job in jobs {
                let mut text = format!("{:>4}  job {}", job.line, job.id);
                attributes(
                    &mut text,
                    job.condition.as_deref(),
                    job.continue_on_error.as_deref(),
                );
                line(&mut out, &text);
                for step in &job.steps {
                    let first = step.run.lines().next().unwrap_or_default();
                    let mut text = format!("{:>4}    {first}", step.line);
                    attributes(
                        &mut text,
                        step.condition.as_deref(),
                        step.continue_on_error.as_deref(),
                    );
                    notes(&mut text, &step.command);
                    line(&mut out, &text);
                }
            }
        }
        Gate::Config(settings) => {
            line(
                &mut out,
                &format!("{label}: {}", count(settings.len(), "setting")),
            );
            for setting in settings {
                line(&mut out, &format!("{:>4}  {setting}", setting.line));
            }
        }
        Gate::Package(package) => {
            line(
                &mut out,
                &format!("{label}: {}", count(package.scripts.len(), "test script")),
            );
            for script in &package.scripts {
                let mut text = format!("{:>4}  {}: {}", script.line, script.name, script.run);
                if let Some(command) = &script.command {
                    notes(&mut text, command);
                }
                line(&mut out, &text);
            }
            for setting in &package.jest {
                line(&mut out, &format!("{:>4}  {setting}", setting.line));
            }
        }
    }
    out
}

fn attributes(text: &mut String, condition: Option<&str>, continue_on_error: Option<&str>) {
    if let Some(condition) = condition {
        write!(text, "  [if: {condition}]").expect("writing to a String");
    }
    if let Some(value) = continue_on_error {
        write!(text, "  [continue-on-error: {value}]").expect("writing to a String");
    }
}

/// What a test command does to failures: `(swallows; retries 2)`.
fn notes(text: &mut String, command: &Command) {
    let mut notes = Vec::new();
    if command.swallows {
        notes.push("swallows failures".to_owned());
    }
    if command.pass_with_no_tests {
        notes.push("passes with no tests".to_owned());
    }
    if let Some(n) = command.retries {
        notes.push(format!("retries {n}"));
    }
    if !command.filters.is_empty() {
        notes.push(format!("only {}", command.filters.join(", ")));
    }
    if !command.excludes.is_empty() {
        notes.push(format!("skips {}", command.excludes.join(", ")));
    }
    if !notes.is_empty() {
        write!(text, "  ({})", notes.join("; ")).expect("writing to a String");
    }
}
