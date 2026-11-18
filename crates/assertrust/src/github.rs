//! `assertrust check --format github`: findings as annotations on the PR diff, and a short
//! table in the job summary (`docs/architecture.md`, step 6).
//!
//! ```text
//! ::warning file=test/a.test.ts,line=12,title=assertrust%3A check-removed (high)::removed expect(b).toBe(2) (1a2b3c4)
//! ```
//!
//! A finding that reaches `fail-on`, or that the judge gives a verdict failing the check, is
//! an `error`, the rest are warnings (weakening of code the PR added never fails, and is
//! annotated only with `show-pr-added`). Allowed, hidden and undone-later findings aren't
//! annotated, nor are those the judge calls fine: the summary lists them, folded. The
//! summary goes to the file GitHub names in `$GITHUB_STEP_SUMMARY`, starting with a marker
//! (`<!-- assertrust -->`) so the Action can find the PR comment that repeats it and edit it
//! in place. Both count findings the way the terminal does. The number of findings listed
//! goes to `$GITHUB_OUTPUT` as `findings`: the Action posts its comment by it, and a workflow
//! can read it.

use std::env;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write as _;

use crate::check_command::{self, commit_list, judged, verdict};
use crate::config::Config;
use crate::judge::Summary;
use crate::output::{count, line, place};
use crate::report::{Report, Reported, Status};

/// Marks the summary, so a PR comment holding it can be found and edited.
pub const MARKER: &str = "<!-- assertrust -->";

pub fn render(
    report: &Report,
    config: &Config,
    judge_run: Option<&Summary>,
) -> Result<String, String> {
    let mut out = String::new();
    let shown = annotated(report, config);
    for reported in &shown {
        let finding = &reported.finding;
        // Weakening of code the PR added can't fail the check.
        let fails = reported.status == Status::Shown && reported.fails(config);
        let kind = if fails { "error" } else { "warning" };
        let mut properties = format!("file={}", property(&finding.path));
        if let Some(n) = finding.line {
            properties.push_str(&format!(",line={n}"));
        }
        properties.push_str(&format!(
            ",title={}",
            property(&format!(
                "assertrust: {} ({})",
                finding.rule, finding.confidence
            ))
        ));
        let mut message = finding.message.clone();
        if !reported.commits.is_empty() {
            message.push_str(&format!(" ({})", commit_list(&reported.commits)));
        }
        if reported.status == Status::PrAdded {
            message.push_str(", added by this PR");
        }
        if let Some(judgment) = &reported.judgment {
            message.push_str(&format!(
                " · judge: {}: {}",
                judgment.verdict.words(),
                judgment.reason
            ));
        }
        line(
            &mut out,
            &format!("::{kind} {properties}::{}", data(&message)),
        );
    }
    for note in &report.notes {
        line(
            &mut out,
            &format!("::notice title=assertrust::{}", data(note)),
        );
    }
    line(
        &mut out,
        &format!(
            "assertrust: {} · {}",
            headline(report, config),
            verdict(report, config)
        ),
    );
    if let Some(judge_run) = judge_run {
        line(&mut out, &format!("assertrust {}", judge_run.line()));
    }
    if let Some(path) = env::var_os("GITHUB_STEP_SUMMARY") {
        append(
            &path,
            &summary(report, config, judge_run),
            "the job summary",
        )?;
    }
    if let Some(path) = env::var_os("GITHUB_OUTPUT") {
        append(
            &path,
            &format!("findings={}\n", shown.len()),
            "the step outputs",
        )?;
    }
    Ok(out)
}

/// Adds `text` to the file GitHub named for `what`.
fn append(path: &OsStr, text: &str, what: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| format!("couldn't open {what}: {err}"))?;
    file.write_all(text.as_bytes())
        .map_err(|err| format!("couldn't write {what}: {err}"))
}

/// The findings annotated on the diff and listed in the summary's table: those shown, and
/// those about code the PR added when `show-pr-added` is on, unless the judge calls them
/// fine.
fn annotated<'r, 'a>(report: &'r Report<'a>, config: &Config) -> Vec<&'r Reported<'a>> {
    report
        .findings
        .iter()
        .filter(|reported| match reported.status {
            Status::Shown => true,
            Status::PrAdded => config.show_pr_added,
            _ => false,
        })
        .filter(|reported| !reported.folded(config))
        .collect()
}

