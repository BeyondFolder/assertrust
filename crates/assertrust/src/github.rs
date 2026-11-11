//! `assertrust check --format github`: findings as annotations on the PR diff, and a short
//! table in the job summary (`docs/architecture.md`, step 6).
//!
//! ```text
//! ::warning file=test/a.test.ts,line=12,title=assertrust%3A check-removed (high)::removed expect(b).toBe(2) (1a2b3c4)
//! ```
//!
//! A finding that reaches `fail-on` is an `error`, the rest are warnings (weakening before
//! landing never fails). Allowed, hidden and undone-later findings aren't annotated. The summary goes to the file GitHub names in
//! `$GITHUB_STEP_SUMMARY`, starting with a marker (`<!-- assertrust -->`) so the Action can
//! find the PR comment that repeats it and edit it in place.

use std::env;
use std::fs::OpenOptions;
use std::io::Write as _;

use crate::check_command::{commit_list, level};
use crate::config::Config;
use crate::output::{count, line};
use crate::report::{Report, Status};

/// Marks the summary, so a PR comment holding it can be found and edited.
pub const MARKER: &str = "<!-- assertrust -->";

pub fn render(report: &Report, config: &Config) -> Result<String, String> {
    let mut out = String::new();
    let shown: Vec<_> = report
        .findings
        .iter()
        .filter(|reported| matches!(reported.status, Status::Shown | Status::BeforeLanding))
        .collect();
    for reported in &shown {
        let finding = &reported.finding;
        // Weakening before landing can't fail the check.
        let fails = reported.status == Status::Shown && config.fail_on.fails(finding.confidence);
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
        if reported.status == Status::BeforeLanding {
            message.push_str(", before landing");
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
    let failing = report.failing(config);
    let verdict = match level(config.fail_on) {
        Some(level) if failing > 0 => {
            format!("{} at {level} or above (exit 1)", count(failing, "finding"))
        }
        Some(level) => format!("none at {level} or above (exit 0)"),
        None => "warn-only (exit 0)".to_owned(),
    };
    line(
        &mut out,
        &format!("assertrust: {} · {verdict}", count(shown.len(), "finding")),
    );
    if let Some(path) = env::var_os("GITHUB_STEP_SUMMARY") {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| format!("couldn't open the job summary: {err}"))?;
        file.write_all(summary(report).as_bytes())
            .map_err(|err| format!("couldn't write the job summary: {err}"))?;
    }
    Ok(out)
}

/// The job summary: a heading, then a table of findings and one of allowed ones.
pub fn summary(report: &Report) -> String {
    let mut out = String::new();
    line(&mut out, MARKER);
    let shown: Vec<_> = report
        .findings
        .iter()
        .filter(|reported| matches!(reported.status, Status::Shown | Status::BeforeLanding))
        .collect();
    if shown.is_empty() {
        line(&mut out, "### assertrust: no findings");
    } else {
        line(
            &mut out,
            &format!("### assertrust: {}", count(shown.len(), "finding")),
        );
        line(&mut out, "");
        line(&mut out, "| Where | Rule | What got weaker |");
        line(&mut out, "|---|---|---|");
        for reported in &shown {
            let finding = &reported.finding;
            let place = match finding.line {
                Some(n) => format!("{}:{n}", finding.path),
                None => finding.path.clone(),
            };
            let mut message = cell(&finding.message);
            if !reported.commits.is_empty() {
                message.push_str(&format!(" · {}", commit_list(&reported.commits)));
            }
            if reported.status == Status::BeforeLanding {
                message.push_str(" (before landing)");
            }
            line(
                &mut out,
                &format!(
                    "| `{}` | {} ({}) | {message} |",
                    cell(&place),
                    finding.rule,
                    finding.confidence
                ),
            );
        }
    }
    let allowed: Vec<_> = report
        .findings
        .iter()
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
