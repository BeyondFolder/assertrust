//! GitHub Actions workflows: the jobs and steps that run tests, and what could let their
//! failures through.

use yaml_rust2::{Yaml, YamlLoader};

use super::{Job, Shell, TestStep, Workflow, command, key_line, line_after};
use crate::ParseError;

pub(super) fn parse(source: &str) -> Result<Workflow, ParseError> {
    let docs = YamlLoader::load_from_str(source).map_err(|err| ParseError::Syntax {
        line: u32::try_from(err.marker().line()).unwrap_or(1),
        message: err.info().to_owned(),
    })?;
    let mut workflow = Workflow::default();
    let Some(jobs) = docs.first().and_then(|doc| doc["jobs"].as_hash()) else {
        return Ok(workflow);
    };
    let jobs_line = key_line(source, "jobs", 1);
    for (id, job) in jobs {
        let Some(id) = id.as_str() else { continue };
        let line = key_line(source, id, jobs_line);
        let mut steps = Vec::new();
        let mut from = line;
        for step in job["steps"].as_vec().into_iter().flatten() {
            let Some(run) = step["run"].as_str() else {
                continue;
            };
            let Some(command) = command::analyze(run, Shell::StopOnError) else {
                continue;
            };
            let run = run.trim().to_owned();
            // The line that runs the tests, or the script's first line.
            let first = run.lines().next().unwrap_or_default();
            let text = if run.contains(&command.runner) {
                command.runner.as_str()
            } else {
                first
            };
            let step_line = line_after(source, text, from);
            from = step_line + 1;
            steps.push(TestStep {
                name: step["name"].as_str().map(str::to_owned),
                run,
                line: step_line,
                condition: scalar(&step["if"]),
                continue_on_error: scalar(&step["continue-on-error"]),
                command,
            });
        }
        workflow.jobs.push(Job {
            id: id.to_owned(),
            line,
            condition: scalar(&job["if"]),
            continue_on_error: scalar(&job["continue-on-error"]),
            steps,
        });
    }
    Ok(workflow)
}

/// A scalar as written: `true`, `false`, `github.event_name == 'push'`.
fn scalar(yaml: &Yaml) -> Option<String> {
    match yaml {
        Yaml::String(s) | Yaml::Real(s) => Some(s.trim().to_owned()),
        Yaml::Boolean(b) => Some(b.to_string()),
        Yaml::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_and_steps_that_run_tests() {
        let source = "
name: CI
on: [push, pull_request]
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: npm run lint
  test:
    runs-on: ubuntu-latest
    continue-on-error: ${{ matrix.experimental }}
    steps:
      - uses: actions/checkout@v4
      - run: npm ci
      - name: Unit
        run: npm test || true
      - name: E2E
        if: github.event_name == 'push'
        run: |
          npx playwright install
          npx playwright test --retries 2
";
        let workflow = parse(source).expect("parses");
        assert!(
            workflow.jobs[0].steps.is_empty(),
            "the lint job runs no tests"
        );
        let job = &workflow.jobs[1];
        assert_eq!((job.id.as_str(), job.line), ("test", 9));
        assert_eq!(
            job.continue_on_error.as_deref(),
            Some("${{ matrix.experimental }}")
        );
        let steps: Vec<(u32, bool, Option<u32>, Option<&str>)> = job
            .steps
            .iter()
            .map(|step| {
                (
                    step.line,
                    step.command.swallows,
                    step.command.retries,
                    step.condition.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            steps,
            [
                (16, true, None, None),
                (21, false, Some(2), Some("github.event_name == 'push'"))
            ]
        );
    }

    #[test]
    fn a_broken_workflow_is_a_syntax_error() {
        assert!(matches!(
            parse("jobs:\n  test: [unclosed\n"),
            Err(ParseError::Syntax { .. })
        ));
    }
}