/// Shown findings the judge calls fine that don't fail the check: listed apart, folded.
fn judged_fine<'r, 'a>(report: &'r Report<'a>, config: &Config) -> Vec<&'r Reported<'a>> {
    report
        .findings
        .iter()
        .filter(|reported| reported.status == Status::Shown && reported.folded(config))
        .collect()
}

/// `2 findings`, or `no findings · 1 judged fine · 1 on code this PR added hidden`.
fn headline(report: &Report, config: &Config) -> String {
    let fine = judged_fine(report, config).len();
    let shown = report.with_status(|status| *status == Status::Shown) - fine;
    let mut text = if shown == 0 {
        "no findings".to_owned()
    } else {
        count(shown, "finding")
    };
    if fine > 0 {
        text.push_str(&format!(" · {fine} judged fine"));
    }
    let pr_added = report.with_status(|status| *status == Status::PrAdded);
    if let Some(part) = check_command::pr_added(pr_added, config.show_pr_added, "this PR", false) {
        text.push_str(&format!(" · {part}"));
    }
    text
}

/// The job summary: a heading, then a table of findings, those the judge calls fine (folded)
/// and a list of allowed ones.
pub fn summary(report: &Report, config: &Config, judge_run: Option<&Summary>) -> String {
    let mut out = String::new();
    line(&mut out, MARKER);
    let shown = annotated(report, config);
    line(
        &mut out,
        &format!("### assertrust: {}", headline(report, config)),
    );
    if !shown.is_empty() {
        line(&mut out, "");
        line(&mut out, "| Where | Rule | What got weaker |");
        line(&mut out, "|---|---|---|");
        for reported in &shown {
            let finding = &reported.finding;
            let mut message = cell(&finding.message);
            if !reported.commits.is_empty() {
                message.push_str(&format!(" · {}", commit_list(&reported.commits)));
            }
            if reported.status == Status::PrAdded {
                message.push_str(" (added by this PR)");
            }
            if let Some(judgment) = &reported.judgment {
                message.push_str(&format!(
                    "<br>**judge: {}**: {}",
                    judgment.verdict.words(),
                    cell(&judgment.reason)
                ));
            }
            line(
                &mut out,
                &format!(
                    "| `{}` | {} ({}) | {message} |",
                    cell(&place(finding)),
                    finding.rule,
                    finding.confidence
                ),
            );
        }
    }
    let fine = judged_fine(report, config);
    if !fine.is_empty() {
        line(&mut out, "");
        line(
            &mut out,
            &format!("<details><summary>{} judged fine</summary>", fine.len()),
        );
        line(&mut out, "");
        for reported in fine {
            let reason = reported
                .judgment
                .as_ref()
                .map(|judgment| cell(&judgment.reason))
                .unwrap_or_default();
            line(
                &mut out,
                &format!(
                    "- `{}` {}: {}. **Judge:** {reason}",
                    cell(&place(&reported.finding)),
                    reported.finding.rule,
                    cell(&reported.finding.message)
                ),
            );
        }
        line(&mut out, "");
        line(&mut out, "</details>");
    }
    let allowed: Vec<_> = report
        .findings
        .iter()
        // Changed outcomes stay out unless asked for, as in the terminal.
        .filter(|reported| !judged(reported, config))
        .filter_map(|reported| match &reported.status {
            Status::Allowed { reason, .. } => Some((reported, reason)),
            _ => None,
        })
        .collect();
    if !allowed.is_empty() {
        line(&mut out, "");
        line(&mut out, &format!("**{} allowed**", allowed.len()));
        line(&mut out, "");
        for (reported, reason) in allowed {
            line(
                &mut out,
                &format!(
                    "- `{}` {}: {} (because: {})",
                    cell(&reported.finding.path),
                    reported.finding.rule,
                    cell(&reported.finding.message),
                    cell(reason)
                ),
            );
        }
    }
    if let Some(judge_run) = judge_run {
        line(&mut out, "");
        line(&mut out, &format!("<sub>{}</sub>", cell(&judge_run.line())));
    }
    out
}

/// A table cell: no line breaks or pipes.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// An annotation's message, escaped as GitHub's workflow commands want.
fn data(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// An annotation property (file, title), which also can't hold `:` or `,`.
fn property(text: &str) -> String {
    data(text).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_workflow_commands() {
        assert_eq!(data("50% done\nnext"), "50%25 done%0Anext");
        assert_eq!(property("a: b, c"), "a%3A b%2C c");
    }
}
