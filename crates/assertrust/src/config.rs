//! `assertrust.toml`, the optional config file at the repo root (`docs/architecture.md`,
//! step 6, "Config"). Flags given on the command line win over it.
//!
//! A range or PR-style check uses the file as it is at its base, so a change can't loosen
//! the check that judges it: the change is reported like a loosened CI config instead.
//!
//! ```text
//! - check-removed = "on"
//! + check-removed = "off"
//! → assertrust.toml:5  HIGH  gate-loosened
//!     turned check-removed off: its findings are no longer reported
//! ```
//!
//! ```toml
//! fail-on = "high"             # never | high | medium | low
//! show-transient = false
//!
//! [files]
//! test = ["**/*.test.ts"]
//!
//! [rules]
//! matcher-swapped = "off"      # off | on | judge
//! ```

use std::collections::HashMap;
use std::fs;
use std::io;

use assertrust_git::{Classifier, Mode, Repo, Rev};
use assertrust_rules::{Confidence, Finding, RuleId};
use toml::{Table, Value};

use crate::cli::FailOn;

/// The file's name, at the repo root.
pub const FILE: &str = "assertrust.toml";

#[derive(Debug)]
pub struct Config {
    pub fail_on: FailOn,
    pub show_transient: bool,
    pub classifier: Classifier,
    /// The `[files]` patterns behind `classifier`, to tell when a change drops some.
    patterns: Patterns,
    rules: HashMap<RuleId, Setting>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Patterns {
    test: Vec<String>,
    gate: Vec<String>,
}

/// What a rule does with its findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Not reported at all.
    Off,
    /// Shown.
    On,
    /// Left to the LLM judge (step 7). Until it exists, shown only with `-v`.
    Judge,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fail_on: FailOn::Never,
            show_transient: false,
            classifier: Classifier::default(),
            patterns: Patterns::default(),
            rules: HashMap::new(),
        }
    }
}

impl Default for Patterns {
    fn default() -> Self {
        let (test, gate, _) = Classifier::default_patterns();
        Self { test, gate }
    }
}

impl Config {
    /// The config a check of `mode` runs with, and notes about it. A range or PR-style check
    /// uses the file at its base, so a change to it applies once it lands; a staged or
    /// worktree check uses the file on disk.
    pub fn for_check(repo: &Repo, mode: &Mode) -> Result<(Self, Vec<String>), String> {
        let Some(base) = repo.base(mode).map_err(|err| err.to_string())? else {
            return Ok((Self::load(repo)?, Vec::new()));
        };
        let at_base = repo.read_file(&base, FILE).map_err(|err| err.to_string())?;
        let config = match at_base.as_deref().map(Self::parse) {
            None => Self::default(),
            Some(Ok(config)) => config,
            // A broken file on the main branch mustn't block the change that fixes it.
            Some(Err(err)) => {
                let note =
                    format!("{FILE} at {base} doesn't parse ({err}), so the one on disk applies");
                return Ok((Self::load(repo)?, vec![note]));
            }
        };
        let mut notes = Vec::new();
        if read_on_disk(repo)? != at_base {
            notes.push(format!(
                "{FILE} differs from the one at {base}, which this check uses: a change to it applies once it lands"
            ));
        }
        Ok((config, notes))
    }

    /// The config CI ran a check of `mode` with, back then (`assertrust backtest`): the file
    /// at its base, whatever is on disk now. When it doesn't parse there, CI used the one on
    /// disk, which was the file at `head`, the commit the check ran on.
    pub fn as_ci_ran(repo: &Repo, mode: &Mode, head: &str) -> Result<(Self, Vec<String>), String> {
        let Some(base) = repo.base(mode).map_err(|err| err.to_string())? else {
            return Ok((Self::default(), Vec::new()));
        };
        let at = |rev: &Rev| -> Result<Option<Result<Self, String>>, String> {
            let source = repo.read_file(rev, FILE).map_err(|err| err.to_string())?;
            Ok(source.as_deref().map(Self::parse))
        };
        match at(&base)? {
            None => Ok((Self::default(), Vec::new())),
            Some(Ok(config)) => Ok((config, Vec::new())),
            Some(Err(err)) => {
                let head = Rev::Commit(head.to_owned());
                let note =
                    format!("{FILE} at {base} doesn't parse ({err}), so the one at {head} applies");
                match at(&head)? {
                    None => Ok((Self::default(), vec![note])),
                    Some(Ok(config)) => Ok((config, vec![note])),
                    Some(Err(err)) => Err(format!("{FILE} at {head}: {err}")),
                }
            }
        }
    }

