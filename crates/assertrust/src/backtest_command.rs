//! `assertrust backtest`: replays a branch's history the way CI would have checked it, to
//! show what assertrust would have flagged and, with a labels file, how often it's right
//! (`docs/architecture.md`, step 6, "Backtest").
//!
//! Each commit on the branch's first-parent line landed one way:
//! - a merge commit merged a PR with its commits: checked PR-style, `C^1...C^2`;
//! - a squash-merged PR (`Fix refunds (#12)`) is checked PR-style against its own commits
//!   when they've been fetched (`refs/pull/12/head`), else as one commit;
//! - any other commit was pushed straight to the branch: checked on its own.
//!
//! ```text
//! PR #12  1a2b3c4  Fix refunds (#12) · squashed, 3 commits
//! test/refund.test.ts:8  HIGH  check-removed  [unexplained]
//!   removed expect(refund.status).toBe('done') · 9f8e7d6
//!
//! 100 commits: 61 PRs, 39 pushes · 1 finding · 0.8s
//! labels: 1 unexplained · precision 100% (1 of 1 right) · high 1 of 1
//! ```
//!
//! With `--judge`, every finding a PR check would report goes to the AI judge (step 7), and
//! changed outcomes too, with no cap. With labels, its verdicts are matched against them:
//!
//! ```text
//! judge on shown findings:
//!   wrong (12): 9 fine, 2 unsure, 1 coverage lost · 1 would fail (8%)
//!   unexplained (3): 3 suspicious · 3 would fail (100%)
//! ```
//!
//! The first commit of a repo isn't checked: there's nothing before it to weaken.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use assertrust_git::{Commit, Mode, Repo};
use assertrust_judge::{Judgment, Prompt, PullRequest, Verdict};
use assertrust_rules::{Confidence, Finding, RuleId};
use serde_json::{Value, json};

use crate::check_command::{self, judged};
use crate::cli::{BacktestArgs, BacktestFormat};
use crate::config::{Config, JudgeConfig};
use crate::json;
use crate::judge::{self, Scope, Summary};
use crate::labels::{Entry, Label, Labels, Pairing};
use crate::output::{count, line};
use crate::report::{self, Reported, Status};

/// How many commits are replayed when neither `--range`, `--last` nor the labels file says.
const DEFAULT_LAST: u64 = 100;

/// Exit code when, with labels, a finding isn't labelled, a right one is gone, or a commit
/// couldn't be checked.
const EXIT_FINDINGS: u8 = 1;

/// The output format's version.
const SCHEMA: u32 = 1;

pub fn run(args: &BacktestArgs) -> Result<ExitCode, String> {
    let started = Instant::now();
    let dir = std::env::current_dir().map_err(|err| format!("can't read current dir: {err}"))?;
    let repo = Repo::open(&dir).map_err(|err| err.to_string())?;
    let labels = args
        .labels
        .as_deref()
        .map(|path| Labels::read(path, args.update_labels))
        .transpose()?;
    let (range, commits) = walk(&repo, args, labels.as_ref())?;
    let mut units = check_all(&repo, &commits, args);
    let judged = args
        .judge
        .on()
        .then(|| judge_all(&repo, &mut units, &judge_config(args)))
        .transpose()?;
    let scored = labels.as_ref().map(|labels| Scored::new(labels, &units));
    let output = match args.format {
        BacktestFormat::Terminal => render(&units, scored.as_ref(), judged.as_ref(), args, started),
        BacktestFormat::Json => render_json(&range, &units, scored.as_ref(), judged.as_ref()),
    };
    crate::print_stdout(&output)?;
    if let (Some(path), Some(labels), Some(scored)) = (&args.labels, &labels, &scored)
        && args.update_labels
    {
        update(&repo, path, labels, scored, &units, range)?;
    }
    let failing = scored.as_ref().is_some_and(|scored| {
        scored.not_labelled() > 0
            || scored.missed().next().is_some()
            || units.iter().any(|unit| unit.error.is_some())
    });
    Ok(if failing {
        ExitCode::from(EXIT_FINDINGS)
    } else {
        ExitCode::SUCCESS
    })
}

/// The judge's settings for a backtest: the flags', whatever each repo's config says, since
/// a backtest measures one model across them all.
fn judge_config(args: &BacktestArgs) -> JudgeConfig {
    args.judge.apply(None)
}

/// What the judge did over the whole backtest, and notes on what went wrong.
struct Judged {
    summary: Summary,
    notes: Vec<String>,
    /// The verdicts that fail a check.
    fail_on: Vec<Verdict>,
}

/// Asks the judge about every finding that has a prompt, all at once, and puts the verdicts
/// on them: a changed outcome it doesn't call fine is then shown, as a check would.
fn judge_all(repo: &Repo, units: &mut [Unit], config: &JudgeConfig) -> Result<Judged, String> {
    let prompts: Vec<Prompt> = units
        .iter()
        .flat_map(|unit| &unit.findings)
        .filter_map(|owned| owned.prompt.clone())
        .collect();
    let asked = judge::ask(&judge::cache_dir(repo)?, config, &prompts);
    let mut judgments = asked.judgments.into_iter();
    for owned in units
        .iter_mut()
        .flat_map(|unit| &mut unit.findings)
        .filter(|owned| owned.prompt.is_some())
    {
        owned.judgment = judgments.next().flatten();
        if let Some(judgment) = &owned.judgment
            && owned.status == Status::Hidden
            && judgment.verdict != Verdict::Fine
        {
            // As a check would show it: on code the PR added, only with --show-pr-added.
            if owned.added {
                owned.status = Status::PrAdded;
                owned.quiet = owned.show_pr_added;
                owned.verbose = owned.show_pr_added;
            } else {
                owned.status = Status::Shown;
                owned.quiet = true;
            }
        }
    }
    Ok(Judged {
        summary: asked.summary,
        notes: asked.notes,
        fail_on: config.fail_on.clone(),
    })
}

