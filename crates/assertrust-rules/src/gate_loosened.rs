//! `gate-loosened`: CI or test config lets more failures through.
//!
//! ```text
//! before: run: npm test
//! after:  run: npm test || true
//! → job test, step "npm test": failures no longer fail it
//! ```

use std::collections::BTreeMap;

use assertrust_ir::{Command, Gate, Job, Package, Setting, TestStep, Value, Workflow};

use crate::{Confidence, Finding, GateFile, RuleId, number, short};

pub(crate) fn check(gates: &[GateFile], findings: &mut Vec<Finding>) {
    for file in gates {
        let Some(before) = file.before else { continue };
        let path = file.after.unwrap_or(before).path;
        let mut found = Found {
            path,
            findings: &mut *findings,
        };
        match (before.gate, file.after.map(|after| after.gate)) {
            (Gate::Workflow(old), Some(Gate::Workflow(new))) => found.workflow(old, new),
            (Gate::Workflow(old), None) => found.workflow(old, &Workflow::default()),
            (Gate::Config(old), Some(Gate::Config(new))) => found.settings(old, new),
            (Gate::Config(old), None) => found.settings(old, &[]),
            (Gate::Package(old), Some(Gate::Package(new))) => found.package(old, new),
            (Gate::Package(old), None) => found.package(old, &Package::default()),
            _ => {}
        }
    }
}

struct Found<'f> {
    path: &'f str,
    findings: &'f mut Vec<Finding>,
}

