//! `gate-loosened`: CI or test config lets more failures through.
//!
//! ```text
//! before: run: npm test
//! after:  run: npm test || true
//! → job test, step "npm test": failures no longer fail it
//! ```

use std::collections::{BTreeMap, BTreeSet};

use assertrust_ir::{Command, Gate, Job, Package, Script, Setting, TestStep, Value, Workflow};

use crate::{Confidence, Finding, GateFile, RuleId, number, short};

pub(crate) fn check(gates: &[GateFile], findings: &mut Vec<Finding>) {
    // The projects each folder's configs list now, in any file: vitest 3 moves them from
    // `vitest.workspace.ts` into `vitest.config.ts`'s `test.projects`.
    let projects: BTreeSet<(&str, &str)> = gates
        .iter()
        .filter_map(|file| file.after)
        .flat_map(|after| {
            let settings = match after.gate {
                Gate::Config(settings) => settings.as_slice(),
                _ => &[],
            };
            settings
                .iter()
                .filter_map(|setting| project_name(&setting.path))
                .map(|name| (folder(after.path), name))
        })
        .collect();
    // Each project's settings before, by folder and name, in any file: a project moved to
    // another file or key is compared with what it was.
    let mut before_projects: BTreeMap<(&str, &str), Vec<&Setting>> = BTreeMap::new();
    for before in gates.iter().filter_map(|file| file.before) {
        let Gate::Config(settings) = before.gate else {
            continue;
        };
        for setting in settings {
            if let Some(name) = project_name(&setting.path) {
                before_projects
                    .entry((folder(before.path), name))
                    .or_default()
                    .push(setting);
            }
        }
    }
    for file in gates {
        let Some(before) = file.before else { continue };
        let path = file.after.unwrap_or(before).path;
        let mut found = Found {
            path,
            findings: &mut *findings,
            projects: &projects,
            before_projects: &before_projects,
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
    /// The projects listed now, by folder, in any config file.
    projects: &'f BTreeSet<(&'f str, &'f str)>,
    /// Each project's settings before, by folder and name, in any config file.
    before_projects: &'f BTreeMap<(&'f str, &'f str), Vec<&'f Setting>>,
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
        // A job renamed: the new one runs tests the same way (`npm test`, `npx playwright test`).
        for job in unpaired.into_iter().filter(|job| !job.steps.is_empty()) {
            let renamed = new_jobs.iter().position(|other| {
                other.steps.iter().any(|step| {
                    job.steps
                        .iter()
                        .any(|old| old.command.runner == step.command.runner)
                })
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
        // Only a job that ran tests can let their failures through: not `lint` or `publish`.
        if !old.steps.is_empty() || looks_like_tests(old) {
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
        }
        let (pairs, removed) = pair_steps(&old.steps, &new.steps);
        for (step, other) in pairs {
            self.step(&new.id, step, other);
        }
        for step in removed {
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
        if old == Some(new) || runs_anyway(new) {
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
        let (only, dropped) = narrowed(old, new);
        if !only.is_empty() {
            changes.push((
                Confidence::Medium,
                format!("now runs only {}", only.join(", ")),
            ));
        }
        if !dropped.is_empty() {
            changes.push((
                Confidence::Medium,
                format!("no longer runs {}", dropped.join(", ")),
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
        let mut added: Vec<&Script> = new
            .scripts
            .iter()
            .filter(|script| !old.scripts.iter().any(|other| other.name == script.name))
            .collect();
        for before in &old.scripts {
            let Some(old_command) = &before.command else {
                continue;
            };
            let after = new
                .scripts
                .iter()
                .find(|script| script.name == before.name)
                .or_else(|| {
                    // Renamed: a new script that runs the tests the same way.
                    let at = added.iter().position(|script| {
                        script
                            .command
                            .as_ref()
                            .is_some_and(|command| command.runner == old_command.runner)
                    })?;
                    Some(added.remove(at))
                });
            let Some(after) = after else {
                // Medium: CI calling the script now fails loudly, so it may not have run there.
                self.push(
                    None,
                    Confidence::Medium,
                    format!(
                        "script {} removed: its tests ({}) no longer run from it",
                        before.name,
                        short(&before.run)
                    ),
                    vec![before.run.clone()],
                    Vec::new(),
                );
                continue;
            };
            if after.run == before.run {
                continue;
            }
            let at = format!("script {}", after.name);
            match &after.command {
                Some(command) => self.command(
                    &at,
                    after.line,
                    &before.run,
                    &after.run,
                    Some(old_command),
                    command,
                ),
                // `echo skip` passes without testing anything; another command (`node
                // scripts/ci.js`) may still run the tests in a way assertrust doesn't follow.
                None => self.push(
                    Some(after.line),
                    if after.noop {
                        Confidence::High
                    } else {
                        Confidence::Medium
                    },
                    format!("{at}: no longer runs {}", old_command.runner),
                    vec![before.run.clone()],
                    vec![after.run.clone()],
                ),
            }
        }
        self.settings(&old.jest, &new.jest);
    }

    /// Test config settings, by path.
    fn settings(&mut self, old: &[Setting], new: &[Setting]) {
        let folder = folder(self.path);
        let here_before = project_names(old);
        let mut old = old.to_vec();
        // A project moved here from another file or key (vitest 3: from `vitest.workspace.ts`
        // into `test.projects`) is compared with what it was, under its new name.
        for project in project_names(new) {
            if here_before.contains(&project) {
                continue;
            }
            let Some(before) =
                project_name(&project).and_then(|name| self.before_projects.get(&(folder, name)))
            else {
                continue;
            };
            for setting in before {
                let rest = project_name(&setting.path)
                    .and_then(|name| setting.path.split_once(&format!("[{name}]")))
                    .map_or("", |(_, rest)| rest);
                old.push(Setting {
                    path: format!("{project}{rest}"),
                    ..(*setting).clone()
                });
            }
        }
        let (old, new) = (&normalized(&old), &normalized(new));
        let new_by_path: BTreeMap<&str, &Setting> = new
            .iter()
            .map(|setting| (setting.path.as_str(), setting))
            .collect();
        let old_projects = project_names(old);
        let new_projects = project_names(new);
        // What each project includes, to tell a file moved to another project from a skip.
        let included: Vec<(Option<String>, String)> = new
            .iter()
            .filter(|setting| {
                let key = setting.path.rsplit('.').next().unwrap_or_default();
                matches!(key, "include" | "testMatch") && picks_tests(&setting.path)
            })
            .flat_map(|setting| {
                let project = project(&setting.path);
                items(Some(&setting.value))
                    .into_iter()
                    .map(move |item| (project.clone(), item))
            })
            .collect();
        for setting in new {
            let project = project(&setting.path);
            // A new project only adds tests.
            if project
                .as_ref()
                .is_some_and(|project| !old_projects.contains(project))
            {
                continue;
            }
            let before = old.iter().find(|other| other.path == setting.path);
            if before.is_some_and(|before| before.value == setting.value) {
                continue;
            }
            let elsewhere: Vec<&str> = included
                .iter()
                .filter(|(other, _)| *other != project)
                .map(|(_, item)| item.as_str())
                .collect();
            if let Some((confidence, what)) =
                loosened(&setting.path, before, Some(setting), &elsewhere)
            {
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
            // A project that moved is compared under its new name, above.
            if project(&setting.path).is_some_and(|project| {
                !new_projects.contains(&project)
                    && project_name(&project)
                        .is_some_and(|name| self.projects.contains(&(folder, name)))
            }) {
                continue;
            }
            if let Some((confidence, what)) = loosened(&setting.path, Some(setting), None, &[]) {
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

    /// A Playwright or vitest project removed: its tests no longer run. Unless it's now
    /// listed in another config file of the same folder.
    fn projects(&mut self, old: &[Setting], new: &[Setting]) {
        let kept = project_names(new);
        let folder = folder(self.path);
        for name in project_names(old) {
            let moved =
                project_name(&name).is_some_and(|bare| self.projects.contains(&(folder, bare)));
            if !kept.contains(&name) && !moved {
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

/// Pairs each old test step with what it became: the same script, else the same name,
/// else the same runner (`npm test` → `npm test || true`), else the one step left on each
/// side. The old steps left over were removed.
fn pair_steps<'s>(
    old: &'s [TestStep],
    new: &'s [TestStep],
) -> (Vec<(&'s TestStep, &'s TestStep)>, Vec<&'s TestStep>) {
    let mut left: Vec<&TestStep> = old.iter().collect();
    let mut new: Vec<&TestStep> = new.iter().collect();
    let mut pairs = Vec::new();
    let ways: [fn(&TestStep, &TestStep) -> bool; 4] = [
        |a, b| a.run == b.run,
        |a, b| a.name.is_some() && a.name == b.name,
        |a, b| a.command.runner == b.command.runner,
        |a, b| script(&a.command.runner).is_some_and(|run| Some(run) == script(&b.command.runner)),
    ];
    for same in ways {
        left.retain(
            |step| match new.iter().position(|other| same(step, other)) {
                Some(at) => {
                    pairs.push((*step, new.remove(at)));
                    false
                }
                None => true,
            },
        );
    }
    if let ([step], [other]) = (left.as_slice(), new.as_slice()) {
        pairs.push((step, other));
        left.clear();
    }
    (pairs, left)
}

/// The script a runner runs, whatever tool runs it: `pnpm test` and `nub run test` both
/// run `test`, `pnpm run --filter api test:all` runs `--filter api test:all`. `None` for a
/// runner called directly (`vitest`, `node --test`): that's no script.
fn script(runner: &str) -> Option<String> {
    let words: Vec<&str> = runner
        .split(' ')
        .skip_while(|word| word.contains('='))
        .skip(1)
        .collect();
    let words = match words.first() {
        Some(&"run" | &"run-script") => &words[1..],
        _ => &words[..],
    };
    (!words.is_empty()).then(|| words.join(" "))
}

/// A job that looks like it runs tests even when assertrust sees no test step: its id or
/// name says so (`test`, `e2e`), or it calls a reusable workflow that does
/// (`uses: ./.github/workflows/test.yml`).
fn looks_like_tests(job: &Job) -> bool {
    let says_tests = |text: &str| {
        text.to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| matches!(word, "test" | "tests" | "e2e"))
    };
    let uses = job
        .uses
        .as_deref()
        .map(|uses| uses.split('@').next().unwrap_or(uses))
        .map(|path| path.rsplit('/').next().unwrap_or(path));
    [Some(job.id.as_str()), job.name.as_deref(), uses]
        .into_iter()
        .flatten()
        .any(says_tests)
}

/// Which filters make a test run smaller, judged per kind of filter (paths, `--project`,
/// `-t`, `--grep`, `--changed`, …): the ones it now runs only, and the ones it dropped.
///
/// - A kind of filter the run didn't have narrows it: `vitest run test/unit` where there
///   was no path, `-t refunds` where there was no name pattern.
/// - Paths and `--project` add up: another one runs more, while one dropped from several
///   runs less (`test/unit test/api` → `test/unit`), and so does a path inside a dropped
///   one (`tests` → `tests/smoke`).
/// - Otherwise a filter swapped for another of its kind can't be compared, so it isn't
///   flagged: `test/unit` → `test/api`, `-t refunds` → `-t charges`. Neither is a run with
///   another `--config` (`--config fastly.config.ts` → `--project fastly`).
fn narrowed<'f>(old: &'f Command, new: &'f Command) -> (Vec<&'f str>, Vec<&'f str>) {
    if old.config.is_some() && old.config != new.config {
        return (Vec::new(), Vec::new());
    }
    let kind = |filter: &str| -> String {
        if filter.starts_with('-') {
            filter.split(' ').next().unwrap_or(filter).to_owned()
        } else {
            String::new()
        }
    };
    // In the order they're written.
    let mut kinds: Vec<String> = Vec::new();
    for filter in old.filters.iter().chain(&new.filters) {
        let kind = kind(filter);
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    let (mut only, mut dropped) = (Vec::new(), Vec::new());
    for wanted in kinds {
        let of_kind = |filters: &'f [String]| -> Vec<&'f String> {
            filters
                .iter()
                .filter(|filter| kind(filter) == wanted)
                .collect()
        };
        let (old_kind, new_kind) = (of_kind(&old.filters), of_kind(&new.filters));
        let added: Vec<&str> = new_kind
            .iter()
            .filter(|filter| !old_kind.contains(filter))
            .map(|filter| filter.as_str())
            .collect();
        let gone: Vec<&str> = old_kind
            .iter()
            .filter(|filter| !new_kind.contains(filter))
            .map(|filter| filter.as_str())
            .collect();
        if old_kind.is_empty() {
            only.extend(added);
        } else if wanted.is_empty() || wanted == "--project" {
            only.extend(
                added
                    .iter()
                    .filter(|path| wanted.is_empty() && gone.iter().any(|old| inside(path, old))),
            );
            if added.is_empty() && !new_kind.is_empty() {
                dropped.extend(gone);
            }
        }
    }
    (only, dropped)
}

/// Whether `path` is inside the folder `parent`: `tests/smoke` in `tests`.
fn inside(path: &str, parent: &str) -> bool {
    let parent = parent.trim_end_matches('/');
    path.strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

/// Whether a pattern setting picks which test files run (or count for coverage): vitest's
/// `test.include` / `test.exclude` and `test.coverage.exclude`, jest's and Playwright's
/// top-level `testMatch`, `testIgnore`, `testPathIgnorePatterns`, …, also inside a project.
/// Not Vite's `optimizeDeps.exclude` or `server.watch.exclude`.
fn picks_tests(path: &str) -> bool {
    let local = path.rsplit_once("].").map_or(path, |(_, rest)| rest);
    let local = local.strip_prefix("jest.").unwrap_or(local);
    match local.rsplit_once('.') {
        Some((place, "include" | "exclude")) => matches!(place, "test" | "test.coverage"),
        Some(_) => false,
        None => !matches!(local, "include" | "exclude"),
    }
}

/// `projects[chromium]` in `projects[chromium].use.channel`. vitest 3.0 also lists them
/// under `test.workspace`.
fn project(path: &str) -> Option<String> {
    let (start, name) = project_at(path)?;
    Some(path[..start + name.len() + 1].to_owned())
}

/// `chromium` in `projects[chromium].use.channel`.
fn project_name(path: &str) -> Option<&str> {
    project_at(path).map(|(_, name)| name)
}

/// Where a project's name starts in a setting's path, and the name.
fn project_at(path: &str) -> Option<(usize, &str)> {
    let start = ["projects[", "workspace["]
        .iter()
        .filter_map(|key| path.find(key).map(|at| at + key.len()))
        .min()?;
    let end = path[start..].find(']')? + start;
    Some((start, &path[start..end]))
}

/// The projects a config lists, as `projects[chromium]`, in order.
fn project_names(settings: &[Setting]) -> Vec<String> {
    let mut names: Vec<String> = settings
        .iter()
        .filter_map(|setting| project(&setting.path))
        .collect();
    names.dedup();
    names
}

/// `packages/web` for `packages/web/vitest.config.ts`, empty at the root.
fn folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// The settings with vitest's negated includes as excludes, the same thing written another
/// way: `include: ['src/**', '!src/legacy/**']` excludes `src/legacy/**`.
fn normalized(settings: &[Setting]) -> Vec<Setting> {
    let mut out = settings.to_vec();
    let mut moved = Vec::new();
    for setting in &mut out {
        let Some(place) = setting.path.strip_suffix(".include") else {
            continue;
        };
        let Value::Array(items) = &mut setting.value else {
            continue;
        };
        let negated: Vec<Value> = items
            .iter()
            .filter_map(|item| match item {
                Value::String(item) => item
                    .strip_prefix('!')
                    .map(|item| Value::String(item.to_owned())),
                _ => None,
            })
            .collect();
        if negated.is_empty() || !picks_tests(&setting.path) {
            continue;
        }
        items.retain(|item| !matches!(item, Value::String(item) if item.starts_with('!')));
        moved.push((format!("{place}.exclude"), negated, setting.line));
    }
    for (path, items, line) in moved {
        match out.iter_mut().find(|setting| setting.path == path) {
            Some(Setting {
                value: Value::Array(excluded),
                ..
            }) => excluded.extend(items),
            Some(_) => {}
            None => out.push(Setting {
                path,
                value: Value::Array(items),
                line,
            }),
        }
    }
    out
}

/// Patterns with their `{a,b}` alternatives spelled out, without quotes:
/// `'src/*.{test,spec}.ts'` → `src/*.test.ts`, `src/*.spec.ts`.
fn expanded<S: AsRef<str>>(patterns: &[S]) -> Vec<String> {
    let mut out = Vec::new();
    for pattern in patterns {
        let pattern = pattern.as_ref().trim_matches(['\'', '"']);
        expand(pattern, &mut out);
    }
    out
}

fn expand(pattern: &str, out: &mut Vec<String>) {
    let Some(open) = pattern.find('{') else {
        out.push(pattern.to_owned());
        return;
    };
    let mut depth = 0;
    let mut parts = Vec::new();
    let mut start = open + 1;
    for (i, c) in pattern[open..].char_indices().map(|(i, c)| (open + i, c)) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    parts.push(&pattern[start..i]);
                    for part in parts {
                        expand(
                            &format!("{}{part}{}", &pattern[..open], &pattern[i + 1..]),
                            out,
                        );
                    }
                    return;
                }
            }
            ',' if depth == 1 => {
                parts.push(&pattern[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    // Unbalanced: as written.
    out.push(pattern.to_owned());
}

/// A pattern vitest excludes anyway (`**/node_modules/**`, `**/dist/**`), or a spread of
/// its defaults (`...configDefaults.exclude`): listing it in vitest's `exclude` skips
/// nothing new. Any other spread (`...flakyTests`) may skip anything.
fn excluded_anyway(item: &str) -> bool {
    let item = item.trim_matches(['\'', '"']);
    matches!(
        item,
        "...configDefaults.exclude" | "...coverageConfigDefaults.exclude" | "...defaultExclude"
    ) || matches!(
        item,
        "**/node_modules/**"
            | "node_modules"
            | "**/node_modules"
            | "**/dist/**"
            | "**/.git/**"
            | "**/cypress/**"
            | "**/.{idea,git,cache,output,temp}/**"
            | "**/{karma,rollup,webpack,vite,vitest,jest,ava,babel,nyc,cypress,tsup,build,eslint,prettier}.config.*"
    )
}

/// Whether a setting's change lets more failures through: its key decides how to read it.
///
/// `elsewhere` is what other projects include: a file excluded here but included there
/// moved, it isn't skipped.
fn loosened(
    path: &str,
    old: Option<&Setting>,
    new: Option<&Setting>,
    elsewhere: &[&str],
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
        "exclude" | "testIgnore" | "testPathIgnorePatterns" | "coveragePathIgnorePatterns"
            if picks_tests(path) =>
        {
            // Compared by what the patterns expand to: `*.{browser,server}.ts` adds only
            // `*.server.ts` to `*.browser.ts`.
            let before = expanded(&items(old_value));
            let elsewhere = expanded(elsewhere);
            let added = items(new_value)
                .into_iter()
                .filter(|item| {
                    expanded(std::slice::from_ref(item))
                        .iter()
                        .any(|one| !before.contains(one) && !elsewhere.contains(one))
                })
                .filter(|item| key != "exclude" || !excluded_anyway(item))
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
        "include" | "testMatch" | "testRegex" | "roots" if !coverage && picks_tests(path) => {
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
        "testDir" if old_value.is_some() && new_value.is_some() && picks_tests(path) => Some((
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

/// A condition that doesn't stop anything running: `always()`, `!cancelled()`,
/// `success()` (the default), `true`.
fn runs_anyway(condition: &str) -> bool {
    let compact: String = bare(condition)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    matches!(
        compact.as_str(),
        "true"
            | "always()"
            | "!cancelled()"
            | "success()"
            | "success()||failure()"
            | "failure()||success()"
    )
}

fn is_true(value: &str) -> bool {
    bare(value) == "true"
}

fn is_false(value: &str) -> bool {
    bare(value) == "false"
}

/// An expression without its `${{ }}`.
fn bare(value: &str) -> &str {
    let value = value.trim();
    value
        .strip_prefix("${{")
        .and_then(|inner| inner.strip_suffix("}}"))
        .unwrap_or(value)
        .trim()
}
