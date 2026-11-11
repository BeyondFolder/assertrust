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
use assertrust_match::match_files;
use assertrust_rules::Finding;

use crate::allow::{self, Blocks, Verdict};
use crate::config::{self, Config, Setting};
use crate::load::{self, Loaded};
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
}

/// What a whole PR did to its test files, to trace its commits' findings: which files and
/// tests were there before it, and which it renamed.
#[derive(Default)]
struct PrTests {
    /// The test files it changed, by path after it (before it, for a deleted one), with the
    /// tests each had before it: `None` for a file it added.
    before: HashMap<String, Option<HashSet<String>>>,
    /// Tests it renamed, `(old, new)`, each with its file in front: `test/a.test.ts: refund`.
    renamed: HashSet<(String, String)>,
    /// Files it renamed: the old path, and the new one.
    moved: HashMap<String, String>,
}

impl PrTests {
    fn new(loaded: &Loaded) -> Self {
        let mut pr = Self::default();
        let files = loaded.file_pairs();
        for file in &files {
            match (&file.before, &file.after) {
                (before, Some(after)) => {
                    let tests = before
                        .as_ref()
                        .map(|before| before.ir.tests.iter().map(Test::id).collect());
                    pr.before.insert(after.path.to_owned(), tests);
                    if let Some(before) = before.as_ref().filter(|b| b.path != after.path) {
                        pr.moved
                            .insert(before.path.to_owned(), after.path.to_owned());
                    }
                }
                (Some(before), None) => {
                    let tests = before.ir.tests.iter().map(Test::id).collect();
                    pr.before.insert(before.path.to_owned(), Some(tests));
                }
                (None, None) => {}
            }
        }
        for pair in &match_files(&files).pairs {
            let (Some(before), Some(after)) = (
                &files[pair.before.file].before,
                &files[pair.after.file].after,
            ) else {
                continue;
            };
            let old = before.ir.tests[pair.before.test].id();
            let new = after.ir.tests[pair.after.test].id();
            if old != new {
                // A finding in a renamed file names its tests with the new path.
                for path in [before.path, after.path] {
                    pr.renamed
                        .insert((format!("{path}: {old}"), format!("{}: {new}", after.path)));
                }
            }
        }
        pr
    }

    /// Whether a commit's test `old` is the whole PR's test `new`, both named in a finding
    /// on `path`: the same id, or one the PR renamed.
    fn same_test(&self, path: &str, old: &str, new: &str) -> bool {
        old == new
            // Named with their file in front already (a finding in a helper).
            || self.renamed.contains(&(old.to_owned(), new.to_owned()))
            || self
                .renamed
                .contains(&(format!("{path}: {old}"), format!("{path}: {new}")))
    }

    /// Whether `test`, in a finding on `path`, is a test the PR renamed into it.
    fn renamed_into(&self, path: &str, test: &str) -> bool {
        let named = format!("{path}: {test}");
        self.renamed.iter().any(|(_, new)| *new == named)
    }
}