impl Found<'_> {
    fn push(
        &mut self,
        line: Option<u32>,
        confidence: Confidence,
        message: String,
        before: Vec<String>,
        after: Vec<String>,
    ) {
        self.findings.push(Finding {
            rule: RuleId::GateLoosened,
            confidence,
            path: self.path.to_owned(),
            line,
            test: None,
            message,
            before,
            after,
        });
    }

    fn workflow(&mut self, old: &Workflow, new: &Workflow) {
        let mut new_jobs: Vec<&Job> = new.jobs.iter().collect();
        let mut unpaired = Vec::new();
        for job in &old.jobs {
            match new_jobs.iter().position(|other| other.id == job.id) {
                Some(at) => self.job(job, new_jobs.remove(at)),
                None => unpaired.push(job),
            }
        }
        // A job renamed: the new one runs one of the same test commands.
        for job in unpaired.into_iter().filter(|job| !job.steps.is_empty()) {
            let renamed = new_jobs.iter().position(|other| {
                other
                    .steps
                    .iter()
                    .any(|step| job.steps.iter().any(|old| old.run == step.run))
            });
            match renamed {
                Some(at) => self.job(job, new_jobs.remove(at)),
                None => {
                    let runs: Vec<String> = job.steps.iter().map(|step| step.run.clone()).collect();
                    self.push(
                        None,
                        Confidence::High,
                        format!(
                            "job {} removed: its tests ({}) no longer run",
                            job.id,
                            short(&runs.join("; "))
                        ),
                        runs,
                        Vec::new(),
                    );
                }
            }
        }
    }

    fn job(&mut self, old: &Job, new: &Job) {
        let at = format!("job {}", new.id);
        self.condition(
            &at,
            new.line,
            old.condition.as_deref(),
            new.condition.as_deref(),
        );
        self.continue_on_error(
            &at,
            new.line,
            old.continue_on_error.as_deref(),
            new.continue_on_error.as_deref(),
        );
        let mut new_steps: Vec<&TestStep> = new.steps.iter().collect();
        let mut unpaired = Vec::new();
        for step in &old.steps {
            match new_steps.iter().position(|other| other.run == step.run) {
                Some(at) => self.step(&new.id, step, new_steps.remove(at)),
                None => unpaired.push(step),
            }
        }
        // One step whose command changed.
        if let ([step], [other]) = (unpaired.as_slice(), new_steps.as_slice()) {
            self.step(&new.id, step, other);
            return;
        }
        for step in unpaired {
            self.push(
                Some(new.line),
                Confidence::High,
                format!("job {}: test step \"{}\" removed", new.id, short(&step.run)),
                vec![step.run.clone()],
                Vec::new(),
            );
        }
    }

    fn step(&mut self, job: &str, old: &TestStep, new: &TestStep) {
        let first = new.run.lines().next().unwrap_or_default();
        let at = format!("job {job}, step \"{}\"", short(first));
        self.condition(
            &at,
            new.line,
            old.condition.as_deref(),
            new.condition.as_deref(),
        );
        self.continue_on_error(
            &at,
            new.line,
            old.continue_on_error.as_deref(),
            new.continue_on_error.as_deref(),
        );
        self.command(
            &at,
            new.line,
            &old.run,
            &new.run,
            Some(&old.command),
            &new.command,
        );
    }

    /// `if:` added or changed: the job or step may not run.
    fn condition(&mut self, at: &str, line: u32, old: Option<&str>, new: Option<&str>) {
        let Some(new) = new else { return };
        if old == Some(new) || is_true(new) {
            return;
        }
        let (confidence, message) = if is_false(new) {
            (Confidence::High, format!("{at}: never runs (if: {new})"))
        } else {
            (
                Confidence::Medium,
                format!("{at}: now runs only if {}", short(new)),
            )
        };
        self.push(
            Some(line),
            confidence,
            message,
            old.map(|old| format!("if: {old}")).into_iter().collect(),
            vec![format!("if: {new}")],
        );
    }

    /// `continue-on-error` turned on: a failure no longer fails the build.
    fn continue_on_error(&mut self, at: &str, line: u32, old: Option<&str>, new: Option<&str>) {
        let Some(new) = new else { return };
        if old == Some(new) || is_false(new) || old.is_some_and(is_true) {
            return;
        }
        let confidence = if is_true(new) {
            Confidence::High
        } else {
            Confidence::Medium
        };
        self.push(
            Some(line),
            confidence,
            format!("{at}: continue-on-error lets failures through"),
            old.map(|old| format!("continue-on-error: {old}"))
                .into_iter()
                .collect(),
            vec![format!("continue-on-error: {new}")],
        );
    }

    /// What a test command does to failures, before and after.
    fn command(
        &mut self,
        at: &str,
        line: u32,
        old_run: &str,
        new_run: &str,
        old: Option<&Command>,
        new: &Command,
    ) {
        let blank = Command::default();
        let old = old.unwrap_or(&blank);
        let mut changes: Vec<(Confidence, String)> = Vec::new();
        if new.swallows && !old.swallows {
            changes.push((Confidence::High, "failures no longer fail it".to_owned()));
        }
        if new.pass_with_no_tests && !old.pass_with_no_tests {
            changes.push((
                Confidence::High,
                "passes when no tests are found".to_owned(),
            ));
        }
        if new.retries.unwrap_or(0) > old.retries.unwrap_or(0) {
            changes.push((
                Confidence::High,
                format!(
                    "retries raised from {} to {}",
                    old.retries.unwrap_or(0),
                    new.retries.unwrap_or(0)
                ),
            ));
        }
        let narrowed: Vec<&String> = new
            .filters
            .iter()
            .filter(|filter| !old.filters.contains(filter))
            .collect();
        if !narrowed.is_empty() {
            let narrowed: Vec<&str> = narrowed.iter().map(|s| s.as_str()).collect();
            changes.push((
                Confidence::Medium,
                format!("now runs only {}", narrowed.join(", ")),
            ));
        }
        let skipped: Vec<&str> = new
            .excludes
            .iter()
            .filter(|exclude| !old.excludes.contains(exclude))
            .map(String::as_str)
            .collect();
        if !skipped.is_empty() {
            changes.push((
                Confidence::High,
                format!("now skips {}", skipped.join(", ")),
            ));
        }
        if changes.is_empty() {
            return;
        }
        let confidence = changes
            .iter()
            .map(|(confidence, _)| *confidence)
            .min()
            .expect("not empty");
        let what: Vec<String> = changes.into_iter().map(|(_, what)| what).collect();
        self.push(
            Some(line),
            confidence,
            format!("{at}: {}", what.join("; ")),
            vec![old_run.to_owned()],
            vec![new_run.to_owned()],
        );
    }

    fn package(&mut self, old: &Package, new: &Package) {
        for script in &new.scripts {
            let Some(command) = &script.command else {
                continue;
            };
            let Some(before) = old.scripts.iter().find(|other| other.name == script.name) else {
                continue;
            };
            if before.run == script.run {
                continue;
            }
            self.command(
                &format!("script {}", script.name),
                script.line,
                &before.run,
                &script.run,
                before.command.as_ref(),
                command,
            );
        }
        self.settings(&old.jest, &new.jest);
    }

    /// Test config settings, by path.
    fn settings(&mut self, old: &[Setting], new: &[Setting]) {
        let new_by_path: BTreeMap<&str, &Setting> = new
            .iter()
            .map(|setting| (setting.path.as_str(), setting))
            .collect();
        for setting in new {
            let before = old.iter().find(|other| other.path == setting.path);
            if before.is_some_and(|before| before.value == setting.value) {
                continue;
            }
            if let Some((confidence, what)) = loosened(&setting.path, before, Some(setting)) {
                self.push(
                    Some(setting.line),
                    confidence,
                    format!("{}: {what}", setting.path),
                    before.map(ToString::to_string).into_iter().collect(),
                    vec![setting.to_string()],
                );
            }
        }
        for setting in old {
            if new_by_path.contains_key(setting.path.as_str()) {
                continue;
            }
            if let Some((confidence, what)) = loosened(&setting.path, Some(setting), None) {
                self.push(
                    None,
                    confidence,
                    format!("{}: {what}", setting.path),
                    vec![setting.to_string()],
                    Vec::new(),
                );
            }
        }
        self.projects(old, new);
    }

    /// A Playwright or vitest project removed: its tests no longer run.
    fn projects(&mut self, old: &[Setting], new: &[Setting]) {
        let names = |settings: &[Setting]| -> Vec<String> {
            let mut names: Vec<String> = settings
                .iter()
                .filter_map(|setting| project(&setting.path))
                .collect();
            names.dedup();
            names
        };
        let kept = names(new);
        for name in names(old) {
            if !kept.contains(&name) {
                self.push(
                    None,
                    Confidence::High,
                    format!("{name} removed: its tests no longer run"),
                    vec![name.clone()],
                    Vec::new(),
                );
            }
        }
    }
}

