//! Acknowledging a deliberate change (`docs/architecture.md`, step 6): an allow comment in
//! the code, or a trailer in the commit message, each with a reason.
//!
//! ```text
//! it('puts the reply into the text', () => {
//!   // assertrust-allow: check-removed preview is tested in MCP now
//!   …
//! })
//! → test/a.test.ts:3  HIGH  check-removed  ALLOWED
//!     removed expect(fields.preview).toBe('Hi')
//!     allowed: preview is tested in MCP now
//! ```
//!
//! A comment counts only in the change that adds it, so one left from an old change can't
//! waive a new weakening. It sits anywhere in the test, hook or helper the finding is in (or
//! the line above it), or on the finding's line or the comment lines just above it (a CI
//! step, a config setting). A trailer (`Assertrust-Allow: test-disabled flaky, see #123`)
//! counts for the findings its commit made.

use std::collections::HashMap;

use assertrust_git::{Commit, Comparison, Repo};
use assertrust_ir::FileIr;
use assertrust_rules::{Finding, RuleId};

/// The marker in a comment.
const MARKER: &str = "assertrust-allow:";

/// The trailer in a commit message.
const TRAILER: &str = "assertrust-allow:";

/// One allow comment or trailer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Allow {
    rules: Vec<RuleId>,
    reason: Option<String>,
    /// Its line in the file (0 for a trailer).
    line: u32,
}

/// How a finding was waived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not waived.
    No,
    /// Waived, with the reason given and where: `comment` or `trailer in 1a2b3c4`.
    Allowed { reason: String, by: String },
    /// An allow names the rule but gives no reason, so it doesn't count.
    NoReason,
}

/// What a comparison needs to tell whether its findings are allowed: where the tests, hooks
/// and helpers of each file are, after the change.
#[derive(Default)]
pub struct Blocks {
    /// `(first line, last line)` of each test, hook and helper, by file.
    by_file: HashMap<String, Vec<(u32, u32)>>,
}

impl Blocks {
    /// Adds the tests, hooks and helpers of a file after the change.
    pub fn add(&mut self, path: &str, file: &FileIr) {
        let blocks = self.by_file.entry(path.to_owned()).or_default();
        blocks.extend(file.tests.iter().map(|test| (test.line, test.end_line)));
        blocks.extend(file.hooks.iter().map(|hook| (hook.line, hook.end_line)));
        blocks.extend(
            file.helpers
                .iter()
                .map(|helper| (helper.line, helper.end_line)),
        );
    }

    /// The innermost block that holds `line`.
    fn around(&self, path: &str, line: u32) -> Option<(u32, u32)> {
        self.by_file
            .get(path)?
            .iter()
            .filter(|(first, last)| (*first..=*last).contains(&line))
            .min_by_key(|(first, last)| last - first)
            .copied()
    }
}

/// Whether each of `findings` (from `comparison`, made by its `commits`) is allowed.
pub fn verdicts(
    repo: &Repo,
    comparison: &Comparison,
    blocks: &Blocks,
    findings: &[(&Finding, &[&Commit])],
) -> Result<Vec<Verdict>, String> {
    let comments = new_comments(repo, comparison, findings)?;
    Ok(findings
        .iter()
        .map(|(finding, commits)| {
            let mut allows: Vec<(Allow, String)> = commits
                .iter()
                .flat_map(|commit| {
                    trailers(&commit.message)
                        .into_iter()
                        .map(|allow| (allow, format!("trailer in {}", commit.short_sha())))
                })
                .collect();
            if let (Some(line), Some(file)) = (finding.line, comments.get(&finding.path)) {
                let around = blocks.around(&finding.path, line);
                allows.extend(
                    file.allows
                        .iter()
                        .filter(|allow| {
                            file.just_above(allow.line, line)
                                || around.is_some_and(|(first, last)| {
                                    (first.saturating_sub(1)..=last).contains(&allow.line)
                                })
                        })
                        .map(|allow| (allow.clone(), "comment".to_owned())),
                );
            }
            verdict(finding.rule, &allows)
        })
        .collect())
}

fn verdict(rule: RuleId, allows: &[(Allow, String)]) -> Verdict {
    let mut named = allows
        .iter()
        .filter(|(allow, _)| allow.rules.contains(&rule))
        .peekable();
    if named.peek().is_none() {
        return Verdict::No;
    }
    named
        .find_map(|(allow, by)| {
            allow.reason.as_ref().map(|reason| Verdict::Allowed {
                reason: reason.clone(),
                by: by.clone(),
            })
        })
        .unwrap_or(Verdict::NoReason)
}

/// A file's allow comments that the change added, with its lines.
struct Comments {
    allows: Vec<Allow>,
    lines: Vec<String>,
}

impl Comments {
    /// `allow` is on `line`, or in the comment lines right above it.
    fn just_above(&self, allow: u32, line: u32) -> bool {
        if allow == line {
            return true;
        }
        if allow > line {
            return false;
        }
        (allow + 1..line).all(|between| {
            self.lines
                .get(between as usize - 1)
                .is_some_and(|text| is_comment(text))
        })
    }
}

