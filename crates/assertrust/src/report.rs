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
//! A commit's finding about code the PR itself added (a new test loosened in a later commit)
//! that the PR's head still has isn't undone, just never on the main branch: it's added by
//! the PR, hidden unless asked for, and can't fail.

use std::collections::{HashMap, HashSet};

use assertrust_git::{Analysis, Commit, Comparison, FileKind, Repo, Rev};
use assertrust_ir::Test;
use assertrust_match::match_files;
use assertrust_rules::{Finding, RuleId};

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
    /// About code the PR itself added and weakened in a later commit (a new test loosened),
    /// and still weaker at the PR's head: shown with `--show-pr-added`, and it can't fail the
    /// check, since the main branch lost nothing.
    PrAdded,
}

impl Status {
    /// The order statuses are shown in: findings first, then allowed, then undone later.
    fn rank(&self) -> u8 {
        match self {
            Self::Shown | Self::Hidden => 0,
            Self::PrAdded => 1,
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

/// What a change (a whole PR, or one commit) did to its test files, to trace findings across
/// changes: which files and tests were there before it, and which it renamed or moved.
#[derive(Default)]
struct PrTests {
    /// The test files it changed, by path after it (before it, for a deleted one), with the
    /// tests each had before it: `None` for a file it added.
    before: HashMap<String, Option<HashSet<String>>>,
    /// Tests it renamed, `(old, new)`, each with its file in front: `test/a.test.ts: refund`.
    renamed: HashSet<(String, String)>,
    /// Files it renamed: the old path, and the new one.
    moved: HashMap<String, String>,
    /// Tests matching paired under another name or in another file, `(file, id)` before and
    /// after.
    pairs: Vec<((String, String), (String, String))>,
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
            if old != new || before.path != after.path {
                pr.pairs.push((
                    (before.path.to_owned(), old.clone()),
                    (after.path.to_owned(), new.clone()),
                ));
            }
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

    /// Where the test `id`, in `path` after the change, was before it, `(file, id)`: in
    /// `file_before` under the same id, unless matching paired it with another.
    fn origin(&self, path: &str, id: &str, file_before: &str) -> (String, String) {
        self.pairs
            .iter()
            .find(|(_, after)| after.0 == path && after.1 == id)
            .map_or_else(
                || (file_before.to_owned(), id.to_owned()),
                |(before, _)| before.clone(),
            )
    }

    /// Whether the test `id` in `path` before the change is the test `new_id` in `new_path`
    /// after it: the same id in the same file (or the one it was renamed to), or a test
    /// matching paired it with.
    fn became(&self, (path, id): (&str, &str), new_path: &str, new_id: &str) -> bool {
        let same_file = path == new_path || self.moved.get(path).is_some_and(|to| to == new_path);
        (same_file && id == new_id)
            || self.pairs.iter().any(|(before, after)| {
                before.0 == path && before.1 == id && after.0 == new_path && after.1 == new_id
            })
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
    let mut steps: Vec<(&Commit, Checked, PrTests)> = Vec::new();
    let pr_style = analysis.net.is_some();
    for step in &analysis.steps {
        let notes = if pr_style {
            &mut step_notes
        } else {
            &mut notes
        };
        let (checked, step_pr) =
            check(repo, &step.comparison, config, pr_style, &mut carry, notes)?;
        steps.push((&step.commit, checked, step_pr));
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
                        .filter(|(_, step, _)| {
                            step.findings.iter().any(|f| related(finding, f, &pr))
                        })
                        .map(|(commit, _, _)| *commit)
                        .collect()
                })
                .collect();
            findings.extend(decide(repo, &net, commits, config)?);
            // The files each commit's later ones change: what they don't, lands as it is.
            let mut touched_later: Vec<Later> = vec![Later::default(); steps.len()];
            for i in (1..steps.len()).rev() {
                let mut later = touched_later[i].clone();
                for file in &steps[i].1.comparison.files {
                    later.paths.extend(file.path_before.as_deref());
                    later.paths.extend(file.path_after.as_deref());
                    later.tests |= matches!(file.kind, FileKind::Test | FileKind::TestSupport);
                }
                touched_later[i - 1] = later;
            }
            // What the whole PR doesn't have: undone, or about code the PR added and still
            // weaker at its head.
            for ((commit, mut step, step_pr), touched) in steps.into_iter().zip(touched_later) {
                step.findings
                    .retain(|finding| !net.findings.iter().any(|f| related(f, finding, &pr)));
                let added = added_in(repo, &net, &pr, &step.findings)?;
                let kept = still_weaker(
                    repo,
                    (commit, &step, &step_pr),
                    &net,
                    config,
                    &added,
                    &touched,
                    &mut notes,
                );
                let commits = vec![vec![commit]; step.findings.len()];
                for (mut reported, kept) in
                    decide(repo, &step, commits, config)?.into_iter().zip(kept)
                {
                    reported.status = match reported.status {
                        Status::Shown if kept => Status::PrAdded,
                        Status::Allowed { .. } | Status::Hidden if kept => reported.status,
                        _ => Status::Transient,
                    };
                    findings.push(reported);
                }
            }
        }
        // Each commit on its own.
        None => {
            for (commit, step, _) in steps {
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

/// Runs the rules on one comparison, leaving out the rules that are off. With `trace` (a
/// PR-style check), also says what it did to its test files.
fn check<'a>(
    repo: &'a Repo,
    comparison: &'a Comparison,
    config: &'a Config,
    trace: bool,
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
    let pr = if trace {
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

/// What the commits after one in a PR change.
#[derive(Clone, Default)]
struct Later<'a> {
    /// Every path they change.
    paths: HashSet<&'a str>,
    /// Whether any of those is a test or test-support file.
    tests: bool,
}

/// For each of a commit's findings about code the PR added (`added`), whether the PR's head
/// is still weaker than the code before that commit. When no later commit touches its file
/// (and its tests weren't deleted, to maybe come back in another one), it is. Otherwise the rules run again from before that commit to the head, across every
/// file between them, and must find the same test (followed through renames and moves) with
/// the same code lost, or deleted or skipped; or its file gone with its tests. A test the PR
/// added, deleted and put back, or moved elsewhere whole, was undone. If that comparison
/// can't be made, the finding counts as added by the PR, with a note.
fn still_weaker(
    repo: &Repo,
    (commit, step, step_pr): (&Commit, &Checked, &PrTests),
    net: &Checked,
    config: &Config,
    added: &[bool],
    later: &Later,
    notes: &mut Vec<String>,
) -> Vec<bool> {
    // Still open: its file changes later, or its tests are gone from the file (deleted with
    // it, or alone) and a later commit changes test files, where they may come back.
    let open: Vec<bool> = step
        .findings
        .iter()
        .zip(added)
        .map(|(finding, &added)| {
            let deleted = finding.rule == RuleId::TestDisabled && finding.line.is_none();
            added && (later.paths.contains(finding.path.as_str()) || (deleted && later.tests))
        })
        .collect();
    if !open.contains(&true) {
        return added.to_vec();
    }
    match at_head(repo, step, step_pr, net, config) {
        Ok(kept) => added
            .iter()
            .zip(open)
            .zip(kept)
            .map(|((&added, open), kept)| added && (!open || kept))
            .collect(),
        Err(err) => {
            notes.push(format!(
                "couldn't tell whether what {} weakened was undone later ({err}), so it counts as added by the PR",
                commit.short_sha()
            ));
            added.to_vec()
        }
    }
}

/// For each of a commit's findings, whether the PR's head is still weaker in the same way
/// than the code before that commit (see `still_weaker`).
fn at_head(
    repo: &Repo,
    step: &Checked,
    step_pr: &PrTests,
    net: &Checked,
    config: &Config,
) -> Result<Vec<bool>, String> {
    let (Rev::Commit(before), Rev::Commit(head)) = (&step.comparison.before, &net.comparison.after)
    else {
        return Ok(vec![true; step.findings.len()]);
    };
    let comparison = repo
        .compare(before, head, &config.classifier)
        .map_err(|err| err.to_string())?;
    // Notes about it were already given for the commit or the whole PR.
    let (now, head_pr) = check(repo, &comparison, config, true, &mut None, &mut Vec::new())?;
    // Files gone at the head whose tests no longer run (they didn't move): whatever was
    // weakened in them is gone too.
    let gone: HashSet<&str> = comparison
        .files
        .iter()
        .filter(|file| file.path_after.is_none())
        .filter_map(|file| file.path_before.as_deref())
        .filter(|path| {
            now.findings
                .iter()
                .any(|f| f.rule == RuleId::TestDisabled && f.path == *path)
        })
        .collect();
    Ok(step
        .findings
        .iter()
        .map(|finding| {
            // Where it was before the commit, which may have renamed its file.
            let file = step
                .comparison
                .files
                .iter()
                .find(|file| file.path_after.as_deref() == Some(finding.path.as_str()))
                .and_then(|file| file.path_before.as_deref())
                .unwrap_or(&finding.path);
            gone.contains(file)
                || now
                    .findings
                    .iter()
                    .any(|f| persists(f, finding, file, step_pr, &head_pr))
        })
        .collect())
}

/// Whether `now`, a finding from before a commit to the PR's head, is the weakening `step`
/// found in that commit, still there. `file` is where `step` was before the commit; `step_pr`
/// says what the commit renamed or moved, `head_pr` what changed from there to the head.
/// Findings about tests need the same test, followed through renames and moves, with code
/// both lost, or the test deleted or skipped at the head. Other findings (CI config) need
/// the same file and rule, and the same message or code lost.
fn persists(
    now: &Finding,
    step: &Finding,
    file: &str,
    step_pr: &PrTests,
    head_pr: &PrTests,
) -> bool {
    let lost = now.before.iter().any(|line| step.before.contains(line));
    let origins: Vec<(String, String)> = step
        .all_tests()
        .map(|test| step_pr.origin(&step.path, test, file))
        .collect();
    if origins.is_empty() || now.all_tests().next().is_none() {
        let same_file = now.path == file || head_pr.moved.get(file) == Some(&now.path);
        return same_file && now.rule == step.rule && (now.message == step.message || lost);
    }
    let same_test = origins.iter().any(|(path, id)| {
        now.all_tests()
            .any(|new| head_pr.became((path, id), &now.path, new))
    });
    same_test && (lost || now.rule == RuleId::TestDisabled)
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