/// The commits to replay, oldest first, and their range with full SHAs (`BASE..HEAD`).
fn walk(
    repo: &Repo,
    args: &BacktestArgs,
    labels: Option<&Labels>,
) -> Result<(String, Vec<Commit>), String> {
    let range = match (&args.range, args.last) {
        (Some(range), _) => Some(range.clone()),
        (None, Some(_)) => None,
        (None, None) => labels.and_then(|labels| labels.range.clone()),
    };
    let resolve = |rev: &str| repo.resolve(rev).map_err(|err| err.to_string());
    let (base, head, mut commits) = match range {
        Some(range) => {
            let (base, head) = range
                .split_once("..")
                .filter(|(base, head)| !base.is_empty() && !head.starts_with('.'))
                .ok_or_else(|| format!("expected BASE..HEAD, got `{range}`"))?;
            let base = resolve(base)?;
            let head = resolve(if head.is_empty() { "HEAD" } else { head })?;
            let commits = repo
                .first_parent(Some(&base), &head, None)
                .map_err(|err| err.to_string())?;
            (base, head, commits)
        }
        None => {
            let last = args.last.unwrap_or(DEFAULT_LAST);
            let head = resolve("HEAD")?;
            let commits = repo
                .first_parent(None, &head, usize::try_from(last).ok())
                .map_err(|err| err.to_string())?;
            let base = match commits.first() {
                // The repo's first commit is where the range starts.
                Some(first) if first.parents.is_empty() => first.sha.clone(),
                Some(first) => first.parents[0].clone(),
                None => return Err("no commits to replay".to_owned()),
            };
            (base, head, commits)
        }
    };
    // Nothing before it to weaken.
    commits.retain(|commit| !commit.parents.is_empty());
    Ok((format!("{base}..{head}"), commits))
}

/// Checks each commit, several at once (each check only reads the repo), in their order.
fn check_all(repo: &Repo, commits: &[Commit], args: &BacktestArgs) -> Vec<Unit> {
    let workers = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .clamp(1, commits.len().max(1));
    let next = AtomicUsize::new(0);
    let mut units: Vec<(usize, Unit)> = thread::scope(|scope| {
        let threads: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(commit) = commits.get(i) else {
                            break done;
                        };
                        done.push((i, Unit::check(repo, commit.clone(), args)));
                    }
                })
            })
            .collect();
        threads
            .into_iter()
            .flat_map(|thread| thread.join().expect("a backtest thread panicked"))
            .collect()
    });
    units.sort_by_key(|(i, _)| *i);
    units.into_iter().map(|(_, unit)| unit).collect()
}

/// How a commit got onto the branch.
enum Landing {
    /// A PR merged with a merge commit.
    Merge { pr: Option<u64> },
    /// A PR squashed into one commit, and its own last commit when fetched.
    Squash { pr: u64, head: Option<String> },
    /// Pushed straight to the branch.
    Push,
}

impl Landing {
    /// How `commit` landed, and a note when its subject names a PR whose fetched commits
    /// aren't what it landed.
    fn of(repo: &Repo, commit: &Commit) -> (Self, Option<String>) {
        if commit.is_merge() {
            let pr = merged_pr(commit.subject());
            return (Self::Merge { pr }, None);
        }
        let Some(pr) = squashed_pr(commit.subject()) else {
            return (Self::Push, None);
        };
        let head = repo.resolve(&format!("refs/pull/{pr}/head")).ok();
        match head {
            Some(head) if !squashed_from(repo, commit, &head) => (
                Self::Push,
                Some(format!(
                    "ends in (#{pr}), but PR #{pr}'s commits don't add up to it (a PR stacked on another, or one it only cites), so it's checked on its own"
                )),
            ),
            head => (Self::Squash { pr, head }, None),
        }
    }

    fn pr(&self) -> Option<u64> {
        match self {
            Self::Merge { pr } => *pr,
            Self::Squash { pr, .. } => Some(*pr),
            Self::Push => None,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Merge { .. } => "merge",
            Self::Squash { .. } => "squash",
            Self::Push => "push",
        }
    }
}

/// Whether `head`, the last commit of a PR, is what the squash merge `commit` landed: each
/// file `commit` changed is one that merging `head` into the commit before changes too. A
/// commit that only cites a PR (`Address review comments from (#12)`) changes files the
/// PR's commits don't, or the PR is already in. If git can't tell (no shared history in a
/// shallow clone, an old git), it's taken as the PR's, so the check reports why it can't be
/// done.
fn squashed_from(repo: &Repo, commit: &Commit, head: &str) -> bool {
    repo.squashes(&commit.parents[0], head, &commit.sha)
        .unwrap_or(true)
}

/// `Merge pull request #12 from org/branch` → 12.
fn merged_pr(subject: &str) -> Option<u64> {
    subject
        .strip_prefix("Merge pull request #")?
        .split(' ')
        .next()?
        .parse()
        .ok()
}

