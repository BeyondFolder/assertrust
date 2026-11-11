//! `assertrust check`: runs the rules (step 5) on a change and reports what got weaker
//! (step 6), in the terminal, as JSON, or as GitHub annotations. It exits with 1 when a
//! finding reaches `fail-on`, which is never by default.
//!
//! ```text
//! test/billing.test.ts:12  HIGH  check-removed
//!   removed expect(refund.status).toBe('done') · 1a2b3c4
//!
//! 1 finding · warn-only (exit 0)
//! ```

use std::fmt::Write as _;
use std::process::ExitCode;

use assertrust_git::{Analysis, Commit, Repo};
use assertrust_rules::RuleId;

use crate::cli::{CheckArgs, FailOn, Format};
use crate::config::Config;
use crate::output::{count, line};
use crate::report::{self, Report, Reported, Status};
use crate::{github, json};

/// Exit code when a finding reaches `fail-on`.
const EXIT_FINDINGS: u8 = 1;

pub fn run(args: &CheckArgs) -> Result<ExitCode, String> {
    let mode = args.mode.to_input()?;
    let dir = std::env::current_dir().map_err(|err| format!("can't read current dir: {err}"))?;
    let repo = Repo::open(&dir).map_err(|err| err.to_string())?;
    // Flags win over the file.
    let mut config = Config::load(&repo)?;
    if let Some(fail_on) = args.fail_on {
        config.fail_on = fail_on;
    }
    config.show_transient |= args.show_transient;
    let analysis = repo
        .analyze(&mode, &config.classifier)
        .map_err(|err| err.to_string())?;
    let report = report::build(&repo, &analysis, &config)?;
    let output = match args.format {
        Format::Terminal => render(&analysis, &report, &config, args.verbose),
        Format::Json => json::render(&analysis, &report, &config),
        Format::Github => github::render(&report, &config)?,
    };
    crate::print_stdout(&output)?;
    Ok(if report.failing(&config) > 0 {
        ExitCode::from(EXIT_FINDINGS)
    } else {
        ExitCode::SUCCESS
    })
}

fn render(analysis: &Analysis, report: &Report, config: &Config, verbose: bool) -> String {
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
    // tell them apart, so only `-v` shows them. Allowed findings come last.
    let visible: Vec<&Reported> = report
        .findings
        .iter()
        .filter(|reported| match reported.status {
            Status::Shown | Status::BeforeLanding | Status::Allowed { .. } => true,
            Status::Hidden => verbose,
            Status::Transient => config.show_transient,
        })
        .collect();
    for (i, reported) in visible.iter().enumerate() {
        // With `-v` each finding takes several lines, so they're kept apart.
        if verbose && i > 0 {
            line(&mut out, "");
        }
        finding(&mut out, reported, verbose);
    }
    if !visible.is_empty() || !report.notes.is_empty() {
        line(&mut out, "");
    }
    for note in &report.notes {
        line(&mut out, &format!("note: {note}"));
    }
    line(&mut out, &summary(report, config, verbose));
    out
}

/// `3 findings · 2 changed outcomes hidden (-v shows them) · 1 before landing (can't fail) ·
/// 1 allowed · warn-only (exit 0)`
fn summary(report: &Report, config: &Config, verbose: bool) -> String {
    let hidden: Vec<&Reported> = report
        .findings
        .iter()
        .filter(|reported| reported.status == Status::Hidden)
        .collect();
    let shown = report.with_status(|status| *status == Status::Shown)
        + if verbose { hidden.len() } else { 0 };
    let mut parts = vec![if shown == 0 {
        "no findings".to_owned()
    } else {
        count(shown, "finding")
    }];
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
    let before_landing = report.with_status(|status| *status == Status::BeforeLanding);
    if before_landing > 0 {
        parts.push(format!("{before_landing} before landing (can't fail)"));
    }
    let allowed = report.with_status(|status| matches!(status, Status::Allowed { .. }));
    if allowed > 0 {
        parts.push(format!("{allowed} allowed"));
    }
    let transient = report.with_status(|status| *status == Status::Transient);
    if transient > 0 && !config.show_transient {
        parts.push(format!(
            "{transient} undone later hidden (--show-transient shows them)"
        ));
    }
    let failing = report.failing(config);
    match level(config.fail_on) {
        Some(level) if failing > 0 => parts.push(format!(
            "{} at {level} or above (exit 1)",
            count(failing, "finding")
        )),
        Some(level) if shown > 0 => parts.push(format!("none at {level} or above (exit 0)")),
        None if shown > 0 => parts.push("warn-only (exit 0)".to_owned()),
        _ => {}
    }
    parts.join(" · ")
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

/// Two lines: where, how sure and which rule; then what got weaker. An allowed finding adds
/// the reason. `-v` adds the test, the code before and after, and the commit message.
fn finding(out: &mut String, reported: &Reported, verbose: bool) {
    let finding = &reported.finding;
    let place = match finding.line {
        Some(n) => format!("{}:{n}", finding.path),
        None => finding.path.clone(),
    };
    let mut header = format!(
        "{place}  {}  {}",
        finding.confidence.as_str().to_uppercase(),
        finding.rule
    );
    match &reported.status {
        Status::Allowed { .. } => header.push_str("  ALLOWED"),
        Status::Transient => header.push_str("  (undone later)"),
        Status::BeforeLanding => header.push_str("  (before landing)"),
        Status::Shown | Status::Hidden => {}
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
