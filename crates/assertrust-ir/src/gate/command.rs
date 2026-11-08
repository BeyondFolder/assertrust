//! Reading a shell command for what it does to test failures: `npm test || true` runs the
//! tests but swallows a failure.
//!
//! How the script is run matters. A workflow step runs under `bash -e`, which stops at the
//! first command that fails, so `npm test; exit 0` still fails the step. npm runs a
//! `package.json` script with `sh -c`, which goes on, so the same script passes there.

use crate::gate::Command;

/// How a script is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shell {
    /// `bash -e`, GitHub Actions' default: a failing command ends the script.
    StopOnError,
    /// `sh -c`, how npm runs scripts: the script goes on, and its last command decides.
    KeepGoing,
}

/// The test run in a shell script, if it has one, and what the script does to its result.
pub(crate) fn analyze(script: &str, shell: Shell) -> Option<Command> {
    let script = script.replace("\\\n", " ");
    let lines: Vec<Vec<Segment>> = script.lines().map(segments).collect();
    let mut errexit = shell == Shell::StopOnError;
    for (l, line) in lines.iter().enumerate() {
        for (at, segment) in line.iter().enumerate() {
            if let Some(on) = set_errexit(&segment.tokens) {
                errexit = on;
                continue;
            }
            let Some(start) = test_run(&segment.tokens) else {
                continue;
            };
            let tokens = &segment.tokens;
            let mut command = Command {
                runner: tokens[..start].join(" "),
                ..Command::default()
            };
            flags(&tokens[start..], &mut command);
            let failure = failure(&lines, l, at, errexit);
            command.swallows = failure.passes;
            if failure.retries > 0 {
                command.retries = Some(command.retries.unwrap_or(0) + failure.retries);
            }
            return Some(command);
        }
    }
    None
}

/// A script that only prints or passes: `echo skip`, `exit 0`, `true`, or nothing.
pub(crate) fn does_nothing(script: &str) -> bool {
    script.lines().flat_map(segments).all(|segment| {
        let words: Vec<&str> = segment.tokens.iter().map(String::as_str).collect();
        matches!(
            words.as_slice(),
            ["echo" | "printf" | "true" | ":", ..] | ["exit", "0"]
        )
    })
}

/// One command of a line, with how it's joined to the next one.
#[derive(Debug, Default)]
struct Segment {
    tokens: Vec<String>,
    /// The commands of a `{ …; }` or `( … )` group, when the segment is one.
    group: Vec<Segment>,
    join: Join,
}

impl Segment {
    fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.group.is_empty()
    }
}

/// How a segment is joined to the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Join {
    And,
    Or,
    Then,
    Pipe,
    #[default]
    End,
}

/// Splits a line into commands and the operators between them: `a && b || c`. Redirects
/// (`2>&1`, `> out.log`) are dropped, and a `{ …; }` or `( … )` group is one command.
fn segments(line: &str) -> Vec<Segment> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut current = Segment::default();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    // The next word is where a redirect goes, not an argument.
    let mut target = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, '#') if word.is_empty() => break,
            (None, '(' | '{')
                if word.is_empty()
                    && current.is_empty()
                    && (c == '(' || chars.get(i).is_none_or(|next| next.is_whitespace())) =>
            {
                let close = closing(&chars, i, c);
                current.group = segments(&chars[i..close].iter().collect::<String>());
                i = close + 1;
            }
            (None, c) if c.is_whitespace() => end_word(&mut word, &mut current.tokens, &mut target),
            (None, '>' | '<') => {
                // `2>`: the descriptor before it is part of the redirect.
                if word.chars().all(|c| c.is_ascii_digit()) {
                    word.clear();
                }
                end_word(&mut word, &mut current.tokens, &mut target);
                if chars.get(i) == Some(&c) {
                    i += 1;
                }
                if chars.get(i) == Some(&'&') {
                    // `>&2`, `2>&1`: the target is a descriptor, written right here.
                    i += 1;
                    while chars
                        .get(i)
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        i += 1;
                    }
                } else {
                    target = true;
                }
            }
            // `&> out.log`
            (None, '&') if chars.get(i) == Some(&'>') => {
                end_word(&mut word, &mut current.tokens, &mut target);
                i += 1;
                if chars.get(i) == Some(&'>') {
                    i += 1;
                }
                target = true;
            }
            (None, '&' | '|' | ';') => {
                end_word(&mut word, &mut current.tokens, &mut target);
                current.join = match (c, chars.get(i)) {
                    ('&', Some('&')) => {
                        i += 1;
                        Join::And
                    }
                    ('|', Some('|')) => {
                        i += 1;
                        Join::Or
                    }
                    ('|', _) => Join::Pipe,
                    _ => Join::Then,
                };
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                current = Segment::default();
            }
            (None, c) => word.push(c),
        }
    }
    end_word(&mut word, &mut current.tokens, &mut target);
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn end_word(word: &mut String, tokens: &mut Vec<String>, target: &mut bool) {
    if word.is_empty() {
        return;
    }
    let word = std::mem::take(word);
    if !std::mem::take(target) {
        tokens.push(word);
    }
}

