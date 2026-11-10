//! `assertrust check`: runs the rules (step 5) on a change and prints what got weaker
//! (step 6). Findings only warn for now, so the exit code is 0 unless assertrust itself
//! fails.
//!
//! ```text
//! test/billing.test.ts:12  HIGH  check-removed
//!   removed expect(refund.status).toBe('done') · 1a2b3c4
//!
//! 1 finding · warn-only (exit 0)
//! ```

use std::fmt::Write as _;

use assertrust_git::{Analysis, Classifier, Commit, Comparison, Repo};
use assertrust_rules::{Finding, RuleId};

use crate::cli::{CheckArgs, Format};
use crate::load;
use crate::output::{count, line};

pub fn run(args: &CheckArgs) -> Result<(), String> {
    match args.format {
        Format::Terminal => {}
        Format::Json | Format::Github => {
            return Err("only --format terminal is supported so far".to_owned());
        }
    }
    if args.show_transient {
        return Err(
            "--show-transient isn't supported yet (it comes with naming the commit behind each finding)"
                .to_owned(),
        );
    }
    let mode = args.mode.to_input()?;
    let dir = std::env::current_dir().map_err(|err| format!("can't read current dir: {err}"))?;
    let repo = Repo::open(&dir).map_err(|err| err.to_string())?;
    let classifier = Classifier::default();
    let analysis = repo
        .analyze(&mode, &classifier)
        .map_err(|err| err.to_string())?;
    let report = report(&repo, &analysis, &classifier)?;
    crate::print_stdout(&render(&analysis, &report, args.verbose))
}

/// Every finding in the change, most sure first.
struct Report<'a> {
    findings: Vec<Located<'a>>,
    /// Files that couldn't be checked, e.g. because they don't parse.
    notes: Vec<String>,
}

struct Located<'a> {
    finding: Finding,
    /// The commit that made the change, when commits are checked one by one. PR-style
    /// checks will name it once findings are traced back to their commit.
    commit: Option<&'a Commit>,
}

fn report<'a>(
    repo: &Repo,
    analysis: &'a Analysis,
    classifier: &Classifier,
) -> Result<Report<'a>, String> {
    // What lands: the whole PR, or each commit on its own.
    let comparisons: Vec<(&Comparison, Option<&Commit>)> = match &analysis.net {
        Some(net) => vec![(net, None)],
        None => analysis
            .steps
            .iter()
            .map(|step| (&step.comparison, Some(&step.commit)))
            .collect(),
    };
    let mut report = Report {
        findings: Vec::new(),
        notes: Vec::new(),
    };
    let mut carry = None;
    for (comparison, commit) in comparisons {
        let loaded = load::load(repo, comparison, classifier, &mut carry)?;
        for note in &loaded.notes {
            // The same file can be skipped in several commits.
            if !report.notes.contains(note) {
                report.notes.push(note.clone());
            }
        }
        report.findings.extend(
            assertrust_rules::check(&loaded.test_files(), &loaded.gate_files())
                .into_iter()
                .map(|finding| Located { finding, commit }),
        );
    }
    // Stable, so within a confidence level findings stay in commit, file and line order.
    report
        .findings
        .sort_by_key(|located| located.finding.confidence);
    // A PR-style check compares the merge-base with the head, which covers merges too.
    if analysis.net.is_none() && !analysis.skipped_merges.is_empty() {
        report.notes.push(format!(
            "{} not checked (a PR-style check, BASE...HEAD, covers what they changed)",
            count(analysis.skipped_merges.len(), "merge commit")
        ));
    }
    Ok(report)
}

fn render(analysis: &Analysis, report: &Report, verbose: bool) -> String {
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
    // tell them apart, so only `-v` shows them.
    let shown: Vec<&Located> = report
        .findings
        .iter()
        .filter(|located| verbose || located.finding.rule != RuleId::OutcomeChanged)
        .collect();
    let hidden = report.findings.len() - shown.len();
    for (i, located) in shown.iter().enumerate() {
        // With `-v` each finding takes several lines, so they're kept apart.
        if verbose && i > 0 {
            line(&mut out, "");
        }
        finding(&mut out, located, verbose);
    }
    if !shown.is_empty() || !report.notes.is_empty() {
        line(&mut out, "");
    }
    for note in &report.notes {
        line(&mut out, &format!("note: {note}"));
    }
    let mut summary = if shown.is_empty() {
        vec!["no findings".to_owned()]
    } else {
        vec![count(shown.len(), "finding")]
    };
    if hidden > 0 {
        summary.push(format!(
            "{} hidden (-v shows them)",
            count(hidden, "changed outcome")
        ));
    }
    if !shown.is_empty() {
        summary.push("warn-only (exit 0)".to_owned());
    }
    line(&mut out, &summary.join(" · "));
    out
}

/// Two lines: where, how sure and which rule; then what got weaker. `-v` adds the test, the
/// code before and after, and the commit message.
fn finding(out: &mut String, located: &Located, verbose: bool) {
    let finding = &located.finding;
    let place = match finding.line {
        Some(n) => format!("{}:{n}", finding.path),
        None => finding.path.clone(),
    };
    line(
        out,
        &format!(
            "{place}  {}  {}",
            finding.confidence.as_str().to_uppercase(),
            finding.rule
        ),
    );
    let mut message = format!("  {}", finding.message);
    if let Some(commit) = located.commit {
        write!(message, " · {}", commit.short_sha()).expect("writing to a String");
    }
    line(out, &message);
    if !verbose {
        return;
    }
    if let Some(test) = &finding.test {
        labelled(out, "test", std::slice::from_ref(test));
    }
    labelled(out, "before", &finding.before);
    labelled(out, "after", &finding.after);
    if let Some(commit) = located.commit {
        let message: Vec<String> = commit
            .message
            .trim_end()
            .lines()
            .map(str::to_owned)
            .collect();
        labelled(out, "commit", &message);
    }
}

/// `    before  first line`, then the other lines lined up under it.
fn labelled(out: &mut String, label: &str, lines: &[String]) {
    for (i, text) in lines.iter().enumerate() {
        let label = if i == 0 { label } else { "" };
        line(out, format!("    {label:<7} {text}").trim_end());
    }
}
