//! The labels file of `assertrust backtest --labels` (`docs/architecture.md`, step 6,
//! "Backtest"): a person's verdict on each finding a backtest made, so a run can say how
//! often assertrust is right, and which findings are new or gone since the labels were set.
//!
//! ```toml
//! repo = "https://github.com/honojs/hono.git"
//! range = "0a1b2c3…..9f8e7d6…"
//!
//! [[finding]]
//! unit = "1534ffa3e9c5"          # the commit that landed: a merge, a squash, a push
//! pr = 5618
//! rule = "check-removed"
//! path = "src/router.test.ts"
//! test = "matches > a path"
//! before = ["expect(res.status).toBe(200)"]
//! after = []
//! message = "removed expect(res.status).toBe(200)"
//! label = "unexplained"          # wrong | intended | unexplained, or "" for not yet
//! note = "dropped while moving the route"
//! ```
//!
//! A finding is the same one when its commit, rule, file, test and code are: the message
//! isn't part of it, so rewording one doesn't lose its label. When the code changes (a
//! snippet printed differently), a finding that's left over still takes the label of the
//! one left-over entry about the same commit, rule, file and test.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use assertrust_rules::{Finding, RuleId};
use toml::{Table, Value};

/// A person's verdict on a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// Nothing got weaker: assertrust misread the change.
    Wrong,
    /// Weaker on purpose: the code the test covered went too, or a commit says why.
    Intended,
    /// Weaker, and nothing in the change says why.
    Unexplained,
}

impl Label {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wrong => "wrong",
            Self::Intended => "intended",
            Self::Unexplained => "unexplained",
        }
    }

    /// Whether the finding is right: the change did make the tests weaker.
    pub fn right(self) -> bool {
        self != Self::Wrong
    }
}

/// One finding and its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The start of the SHA of the commit that landed (12 characters when written).
    pub unit: String,
    pub pr: Option<u64>,
    pub rule: String,
    pub path: String,
    pub test: Option<String>,
    pub tests: Vec<String>,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub message: String,
    pub label: Option<Label>,
    pub note: String,
}

impl Entry {
    /// An entry for a finding a backtest made, not labelled yet.
    pub fn new(unit: &str, pr: Option<u64>, finding: &Finding) -> Self {
        Self {
            unit: unit.chars().take(12).collect(),
            pr,
            rule: finding.rule.as_str().to_owned(),
            path: finding.path.clone(),
            test: finding.test.clone(),
            tests: finding.tests.clone(),
            before: finding.before.clone(),
            after: finding.after.clone(),
            message: finding.message.clone(),
            label: None,
            note: String::new(),
        }
    }

    /// Whether `sha` is the commit it's about.
    pub fn about(&self, sha: &str) -> bool {
        !self.unit.is_empty() && sha.starts_with(&self.unit)
    }

    /// The same commit, rule, file and test.
    fn close(&self, other: &Self) -> bool {
        (self.unit.starts_with(&other.unit) || other.unit.starts_with(&self.unit))
            && self.rule == other.rule
            && self.path == other.path
            && self.test == other.test
            && self.tests == other.tests
    }

    /// The same finding: also the same code.
    fn same(&self, other: &Self) -> bool {
        self.close(other) && self.before == other.before && self.after == other.after
    }
}

/// How a finding found its entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pairing {
    /// The same finding.
    Same(usize),
    /// The only left-over entry about the same commit, rule, file and test: its code is
    /// printed differently now.
    Carried(usize),
}

impl Pairing {
    pub fn entry(self) -> usize {
        match self {
            Self::Same(i) | Self::Carried(i) => i,
        }
    }
}

/// The whole file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Labels {
    /// Where to clone the repo from (for `backtest/run.sh`).
    pub repo: Option<String>,
    /// The commits replayed, `BASE..HEAD` with full SHAs.
    pub range: Option<String>,
    pub entries: Vec<Entry>,
}