/// Where the group opened just before `from` closes: its matching `)` or `}`, or the end
/// of the line.
fn closing(chars: &[char], from: usize, open: char) -> usize {
    let close = if open == '(' { ')' } else { '}' };
    let mut depth = 1;
    let mut quote = None;
    for (i, &c) in chars.iter().enumerate().skip(from) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c == open => depth += 1,
            (None, c) if c == close => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    chars.len()
}

/// `set -e` / `set -o errexit` turn stopping on errors on, `set +e` off. `None` when the
/// command doesn't change it.
fn set_errexit(tokens: &[String]) -> Option<bool> {
    if tokens.first().map(String::as_str) != Some("set") {
        return None;
    }
    let mut errexit = None;
    for (i, token) in tokens.iter().enumerate().skip(1) {
        let (on, flags) = match token.split_at_checked(1) {
            Some(("-", flags)) => (true, flags),
            Some(("+", flags)) => (false, flags),
            _ => continue,
        };
        if flags == "o" {
            if tokens.get(i + 1).is_some_and(|next| next == "errexit") {
                errexit = Some(on);
            }
        } else if !flags.starts_with('-') && flags.contains('e') {
            errexit = Some(on);
        }
    }
    errexit
}

/// What a failing test run leads to.
struct Failure {
    /// The script still passes.
    passes: bool,
    /// How many times the tests are run again after failing: `npm test || npm test`.
    retries: u32,
}

/// Follows a failure of the test run at `lines[line][at]` to the end of the script.
///
/// First through the rest of its `&&` / `||` list, the way the shell does: `&&` skips the
/// next command, `||` runs it (`npm test && echo ok || true` passes). If the list still
/// fails, `bash -e` stops the script there, unless the failure is followed by `&&`. A
/// script that goes on passes unless nothing comes after, or an `exit` passes it on.
fn failure(lines: &[Vec<Segment>], line: usize, at: usize, errexit: bool) -> Failure {
    let segments = &lines[line];
    let mut retries = 0;
    let mut ok = false;
    let mut end = pipeline_end(segments, at);
    // The last command that ran ends the list: only then does `bash -e` stop.
    let mut last_ran = end;
    while matches!(segments[end].join, Join::And | Join::Or) && end + 1 < segments.len() {
        let next = end + 1;
        let runs = (segments[end].join == Join::And) == ok;
        end = pipeline_end(segments, next);
        if !runs {
            continue;
        }
        let segment = &segments[next];
        if !ok && test_run(&segment.tokens).is_some() {
            retries += 1;
        }
        match effect(segment, ok) {
            Effect::Exit(passes) => return Failure { passes, retries },
            Effect::Status(status) => ok = status,
        }
        last_ran = end;
    }
    let passes = if ok {
        true
    } else if errexit && last_ran == end {
        false
    } else {
        let mut rest = segments[end + 1..]
            .iter()
            .chain(lines[line + 1..].iter().flatten());
        match rest.next() {
            None => false,
            Some(next) => match effect(next, false) {
                Effect::Exit(passes) => passes,
                // More commands: the last one decides, not the tests.
                Effect::Status(_) => true,
            },
        }
    };
    Failure { passes, retries }
}

/// The last segment of the pipeline starting at `at`: `npm test | tee log` is one command.
fn pipeline_end(segments: &[Segment], mut at: usize) -> usize {
    while segments[at].join == Join::Pipe && at + 1 < segments.len() {
        at += 1;
    }
    at
}

/// What running one command does.
enum Effect {
    /// The script exits, passing or not.
    Exit(bool),
    /// The command passes or fails, and the script goes on.
    Status(bool),
}