/// `Fix refunds (#12)` → 12, as GitHub names a squash merge.
fn squashed_pr(subject: &str) -> Option<u64> {
    subject
        .trim_end()
        .strip_suffix(')')?
        .rsplit_once("(#")?
        .1
        .parse()
        .ok()
}

/// One commit on the branch, checked as CI would have.
struct Unit {
    commit: Commit,
    landing: Landing,
    /// The commits its check walked: the PR's own, or the pushed one.
    walked: usize,
    findings: Vec<Owned>,
    notes: Vec<String>,
    /// Why it couldn't be checked.
    error: Option<String>,
}

/// A finding of one unit's check, kept after the check is done.
struct Owned {
    finding: Finding,
    status: Status,
    commits: Vec<Commit>,
    /// Its rule is left to the judge, so only `-v` shows it.
    judged: bool,
    /// Labels are about it: a PR check would report it (shown, allowed, or added by its PR,
    /// shown only with `--show-pr-added`), or it's left to the judge and not allowed (a
    /// changed outcome, shown only with `-v`, or by the judge).
    reported: bool,
    /// Shown without `-v`, and with it.
    quiet: bool,
    verbose: bool,
    /// About code its PR added (`judge::pr_added`).
    added: bool,
    /// Whether its check shows findings on code a PR added.
    show_pr_added: bool,
    /// What the judge is sent about it, with `--judge`.
    prompt: Option<Prompt>,
    /// What the judge said.
    judgment: Option<Judgment>,
}

impl Owned {
    fn shown(&self, verbose: bool) -> bool {
        if verbose { self.verbose } else { self.quiet }
    }

    fn view(&self) -> Reported<'_> {
        Reported {
            finding: self.finding.clone(),
            commits: self.commits.iter().collect(),
            status: self.status.clone(),
            comparison: None,
            judgment: self.judgment.clone(),
        }
    }
}

impl Unit {
    fn check(repo: &Repo, commit: Commit, args: &BacktestArgs) -> Self {
        let (landing, note) = Landing::of(repo, &commit);
        let parent = &commit.parents[0];
        // What CI checked, and the commit it ran on.
        let (range, head) = match &landing {
            Landing::Merge { .. } => (
                format!("{parent}...{}", commit.parents[1]),
                commit.parents[1].clone(),
            ),
            Landing::Squash {
                head: Some(head), ..
            } => (format!("{parent}...{head}"), head.clone()),
            Landing::Squash { head: None, .. } | Landing::Push => {
                (format!("{parent}..{}", commit.sha), commit.sha.clone())
            }
        };
        let mut unit = Self {
            commit,
            landing,
            walked: 0,
            findings: Vec::new(),
            notes: Vec::new(),
            error: None,
        };
        // The PR's title, from the commit that landed it; its description isn't in git.
        let pull_request = unit.landing.pr().map(|_| PullRequest {
            title: title(&unit.commit).to_owned(),
            body: String::new(),
        });
        match run_check(repo, &Mode::Range(range), &head, args, pull_request) {
            Ok((walked, findings, notes)) => {
                unit.walked = walked;
                unit.findings = findings;
                unit.notes = notes;
            }
            Err(err) => unit.error = Some(err),
        }
        unit.notes.splice(0..0, note);
        unit
    }

    /// `PR #12  1a2b3c4  Fix refunds (#12) · squashed, 3 commits`
    fn header(&self) -> String {
        let short = self.commit.short_sha();
        let title = title(&self.commit);
        let walked = count(self.walked, "commit");
        let (name, how) = match &self.landing {
            Landing::Merge { pr: Some(pr) } => (format!("PR #{pr}"), walked),
            Landing::Merge { pr: None } => ("merge".to_owned(), walked),
            Landing::Squash {
                pr, head: Some(_), ..
            } => (format!("PR #{pr}"), format!("squashed, {walked}")),
            Landing::Squash { pr, head: None } => (
                format!("PR #{pr}"),
                "squashed, checked as one commit".to_owned(),
            ),
            Landing::Push => ("push".to_owned(), String::new()),
        };
        if how.is_empty() || self.error.is_some() {
            format!("{name}  {short}  {title}")
        } else {
            format!("{name}  {short}  {title} · {how}")
        }
    }
}

/// A PR's title: a GitHub merge commit's subject only says where it came from, and the
/// title is the message's next line.
fn title(commit: &Commit) -> &str {
    let subject = commit.subject();
    if subject.starts_with("Merge pull request #")
        && let Some(title) = commit
            .message
            .lines()
            .skip(1)
            .map(str::trim)
            .find(|text| !text.is_empty())
    {
        return title;
    }
    subject
}