impl Labels {
    /// Reads the file. A missing one is empty when `may_be_missing` (it's about to be
    /// written), else an error.
    pub fn read(path: &Path, may_be_missing: bool) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|err| format!("{}: {err}", path.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound && may_be_missing => {
                Ok(Self::default())
            }
            Err(err) => Err(format!("couldn't read {}: {err}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let table: Table = toml::from_str(text).map_err(|err| err.message().to_owned())?;
        let mut labels = Self::default();
        for (key, value) in &table {
            match key.as_str() {
                "repo" => labels.repo = Some(string(key, value)?.to_owned()),
                "range" => labels.range = Some(string(key, value)?.to_owned()),
                "finding" => {
                    let entries = value
                        .as_array()
                        .ok_or("`finding` must be a list of tables, [[finding]]")?;
                    for (i, entry) in entries.iter().enumerate() {
                        labels.entries.push(
                            entry_from(entry).map_err(|err| format!("finding {}: {err}", i + 1))?,
                        );
                    }
                }
                other => return Err(format!("unknown key `{other}`")),
            }
        }
        Ok(labels)
    }

    /// Pairs each finding with its entry: the same finding first, then the one left-over
    /// entry about the same commit, rule, file and test, when there's exactly one each way.
    pub fn pair(&self, found: &[Entry]) -> Vec<Option<Pairing>> {
        let mut used = vec![false; self.entries.len()];
        let mut pairs: Vec<Option<Pairing>> = found
            .iter()
            .map(|finding| {
                let i =
                    (0..self.entries.len()).find(|&i| !used[i] && self.entries[i].same(finding))?;
                used[i] = true;
                Some(Pairing::Same(i))
            })
            .collect();
        let left: Vec<usize> = (0..found.len()).filter(|&f| pairs[f].is_none()).collect();
        for &f in &left {
            let candidates: Vec<usize> = (0..self.entries.len())
                .filter(|&i| !used[i] && self.entries[i].close(&found[f]))
                .collect();
            if let [i] = candidates[..] {
                let rivals = left
                    .iter()
                    .filter(|&&g| pairs[g].is_none() && self.entries[i].close(&found[g]))
                    .count();
                if rivals == 1 {
                    used[i] = true;
                    pairs[f] = Some(Pairing::Carried(i));
                }
            }
        }
        pairs
    }

    /// The file's text: a header saying what the labels mean, then every entry.
    pub fn render(&self) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        if let Some(repo) = &self.repo {
            writeln!(out, "repo = {}", quoted(repo)).expect("writing to a String");
        }
        if let Some(range) = &self.range {
            writeln!(out, "range = {}", quoted(range)).expect("writing to a String");
        }
        for entry in &self.entries {
            out.push_str("\n[[finding]]\n");
            let mut field = |key: &str, value: String| {
                writeln!(out, "{key} = {value}").expect("writing to a String");
            };
            field("unit", quoted(&entry.unit));
            if let Some(pr) = entry.pr {
                field("pr", pr.to_string());
            }
            field("rule", quoted(&entry.rule));
            field("path", quoted(&entry.path));
            if let Some(test) = &entry.test {
                field("test", quoted(test));
            }
            if !entry.tests.is_empty() {
                field("tests", list(&entry.tests));
            }
            field("before", list(&entry.before));
            field("after", list(&entry.after));
            field("message", quoted(&entry.message));
            field("label", quoted(entry.label.map_or("", Label::as_str)));
            field("note", quoted(&entry.note));
        }
        out
    }
}

const HEADER: &str = "\
# Labels for `assertrust backtest --labels`: a verdict on each finding.
#   wrong        nothing got weaker: assertrust misread the change
#   intended     weaker on purpose: the code the test covered went too, or a commit says why
#   unexplained  weaker, and nothing in the change says why
# \"\" is a finding not labelled yet. `--update-labels` rewrites this file: it adds new
# findings and drops false alarms no longer found. Keep remarks in `note`.
";

fn entry_from(value: &Value) -> Result<Entry, String> {
    let table = value.as_table().ok_or("must be a table")?;
    let mut entry = Entry {
        unit: String::new(),
        pr: None,
        rule: String::new(),
        path: String::new(),
        test: None,
        tests: Vec::new(),
        before: Vec::new(),
        after: Vec::new(),
        message: String::new(),
        label: None,
        note: String::new(),
    };
    for (key, value) in table {
        match key.as_str() {
            "unit" => string(key, value)?.clone_into(&mut entry.unit),
            "pr" => {
                let pr = value.as_integer().ok_or("`pr` must be a number")?;
                entry.pr = Some(u64::try_from(pr).map_err(|_| "`pr` must be positive")?);
            }
            "rule" => {
                let rule = string(key, value)?;
                if RuleId::from_name(rule).is_none() {
                    return Err(format!("unknown rule `{rule}`"));
                }
                rule.clone_into(&mut entry.rule);
            }
            "path" => string(key, value)?.clone_into(&mut entry.path),
            "test" => entry.test = Some(string(key, value)?.to_owned()),
            "tests" => entry.tests = strings(key, value)?,
            "before" => entry.before = strings(key, value)?,
            "after" => entry.after = strings(key, value)?,
            "message" => string(key, value)?.clone_into(&mut entry.message),
            "label" => {
                entry.label = match string(key, value)? {
                    "" => None,
                    "wrong" => Some(Label::Wrong),
                    "intended" => Some(Label::Intended),
                    "unexplained" => Some(Label::Unexplained),
                    other => {
                        return Err(format!(
                            "unknown label `{other}` (expected wrong, intended, unexplained or \"\")"
                        ));
                    }
                }
            }
            "note" => string(key, value)?.clone_into(&mut entry.note),
            other => return Err(format!("unknown key `{other}`")),
        }
    }
    for (key, value) in [
        ("unit", &entry.unit),
        ("rule", &entry.rule),
        ("path", &entry.path),
    ] {
        if value.is_empty() {
            return Err(format!("`{key}` is missing"));
        }
    }
    Ok(entry)
}

fn string<'v>(key: &str, value: &'v Value) -> Result<&'v str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn strings(key: &str, value: &Value) -> Result<Vec<String>, String> {
    let error = || format!("`{key}` must be a list of strings");
    value
        .as_array()
        .ok_or_else(error)?
        .iter()
        .map(|item| item.as_str().map(str::to_owned).ok_or_else(error))
        .collect()
}

