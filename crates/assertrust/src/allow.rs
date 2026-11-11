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
//! A comment counts only in the change that adds it (a line the change added, not one it
//! moved), so one left from an old change can't waive a new weakening. It sits anywhere in the test, hook or helper the finding is in (or
//! the line above it), or on the finding's line or the comment lines just above it (a CI
//! step, a config setting). A trailer (`Assertrust-Allow: test-disabled flaky, see #123`)
//! counts for the findings its commit made.

use std::collections::{HashMap, HashSet};

use assertrust_git::{Commit, Comparison, Repo};
use assertrust_ir::FileIr;
use assertrust_rules::{Finding, RuleId};

/// The marker in a comment, and the trailer in a commit message (in any case there).
const MARKER: &str = "assertrust-allow:";

/// How a comment starts, in code and in CI and test config files.
const COMMENT_STARTS: [&str; 4] = ["//", "#", "/*", "*"];

/// Above this many line pairs, two versions of a file aren't lined up line by line (it
/// takes 4 bytes each): a line is then new when its text wasn't there before.
const MAX_LINE_PAIRS: usize = 4_000_000;

/// One allow comment or trailer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Allow {
    rules: Vec<RuleId>,
    reason: Option<String>,
    /// Its line in the file (0 for a trailer).
    line: u32,
}

impl Allow {
    fn says_same(&self, other: &Self) -> bool {
        (&self.rules, &self.reason) == (&other.rules, &other.reason)
    }
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
    // Each commit's trailers, read once.
    let mut by_commit: HashMap<&str, Vec<Allow>> = HashMap::new();
    for commit in findings.iter().flat_map(|(_, commits)| commits.iter()) {
        by_commit
            .entry(commit.sha.as_str())
            .or_insert_with(|| trailers(&commit.message));
    }
    Ok(findings
        .iter()
        .map(|(finding, commits)| {
            let mut allows: Vec<(Allow, String)> = commits
                .iter()
                .flat_map(|commit| {
                    by_commit[commit.sha.as_str()]
                        .iter()
                        .map(|allow| (allow.clone(), format!("trailer in {}", commit.short_sha())))
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
        let allows = comments(&after);
        if allows.is_empty() {
            continue;
        }
        let before = text(before).unwrap_or_default();
        let lines: Vec<&str> = after.lines().collect();
        let (gone, added) = diff(&before.lines().collect::<Vec<_>>(), &lines);
        // A comment that only moved (its code moved, or was indented) is still the old one.
        let moved: Vec<Allow> = comments(&before)
            .into_iter()
            .filter(|allow| gone[allow.line as usize - 1])
            .collect();
        let allows: Vec<Allow> = allows
            .into_iter()
            .filter(|allow| {
                added[allow.line as usize - 1] && !moved.iter().any(|old| old.says_same(allow))
            })
            .collect();
        if allows.is_empty() {
            continue;
        }
        let lines = lines.into_iter().map(str::to_owned).collect();
        found.insert(path, Comments { allows, lines });
    }
    Ok(found)
}

/// Which lines of `before` are gone and which of `after` are new: those outside a longest
/// common subsequence of the two, as `git diff` would show them.
fn diff(before: &[&str], after: &[&str]) -> (Vec<bool>, Vec<bool>) {
    let mut gone = vec![false; before.len()];
    let mut added = vec![false; after.len()];
    // Most of a file is usually the same at both ends.
    let start = before
        .iter()
        .zip(after)
        .take_while(|(old, new)| old == new)
        .count();
    let end = before[start..]
        .iter()
        .rev()
        .zip(after[start..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    let old = &before[start..before.len() - end];
    let new = &after[start..after.len() - end];
    if old.len().saturating_mul(new.len()) > MAX_LINE_PAIRS {
        let (olds, news): (HashSet<&str>, HashSet<&str>) =
            (old.iter().copied().collect(), new.iter().copied().collect());
        for (i, line) in old.iter().enumerate() {
            gone[start + i] = !news.contains(line);
        }
        for (j, line) in new.iter().enumerate() {
            added[start + j] = !olds.contains(line);
        }
        return (gone, added);
    }
    // `common[i * width + j]`: how many lines `old[i..]` and `new[j..]` have in common.
    let width = new.len() + 1;
    let mut common = vec![0u32; (old.len() + 1) * width];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i * width + j] = if old[i] == new[j] {
                common[(i + 1) * width + j + 1] + 1
            } else {
                common[(i + 1) * width + j].max(common[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            i += 1;
            j += 1;
        } else if common[(i + 1) * width + j] >= common[i * width + j + 1] {
            gone[start + i] = true;
            i += 1;
        } else {
            added[start + j] = true;
            j += 1;
        }
    }
    for i in i..old.len() {
        gone[start + i] = true;
    }
    for j in j..new.len() {
        added[start + j] = true;
    }
    (gone, added)
}

/// The allow comments in a file: `// assertrust-allow: rule[,rule] reason`, also after `#`
/// or in a `/* … */` block.
fn comments(text: &str) -> Vec<Allow> {
    text.lines()
        .zip(1..)
        .filter_map(|(line, number)| {
            let at = line.find(MARKER)?;
            let before = line[..at].trim_end();
            let in_comment = COMMENT_STARTS.iter().any(|start| before.ends_with(start));
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
            let (key, rest) = line.split_at_checked(MARKER.len())?;
            key.eq_ignore_ascii_case(MARKER)
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
    COMMENT_STARTS.iter().any(|start| line.starts_with(start))
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
    fn diff_finds_lines_added_and_gone() {
        let (gone, added) = diff(&["a", "x", "b", "c"], &["a", "b", "y", "c", "x"]);
        assert_eq!(gone, [false, true, false, false]);
        assert_eq!(added, [false, false, true, false, true]);
        // Two copies of a line: only the one added counts as new.
        let (_, added) = diff(
            &["a", "// allow", "b", "c"],
            &["a", "// allow", "b", "// allow", "c"],
        );
        assert_eq!(added, [false, false, false, true, false]);
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