/// What running `segment` does, after a command that passed or not (`ok`, its `$?`).
fn effect(segment: &Segment, ok: bool) -> Effect {
    if !segment.group.is_empty() {
        // A group's last command decides.
        let mut ok = ok;
        for inner in &segment.group {
            match effect(inner, ok) {
                exit @ Effect::Exit(_) => return exit,
                Effect::Status(status) => ok = status,
            }
        }
        return Effect::Status(ok);
    }
    let words: Vec<&str> = segment.tokens.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["exit"] | ["exit", "$?"] => Effect::Exit(ok),
        ["exit", code, ..] => Effect::Exit(*code == "0"),
        ["false"] => Effect::Status(false),
        // The tests run again: they may fail again.
        _ if test_run(&segment.tokens).is_some() => Effect::Status(false),
        // `true`, `echo failed`, `cat log`, a notification: they pass.
        _ => Effect::Status(true),
    }
}

/// Where a test run's own arguments start, if the command runs tests: after `npm test`,
/// `npx vitest`, `pnpm --filter web test`, `xvfb-run -a npx playwright test`, …
fn test_run(tokens: &[String]) -> Option<usize> {
    let word = |i: usize| tokens.get(i).map(String::as_str);
    let at = launcher(tokens)?;
    match word(at)? {
        manager @ ("npm" | "pnpm" | "yarn" | "bun") => {
            let at = manager_flags(tokens, at + 1, manager);
            match word(at)? {
                "test" => Some(at + 1),
                "t" if manager == "npm" => Some(at + 1),
                "run" | "run-script" => {
                    let at = manager_flags(tokens, at + 1, manager);
                    is_test_script(word(at)?).then_some(at + 1)
                }
                "exec" | "dlx" | "x" => runner(tokens, runner_flags(tokens, at + 1)),
                // `pnpm test:unit`, `yarn vitest`
                script if manager != "npm" && is_test_script(script) => Some(at + 1),
                _ if manager != "npm" => runner(tokens, at),
                _ => None,
            }
        }
        "npx" | "bunx" => runner(tokens, runner_flags(tokens, at + 1)),
        _ => runner(tokens, at),
    }
}

/// Commands that run another one: `timeout 600 npm test`, `xvfb-run -a npx playwright test`.
const WRAPPERS: &[&str] = &[
    "env",
    "cross-env",
    "dotenv",
    "time",
    "timeout",
    "nice",
    "nohup",
    "xvfb-run",
];

/// Where the command that does the work starts: past environment variables
/// (`CI=true npm test`) and wrappers with their own arguments.
fn launcher(tokens: &[String]) -> Option<usize> {
    let mut wrapped = false;
    for (at, token) in tokens.iter().enumerate() {
        if WRAPPERS.contains(&token.as_str()) {
            wrapped = true;
        } else if !is_assignment(token) && (!wrapped || is_launcher(token)) {
            return Some(at);
        }
    }
    None
}

fn is_launcher(token: &str) -> bool {
    matches!(
        binary(token),
        "npm"
            | "pnpm"
            | "yarn"
            | "bun"
            | "npx"
            | "bunx"
            | "vitest"
            | "jest"
            | "mocha"
            | "playwright"
            | "node"
    )
}