/// Runs `check` on one landing: how many commits it walked, its findings and notes. With
/// `--judge`, each finding the judge is asked about gets its prompt.
fn run_check(
    repo: &Repo,
    mode: &Mode,
    head: &str,
    args: &BacktestArgs,
    pull_request: Option<PullRequest>,
) -> Result<(usize, Vec<Owned>, Vec<String>), String> {
    let (mut config, mut notes) = Config::as_ci_ran(repo, mode, head)?;
    config.show_transient |= args.show_transient;
    config.show_pr_added |= args.show_pr_added;
    let analysis = repo
        .analyze(mode, &config.classifier)
        .map_err(|err| err.to_string())?;
    let report = report::build(repo, &analysis, &config)?;
    notes.extend(report.notes.iter().cloned());
    let mut prompts: Vec<Option<Prompt>> = vec![None; report.findings.len()];
    if args.judge.on() {
        let scope = Scope {
            pr_added: true,
            cap: None,
        };
        let (picked, _) = judge::select(&report, &scope, analysis.net.as_ref());
        let built = judge::prompts(repo, &report, &picked, pull_request.as_ref());
        for (i, prompt) in picked.into_iter().zip(built) {
            prompts[i] = Some(prompt);
        }
    }
    let findings = report
        .findings
        .iter()
        .map(|reported| Owned {
            finding: reported.finding.clone(),
            status: reported.status.clone(),
            commits: reported
                .commits
                .iter()
                .map(|&commit| commit.clone())
                .collect(),
            judged: judged(reported, &config),
            reported: match reported.status {
                Status::Shown | Status::PrAdded | Status::Hidden => true,
                Status::Allowed { .. } => !judged(reported, &config),
                Status::Transient => false,
            },
            quiet: check_command::visible(reported, &config, false),
            verbose: check_command::visible(reported, &config, true),
            added: judge::pr_added(reported, analysis.net.as_ref()),
            show_pr_added: config.show_pr_added,
            prompt: None,
            judgment: None,
        })
        .zip(prompts)
        .map(|(owned, prompt)| Owned { prompt, ..owned })
        .collect();
    Ok((analysis.steps.len(), findings, notes))
}

/// The labels matched against what the backtest found.
struct Scored {
    /// Per unit and finding (reported ones only): the entry it matched, and that entry's
    /// label; `None` when it matched none, so it isn't labelled.
    tags: Vec<Vec<Option<Tag>>>,
    /// The entries some finding matched.
    used: HashSet<usize>,
    /// Entries about a commit that was checked, that no finding matched.
    lost: Vec<Entry>,
}

#[derive(Clone)]
struct Tag {
    /// The finding as an entry, with the label and note it matched, if any.
    entry: Entry,
    pairing: Option<Pairing>,
}

impl Tag {
    fn label(&self) -> Option<Label> {
        self.entry.label
    }

    /// `[unexplained]`, `[wrong, carried over]`, `[not labelled]`.
    fn text(&self) -> String {
        let label = self.label().map_or("not labelled", Label::as_str);
        match self.pairing {
            Some(Pairing::Carried(_)) => format!("{label}, carried over"),
            _ => label.to_owned(),
        }
    }
}

impl Scored {
    fn new(labels: &Labels, units: &[Unit]) -> Self {
        let mut found = Vec::new();
        for unit in units {
            for owned in unit.findings.iter().filter(|owned| owned.reported) {
                found.push(Entry::new(
                    &unit.commit.sha,
                    unit.landing.pr(),
                    &owned.finding,
                ));
            }
        }
        let pairs = labels.pair(&found);
        let used: HashSet<usize> = pairs
            .iter()
            .flatten()
            .map(|pairing| pairing.entry())
            .collect();
        let mut tags = Vec::new();
        let mut next = found.into_iter().zip(pairs);
        for unit in units {
            let row = unit
                .findings
                .iter()
                .map(|owned| {
                    if !owned.reported {
                        return None;
                    }
                    let (mut entry, pairing) = next.next().expect("one entry per reported finding");
                    if let Some(pairing) = pairing {
                        let labelled = &labels.entries[pairing.entry()];
                        entry.label = labelled.label;
                        entry.note.clone_from(&labelled.note);
                    }
                    Some(Tag { entry, pairing })
                })
                .collect();
            tags.push(row);
        }
        // Only commits that were checked can say a finding is gone.
        let checked: Vec<&str> = units
            .iter()
            .filter(|unit| unit.error.is_none())
            .map(|unit| unit.commit.sha.as_str())
            .collect();
        let lost = labels
            .entries
            .iter()
            .enumerate()
            .filter(|(i, entry)| !used.contains(i) && checked.iter().any(|sha| entry.about(sha)))
            .map(|(_, entry)| entry.clone())
            .collect();
        Self { tags, used, lost }
    }

    fn all(&self) -> impl Iterator<Item = &Tag> {
        self.tags.iter().flatten().flatten()
    }

    fn not_labelled(&self) -> usize {
        self.all().filter(|tag| tag.label().is_none()).count()
    }

    /// Right findings that are gone.
    fn missed(&self) -> impl Iterator<Item = &Entry> {
        self.lost
            .iter()
            .filter(|entry| entry.label.is_some_and(Label::right))
    }

    /// False alarms that are gone.
    fn fixed(&self) -> impl Iterator<Item = &Entry> {
        self.lost
            .iter()
            .filter(|entry| entry.label == Some(Label::Wrong))
    }
}