/// `projects[chromium]` in `projects[chromium].use.channel`.
fn project(path: &str) -> Option<String> {
    let start = path.find("projects[")?;
    let end = path[start..].find(']')? + start + 1;
    Some(path[..end].to_owned())
}

/// Whether a setting's change lets more failures through: its key decides how to read it.
fn loosened(
    path: &str,
    old: Option<&Setting>,
    new: Option<&Setting>,
) -> Option<(Confidence, String)> {
    let key = path.rsplit('.').next().unwrap_or(path);
    let old_value = old.map(|setting| &setting.value);
    let new_value = new.map(|setting| &setting.value);
    let coverage = path.contains("coverage") || path.contains("Coverage");
    match key {
        "retry" | "retries" => {
            let before = old_value.map_or(Some(0.0), number);
            let after = new_value.map_or(Some(0.0), number);
            match (before, after) {
                (Some(b), Some(a)) if a > b => Some((
                    Confidence::High,
                    format!(
                        "retries raised from {} to {}",
                        show(old_value, "0"),
                        show(new_value, "0")
                    ),
                )),
                (Some(0.0), None) => Some((
                    Confidence::Medium,
                    format!("retries set to {}", show(new_value, "?")),
                )),
                _ => None,
            }
        }
        "passWithNoTests" if new_value == Some(&Value::Bool(true)) => Some((
            Confidence::High,
            "passes when no tests are found".to_owned(),
        )),
        "allowOnly" if new_value == Some(&Value::Bool(true)) => Some((
            Confidence::Medium,
            "a committed .only now passes".to_owned(),
        )),
        "forbidOnly" if old_value.is_some() && new_value != Some(&Value::Bool(true)) => Some((
            Confidence::Medium,
            "a committed .only may now pass".to_owned(),
        )),
        "lines" | "functions" | "branches" | "statements"
            if coverage || path.contains("threshold") || path.contains("Threshold") =>
        {
            match (old_value.and_then(number), new_value) {
                (Some(_), None) => {
                    Some((Confidence::High, "coverage threshold removed".to_owned()))
                }
                (Some(b), Some(new)) if number(new).is_some_and(|a| a < b) => Some((
                    Confidence::High,
                    format!(
                        "coverage threshold lowered from {} to {new}",
                        show(old_value, "?")
                    ),
                )),
                _ => None,
            }
        }
        "exclude" | "testIgnore" | "testPathIgnorePatterns" | "coveragePathIgnorePatterns" => {
            let added = items(new_value)
                .into_iter()
                .filter(|item| !items(old_value).contains(item))
                .collect::<Vec<_>>();
            if added.is_empty() {
                return None;
            }
            let list = short(&added.join(", "));
            if coverage {
                Some((Confidence::Medium, format!("coverage now ignores {list}")))
            } else {
                Some((Confidence::High, format!("now skips {list}")))
            }
        }
        "include" | "testMatch" | "testRegex" | "roots" if !coverage => {
            let removed = items(old_value)
                .into_iter()
                .filter(|item| !items(new_value).contains(item))
                .collect::<Vec<_>>();
            (!removed.is_empty() && new_value.is_some()).then(|| {
                (
                    Confidence::High,
                    format!("no longer includes {}", short(&removed.join(", "))),
                )
            })
        }
        "testDir" if old_value.is_some() && new_value.is_some() => Some((
            Confidence::Medium,
            format!(
                "tests now come from {} instead of {}",
                show(new_value, "?"),
                show(old_value, "?")
            ),
        )),
        _ => None,
    }
}

/// The items of a list setting (one item for a plain value), as written.
fn items(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(ToString::to_string).collect(),
        Some(value) => vec![value.to_string()],
        None => Vec::new(),
    }
}

fn show(value: Option<&Value>, missing: &str) -> String {
    value.map_or_else(|| missing.to_owned(), |value| short(&value.to_string()))
}

fn is_true(value: &str) -> bool {
    matches!(value, "true" | "${{ true }}")
}

fn is_false(value: &str) -> bool {
    matches!(value, "false" | "${{ false }}")
}
