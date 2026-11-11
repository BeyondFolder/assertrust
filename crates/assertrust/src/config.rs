//! `assertrust.toml`, the optional config file at the repo root (`docs/architecture.md`,
//! step 6, "Config"). Flags given on the command line win over it.
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

use assertrust_git::{Classifier, Repo};
use assertrust_rules::{Confidence, RuleId};
use toml::{Table, Value};

use crate::cli::FailOn;

/// The file's name, at the repo root.
pub const FILE: &str = "assertrust.toml";

#[derive(Debug)]
pub struct Config {
    pub fail_on: FailOn,
    pub show_transient: bool,
    pub classifier: Classifier,
    rules: HashMap<RuleId, Setting>,
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
            rules: HashMap::new(),
        }
    }
}

impl Config {
    /// The repo's `assertrust.toml`, or the defaults when there's none.
    pub fn load(repo: &Repo) -> Result<Self, String> {
        let path = repo.root().join(FILE);
        match fs::read_to_string(&path) {
            Ok(source) => Self::parse(&source).map_err(|err| format!("{FILE}: {err}")),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(format!("couldn't read {FILE}: {err}")),
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
                "files" => config.classifier = files(value)?,
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
fn files(value: &Value) -> Result<Classifier, String> {
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
    Classifier::new(&test, &gate, &test_support).map_err(|err| format!("files: {err}"))
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
    fn fail_on_levels() {
        assert!(!FailOn::Never.fails(Confidence::High));
        assert!(FailOn::High.fails(Confidence::High));
        assert!(!FailOn::High.fails(Confidence::Medium));
        assert!(FailOn::Medium.fails(Confidence::Medium));
        assert!(!FailOn::Medium.fails(Confidence::Low));
        assert!(FailOn::Low.fails(Confidence::Low));
    }
}
