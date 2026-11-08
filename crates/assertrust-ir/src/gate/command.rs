//! Reading a shell command for what it does to test failures: `npm test || true` runs the
//! tests but swallows a failure.

use crate::gate::Command;

/// The test run in a shell script, if it has one, and what the script does to its result.
pub(crate) fn analyze(script: &str) -> Option<Command> {
    let script = script.replace("\\\n", " ");
    let swallowed_by_set_e = script
        .lines()
        .any(|line| line.trim() == "set +e" || line.trim_start().starts_with("set +e "));
    for line in script.lines() {
        let segments = segments(line);
        let Some(at) = segments
            .iter()
            .position(|(tokens, _)| test_run(tokens).is_some())
        else {
            continue;
        };
        let (tokens, after) = &segments[at];
        let start = test_run(tokens).expect("found above");
        let mut command = Command {
            runner: tokens[..start].join(" "),
            ..Command::default()
        };
        flags(&tokens[start..], &mut command);
        command.swallows = swallowed_by_set_e || swallows(*after, &segments[at + 1..]);
        return Some(command);
    }
    None
}

/// How a segment is joined to the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Join {
    And,
    Or,
    Then,
    Pipe,
    End,
}

/// Splits a line into commands and the operators between them: `a && b || c`.
fn segments(line: &str) -> Vec<(Vec<String>, Join)> {
    let mut segments = Vec::new();
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    let flush = |word: &mut String, tokens: &mut Vec<String>| {
        if !word.is_empty() {
            tokens.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, '#') if word.is_empty() => break,
            (None, c) if c.is_whitespace() => flush(&mut word, &mut tokens),
            (None, '&' | '|' | ';') => {
                flush(&mut word, &mut tokens);
                let join = match (c, chars.peek()) {
                    ('&', Some('&')) => {
                        chars.next();
                        Join::And
                    }
                    ('|', Some('|')) => {
                        chars.next();
                        Join::Or
                    }
                    ('|', _) => Join::Pipe,
                    _ => Join::Then,
                };
                segments.push((std::mem::take(&mut tokens), join));
            }
            (None, c) => word.push(c),
        }
    }
    flush(&mut word, &mut tokens);
    if !tokens.is_empty() {
        segments.push((tokens, Join::End));
    }
    segments
}

/// Where a test run's own arguments start, if the command runs tests: after `npm test`,
/// `npx vitest`, `pnpm exec playwright test`, …
fn test_run(tokens: &[String]) -> Option<usize> {
    // Environment variables first: `CI=true npm test`.
    let mut at = tokens
        .iter()
        .take_while(|token| is_assignment(token))
        .count();
    let word = |i: usize| tokens.get(i).map(String::as_str);
    match word(at)? {
        "npm" | "pnpm" | "yarn" | "bun" => {
            let manager = word(at)?;
            at += 1;
            match word(at)? {
                "test" => Some(at + 1),
                "t" if manager == "npm" => Some(at + 1),
                "run" | "run-script" => is_test_script(word(at + 1)?).then_some(at + 2),
                "exec" | "dlx" | "x" => runner(tokens, at + 1),
                // `pnpm test:unit`, `yarn vitest`
                script if manager != "npm" && is_test_script(script) => Some(at + 1),
                _ if manager != "npm" => runner(tokens, at),
                _ => None,
            }
        }
        "npx" | "bunx" => runner(tokens, at + 1),
        _ => runner(tokens, at),
    }
}

/// A test runner called directly: `vitest`, `jest`, `playwright test`, `node --test`.
fn runner(tokens: &[String], at: usize) -> Option<usize> {
    let word = |i: usize| tokens.get(i).map(String::as_str);
    match word(at)? {
        "vitest" | "jest" | "mocha" => Some(at + 1),
        "playwright" if word(at + 1)? == "test" => Some(at + 2),
        "node" if tokens[at + 1..].iter().any(|token| token == "--test") => Some(at + 1),
        _ => None,
    }
}

/// A `package.json` script that runs tests, by its name: `test`, `test:unit`, `e2e`,
/// `unit:test`. Not one that only builds or sets up for them (`test:build`).
pub(crate) fn is_test_script(name: &str) -> bool {
    let tests = name == "test"
        || name.starts_with("test:")
        || name.starts_with("e2e")
        || name.ends_with(":test")
        || name.contains(":test:");
    let support = ["build", "setup", "prepare", "install", "watch"]
        .iter()
        .any(|word| name.contains(word));
    tests && !support
}

