//! What `assertrust check` reports (`docs/architecture.md`, step 6): every finding with the
//! commits that made it, and whether it's shown, hidden, allowed or undone later.
//!
//! A PR-style check runs the rules on the whole PR and on each of its commits. A finding in
//! the whole PR names the commits whose findings are about the same thing; a commit's
//! finding that the whole PR doesn't have was undone later in the PR (transient):
//!
//! ```text
//! c1  remove flaky check   → removed expect(res.body).toEqual({…})   (put back by c2)
//! c3  skip slow test       → now skipped (skip)
//! whole PR                 → now skipped (skip) · c3;   c1's finding is transient
//! ```
//!
//! A commit's finding about code the PR itself added (a new test loosened before it
//! landed) isn't undone, just never on the main branch: it's shown, marked, and can't fail.

use std::collections::{HashMap, HashSet};

use assertrust_git::{Analysis, Commit, Comparison, Repo};
use assertrust_ir::Test;
use assertrust_rules::Finding;

use crate::allow::{self, Blocks, Verdict};
use crate::config::{Config, Setting};
use crate::load::{self};
use crate::modules::Modules;
use crate::output::count;

/// Every finding of a check, in the order it's shown.
pub struct Report<'a> {
    pub findings: Vec<Reported<'a>>,
    /// Files that couldn't be checked, and merge commits a per-commit check skipped.
    pub notes: Vec<String>,
}

pub struct Reported<'a> {
    pub finding: Finding,
    /// The commits that made it, oldest first: none for staged or unsaved changes.
    pub commits: Vec<&'a Commit>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Shown,
    /// Its rule is left to the judge (`outcome-changed` by default): shown with `-v`.
    Hidden,
    /// Waived by an allow comment or trailer: shown last, with the reason.
    Allowed {
        reason: String,
        by: String,
    },
    /// Undone later in the same PR: shown with `--show-transient`.
    Transient,
    /// About code the PR itself added and weakened before it landed (a new test loosened
    /// in a later commit): shown, but it can't fail the check, since the main branch lost
    /// nothing.
    BeforeLanding,
}

impl Status {
    /// The order statuses are shown in: findings first, then allowed, then undone later.
    fn rank(&self) -> u8 {
        match self {
            Self::Shown | Self::Hidden => 0,
            Self::BeforeLanding => 1,
            Self::Allowed { .. } => 2,
            Self::Transient => 3,
        }
    }
}

impl Report<'_> {
    /// The findings that make the check fail (exit 1) at this `fail-on` level.
    pub fn failing(&self, config: &Config) -> usize {
        self.findings
            .iter()
            .filter(|reported| {
                reported.status == Status::Shown
                    && config.fail_on.fails(reported.finding.confidence)
            })
            .count()
    }

    pub fn with_status(&self, wanted: fn(&Status) -> bool) -> usize {
        self.findings
            .iter()
            .filter(|reported| wanted(&reported.status))
            .count()
    }
}

/// One comparison's findings, with what's needed to tell whether they're allowed.
struct Checked<'a> {
    comparison: &'a Comparison,
    findings: Vec<Finding>,
    blocks: Blocks,
    /// The test files the comparison changed, with the tests each had before it (none for
    /// a file it added).
    tests_before: HashMap<String, Option<HashSet<String>>>,
}

