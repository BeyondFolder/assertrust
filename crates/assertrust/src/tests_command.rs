//! `assertrust tests`: shows the tests and checks assertrust finds in one file, without
//! checking anything. It prints what parsing (steps 2–3) sees, for debugging.
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

use assertrust_git::{Repo, Rev};
use assertrust_ir::{Body, FileIr, ItemKind, ParseError};

use crate::cli::TestsArgs;
use crate::output::{count, line};

pub fn run(args: &TestsArgs) -> Result<(), String> {
    let file = read(args)?;
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
    /// The path, for picking the language.
    path: String,
    /// The path plus where it was read from, e.g. `test/a.test.ts (HEAD~1)`.
    label: String,
    source: String,
}

/// Reads the file from disk, or from git with `--rev` / `--staged`.
fn read(args: &TestsArgs) -> Result<File, String> {
    if args.rev.is_none() && !args.staged {
        let path = args.file.display().to_string();
        let source = fs::read_to_string(&args.file)
            .map_err(|err| format!("couldn't read `{path}`: {err}"))?;
        return Ok(File {
            label: path.clone(),
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