/// A TOML basic string: quoted, with quotes, backslashes and control characters escaped.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                write!(out, "\\u{:04X}", u32::from(c)).expect("writing to a String")
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|item| quoted(item)).collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(unit: &str, test: &str, before: &str) -> Entry {
        Entry {
            unit: unit.to_owned(),
            pr: None,
            rule: "check-removed".to_owned(),
            path: "a.test.ts".to_owned(),
            test: Some(test.to_owned()),
            tests: Vec::new(),
            before: vec![before.to_owned()],
            after: Vec::new(),
            message: format!("removed {before}"),
            label: None,
            note: String::new(),
        }
    }

    #[test]
    fn a_file_reads_back_as_written() {
        let mut first = entry("1a2b3c4d5e6f", "refunds", "expect(a).toBe('x \"y\"')");
        first.pr = Some(12);
        first.label = Some(Label::Unexplained);
        first.note = "dropped\nin a refactor".to_owned();
        let mut second = entry("9f8e7d6c5b4a", "charges", "expect(b).toBe(2)");
        second.test = None;
        second.tests = vec!["one".to_owned(), "two".to_owned()];
        let labels = Labels {
            repo: Some("https://example.test/repo.git".to_owned()),
            range: Some("aaa..bbb".to_owned()),
            entries: vec![first, second],
        };
        assert_eq!(Labels::parse(&labels.render()).unwrap(), labels);
    }

    #[test]
    fn a_typo_is_an_error() {
        let err = Labels::parse("[[finding]]\nunit = \"1a2b\"\nrule = \"check-removed\"\npath = \"a\"\nlabel = \"rigth\"\n").unwrap_err();
        assert_eq!(
            err,
            "finding 1: unknown label `rigth` (expected wrong, intended, unexplained or \"\")"
        );
        let err =
            Labels::parse("[[finding]]\nunit = \"1a2b\"\nrule = \"check-remved\"\npath = \"a\"\n")
                .unwrap_err();
        assert_eq!(err, "finding 1: unknown rule `check-remved`");
        assert_eq!(
            Labels::parse("rnage = \"a..b\"").unwrap_err(),
            "unknown key `rnage`"
        );
    }

    #[test]
    fn a_finding_printed_differently_keeps_its_label_when_it_is_the_only_one() {
        let labels = Labels {
            entries: vec![
                entry("1a2b3c4d5e6f", "refunds", "expect(a).toBe(1)"),
                entry("1a2b3c4d5e6f", "charges", "expect(b).toBe(2)"),
                entry("1a2b3c4d5e6f", "charges", "expect(c).toBe(3)"),
            ],
            ..Labels::default()
        };
        let found = vec![
            // Same as the second entry.
            entry("1a2b3c4d5e6f", "charges", "expect(b).toBe(2)"),
            // Printed differently: the only left-over entry about `refunds`.
            entry("1a2b3c4d5e6f", "refunds", "expect(a).toBe(1.0)"),
            // Two findings about `charges` left, but one entry: ambiguous.
            entry("1a2b3c4d5e6f", "charges", "expect(c).toBe(3.0)"),
            entry("1a2b3c4d5e6f", "charges", "expect(d).toBe(4)"),
        ];
        assert_eq!(
            labels.pair(&found),
            vec![
                Some(Pairing::Same(1)),
                Some(Pairing::Carried(0)),
                None,
                None
            ]
        );
    }
}