fn render(
    units: &[Unit],
    scored: Option<&Scored>,
    judged: Option<&Judged>,
    args: &BacktestArgs,
    started: Instant,
) -> String {
    let mut out = String::new();
    let mut first = true;
    for (u, unit) in units.iter().enumerate() {
        let visible: Vec<(usize, &Owned)> = unit
            .findings
            .iter()
            .enumerate()
            .filter(|(_, owned)| owned.shown(args.verbose))
            .collect();
        if visible.is_empty() && unit.notes.is_empty() && unit.error.is_none() {
            continue;
        }
        if !first {
            line(&mut out, "");
        }
        first = false;
        line(&mut out, &unit.header());
        for (i, (f, owned)) in visible.iter().enumerate() {
            if args.verbose && i > 0 {
                line(&mut out, "");
            }
            let tag = scored
                .and_then(|scored| scored.tags[u][*f].as_ref())
                .map(Tag::text);
            check_command::finding(&mut out, &owned.view(), args.verbose, tag.as_deref());
        }
        for note in &unit.notes {
            line(&mut out, &format!("note: {note}"));
        }
        if let Some(error) = &unit.error {
            line(&mut out, &format!("couldn't check: {error}"));
        }
    }
    if !first {
        line(&mut out, "");
    }
    let unfetched = units
        .iter()
        .filter(|unit| matches!(unit.landing, Landing::Squash { head: None, .. }))
        .count();
    if unfetched > 0 {
        line(
            &mut out,
            &format!(
                "note: {} checked as one commit, since their own commits aren't fetched \
                 (git fetch origin refs/pull/N/head:refs/pull/N/head)",
                count(unfetched, "squash-merged PR")
            ),
        );
    }
    line(
        &mut out,
        &summary(units, args, started.elapsed().as_secs_f64()),
    );
    if let Some(scored) = scored {
        label_summary(&mut out, units, scored, args.verbose);
    }
    if let Some(judged) = judged {
        for note in &judged.notes {
            line(&mut out, &format!("note: {note}"));
        }
        line(&mut out, &judged.summary.line());
        if let Some(scored) = scored {
            judge_summary(&mut out, units, scored, &judged.fail_on);
        }
    }
    out
}

/// Each kind's labelled findings, with how many got each verdict.
/// Each kind: its JSON key, its name in the terminal, and its rows.
type Matrix = Vec<(
    &'static str,
    &'static str,
    Vec<(Label, Vec<(Option<Verdict>, usize)>)>,
)>;

/// How the judge's verdicts fall on each label, for each kind of finding.
fn matrix(units: &[Unit], scored: &Scored) -> Matrix {
    let Split {
        shown,
        pr_added,
        judged,
    } = labelled(units, scored);
    [
        ("shown", "shown findings", shown),
        ("pr_added", "findings on code its PR added", pr_added),
        ("judged", "changed outcomes", judged),
    ]
    .into_iter()
    .filter(|(_, _, labelled)| labelled.iter().any(|(_, owned)| owned.prompt.is_some()))
    .map(|(key, kind, labelled)| {
        let rows = [Label::Wrong, Label::Unexplained, Label::Intended]
            .into_iter()
            .filter_map(|label| {
                let of: Vec<Option<Verdict>> = labelled
                    .iter()
                    .filter(|(l, owned)| *l == label && owned.prompt.is_some())
                    .map(|(_, owned)| owned.judgment.as_ref().map(|j| j.verdict))
                    .collect();
                if of.is_empty() {
                    return None;
                }
                let counts = Verdict::ALL
                    .into_iter()
                    .map(Some)
                    .chain([None])
                    .map(|verdict| (verdict, of.iter().filter(|v| **v == verdict).count()))
                    .filter(|(_, n)| *n > 0)
                    .collect();
                Some((label, counts))
            })
            .collect();
        (key, kind, rows)
    })
    .collect()
}

/// `judge on shown findings:`, then per label how the verdicts fall and how many would fail
/// a check: on a wrong finding that's a false alarm the judge confirmed, on an unexplained
/// one a weakening it caught.
fn judge_summary(out: &mut String, units: &[Unit], scored: &Scored, fail_on: &[Verdict]) {
    for (_, kind, rows) in matrix(units, scored) {
        line(out, &format!("judge on {kind}:"));
        for (label, counts) in rows {
            let total: usize = counts.iter().map(|(_, n)| n).sum();
            let failing: usize = counts
                .iter()
                .filter(|(verdict, _)| verdict.is_some_and(|v| fail_on.contains(&v)))
                .map(|(_, n)| n)
                .sum();
            let split: Vec<String> = counts
                .iter()
                .map(|(verdict, n)| match verdict {
                    Some(verdict) => format!("{n} {}", verdict.words()),
                    None => format!("{n} without a verdict"),
                })
                .collect();
            line(
                out,
                &format!(
                    "  {} ({total}): {} · {failing} would fail ({}%)",
                    label.as_str(),
                    split.join(", "),
                    failing * 100 / total
                ),
            );
        }
    }
}