    /// The repo's `assertrust.toml`, or the defaults when there's none.
    pub fn load(repo: &Repo) -> Result<Self, String> {
        match read_on_disk(repo)? {
            Some(source) => Self::parse(&source).map_err(|err| format!("{FILE}: {err}")),
            None => Ok(Self::default()),
        }
    }

    /// Reads the file's contents. Unknown keys are errors, so a typo doesn't go unnoticed.
    pub fn parse(source: &str) -> Result<Self, String> {
        let table: Table = toml::from_str(source).map_err(|err| err.message().to_owned())?;
        let mut config = Self::default();
        for (key, value) in &table {
            match key.as_str() {
                "fail-on" => {
                    config.fail_on = match string(key, value)? {
                        "never" => FailOn::Never,
                        "high" => FailOn::High,
                        "medium" => FailOn::Medium,
                        "low" => FailOn::Low,
                        other => {
                            return Err(format!(
                                "`fail-on` is `{other}`; it can be never, high, medium or low"
                            ));
                        }
                    };
                }
                "show-transient" => {
                    config.show_transient = value
                        .as_bool()
                        .ok_or_else(|| "`show-transient` must be true or false".to_owned())?;
                }
                "files" => (config.classifier, config.patterns) = files(value)?,
                "rules" => config.rules = rules(value)?,
                other => return Err(format!("unknown key `{other}`")),
            }
        }
        Ok(config)
    }

    /// What `rule` does: `outcome-changed` is left to the judge unless the file says
    /// otherwise; every other rule is on.
    pub fn setting(&self, rule: RuleId) -> Setting {
        self.rules.get(&rule).copied().unwrap_or(match rule {
            RuleId::OutcomeChanged => Setting::Judge,
            _ => Setting::On,
        })
    }

    /// What a change from `before` to `after` loosened, as `gate-loosened` findings on the
    /// file: a rule turned off or left to the judge, `fail-on` lowered, test or CI file
    /// patterns dropped. `text` is the file after the change, for the lines.
    pub fn loosened(before: &Self, after: &Self, text: &str) -> Vec<Finding> {
        let mut findings = Vec::new();
        let mut push = |line, confidence, message, before, after| {
            findings.push(Finding {
                rule: RuleId::GateLoosened,
                confidence,
                path: FILE.to_owned(),
                line,
                test: None,
                tests: Vec::new(),
                message,
                before,
                after,
            });
        };
        if strictness(after.fail_on) < strictness(before.fail_on) {
            let (old, new) = (fail_on_name(before.fail_on), fail_on_name(after.fail_on));
            push(
                key_line(text, None, "fail-on"),
                Confidence::High,
                format!("lowered fail-on from {old} to {new}"),
                vec![format!("fail-on = \"{old}\"")],
                vec![format!("fail-on = \"{new}\"")],
            );
        }
        for rule in RuleId::ALL {
            let (old, new) = (before.setting(rule), after.setting(rule));
            if new.shows() >= old.shows() {
                continue;
            }
            let message = match new {
                Setting::Off => format!("turned {rule} off: its findings are no longer reported"),
                _ => format!("left {rule} to the judge: its findings show only with -v"),
            };
            push(
                key_line(text, Some("rules"), rule.as_str()),
                Confidence::High,
                message,
                vec![format!("{rule} = \"{}\"", old.as_str())],
                vec![format!("{rule} = \"{}\"", new.as_str())],
            );
        }
        let kinds = [
            (
                "test",
                "test files",
                &before.patterns.test,
                &after.patterns.test,
            ),
            (
                "gate",
                "CI and test config files",
                &before.patterns.gate,
                &after.patterns.gate,
            ),
        ];
        for (key, what, old, new) in kinds {
            let dropped: Vec<String> = old
                .iter()
                .filter(|pattern| !new.contains(pattern))
                .map(|pattern| format!("`{pattern}`"))
                .collect();
            if dropped.is_empty() {
                continue;
            }
            // Often a pattern rewritten to match the same files.
            push(
                key_line(text, Some("files"), key),
                Confidence::Medium,
                format!("no longer counts {} as {what}", dropped.join(", ")),
                old.clone(),
                new.clone(),
            );
        }
        findings
    }
}