/// `vitest` in `./node_modules/.bin/vitest`.
fn binary(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// Past a package manager's own flags: `pnpm --filter web test`, `npm -w api test`,
/// `yarn workspace web test`.
fn manager_flags(tokens: &[String], mut at: usize, manager: &str) -> usize {
    let takes_value: &[&str] = match manager {
        "npm" => &["-w", "--workspace", "--prefix"],
        "pnpm" => &["--filter", "-F", "-C", "--dir"],
        "yarn" => &["--cwd"],
        _ => &["--filter", "--cwd"],
    };
    while let Some(token) = tokens.get(at) {
        if token.starts_with('-') {
            let value = !token.contains('=') && takes_value.contains(&token.as_str());
            at += if value { 2 } else { 1 };
        } else if manager == "yarn" && token == "workspace" {
            at += 2;
        } else {
            break;
        }
    }
    at
}

/// Past `npx`'s or `pnpm exec`'s own flags: `npx -y vitest`,
/// `npx -p @playwright/test playwright test`.
fn runner_flags(tokens: &[String], mut at: usize) -> usize {
    while let Some(token) = tokens.get(at) {
        if !token.starts_with('-') {
            break;
        }
        at += if matches!(token.as_str(), "-p" | "--package") {
            2
        } else {
            1
        };
    }
    at
}

/// A test runner called directly: `vitest`, `jest`, `playwright test`, `node --test`.
fn runner(tokens: &[String], at: usize) -> Option<usize> {
    let word = |i: usize| tokens.get(i).map(String::as_str);
    match binary(word(at)?) {
        "vitest" | "jest" | "mocha" => Some(at + 1),
        "playwright" if word(at + 1)? == "test" => Some(at + 2),
        "node" if tokens[at + 1..].iter().any(|token| token == "--test") => Some(at + 1),
        _ => None,
    }
}

/// A `package.json` script that runs tests, by its name: `test`, `tests`, `test:unit`,
/// `test-unit`, `unit:test`, `e2e`. Not one that only builds or sets up for them
/// (`test:build`), or watches (`test:watch`).
pub(crate) fn is_test_script(name: &str) -> bool {
    let tests = name.starts_with("e2e")
        || name
            .split([':', '-', '_', '.', '/'])
            .any(|word| matches!(word, "test" | "tests" | "e2e"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn read(script: &str, shell: Shell) -> Option<String> {
        analyze(script, shell).map(|command| {
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
            ("npm run test-unit", "npm run test-unit"),
            ("npm run tests", "npm run tests"),
            ("CI=true pnpm test", "CI=true pnpm test"),
            ("yarn test:e2e", "yarn test:e2e"),
            ("npx vitest run", "npx vitest"),
            ("npx -y vitest run", "npx -y vitest"),
            ("pnpm exec playwright test", "pnpm exec playwright test"),
            ("pnpm --filter web test", "pnpm --filter web test"),
            ("pnpm -r test", "pnpm -r test"),
            ("npm -w api test", "npm -w api test"),
            ("yarn workspace web test", "yarn workspace web test"),
            (
                "xvfb-run -a npx playwright test",
                "xvfb-run -a npx playwright test",
            ),
            ("timeout 600 npm test", "timeout 600 npm test"),
            (
                "./node_modules/.bin/vitest run",
                "./node_modules/.bin/vitest",
            ),
            ("bun test", "bun test"),
            ("node --test", "node"),
            ("npm ci && npm test", "npm test"),
        ];
        for (script, runner) in cases {
            assert_eq!(
                read(script, Shell::StopOnError).as_deref(),
                Some(runner),
                "{script}"
            );
        }
        for script in [
            "npm ci",
            "npm run lint",
            "npm run test:build",
            "echo test",
            "time make test",
        ] {
            assert_eq!(read(script, Shell::StopOnError), None, "{script}");
        }
    }

    #[test]
    fn what_a_workflow_step_does_to_failures() {
        let cases = [
            ("npm test || true", "npm test; swallows"),
            ("npm test || echo 'tests failed'", "npm test; swallows"),
            ("npm test 2>&1 || true", "npm test; swallows"),
            ("npm test > out.log 2>&1", "npm test"),
            ("npm test | tee log || true", "npm test; swallows"),
            ("npm test && echo ok || true", "npm test; swallows"),
            ("npm test || exit 1", "npm test"),
            ("npm test || exit 2", "npm test"),
            ("npm test || { cat log; exit 1; }", "npm test"),
            ("npm test || { cat log; exit $?; }", "npm test; swallows"),
            ("npm test || (echo failed && exit 1)", "npm test"),
            ("npm test || npm test", "npm test; retries 1"),
            ("npm test && npm run lint", "npm test"),
            // `bash -e` stops at the failing test.
            ("npm test; exit 0", "npm test"),
            ("npm test\nset +e\n./cleanup.sh", "npm test"),
            // Without it, the last command decides.
            ("set +e\nnpm test", "npm test"),
            ("set +e\nnpm test\n./cleanup.sh", "npm test; swallows"),
            ("npm test && echo passed\necho done", "npm test; swallows"),
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
            assert_eq!(
                read(script, Shell::StopOnError).as_deref(),
                Some(expected),
                "{script}"
            );
        }
    }

    #[test]
    fn what_a_package_script_does_to_failures() {
        let cases = [
            ("vitest run", "vitest"),
            ("vitest run && eslint .", "vitest"),
            ("vitest run; exit 0", "vitest; swallows"),
            ("vitest run; eslint .", "vitest; swallows"),
            ("vitest run; exit", "vitest"),
        ];
        for (script, expected) in cases {
            assert_eq!(
                read(script, Shell::KeepGoing).as_deref(),
                Some(expected),
                "{script}"
            );
        }
    }

    #[test]
    fn scripts_that_do_nothing() {
        for script in [
            "echo skip",
            "exit 0",
            "true",
            "",
            "echo 'no tests' && exit 0",
        ] {
            assert!(does_nothing(script), "{script}");
        }
        for script in ["turbo test", "echo skip; exit 1", "vitest run"] {
            assert!(!does_nothing(script), "{script}");
        }
    }
}
