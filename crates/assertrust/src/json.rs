//! `assertrust check --format json`: every finding, for scripts and other tools
//! (`docs/architecture.md`, step 6). Versioned by `schema`; a field is only ever added
//! within a version. Keys come sorted.
//!
//! ```json
//! { "exit": 0, "fail_on": "never",
//!   "findings": [ { "after": [], "before": ["expect(b).toBe(2)"],
//!                   "commits": [ { "sha": "1a2b…", "subject": "Trim" } ],
//!                   "confidence": "high", "line": 12, "message": "removed expect(b).toBe(2)",
//!                   "path": "test/a.test.ts", "rule": "check-removed", "status": "shown",
//!                   "test": "refunds" } ],
//!   "mode": "per-commit", "notes": [], "range": "main..HEAD", "schema": 1 }
//! ```
//!
//! Hidden, allowed, undone-later and before-landing findings are all there, with their
//! `status` (`shown`, `hidden`, `allowed`, `transient`, `before-landing`); an allowed one has
//! `"allowed": { "reason", "by" }`.

use assertrust_git::Analysis;
use serde_json::{Value, json};

use crate::check_command::level;
use crate::config::Config;
use crate::output::mode_name;
use crate::report::{Report, Reported, Status};

/// The output format's version.
const SCHEMA: u32 = 1;

pub fn render(analysis: &Analysis, report: &Report, config: &Config) -> String {
    let findings: Vec<Value> = report.findings.iter().map(finding).collect();
    let document = json!({
        "schema": SCHEMA,
        "mode": mode_name(analysis.kind),
        "range": analysis.label,
        "fail_on": level(config.fail_on).unwrap_or("never"),
        "exit": u8::from(report.failing(config) > 0),
        "findings": findings,
        "notes": report.notes,
    });
    pretty(&document)
}

/// One finding, as `check` and `backtest` list it.
pub fn finding(reported: &Reported) -> Value {
    let finding = &reported.finding;
    let status = match &reported.status {
        Status::Shown => "shown",
        Status::Hidden => "hidden",
        Status::Allowed { .. } => "allowed",
        Status::Transient => "transient",
        Status::BeforeLanding => "before-landing",
    };
    let mut value = json!({
        "rule": finding.rule.as_str(),
        "confidence": finding.confidence.as_str(),
        "status": status,
        "path": finding.path,
        "line": finding.line,
        "test": finding.test,
        "message": finding.message,
        "before": finding.before,
        "after": finding.after,
        "commits": reported.commits.iter().map(|commit| json!({
            "sha": commit.sha,
            "subject": commit.subject(),
        })).collect::<Vec<_>>(),
    });
    if let Status::Allowed { reason, by } = &reported.status {
        value["allowed"] = json!({ "reason": reason, "by": by });
    }
    value
}

/// The document, indented, with a final newline.
pub fn pretty(document: &Value) -> String {
    let mut text = serde_json::to_string_pretty(document).expect("JSON values always serialize");
    text.push('\n');
    text
}
