//! `assertrust check`: runs the rules (step 5) on a change and reports what got weaker
//! (step 6), in the terminal, as JSON, or as GitHub annotations. With the AI judge on (step
//! 7), each finding gets its verdict. It exits with 1 when a finding reaches `fail-on`,
//! which is never by default, or the judge gives it a verdict its `fail-on` names
//! (`coverage-lost` and `suspicious` by default).
//!
//! ```text
//! test/billing.test.ts:12  HIGH  check-removed
//!   removed expect(refund.status).toBe('done') · 1a2b3c4
//!   judge: coverage lost: refunds still get a status, and nothing checks it any more
//!
//! judged fine (1):
//!   test/billing.test.ts:30  check-removed: the same check moved into expectRefunded()
//!
//! 1 finding · 1 judged fine · 1 judged coverage lost or suspicious (exit 1)
//! judge: anthropic/claude-sonnet-5-5 (effort medium) · 2 judged · 18.2K tokens in, 140 out · $0.04
//! ```

use std::fmt::Write as _;
use std::process::ExitCode;

use assertrust_git::{Analysis, Commit, Repo};
use assertrust_rules::RuleId;

use crate::cli::{CheckArgs, FailOn, Format};
use crate::config::{Config, Setting};
use crate::judge::{self, Scope, Summary};
use crate::output::{count, line, place};
use crate::report::{self, Report, Reported, Status};
use crate::{github, json};

/// Exit code when a finding reaches `fail-on`.
const EXIT_FINDINGS: u8 = 1;

pub fn run(args: &CheckArgs) -> Result<ExitCode, String> {
    let mode = args.mode.to_input()?;
    let dir = std::env::current_dir().map_err(|err| format!("can't read current dir: {err}"))?;
    let repo = Repo::open(&dir).map_err(|err| err.to_string())?;
    // Flags win over the file.
    let (mut config, config_notes) = Config::for_check(&repo, &mode)?;
    if let Some(fail_on) = args.fail_on {
        config.fail_on = fail_on;
    }
    config.show_transient |= args.show_transient;
    config.show_pr_added |= args.show_pr_added;
    if args.judge.on() || args.judge_dry_run || config.judge.is_some() {
        config.judge = Some(args.judge.apply(config.judge.as_ref()));
    }
    let analysis = repo
        .analyze(&mode, &config.classifier)
        .map_err(|err| err.to_string())?;
    let mut report = report::build(&repo, &analysis, &config)?;
    report.notes.splice(0..0, config_notes);
    let mut judged = None;
    if let Some(judge) = &config.judge {
        let scope = Scope {
            pr_added: false,
            cap: Some(judge.max_findings),
        };
        let (picked, capped) = judge::select(&report, &scope, analysis.net.as_ref());
        let pull_request = judge::pull_request(&analysis);
        let prompts = judge::prompts(&repo, &report, &picked, pull_request.as_ref());
        if args.judge_dry_run {
            let text =
                judge::dry_run(&judge::cache_dir(&repo)?, judge, &report, &picked, &prompts)?;
            crate::print_stdout(&text)?;
            return Ok(ExitCode::SUCCESS);
        }
        if !picked.is_empty() {
            let asked = judge::ask(&judge::cache_dir(&repo)?, judge, &prompts);
            judge::apply(&mut report, &picked, asked.judgments, analysis.net.as_ref());
            report.notes.extend(asked.notes);
            judged = Some(Summary {
                capped,
                ..asked.summary
            });
        }
    }
    let judged = judged.as_ref();
    let output = match args.format {
        Format::Terminal => render(&analysis, &report, &config, args.verbose, judged),
        Format::Json => json::render(&analysis, &report, &config, judged),
        Format::Github => github::render(&report, &config, judged)?,
    };
    crate::print_stdout(&output)?;
    Ok(if report.failing(&config) > 0 {
        ExitCode::from(EXIT_FINDINGS)
    } else {
        ExitCode::SUCCESS
    })
}