pub fn build<'a>(
    repo: &'a Repo,
    analysis: &'a Analysis,
    config: &'a Config,
) -> Result<Report<'a>, String> {
    let mut notes = Vec::new();
    let mut carry = None;
    let mut findings = Vec::new();
    let steps: Vec<(&Commit, Checked)> = analysis
        .steps
        .iter()
        .map(|step| {
            let checked = check(repo, &step.comparison, config, &mut carry, &mut notes)?;
            Ok((&step.commit, checked))
        })
        .collect::<Result<_, String>>()?;
    match &analysis.net {
        // What lands, traced back to the commits that made it.
        Some(net) => {
            let net = check(repo, net, config, &mut None, &mut notes)?;
            let commits: Vec<Vec<&Commit>> = net
                .findings
                .iter()
                .map(|finding| {
                    steps
                        .iter()
                        .filter(|(_, step)| step.findings.iter().any(|f| related(finding, f)))
                        .map(|(commit, _)| *commit)
                        .collect()
                })
                .collect();
            findings.extend(decide(repo, &net, commits, config)?);
            // What the whole PR doesn't have: undone, or about code the PR added.
            for (commit, mut step) in steps {
                step.findings
                    .retain(|finding| !net.findings.iter().any(|f| related(f, finding)));
                let added = added_in(repo, &net, &step.findings)?;
                let commits = vec![vec![commit]; step.findings.len()];
                for (mut reported, added) in
                    decide(repo, &step, commits, config)?.into_iter().zip(added)
                {
                    reported.status = match reported.status {
                        Status::Shown if added => Status::BeforeLanding,
                        Status::Allowed { .. } | Status::Hidden if added => reported.status,
                        _ => Status::Transient,
                    };
                    findings.push(reported);
                }
            }
        }
        // Each commit on its own.
        None => {
            for (commit, step) in steps {
                let commits = vec![vec![commit]; step.findings.len()];
                findings.extend(decide(repo, &step, commits, config)?);
            }
        }
    }
    // Stable, so within a level findings stay in commit, file and line order.
    findings.sort_by_key(|reported| (reported.status.rank(), reported.finding.confidence));
    // A PR-style check compares the merge-base with the head, which covers merges too.
    if analysis.net.is_none() && !analysis.skipped_merges.is_empty() {
        notes.push(format!(
            "{} not checked (a PR-style check, BASE...HEAD, covers what they changed)",
            count(analysis.skipped_merges.len(), "merge commit")
        ));
    }
    Ok(Report { findings, notes })
}

/// Runs the rules on one comparison, leaving out the rules that are off.
fn check<'a>(
    repo: &'a Repo,
    comparison: &'a Comparison,
    config: &'a Config,
    carry: &mut Option<Modules<'a>>,
    notes: &mut Vec<String>,
) -> Result<Checked<'a>, String> {
    let loaded = load::load(repo, comparison, &config.classifier, carry)?;
    for note in &loaded.notes {
        // The same file can be skipped in several commits.
        if !notes.contains(note) {
            notes.push(note.clone());
        }
    }
    let findings = assertrust_rules::check(&loaded.test_files(), &loaded.gate_files())
        .into_iter()
        .filter(|finding| config.setting(finding.rule) != Setting::Off)
        .collect();
    let tests_before = loaded
        .files
        .iter()
        .filter_map(|file| {
            let path = file.after.as_ref().or(file.before.as_ref())?.path.clone();
            let tests = file
                .before
                .as_ref()
                .map(|before| before.ir.tests.iter().map(Test::id).collect());
            Some((path, tests))
        })
        .collect();
    Ok(Checked {
        comparison,
        findings,
        blocks: loaded.blocks,
        tests_before,
    })
}

/// Whether each finding is shown, hidden or allowed.
fn decide<'a>(
    repo: &Repo,
    checked: &Checked,
    commits: Vec<Vec<&'a Commit>>,
    config: &Config,
) -> Result<Vec<Reported<'a>>, String> {
    let pairs: Vec<(&Finding, &[&Commit])> = checked
        .findings
        .iter()
        .zip(&commits)
        .map(|(finding, commits)| (finding, commits.as_slice()))
        .collect();
    let verdicts = allow::verdicts(repo, checked.comparison, &checked.blocks, &pairs)?;
    Ok(checked
        .findings
        .iter()
        .zip(commits)
        .zip(verdicts)
        .map(|((finding, commits), verdict)| {
            let mut finding = finding.clone();
            let status = match verdict {
                Verdict::Allowed { reason, by } => Status::Allowed { reason, by },
                Verdict::NoReason | Verdict::No => {
                    if verdict == Verdict::NoReason {
                        finding
                            .message
                            .push_str("; allow ignored: it gives no reason");
                    }
                    match config.setting(finding.rule) {
                        Setting::Judge => Status::Hidden,
                        Setting::On | Setting::Off => Status::Shown,
                    }
                }
            };
            Reported {
                finding,
                commits,
                status,
            }
        })
        .collect())
}