/// `100 commits: 61 PRs, 39 pushes · 3 findings · 2 changed outcomes hidden (-v shows them)
/// · 1 undone later hidden (--show-transient shows them) · 0.8s`
fn summary(units: &[Unit], args: &BacktestArgs, seconds: f64) -> String {
    let prs = units
        .iter()
        .filter(|unit| unit.landing.pr().is_some())
        .count();
    // Merges that aren't PRs: `Merge branch 'main' of …` from a pull on the branch.
    let merges = units
        .iter()
        .filter(|unit| matches!(unit.landing, Landing::Merge { pr: None }))
        .count();
    let mut landed = vec![count(prs, "PR")];
    if merges > 0 {
        landed.push(count(merges, "other merge"));
    }
    landed.push(pushes(units.len() - prs - merges));
    let mut parts = vec![format!(
        "{}: {}",
        count(units.len(), "commit"),
        landed.join(", ")
    )];
    let findings = || units.iter().flat_map(|unit| &unit.findings);
    let with = |wanted: fn(&Owned) -> bool| findings().filter(|owned| wanted(owned)).count();
    let shown = with(|owned| owned.status == Status::Shown && owned.quiet)
        + if args.verbose {
            with(|owned| owned.status == Status::Hidden)
        } else {
            0
        };
    parts.push(if shown == 0 {
        "no findings".to_owned()
    } else {
        count(shown, "finding")
    });
    let hidden: Vec<&Owned> = findings()
        .filter(|owned| owned.status == Status::Hidden)
        .collect();
    if !args.verbose && !hidden.is_empty() {
        let outcomes = hidden
            .iter()
            .all(|owned| owned.finding.rule == RuleId::OutcomeChanged);
        parts.push(if outcomes {
            format!(
                "{} hidden (-v shows them)",
                count(hidden.len(), "changed outcome")
            )
        } else {
            format!("{} for the judge hidden (-v shows them)", hidden.len())
        });
    }
    // Findings left to the judge stay out of the counts too, unless `-v` shows them.
    let counted = |owned: &Owned| args.verbose || !owned.judged;
    let (added_shown, added_hidden): (Vec<&Owned>, Vec<&Owned>) = findings()
        .filter(|owned| owned.status == Status::PrAdded && counted(owned))
        .partition(|owned| owned.shown(args.verbose));
    parts.extend(check_command::pr_added(
        added_shown.len(),
        true,
        "its PR",
        true,
    ));
    parts.extend(check_command::pr_added(
        added_hidden.len(),
        false,
        "its PR",
        true,
    ));
    let allowed = findings()
        .filter(|owned| matches!(owned.status, Status::Allowed { .. }) && counted(owned))
        .count();
    if allowed > 0 {
        parts.push(format!("{allowed} allowed"));
    }
    let transient = findings()
        .filter(|owned| {
            owned.status == Status::Transient && !owned.shown(args.verbose) && counted(owned)
        })
        .count();
    if transient > 0 {
        parts.push(format!(
            "{transient} undone later hidden (--show-transient shows them)"
        ));
    }
    let errors = units.iter().filter(|unit| unit.error.is_some()).count();
    if errors > 0 {
        parts.push(format!("{errors} couldn't be checked"));
    }
    parts.push(format!("{seconds:.1}s"));
    parts.join(" · ")
}

/// `1 push`, `2 pushes`.
fn pushes(n: usize) -> String {
    if n == 1 {
        "1 push".to_owned()
    } else {
        format!("{n} pushes")
    }
}

/// Each labelled finding with its label, split by how a check shows it, since each kind is
/// counted apart.
#[derive(Default)]
struct Split<'a> {
    /// Shown, or allowed.
    shown: Vec<(Label, &'a Owned)>,
    /// On code its PR added: hidden by default.
    pr_added: Vec<(Label, &'a Owned)>,
    /// Left to the judge (changed outcomes): hidden until it's on.
    judged: Vec<(Label, &'a Owned)>,
}

fn labelled<'a>(units: &'a [Unit], scored: &Scored) -> Split<'a> {
    let mut split = Split::default();
    let tagged = units
        .iter()
        .zip(&scored.tags)
        .flat_map(|(unit, row)| unit.findings.iter().zip(row))
        .filter_map(|(owned, tag)| Some((tag.as_ref()?.label()?, owned)));
    for (label, owned) in tagged {
        // Left to the judge, whether it showed them or not.
        if owned.judged {
            split.judged.push((label, owned));
        } else if owned.status == Status::PrAdded {
            split.pr_added.push((label, owned));
        } else {
            split.shown.push((label, owned));
        }
    }
    split
}

/// What the labels say: how often assertrust is right, and what isn't labelled or is gone.
/// Findings on code a PR added and changed outcomes, hidden by default, are counted apart.
fn label_summary(out: &mut String, units: &[Unit], scored: &Scored, verbose: bool) {
    let Split {
        shown,
        pr_added,
        judged,
    } = labelled(units, scored);
    if shown.is_empty() && pr_added.is_empty() && judged.is_empty() {
        line(out, "labels: none of the findings are labelled yet");
    }
    if !shown.is_empty() {
        let by_level: Vec<String> = [Confidence::High, Confidence::Medium, Confidence::Low]
            .into_iter()
            .filter_map(|level| {
                let (right, of) =
                    score(shown.iter().filter(|(_, o)| o.finding.confidence == level));
                (of > 0).then(|| format!("{level} {right} of {of}"))
            })
            .collect();
        line(
            out,
            &format!("labels: {} · {}", precision(&shown), by_level.join(", ")),
        );
        let by_rule: Vec<String> = RuleId::ALL
            .into_iter()
            .filter_map(|rule| {
                let (right, of) = score(shown.iter().filter(|(_, o)| o.finding.rule == rule));
                (of > 0).then(|| format!("{rule} {right} of {of}"))
            })
            .collect();
        line(out, &format!("by rule: {}", by_rule.join(", ")));
    }
    if !pr_added.is_empty() {
        line(
            out,
            &format!(
                "on code its PR added (hidden by default): {}",
                precision(&pr_added)
            ),
        );
    }
    if !judged.is_empty() {
        line(
            out,
            &format!(
                "changed outcomes (hidden until the judge is on): {}",
                precision(&judged)
            ),
        );
    }
    // Those not labelled are marked, unless hidden: say how to see them.
    let unlabelled: Vec<&Owned> = units
        .iter()
        .zip(&scored.tags)
        .flat_map(|(unit, row)| unit.findings.iter().zip(row))
        .filter(|(_, tag)| tag.as_ref().is_some_and(|tag| tag.label().is_none()))
        .map(|(owned, _)| owned)
        .collect();
    let marked = unlabelled.iter().filter(|o| o.shown(verbose)).count();
    let added = unlabelled
        .iter()
        .filter(|o| !o.shown(verbose) && o.status == Status::PrAdded)
        .count();
    let outcomes = unlabelled
        .iter()
        .filter(|o| !o.shown(verbose) && o.status == Status::Hidden)
        .count();
    let total = unlabelled.len();
    match (marked, added, outcomes) {
        (0, 0, 0) => {}
        (_, 0, 0) => line(out, &format!("{total} not labelled yet (marked above)")),
        (0, _, 0) => line(
            out,
            &format!("{total} not labelled yet, on code its PR added (--show-pr-added shows them)"),
        ),
        (0, 0, _) => line(
            out,
            &format!("{total} not labelled yet, changed outcomes (-v shows them)"),
        ),
        _ => {
            let mut parts = Vec::new();
            if marked > 0 {
                parts.push(format!("{marked} marked above"));
            }
            if added > 0 {
                parts.push(format!(
                    "{added} on code its PR added (--show-pr-added shows them)"
                ));
            }
            if outcomes > 0 {
                parts.push(format!(
                    "{} (-v shows them)",
                    count(outcomes, "changed outcome")
                ));
            }
            line(
                out,
                &format!("{total} not labelled yet: {}", parts.join(", ")),
            );
        }
    }
    let missed: Vec<&Entry> = scored.missed().collect();
    if !missed.is_empty() {
        line(out, "no longer found, though labelled right:");
        for entry in missed {
            lost_entry(out, entry);
        }
    }
    let fixed: Vec<&Entry> = scored.fixed().collect();
    if !fixed.is_empty() {
        line(out, "false alarms no longer found:");
        for entry in fixed {
            lost_entry(out, entry);
        }
    }
}