fn is_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Flags of a test run that change which tests run or what counts as passing.
fn flags(tokens: &[String], command: &mut Command) {
    // Flags whose value is the next token, unless written `--flag=value`.
    const TAKE_VALUE: &[&str] = &[
        "--reporter",
        "--config",
        "-c",
        "--shard",
        "--workers",
        "-j",
        "--maxWorkers",
        "--root",
        "-r",
        "--dir",
        "--mode",
        "--environment",
        "--pool",
        "--outputFile",
        "--output",
        "--timeout",
        "--testTimeout",
        "-w",
        "--workspace",
        "--filter",
        "--prefix",
        "-C",
    ];
    // Subcommands, not file filters.
    const SUBCOMMANDS: &[&str] = &["run", "watch", "dev", "related", "bench", "list", "--"];
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i].as_str();
        let (flag, inline) = match token.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value.to_owned())),
            _ => (token, None),
        };
        let value = |i: &mut usize| {
            inline.clone().or_else(|| {
                *i += 1;
                tokens.get(*i).cloned()
            })
        };
        match flag {
            "--passWithNoTests" | "--pass-with-no-tests" => command.pass_with_no_tests = true,
            "--retry" | "--retries" => {
                command.retries = value(&mut i).and_then(|n| n.parse().ok());
            }
            "--exclude" | "--testPathIgnorePatterns" | "--grep-invert" | "--testIgnore" => {
                command.excludes.extend(value(&mut i));
            }
            "-t" | "--testNamePattern" | "--grep" | "-g" | "--project" => {
                if let Some(value) = value(&mut i) {
                    command.filters.push(format!("{flag} {value}"));
                }
            }
            "--changed" | "--onlyChanged" | "-o" | "--findRelatedTests" => {
                command.filters.push(flag.to_owned());
            }
            flag if inline.is_none() && TAKE_VALUE.contains(&flag) => i += 1,
            flag if flag.starts_with('-') => {}
            word if SUBCOMMANDS.contains(&word) => {}
            path => command.filters.push(path.to_owned()),
        }
        i += 1;
    }
}

/// Whether what follows the test run lets it fail without failing the script:
/// `|| true`, `|| echo failed`, `; exit 0`.
fn swallows(join: Join, rest: &[(Vec<String>, Join)]) -> bool {
    let first = rest.first().map(|(tokens, _)| tokens.join(" "));
    match (join, first.as_deref()) {
        (Join::Or, Some("exit 1" | "false" | "exit $?" | "exit")) => false,
        (Join::Or, Some(_)) => true,
        (Join::Then, Some("exit 0" | "true" | ":")) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(script: &str) -> Option<String> {
        analyze(script).map(|command| {
            let mut parts = vec![command.runner];
            if command.swallows {
                parts.push("swallows".to_owned());
            }
            if command.pass_with_no_tests {
                parts.push("pass-with-no-tests".to_owned());
            }
            if let Some(n) = command.retries {
                parts.push(format!("retries {n}"));
            }
            if !command.filters.is_empty() {
                parts.push(format!("only {}", command.filters.join(", ")));
            }
            if !command.excludes.is_empty() {
                parts.push(format!("skips {}", command.excludes.join(", ")));
            }
            parts.join("; ")
        })
    }

    #[test]
    fn test_runs_are_found_in_many_spellings() {
        let cases = [
            ("npm test", "npm test"),
            ("npm run test:unit", "npm run test:unit"),
            ("CI=true pnpm test", "CI=true pnpm test"),
            ("yarn test:e2e", "yarn test:e2e"),
            ("npx vitest run", "npx vitest"),
            ("pnpm exec playwright test", "pnpm exec playwright test"),
            ("bun test", "bun test"),
            ("node --test", "node"),
            ("npm ci && npm test", "npm test"),
        ];
        for (script, runner) in cases {
            assert_eq!(read(script).as_deref(), Some(runner), "{script}");
        }
        for script in ["npm ci", "npm run lint", "npm run test:build", "echo test"] {
            assert_eq!(read(script), None, "{script}");
        }
    }

    #[test]
    fn what_the_script_does_to_failures() {
        let cases = [
            ("npm test || true", "npm test; swallows"),
            ("npm test || echo 'tests failed'", "npm test; swallows"),
            ("npm test; exit 0", "npm test; swallows"),
            ("set +e\nnpm test", "npm test; swallows"),
            ("npm test || exit 1", "npm test"),
            ("npm test && npm run lint", "npm test"),
            ("vitest run --passWithNoTests", "vitest; pass-with-no-tests"),
            (
                "npx playwright test --retries=2",
                "npx playwright test; retries 2",
            ),
            ("npx vitest run --retry 3", "npx vitest; retries 3"),
            (
                "vitest run test/unit --reporter dot",
                "vitest; only test/unit",
            ),
            (
                "npx playwright test --project chromium",
                "npx playwright test; only --project chromium",
            ),
            ("npm test -- -t 'refunds'", "npm test; only -t refunds"),
            (
                "vitest run --exclude 'test/api/**'",
                "vitest; skips test/api/**",
            ),
            (
                "npx jest --testPathIgnorePatterns=e2e",
                "npx jest; skips e2e",
            ),
        ];
        for (script, expected) in cases {
            assert_eq!(read(script).as_deref(), Some(expected), "{script}");
        }
    }
}