/// For each of a commit's `findings` the whole PR doesn't have, whether it's about code the
/// PR added: a file that wasn't there before it, or a test that wasn't. One about a file the
/// PR leaves as it found it was undone.
fn added_in(repo: &Repo, net: &Checked, findings: &[Finding]) -> Result<Vec<bool>, String> {
    let paths: Vec<String> = findings
        .iter()
        .map(|finding| finding.path.clone())
        .collect();
    let existed = repo
        .read_files(&net.comparison.before, &paths)
        .map_err(|err| err.to_string())?;
    Ok(findings
        .iter()
        .zip(existed)
        .map(|(finding, existed)| {
            if existed.is_none() {
                return true;
            }
            // A test the PR added to a file that was there.
            match (net.tests_before.get(&finding.path), &finding.test) {
                (Some(Some(tests)), Some(test)) => !tests.contains(test),
                _ => false,
            }
        })
        .collect())
}

/// Whether a finding in the whole PR and one in a commit are about the same thing: the same
/// file, and the same rule on the same test (or saying the same), or the same code lost.
fn related(net: &Finding, step: &Finding) -> bool {
    if net.path != step.path {
        return false;
    }
    let same_test = net.test.is_some() && net.test == step.test;
    let shared = net.before.iter().any(|line| step.before.contains(line));
    (net.rule == step.rule && (same_test || net.message == step.message || shared))
        || (shared && (same_test || net.test.is_none() || step.test.is_none()))
}

#[cfg(test)]
mod tests {
    use assertrust_rules::{Confidence, RuleId};

    use super::*;

    fn finding(rule: RuleId, test: Option<&str>, message: &str, before: &[&str]) -> Finding {
        Finding {
            rule,
            confidence: Confidence::High,
            path: "test/a.test.ts".to_owned(),
            line: Some(1),
            test: test.map(str::to_owned),
            message: message.to_owned(),
            before: before.iter().map(|line| (*line).to_owned()).collect(),
            after: Vec::new(),
        }
    }

    #[test]
    fn findings_about_the_same_thing_are_related() {
        let net = finding(
            RuleId::CheckRemoved,
            Some("refunds"),
            "removed 2 checks, including expect(a).toBe(1)",
            &["expect(a).toBe(1)", "expect(b).toBe(2)"],
        );
        // The same test, the same rule.
        assert!(related(
            &net,
            &finding(
                RuleId::CheckRemoved,
                Some("refunds"),
                "removed expect(b).toBe(2)",
                &["expect(b).toBe(2)"]
            )
        ));
        // The test was renamed later: the code it lost says it.
        assert!(related(
            &net,
            &finding(
                RuleId::CheckRemoved,
                Some("refund"),
                "removed expect(a).toBe(1)",
                &["expect(a).toBe(1)"]
            )
        ));
        // A matcher loosened first, then the check removed: the same code.
        assert!(related(
            &net,
            &finding(
                RuleId::MatcherLoosened,
                Some("refunds"),
                "toBe(1) → toBeTruthy()",
                &["expect(a).toBe(1)"]
            )
        ));
        // Another test that happens to share a line, with another rule.
        assert!(!related(
            &net,
            &finding(
                RuleId::MatcherLoosened,
                Some("charges"),
                "toBe(1) → toBeTruthy()",
                &["expect(a).toBe(1)"]
            )
        ));
        let mut elsewhere = net.clone();
        elsewhere.path = "test/b.test.ts".to_owned();
        assert!(!related(&net, &elsewhere));
    }
}