/// How many of these labels say the finding is right, out of how many.
fn score<'a>(labelled: impl Iterator<Item = &'a (Label, &'a Owned)>) -> (usize, usize) {
    labelled.fold((0, 0), |(right, of), (label, _)| {
        (right + usize::from(label.right()), of + 1)
    })
}

/// `2 unexplained, 1 wrong · precision 66% (2 of 3 right)`
fn precision(labelled: &[(Label, &Owned)]) -> String {
    let counts: Vec<String> = [Label::Unexplained, Label::Intended, Label::Wrong]
        .into_iter()
        .filter_map(|label| {
            let n = labelled.iter().filter(|(l, _)| *l == label).count();
            (n > 0).then(|| format!("{n} {}", label.as_str()))
        })
        .collect();
    let (right, of) = score(labelled.iter());
    format!(
        "{} · precision {}% ({right} of {of} right)",
        counts.join(", "),
        right * 100 / of
    )
}

/// `  1a2b3c4d5e6f (PR #12)  test/a.test.ts  check-removed  removed expect(b).toBe(2)  [unexplained]`
fn lost_entry(out: &mut String, entry: &Entry) {
    let pr = entry
        .pr
        .map(|pr| format!(" (PR #{pr})"))
        .unwrap_or_default();
    let label = entry.label.map_or("not labelled", Label::as_str);
    line(
        out,
        &format!(
            "  {}{pr}  {}  {}  {}  [{label}]",
            entry.unit, entry.path, entry.rule, entry.message
        ),
    );
}

fn render_json(
    range: &str,
    units: &[Unit],
    scored: Option<&Scored>,
    judged: Option<&Judged>,
) -> String {
    let units_json: Vec<Value> = units
        .iter()
        .enumerate()
        .map(|(u, unit)| {
            let findings: Vec<Value> = unit
                .findings
                .iter()
                .enumerate()
                .map(|(f, owned)| {
                    let mut value = json::finding(&owned.view());
                    if let Some(scored) = scored
                        && owned.reported
                    {
                        let tag = scored.tags[u][f].as_ref();
                        value["label"] = json!(tag.and_then(Tag::label).map(Label::as_str));
                    }
                    value
                })
                .collect();
            json!({
                "sha": unit.commit.sha,
                "subject": unit.commit.subject(),
                "landing": unit.landing.name(),
                "pr": unit.landing.pr(),
                "fetched": match &unit.landing {
                    Landing::Squash { head, .. } => Some(head.is_some()),
                    _ => None,
                },
                "commits": unit.walked,
                "findings": findings,
                "notes": unit.notes,
                "error": unit.error,
            })
        })
        .collect();
    let mut document = json!({
        "schema": SCHEMA,
        "range": range,
        "units": units_json,
    });
    if let Some(scored) = scored {
        let entries = |entries: Vec<&Entry>| -> Vec<Value> {
            entries
                .into_iter()
                .map(|entry| {
                    json!({
                        "unit": entry.unit,
                        "pr": entry.pr,
                        "rule": entry.rule,
                        "path": entry.path,
                        "test": entry.test,
                        "message": entry.message,
                        "label": entry.label.map(Label::as_str),
                    })
                })
                .collect()
        };
        // Labels of findings on code their PR added and of changed outcomes (hidden by
        // default) are counted apart.
        let Split {
            shown,
            pr_added,
            judged,
        } = labelled(units, scored);
        let with =
            |of: &[(Label, &Owned)], label: Label| of.iter().filter(|(l, _)| *l == label).count();
        document["labels"] = json!({
            "wrong": with(&shown, Label::Wrong),
            "intended": with(&shown, Label::Intended),
            "unexplained": with(&shown, Label::Unexplained),
            "pr_added": {
                "wrong": with(&pr_added, Label::Wrong),
                "intended": with(&pr_added, Label::Intended),
                "unexplained": with(&pr_added, Label::Unexplained),
            },
            "judged": {
                "wrong": with(&judged, Label::Wrong),
                "intended": with(&judged, Label::Intended),
                "unexplained": with(&judged, Label::Unexplained),
            },
            "not_labelled": scored.not_labelled(),
            "missed": entries(scored.missed().collect()),
            "fixed": entries(scored.fixed().collect()),
        });
    }
    if let Some(judged) = judged {
        let mut judge = json::judge(&judged.summary);
        judge["notes"] = json!(judged.notes);
        if let Some(scored) = scored {
            let kinds: serde_json::Map<String, Value> = matrix(units, scored)
                .into_iter()
                .map(|(key, _, rows)| {
                    let rows: serde_json::Map<String, Value> = rows
                        .into_iter()
                        .map(|(label, counts)| {
                            let counts: serde_json::Map<String, Value> = counts
                                .into_iter()
                                .map(|(verdict, n)| {
                                    (verdict.map_or("none", Verdict::as_str).to_owned(), json!(n))
                                })
                                .collect();
                            (label.as_str().to_owned(), Value::Object(counts))
                        })
                        .collect();
                    (key.to_owned(), Value::Object(rows))
                })
                .collect();
            judge["labels"] = Value::Object(kinds);
        }
        document["judge"] = judge;
    }
    json::pretty(&document)
}