pub fn build<'a>(
    repo: &'a Repo,
    analysis: &'a Analysis,
    config: &'a Config,
) -> Result<Report<'a>, String> {
    let mut notes = Vec::new();
    let mut carry = None;
    let mut findings = Vec::new();
    // In a PR-style check, a file a commit couldn't read is noted only when the whole PR
    // can't read it either: code that never lands isn't worth a note.
    let mut step_notes = Vec::new();
    let mut steps: Vec<(&Commit, Checked)> = Vec::new();
    for step in &analysis.steps {
        let notes = if analysis.net.is_some() {
            &mut step_notes
        } else {
            &mut notes
        };
        let (checked, _) = check(repo, &step.comparison, config, false, &mut carry, notes)?;
        steps.push((&step.commit, checked));
    }
    match &analysis.net {
        // What lands, traced back to the commits that made it.
        Some(net) => {
            let (net, pr) = check(repo, net, config, true, &mut None, &mut notes)?;
            let commits: Vec<Vec<&Commit>> = net
                .findings
                .iter()
                .map(|finding| {
                    steps
                        .iter()
                        .filter(|(_, step)| step.findings.iter().any(|f| related(finding, f, &pr)))
                        .map(|(commit, _)| *commit)
                        .collect()
                })
                .collect();
            findings.extend(decide(repo, &net, commits, config)?);
            // What the whole PR doesn't have: undone, or about code the PR added.
            for (commit, mut step) in steps {
                step.findings
                    .retain(|finding| !net.findings.iter().any(|f| related(f, finding, &pr)));
                let added = added_in(repo, &net, &pr, &step.findings)?;
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

/// Runs the rules on one comparison, leaving out the rules that are off. For a whole PR
/// (`whole_pr`), also says what it did to its test files.
fn check<'a>(
    repo: &'a Repo,
    comparison: &'a Comparison,
    config: &'a Config,
    whole_pr: bool,
    carry: &mut Option<Modules<'a>>,
    notes: &mut Vec<String>,
) -> Result<(Checked<'a>, PrTests), String> {
    let loaded = load::load(repo, comparison, &config.classifier, carry)?;
    let mut new_notes = loaded.notes.clone();
    let mut findings = assertrust_rules::check(&loaded.test_files(), &loaded.gate_files());
    findings.extend(config_changes(repo, comparison, &mut new_notes)?);
    findings.retain(|finding| config.setting(finding.rule) != Setting::Off);
    for note in new_notes {
        // The same file can be skipped in several commits.
        if !notes.contains(&note) {
            notes.push(note);
        }
    }
    let pr = if whole_pr {
        PrTests::new(&loaded)
    } else {
        PrTests::default()
    };
    let checked = Checked {
        comparison,
        findings,
        blocks: loaded.blocks,
    };
    Ok((checked, pr))
}

/// What a change to `assertrust.toml` itself loosened (`gate-loosened`).
fn config_changes(
    repo: &Repo,
    comparison: &Comparison,
    notes: &mut Vec<String>,
) -> Result<Vec<Finding>, String> {
    let is_config = |path: &Option<String>| path.as_deref() == Some(config::FILE);
    if !comparison
        .files
        .iter()
        .any(|file| is_config(&file.path_before) || is_config(&file.path_after))
    {
        return Ok(Vec::new());
    }
    let read = |rev| -> Result<Option<String>, String> {
        let mut texts = repo
            .read_files(rev, &[config::FILE.to_owned()])
            .map_err(|err| err.to_string())?;
        Ok(texts
            .pop()
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok()))
    };
    let parse = |text: &Option<String>| {
        text.as_deref()
            .map_or_else(|| Ok(Config::default()), Config::parse)
    };
    let after = read(&comparison.after)?;
    // Without the old settings, what changed can't be told.
    let Ok(old) = parse(&read(&comparison.before)?) else {
        return Ok(Vec::new());
    };
    match parse(&after) {
        Ok(new) => Ok(Config::loosened(
            &old,
            &new,
            after.as_deref().unwrap_or_default(),
        )),
        Err(err) => {
            notes.push(format!(
                "{} ({}): skipped, doesn't parse ({err})",
                config::FILE,
                comparison.after
            ));
            Ok(Vec::new())
        }
    }
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
/// PR added: a file that wasn't there before it, or tests that weren't (renamed ones were).
/// One about code the PR leaves as it found it was undone.
fn added_in(
    repo: &Repo,
    net: &Checked,
    pr: &PrTests,
    findings: &[Finding],
) -> Result<Vec<bool>, String> {
    // A file that isn't one of the PR's test files: the PR's list of changed files says
    // whether it added it, and for one it didn't touch, the base does.
    let changed = |path: &str| {
        net.comparison
            .files
            .iter()
            .find(|file| file.path_after.as_deref() == Some(path))
            .map(|file| file.path_before.is_none())
    };
    let mut unknown: Vec<String> = findings
        .iter()
        .filter(|f| !pr.before.contains_key(&f.path) && changed(&f.path).is_none())
        .map(|f| f.path.clone())
        .collect();
    unknown.sort();
    unknown.dedup();
    let at_base = repo
        .read_files(&net.comparison.before, &unknown)
        .map_err(|err| err.to_string())?;
    let missing: HashSet<&String> = unknown
        .iter()
        .zip(at_base)
        .filter_map(|(path, text)| text.is_none().then_some(path))
        .collect();
    Ok(findings
        .iter()
        .map(|finding| match pr.before.get(&finding.path) {
            Some(None) => true,
            Some(Some(tests)) => {
                let mut ids = finding.all_tests().peekable();
                ids.peek().is_some()
                    && ids.all(|id| !tests.contains(id) && !pr.renamed_into(&finding.path, id))
            }
            None => changed(&finding.path).unwrap_or_else(|| missing.contains(&finding.path)),
        })
        .collect())
}

/// Whether a finding in the whole PR and one in a commit are about the same thing: the same
/// file (or the one it was renamed to), and, when both name their tests, a test in common
/// (or one renamed); then the same rule on it (or saying the same), or the same code lost.
fn related(net: &Finding, step: &Finding, pr: &PrTests) -> bool {
    if net.path != step.path && pr.moved.get(&step.path) != Some(&net.path) {
        return false;
    }
    let same_test = net.all_tests().any(|new| {
        step.all_tests()
            .any(|old| pr.same_test(&net.path, old, new))
    });
    let both_named = net.all_tests().next().is_some() && step.all_tests().next().is_some();
    if both_named && !same_test {
        return false;
    }
    let shared = net.before.iter().any(|line| step.before.contains(line));
    (net.rule == step.rule && (same_test || net.message == step.message || shared)) || shared
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
            tests: Vec::new(),
            message: message.to_owned(),
            before: before.iter().map(|line| (*line).to_owned()).collect(),
            after: Vec::new(),
        }
    }

    #[test]
    fn findings_about_the_same_thing_are_related() {
        let mut pr = PrTests::default();
        pr.renamed.insert((
            "test/a.test.ts: refund".to_owned(),
            "test/a.test.ts: refunds".to_owned(),
        ));
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
            ),
            &pr
        ));
        // The PR renamed the test later.
        assert!(related(
            &net,
            &finding(
                RuleId::CheckRemoved,
                Some("refund"),
                "removed expect(a).toBe(1)",
                &["expect(a).toBe(1)"]
            ),
            &pr
        ));
        // A matcher loosened first, then the check removed: the same code.
        assert!(related(
            &net,
            &finding(
                RuleId::MatcherLoosened,
                Some("refunds"),
                "toBe(1) → toBeTruthy()",
                &["expect(a).toBe(1)"]
            ),
            &pr
        ));
        // Another test that happens to share a line, with the same rule or another.
        for rule in [RuleId::CheckRemoved, RuleId::MatcherLoosened] {
            assert!(!related(
                &net,
                &finding(
                    rule,
                    Some("charges"),
                    "removed expect(a).toBe(1)",
                    &["expect(a).toBe(1)"]
                ),
                &pr
            ));
        }
        let mut elsewhere = net.clone();
        elsewhere.path = "test/b.test.ts".to_owned();
        assert!(!related(&net, &elsewhere, &pr));
        // Unless the PR renamed the file.
        pr.moved
            .insert("test/b.test.ts".to_owned(), "test/a.test.ts".to_owned());
        assert!(related(&net, &elsewhere, &pr));
    }

    #[test]
    fn another_test_saying_the_same_is_not_related() {
        let skipped = |test| finding(RuleId::TestDisabled, Some(test), "now skipped (skip)", &[]);
        assert!(!related(
            &skipped("refunds"),
            &skipped("charges"),
            &PrTests::default()
        ));
    }

    #[test]
    fn a_grouped_deletion_is_related_to_each_test_it_covers() {
        let mut grouped = finding(
            RuleId::TestDisabled,
            None,
            "deleted 4 tests, including case 1",
            &["case 1", "case 2", "case 3", "case 4"],
        );
        grouped.tests = grouped.before.clone();
        let one = finding(
            RuleId::TestDisabled,
            Some("case 3"),
            "deleted: case 3",
            &["expect(f(3)).toBe(3)"],
        );
        assert!(related(&grouped, &one, &PrTests::default()));
        assert!(related(&one, &grouped, &PrTests::default()));
        let other = finding(
            RuleId::TestDisabled,
            Some("case 9"),
            "deleted: case 9",
            &["expect(f(9)).toBe(9)"],
        );
        assert!(!related(&grouped, &other, &PrTests::default()));
    }
}