fn render(
    analysis: &Analysis,
    report: &Report,
    config: &Config,
    verbose: bool,
    judged: Option<&Summary>,
) -> String {
    let mut out = String::new();
    if analysis.is_empty() {
        // A range of merge commits only still says they were skipped.
        for note in &report.notes {
            line(&mut out, &format!("note: {note}"));
        }
        line(&mut out, "nothing to check");
        return out;
    }
    // Changed outcomes are usually legitimate behaviour changes: noise until the judge can
    // tell them apart, so only `-v` shows them, allowed or undone later too. Allowed
    // findings come last. Those the judge calls fine are listed apart, at the end.
    let (fine, visible): (Vec<&Reported>, Vec<&Reported>) = report
        .findings
        .iter()
        .filter(|reported| visible(reported, config, verbose))
        .partition(|reported| reported.folded(config));
    for (i, reported) in visible.iter().enumerate() {
        // With `-v` each finding takes several lines, so they're kept apart.
        if verbose && i > 0 {
            line(&mut out, "");
        }
        finding(&mut out, reported, verbose, None);
    }
    if !fine.is_empty() {
        if !visible.is_empty() {
            line(&mut out, "");
        }
        line(&mut out, &format!("judged fine ({}):", fine.len()));
        for reported in &fine {
            if verbose {
                finding(&mut out, reported, true, None);
            } else {
                let reason = reported
                    .judgment
                    .as_ref()
                    .map(|judgment| judgment.reason.as_str())
                    .unwrap_or_default();
                line(
                    &mut out,
                    &format!(
                        "  {}  {}: {reason}",
                        place(&reported.finding),
                        reported.finding.rule
                    ),
                );
            }
        }
    }
    if !visible.is_empty() || !fine.is_empty() || !report.notes.is_empty() {
        line(&mut out, "");
    }
    for note in &report.notes {
        line(&mut out, &format!("note: {note}"));
    }
    line(&mut out, &summary(report, config, verbose));
    if let Some(judged) = judged {
        line(&mut out, &judged.line());
    }
    out
}

/// `3 findings · 2 changed outcomes hidden (-v shows them) · 1 on code this PR added hidden
/// (--show-pr-added shows them) · 1 allowed · warn-only (exit 0)`
fn summary(report: &Report, config: &Config, verbose: bool) -> String {
    let hidden: Vec<&Reported> = report
        .findings
        .iter()
        .filter(|reported| reported.status == Status::Hidden)
        .collect();
    // Those the judge calls fine are counted apart: shown ones, and hidden ones `-v` lists.
    let fine_shown = report
        .findings
        .iter()
        .filter(|reported| reported.status == Status::Shown && reported.folded(config))
        .count();
    let fine_hidden = if verbose {
        hidden
            .iter()
            .filter(|reported| reported.folded(config))
            .count()
    } else {
        0
    };
    let fine = fine_shown + fine_hidden;
    let shown = report.with_status(|status| *status == Status::Shown) - fine_shown
        + if verbose {
            hidden.len() - fine_hidden
        } else {
            0
        };
    let mut parts = vec![if shown == 0 {
        "no findings".to_owned()
    } else {
        count(shown, "finding")
    }];
    if fine > 0 {
        parts.push(format!("{fine} judged fine"));
    }
    if !verbose && !hidden.is_empty() {
        let outcomes = hidden
            .iter()
            .all(|reported| reported.finding.rule == RuleId::OutcomeChanged);
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
    let counted = |wanted: fn(&Status) -> bool| {
        report
            .findings
            .iter()
            .filter(|reported| wanted(&reported.status))
            .filter(|reported| verbose || !judged(reported, config))
            .count()
    };
    let added = counted(|status| *status == Status::PrAdded);
    parts.extend(pr_added(added, config.show_pr_added, "this PR", true));
    let allowed = counted(|status| matches!(status, Status::Allowed { .. }));
    if allowed > 0 {
        parts.push(format!("{allowed} allowed"));
    }
    let transient = counted(|status| *status == Status::Transient);
    if transient > 0 && !config.show_transient {
        parts.push(format!(
            "{transient} undone later hidden (--show-transient shows them)"
        ));
    }
    // Something was found, so say whether it fails the check.
    if shown + fine > 0 {
        parts.push(verdict(report, config));
    }
    parts.join(" · ")
}

/// Whether the terminal output shows the finding.
pub fn visible(reported: &Reported, config: &Config, verbose: bool) -> bool {
    let shown = match reported.status {
        Status::Shown | Status::Allowed { .. } => true,
        Status::Hidden => verbose,
        Status::Transient => config.show_transient,
        Status::PrAdded => config.show_pr_added,
    };
    shown && (verbose || !judged(reported, config))
}

/// The summary's part about weakening of code a PR added, if there's any: `2 on code this
/// PR added (can't fail)` when `shown`, else `… hidden`, naming the flag that shows them
/// when the output has one (`flag`). `whose` is `this PR`, or `its PR` over many.
pub fn pr_added(count: usize, shown: bool, whose: &str, flag: bool) -> Option<String> {
    (count > 0).then(|| match (shown, flag) {
        (true, _) => format!("{count} on code {whose} added (can't fail)"),
        (false, true) => {
            format!("{count} on code {whose} added hidden (--show-pr-added shows them)")
        }
        (false, false) => format!("{count} on code {whose} added hidden"),
    })
}

/// Whether the finding's rule is left to the judge and it has no verdict, so only `-v`
/// shows it.
pub fn judged(reported: &Reported, config: &Config) -> bool {
    config.setting(reported.finding.rule) == Setting::Judge && reported.judgment.is_none()
}

/// Whether the check fails, and why: `1 finding at high or above (exit 1)`, `1 judged
/// coverage lost or suspicious (exit 1)`, `none at high or above (exit 0)` or `warn-only
/// (exit 0)`.
pub fn verdict(report: &Report, config: &Config) -> String {
    let shown = || {
        report
            .findings
            .iter()
            .filter(|reported| reported.status == Status::Shown)
    };
    let by_level = shown()
        .filter(|reported| config.fail_on.fails(reported.finding.confidence))
        .count();
    let by_judge = shown()
        .filter(|reported| {
            !config.fail_on.fails(reported.finding.confidence) && reported.judge_fails(config)
        })
        .count();
    let level = level(config.fail_on).map(|level| format!("at {level} or above"));
    let judged = config
        .judge
        .as_ref()
        .filter(|judge| !judge.fail_on.is_empty())
        .map(|judge| {
            let words: Vec<&str> = judge
                .fail_on
                .iter()
                .map(|verdict| verdict.words())
                .collect();
            format!("judged {}", words.join(" or "))
        });
    if by_level + by_judge > 0 {
        let mut parts = Vec::new();
        if let (Some(level), true) = (&level, by_level > 0) {
            parts.push(format!("{} {level}", count(by_level, "finding")));
        }
        if let (Some(judged), true) = (&judged, by_judge > 0) {
            if by_level > 0 {
                parts.push(format!("{by_judge} more {judged}"));
            } else {
                parts.push(format!("{by_judge} {judged}"));
            }
        }
        return format!("{} (exit 1)", parts.join(", "));
    }
    match (level, judged) {
        (None, None) => "warn-only (exit 0)".to_owned(),
        (Some(level), None) => format!("none {level} (exit 0)"),
        (None, Some(judged)) => format!("none {judged} (exit 0)"),
        (Some(level), Some(judged)) => format!("none {level} or {judged} (exit 0)"),
    }
}

/// The `fail-on` level's name, or `None` for never.
pub fn level(fail_on: FailOn) -> Option<&'static str> {
    match fail_on {
        FailOn::Never => None,
        FailOn::High => Some("high"),
        FailOn::Medium => Some("medium"),
        FailOn::Low => Some("low"),
    }
}