impl Setting {
    /// How much of a rule's findings people see: off < judge (with `-v`) < on.
    fn shows(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Judge => 1,
            Self::On => 2,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Judge => "judge",
        }
    }
}

/// How many findings make a check fail: never < high < medium < low.
fn strictness(fail_on: FailOn) -> u8 {
    match fail_on {
        FailOn::Never => 0,
        FailOn::High => 1,
        FailOn::Medium => 2,
        FailOn::Low => 3,
    }
}

fn fail_on_name(fail_on: FailOn) -> &'static str {
    match fail_on {
        FailOn::Never => "never",
        FailOn::High => "high",
        FailOn::Medium => "medium",
        FailOn::Low => "low",
    }
}

/// The line `key = …` is on, under `[table]` (or above every table, for `None`).
fn key_line(text: &str, table: Option<&str>, key: &str) -> Option<u32> {
    let quoted = format!("\"{key}\"");
    let mut current = None;
    for (line, number) in text.lines().zip(1..) {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            current = header.split(']').next().map(str::trim);
            continue;
        }
        if current != table {
            continue;
        }
        let rest = line
            .strip_prefix(key)
            .or_else(|| line.strip_prefix(quoted.as_str()));
        if rest.is_some_and(|rest| rest.trim_start().starts_with('=')) {
            return Some(number);
        }
    }
    None
}

/// The file on disk, if there's one.
fn read_on_disk(repo: &Repo) -> Result<Option<String>, String> {
    match fs::read_to_string(repo.root().join(FILE)) {
        Ok(source) => Ok(Some(source)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("couldn't read {FILE}: {err}")),
    }
}

impl FailOn {
    /// Whether a finding this sure makes the check fail.
    pub fn fails(self, confidence: Confidence) -> bool {
        match self {
            Self::Never => false,
            Self::High => confidence == Confidence::High,
            Self::Medium => confidence <= Confidence::Medium,
            Self::Low => true,
        }
    }
}

fn string<'v>(key: &str, value: &'v Value) -> Result<&'v str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("`{key}` must be a string"))
}

/// `[files]`: glob patterns per kind, replacing the built-in ones for the kinds given.
fn files(value: &Value) -> Result<(Classifier, Patterns), String> {
    let table = value
        .as_table()
        .ok_or_else(|| "`files` must be a table".to_owned())?;
    let (mut test, mut gate, mut test_support) = Classifier::default_patterns();
    for (key, value) in table {
        let patterns = value
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| format!("`files.{key}` must be a list of patterns"))?;
        match key.as_str() {
            "test" => test = patterns,
            "gate" => gate = patterns,
            "test-support" => test_support = patterns,
            other => {
                return Err(format!(
                    "unknown key `files.{other}`; it can be test, test-support or gate"
                ));
            }
        }
    }
    let classifier =
        Classifier::new(&test, &gate, &test_support).map_err(|err| format!("files: {err}"))?;
    Ok((classifier, Patterns { test, gate }))
}