/// Rewrites the labels file: every reported finding with the label it matched (new ones
/// not labelled yet), right findings that are gone (still to look at), and entries about
/// commits this run didn't check. Gone false alarms and unlabelled entries are dropped.
fn update(
    repo: &Repo,
    path: &Path,
    labels: &Labels,
    scored: &Scored,
    units: &[Unit],
    range: String,
) -> Result<(), String> {
    let position = |entry: &Entry| units.iter().position(|unit| entry.about(&unit.commit.sha));
    // In the order of the commits, findings before the gone entries about the same one.
    let mut keyed: Vec<((usize, u8), Entry)> = Vec::new();
    for (u, row) in scored.tags.iter().enumerate() {
        for tag in row.iter().flatten() {
            keyed.push(((u, 0), tag.entry.clone()));
        }
    }
    let mut dropped = 0;
    for (i, entry) in labels.entries.iter().enumerate() {
        if scored.used.contains(&i) {
            continue;
        }
        let checked = position(entry).is_some_and(|u| units[u].error.is_none());
        // Gone: a right finding stays, to look at; a false alarm or an unlabelled one goes.
        if checked && !entry.label.is_some_and(Label::right) {
            dropped += 1;
            continue;
        }
        keyed.push(((position(entry).unwrap_or(usize::MAX), 1), entry.clone()));
    }
    keyed.sort_by_key(|(key, _)| *key);
    let added = scored.all().filter(|tag| tag.pairing.is_none()).count();
    let updated = Labels {
        repo: labels
            .repo
            .clone()
            .or_else(|| repo.origin_url().map(|url| without_credentials(&url))),
        range: Some(range),
        entries: keyed.into_iter().map(|(_, entry)| entry).collect(),
    };
    std::fs::write(path, updated.render())
        .map_err(|err| format!("couldn't write {}: {err}", path.display()))?;
    let mut message = format!("wrote {}: {} new", path.display(), added);
    if dropped > 0 {
        write!(message, ", {dropped} dropped").expect("writing to a String");
    }
    eprintln!("{message}");
    Ok(())
}

/// `url` without the credentials it may carry, since the labels file gets committed:
/// `https://x-access-token:ghp_…@github.com/org/repo.git` → `https://github.com/org/repo.git`.
/// Over HTTP any user name goes (a token is often passed as one); elsewhere only a password
/// does, so `ssh://git@github.com/…` keeps the `git` user it needs.
fn without_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let authority = &rest[..rest.find('/').unwrap_or(rest.len())];
    let Some((user, _)) = authority.rsplit_once('@') else {
        return url.to_owned();
    };
    if scheme.starts_with("http") || user.contains(':') {
        format!("{scheme}://{}", &rest[user.len() + 1..])
    } else {
        url.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_stay_out_of_the_repo_url() {
        assert_eq!(
            without_credentials("https://x-access-token:ghp_abc@github.com/org/repo.git"),
            "https://github.com/org/repo.git"
        );
        assert_eq!(
            without_credentials("https://ghp_abc@github.com/org/repo.git"),
            "https://github.com/org/repo.git"
        );
        assert_eq!(
            without_credentials("ssh://git:secret@host.test/repo.git"),
            "ssh://host.test/repo.git"
        );
        for kept in [
            "ssh://git@github.com/org/repo.git",
            "git@github.com:org/repo.git",
            "https://github.com/org/repo.git",
            "/srv/repos/a@b.git",
        ] {
            assert_eq!(without_credentials(kept), kept);
        }
    }

    #[test]
    fn pr_numbers_come_from_github_subjects() {
        assert_eq!(
            merged_pr("Merge pull request #120 from org/branch"),
            Some(120)
        );
        assert_eq!(merged_pr("Merge branch 'main' into x"), None);
        assert_eq!(squashed_pr("Fix refunds (#12)"), Some(12));
        assert_eq!(squashed_pr("Revert \"Fix refunds (#12)\" (#14)"), Some(14));
        assert_eq!(squashed_pr("Fix refunds (see #12)"), None);
        assert_eq!(squashed_pr("Fix refunds"), None);
    }
}