/// The allow comments each finding's file gained in `comparison`.
fn new_comments(
    repo: &Repo,
    comparison: &Comparison,
    findings: &[(&Finding, &[&Commit])],
) -> Result<HashMap<String, Comments>, String> {
    let mut paths: Vec<String> = findings
        .iter()
        .filter(|(finding, _)| finding.line.is_some())
        .map(|(finding, _)| finding.path.clone())
        .collect();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok(HashMap::new());
    }
    // The same file before the change: its old name if it was renamed.
    let before_paths: Vec<String> = paths
        .iter()
        .map(|path| {
            comparison
                .files
                .iter()
                .find(|file| file.path_after.as_deref() == Some(path))
                .and_then(|file| file.path_before.clone())
                .unwrap_or_else(|| path.clone())
        })
        .collect();
    let after = repo
        .read_files(&comparison.after, &paths)
        .map_err(|err| err.to_string())?;
    let before = repo
        .read_files(&comparison.before, &before_paths)
        .map_err(|err| err.to_string())?;
    let text = |bytes: Option<Vec<u8>>| bytes.and_then(|bytes| String::from_utf8(bytes).ok());
    let mut found = HashMap::new();
    for ((path, after), before) in paths.into_iter().zip(after).zip(before) {
        let Some(after) = text(after) else {
            continue;
        };
        let old: Vec<Allow> = text(before).map(|text| comments(&text)).unwrap_or_default();
        let allows: Vec<Allow> = comments(&after)
            .into_iter()
            .filter(|allow| {
                !old.iter()
                    .any(|old| (&old.rules, &old.reason) == (&allow.rules, &allow.reason))
            })
            .collect();
        if allows.is_empty() {
            continue;
        }
        let lines = after.lines().map(str::to_owned).collect();
        found.insert(path, Comments { allows, lines });
    }
    Ok(found)
}

/// The allow comments in a file: `// assertrust-allow: rule[,rule] reason`, also after `#`
/// or in a `/* … */` block.
fn comments(text: &str) -> Vec<Allow> {
    text.lines()
        .zip(1..)
        .filter_map(|(line, number)| {
            let at = line.find(MARKER)?;
            let before = line[..at].trim_end();
            let in_comment = ["//", "#", "/*", "*"]
                .iter()
                .any(|marker| before.ends_with(marker));
            if !in_comment {
                return None;
            }
            let rest = line[at + MARKER.len()..].trim_end();
            let rest = rest.strip_suffix("*/").unwrap_or(rest);
            parse(rest, number)
        })
        .collect()
}

/// The allow trailers in a commit message: `Assertrust-Allow: rule[,rule] reason`.
fn trailers(message: &str) -> Vec<Allow> {
    message
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (key, rest) = line.split_at_checked(TRAILER.len())?;
            key.eq_ignore_ascii_case(TRAILER)
                .then(|| parse(rest, 0))
                .flatten()
        })
        .collect()
}

/// `rule[,rule] reason`. `None` when it names no rule assertrust knows.
fn parse(text: &str, line: u32) -> Option<Allow> {
    let text = text.trim();
    let (names, reason) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let rules: Vec<RuleId> = names.split(',').filter_map(RuleId::from_name).collect();
    if rules.is_empty() {
        return None;
    }
    let reason = reason.trim();
    Some(Allow {
        rules,
        reason: (!reason.is_empty()).then(|| reason.to_owned()),
        line,
    })
}

/// A line that's only a comment.
fn is_comment(line: &str) -> bool {
    let line = line.trim_start();
    ["//", "#", "/*", "*"]
        .iter()
        .any(|marker| line.starts_with(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_comments_in_code_and_config() {
        let text = "it('a', () => {\n  // assertrust-allow: check-removed,matcher-loosened moved to MCP\n  expect(a)\n})\n  # assertrust-allow: gate-loosened flaky on CI\n/* assertrust-allow: test-disabled */\nconst s = 'assertrust-allow: check-removed not a comment'\n";
        assert_eq!(
            comments(text),
            [
                Allow {
                    rules: vec![RuleId::CheckRemoved, RuleId::MatcherLoosened],
                    reason: Some("moved to MCP".to_owned()),
                    line: 2,
                },
                Allow {
                    rules: vec![RuleId::GateLoosened],
                    reason: Some("flaky on CI".to_owned()),
                    line: 5,
                },
                Allow {
                    rules: vec![RuleId::TestDisabled],
                    reason: None,
                    line: 6,
                },
            ]
        );
    }

    #[test]
    fn reads_trailers() {
        let message = "Drop the flaky test\n\nIt fails one run in ten.\n\nAssertrust-Allow: test-disabled flaky on CI, see #123\nCo-Authored-By: someone";
        assert_eq!(
            trailers(message),
            [Allow {
                rules: vec![RuleId::TestDisabled],
                reason: Some("flaky on CI, see #123".to_owned()),
                line: 0,
            }]
        );
        assert!(trailers("assertrust-allow: no-such-rule because").is_empty());
    }

    #[test]
    fn a_reason_is_required() {
        let allow = |reason: Option<&str>| {
            vec![(
                Allow {
                    rules: vec![RuleId::CheckRemoved],
                    reason: reason.map(str::to_owned),
                    line: 1,
                },
                "comment".to_owned(),
            )]
        };
        assert_eq!(
            verdict(RuleId::CheckRemoved, &allow(Some("moved"))),
            Verdict::Allowed {
                reason: "moved".to_owned(),
                by: "comment".to_owned()
            }
        );
        assert_eq!(
            verdict(RuleId::CheckRemoved, &allow(None)),
            Verdict::NoReason
        );
        assert_eq!(
            verdict(RuleId::TestDisabled, &allow(Some("moved"))),
            Verdict::No
        );
    }
}