/// `[rules]`: `rule-id = "off" | "on" | "judge"`.
fn rules(value: &Value) -> Result<HashMap<RuleId, Setting>, String> {
    let table = value
        .as_table()
        .ok_or_else(|| "`rules` must be a table".to_owned())?;
    let mut settings = HashMap::new();
    for (key, value) in table {
        let rule = RuleId::from_name(key).ok_or_else(|| format!("unknown rule `{key}`"))?;
        let setting = match string(&format!("rules.{key}"), value)? {
            "off" => Setting::Off,
            "on" => Setting::On,
            "judge" => Setting::Judge,
            other => {
                return Err(format!(
                    "`rules.{key}` is `{other}`; it can be off, on or judge"
                ));
            }
        };
        settings.insert(rule, setting);
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_key() {
        let config = Config::parse(
            r#"
fail-on = "medium"
show-transient = true

[files]
test = ["spec/**/*.ts"]

[rules]
matcher-swapped = "off"
outcome-changed = "on"
"#,
        )
        .unwrap();
        assert_eq!(config.fail_on, FailOn::Medium);
        assert!(config.show_transient);
        assert_eq!(config.setting(RuleId::MatcherSwapped), Setting::Off);
        assert_eq!(config.setting(RuleId::OutcomeChanged), Setting::On);
        assert_eq!(config.setting(RuleId::CheckRemoved), Setting::On);
        assert_eq!(
            config.classifier.classify("spec/a.ts"),
            assertrust_git::FileKind::Test
        );
    }

    #[test]
    fn typos_are_errors() {
        assert_eq!(
            Config::parse("fail_on = \"high\"").unwrap_err(),
            "unknown key `fail_on`"
        );
        assert_eq!(
            Config::parse("[rules]\ncheck-remove = \"off\"").unwrap_err(),
            "unknown rule `check-remove`"
        );
        assert_eq!(
            Config::parse("fail-on = \"always\"").unwrap_err(),
            "`fail-on` is `always`; it can be never, high, medium or low"
        );
    }

    #[test]
    fn a_change_that_loosens_the_file_is_a_finding() {
        let before = Config::parse("fail-on = \"medium\"\n").unwrap();
        let text = "fail-on = \"high\"\n\n[files]\ntest = [\"src/**/*.test.ts\"]\n\n[rules]\n\"matcher-swapped\" = \"judge\"\noutcome-changed = \"on\"\n";
        let after = Config::parse(text).unwrap();
        let found: Vec<(Option<u32>, Confidence, String)> = Config::loosened(&before, &after, text)
            .into_iter()
            .map(|finding| (finding.line, finding.confidence, finding.message))
            .collect();
        assert_eq!(
            found,
            [
                (Some(1), Confidence::High, "lowered fail-on from medium to high".to_owned()),
                (
                    Some(7),
                    Confidence::High,
                    "left matcher-swapped to the judge: its findings show only with -v".to_owned()
                ),
                (
                    Some(4),
                    Confidence::Medium,
                    "no longer counts `**/*.{test,spec}.{ts,tsx,js,jsx,mts,cts,mjs,cjs}`, `**/__tests__/**/*.{ts,tsx,js,jsx,mts,cts,mjs,cjs}` as test files".to_owned()
                ),
            ]
        );
        // Tightening isn't. Going back loosens what `after` tightened: outcome-changed is
        // left to the judge again, and the `src` pattern is dropped.
        assert!(Config::loosened(&Config::default(), &before, "").is_empty());
        let back: Vec<String> = Config::loosened(&after, &before, "")
            .into_iter()
            .map(|finding| finding.message)
            .collect();
        assert_eq!(
            back,
            [
                "left outcome-changed to the judge: its findings show only with -v",
                "no longer counts `src/**/*.test.ts` as test files"
            ]
        );
    }

    #[test]
    fn fail_on_levels() {
        assert!(!FailOn::Never.fails(Confidence::High));
        assert!(FailOn::High.fails(Confidence::High));
        assert!(!FailOn::High.fails(Confidence::Medium));
        assert!(FailOn::Medium.fails(Confidence::Medium));
        assert!(!FailOn::Medium.fails(Confidence::Low));
        assert!(FailOn::Low.fails(Confidence::Low));
    }
}