/// `1a2b3c4, 9f8e7d6, +2 more`: the commits that made a finding, at most three named.
pub fn commit_list(commits: &[&Commit]) -> String {
    let mut shas: Vec<String> = commits
        .iter()
        .take(3)
        .map(|commit| commit.short_sha().to_owned())
        .collect();
    if commits.len() > 3 {
        shas.push(format!("+{} more", commits.len() - 3));
    }
    shas.join(", ")
}

/// Two lines: where, how sure and which rule (then `tag`, if any); then what got weaker. An
/// allowed finding adds the reason. `-v` adds the test, the code before and after, and the
/// commit message.
pub(crate) fn finding(out: &mut String, reported: &Reported, verbose: bool, tag: Option<&str>) {
    let finding = &reported.finding;
    let mut header = format!(
        "{}  {}  {}",
        place(finding),
        finding.confidence.as_str().to_uppercase(),
        finding.rule
    );
    match &reported.status {
        Status::Allowed { .. } => header.push_str("  ALLOWED"),
        Status::Transient => header.push_str("  (undone later)"),
        Status::PrAdded => header.push_str("  (added by this PR)"),
        Status::Shown | Status::Hidden => {}
    }
    if let Some(tag) = tag {
        write!(header, "  [{tag}]").expect("writing to a String");
    }
    line(out, &header);
    let mut message = format!("  {}", finding.message);
    if !reported.commits.is_empty() {
        write!(message, " · {}", commit_list(&reported.commits)).expect("writing to a String");
    }
    line(out, &message);
    if let Status::Allowed { reason, by } = &reported.status {
        line(out, &format!("  allowed: {reason} ({by})"));
    }
    if let Some(judgment) = &reported.judgment {
        line(
            out,
            &format!("  judge: {}: {}", judgment.verdict.words(), judgment.reason),
        );
    }
    if !verbose {
        return;
    }
    if let Some(test) = &finding.test {
        labelled(out, "test", std::slice::from_ref(test));
    }
    labelled(out, "before", &finding.before);
    labelled(out, "after", &finding.after);
    match reported.commits.as_slice() {
        [] => {}
        // One commit: its whole message. Several: a line each.
        [commit] => {
            let message: Vec<String> = commit
                .message
                .trim_end()
                .lines()
                .map(str::to_owned)
                .collect();
            labelled(out, "commit", &message);
        }
        commits => {
            let lines: Vec<String> = commits
                .iter()
                .map(|commit| format!("{} {}", commit.short_sha(), commit.subject()))
                .collect();
            labelled(out, "commits", &lines);
        }
    }
}

/// `    before  first line`, then the other lines lined up under it.
fn labelled(out: &mut String, label: &str, lines: &[String]) {
    for (i, text) in lines.iter().enumerate() {
        let label = if i == 0 { label } else { "" };
        line(out, format!("    {label:<7} {text}").trim_end());
    }
}
