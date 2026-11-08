//! `gate-loosened` on small before/after CI and test config files (`docs/architecture.md`,
//! step 5).

use assertrust_ir::parse_gate;
use assertrust_rules::{GateFile, GateSide, check};

/// Findings for one gate file before and after, as `confidence line: message`.
fn findings(path: &str, before: &str, after: &str) -> Vec<String> {
    let before = parse_gate(path, before).expect("before parses");
    let after = parse_gate(path, after).expect("after parses");
    let file = GateFile {
        before: Some(GateSide {
            path,
            gate: &before,
        }),
        after: Some(GateSide { path, gate: &after }),
    };
    check(&[], &[file])
        .iter()
        .map(|finding| {
            assert_eq!(finding.rule.as_str(), "gate-loosened");
            let line = finding.line.map_or("-".to_owned(), |line| line.to_string());
            format!("{} {line}: {}", finding.confidence, finding.message)
        })
        .collect()
}

fn workflow(steps: &str) -> String {
    format!(
        "on: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npm ci\n{steps}"
    )
}

const WORKFLOW: &str = ".github/workflows/ci.yml";

#[test]
fn a_test_step_that_lets_failures_through() {
    let before = workflow("      - run: npm test\n");
    let cases = [
        (
            "      - run: npm test || true\n",
            "high 7: job test, step \"npm test || true\": failures no longer fail it",
        ),
        (
            "      - run: npm test\n        continue-on-error: true\n",
            "high 7: job test, step \"npm test\": continue-on-error lets failures through",
        ),
        (
            "      - run: npm test\n        if: false\n",
            "high 7: job test, step \"npm test\": never runs (if: false)",
        ),
        (
            "      - run: npm test\n        if: github.event_name == 'push'\n",
            "medium 7: job test, step \"npm test\": now runs only if github.event_name == 'push'",
        ),
        (
            "      - run: npm test -- --retry=2 --passWithNoTests\n",
            "high 7: job test, step \"npm test -- --retry=2 --passWithNoTests\": passes when no tests are found; retries raised from 0 to 2",
        ),
        (
            "      - run: npx vitest run --exclude 'test/api/**'\n",
            "high 7: job test, step \"npx vitest run --exclude 'test/api/**'\": now skips test/api/**",
        ),
        (
            "      - run: npx vitest run test/unit\n",
            "medium 7: job test, step \"npx vitest run test/unit\": now runs only test/unit",
        ),
        (
            "      - run: npm run lint\n",
            "high 3: job test: test step \"npm test\" removed",
        ),
    ];
    for (after, expected) in cases {
        assert_eq!(
            findings(WORKFLOW, &before, &workflow(after)),
            [expected],
            "{after}"
        );
    }
}

#[test]
fn a_job_that_lets_failures_through_or_is_gone() {
    let before = "jobs:\n  test:\n    steps:\n      - run: npm test\n";
    assert_eq!(
        findings(
            WORKFLOW,
            before,
            "jobs:\n  test:\n    continue-on-error: true\n    steps:\n      - run: npm test\n"
        ),
        ["high 2: job test: continue-on-error lets failures through"]
    );
    assert_eq!(
        findings(
            WORKFLOW,
            before,
            "jobs:\n  lint:\n    steps:\n      - run: npm run lint\n"
        ),
        ["high -: job test removed: its tests (npm test) no longer run"]
    );
}

#[test]
fn workflow_changes_that_keep_failures_failing() {
    let before = workflow("      - run: npm test\n");
    let cases = [
        // Reformatted, with a step added.
        "      - name: Unit tests\n        run: 'npm test'\n      - run: npx playwright test\n"
            .to_owned(),
        // `|| true` taken away from another step, and `continue-on-error: false`.
        "      - run: npm test\n        continue-on-error: false\n".to_owned(),
    ];
    for after in cases {
        assert_eq!(
            findings(WORKFLOW, &before, &workflow(&after)),
            [""; 0],
            "{after}"
        );
    }
    // The job renamed: the same tests still run.
    assert_eq!(
        findings(
            WORKFLOW,
            "jobs:\n  test:\n    steps:\n      - run: npm test\n",
            "jobs:\n  unit-tests:\n    steps:\n      - run: npm test\n",
        ),
        [""; 0]
    );
}

#[test]
fn test_config_that_lets_failures_through() {
    let before = "export default defineConfig({
  retries: 0,
  forbidOnly: !!process.env.CI,
  testIgnore: ['**/legacy/**'],
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
    { name: 'webkit', use: { browserName: 'webkit' } },
  ],
});";
    let after = "export default defineConfig({
  retries: 2,
  testIgnore: ['**/legacy/**', '**/checkout/**'],
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
  ],
});";
    assert_eq!(
        findings("playwright.config.ts", before, after),
        [
            "high -: projects[webkit] removed: its tests no longer run",
            "high 2: retries: retries raised from 0 to 2",
            "high 3: testIgnore: now skips '**/checkout/**'",
            "medium -: forbidOnly: a committed .only may now pass",
        ]
    );
}

#[test]
fn coverage_and_include_settings() {
    let before = "export default defineConfig({ test: {
  include: ['test/**/*.test.ts'],
  coverage: { exclude: ['src/generated/**'], thresholds: { lines: 80, branches: 70 } },
} });";
    let after = "export default defineConfig({ test: {
  include: ['test/unit/**/*.test.ts'],
  passWithNoTests: true,
  coverage: { exclude: ['src/generated/**', 'src/billing/**'], thresholds: { lines: 60 } },
} });";
    assert_eq!(
        findings("vitest.config.ts", before, after),
        [
            "high -: test.coverage.thresholds.branches: coverage threshold removed",
            "high 2: test.include: no longer includes 'test/**/*.test.ts'",
            "high 3: test.passWithNoTests: passes when no tests are found",
            "high 4: test.coverage.thresholds.lines: coverage threshold lowered from 80 to 60",
            "medium 4: test.coverage.exclude: coverage now ignores 'src/billing/**'",
        ]
    );
    // Stricter: a threshold raised, a skip removed, more files included.
    assert_eq!(
        findings(
            "vitest.config.ts",
            "export default { test: { exclude: ['e2e/**'], coverage: { thresholds: { lines: 60 } } } }",
            "export default { test: { include: ['**/*.test.ts'], coverage: { thresholds: { lines: 80 } } } }",
        ),
        [""; 0]
    );
}

#[test]
fn package_json_test_scripts_and_jest_settings() {
    let before = r#"{
  "scripts": { "test": "vitest run", "test:e2e": "playwright test", "lint": "eslint ." },
  "jest": { "coverageThreshold": { "global": { "lines": 90 } } }
}"#;
    let after = r#"{
  "scripts": { "test": "vitest run --passWithNoTests", "test:e2e": "playwright test || true", "lint": "eslint . || true" },
  "jest": { "coverageThreshold": { "global": { "lines": 50 } } }
}"#;
    assert_eq!(
        findings("package.json", before, after),
        [
            "high 2: script test: passes when no tests are found",
            "high 2: script test:e2e: failures no longer fail it",
            "high 3: jest.coverageThreshold.global.lines: coverage threshold lowered from 90 to 50",
        ]
    );
}

#[test]
fn a_deleted_workflow_or_config() {
    let before = parse_gate(
        WORKFLOW,
        "jobs:\n  test:\n    steps:\n      - run: npm test\n",
    )
    .expect("parses");
    let file = GateFile {
        before: Some(GateSide {
            path: WORKFLOW,
            gate: &before,
        }),
        after: None,
    };
    let messages: Vec<String> = check(&[], &[file])
        .into_iter()
        .map(|finding| finding.message)
        .collect();
    assert_eq!(
        messages,
        ["job test removed: its tests (npm test) no longer run"]
    );
}

#[test]
fn what_the_shell_does_with_a_failing_test_step() {
    let before = workflow("      - run: npm test\n");
    let swallowed = [
        "npm test 2>&1 || true",
        "npm test | tee test.log || true",
        "npm test && echo passed || true",
    ];
    for run in swallowed {
        assert_eq!(
            findings(
                WORKFLOW,
                &before,
                &workflow(&format!("      - run: {run}\n"))
            ),
            [format!(
                "high 7: job test, step \"{run}\": failures no longer fail it"
            )],
            "{run}"
        );
    }
    // Still fails the step: a redirect, a handler that exits non-zero, and `bash -e`
    // stopping at the failing test before `exit 0` or a later `set +e`.
    let still_failing = [
        "      - run: npm test > test.log 2>&1\n",
        "      - run: 'npm test || { cat test.log; exit 1; }'\n",
        "      - run: npm test || (echo failed && exit 1)\n",
        "      - run: npm test || exit 2\n",
        "      - run: npm test; exit 0\n",
        "      - run: |\n          set +e\n          npm test\n",
        "      - run: |\n          npm test\n          set +e\n          ./cleanup.sh\n",
    ];
    for after in still_failing {
        assert_eq!(
            findings(WORKFLOW, &before, &workflow(after)),
            [""; 0],
            "{after}"
        );
    }
    assert_eq!(
        findings(
            WORKFLOW,
            &before,
            &workflow(
                "      - run: |\n          set +e\n          npm test\n          ./cleanup.sh\n"
            )
        ),
        ["high 9: job test, step \"set +e\": failures no longer fail it"]
    );
    assert_eq!(
        findings(
            WORKFLOW,
            &before,
            &workflow("      - run: npm test || npm test\n")
        ),
        ["high 7: job test, step \"npm test || npm test\": retries raised from 0 to 1"]
    );
}

#[test]
fn test_steps_run_through_wrappers_and_workspaces() {
    for run in [
        "xvfb-run -a npx playwright test",
        "pnpm --filter web test",
        "timeout 600 npm test",
        "npx -y vitest run",
        "./node_modules/.bin/vitest run",
    ] {
        assert_eq!(
            findings(
                WORKFLOW,
                &workflow(&format!("      - run: {run}\n")),
                &workflow(&format!("      - run: {run} || true\n")),
            ),
            [format!(
                "high 7: job test, step \"{run} || true\": failures no longer fail it"
            )],
            "{run}"
        );
    }
    for run in ["npm run test-unit", "npm run tests"] {
        assert_eq!(
            findings(
                WORKFLOW,
                &workflow(&format!("      - run: {run}\n")),
                &workflow("      - run: npm run lint\n"),
            ),
            [format!("high 3: job test: test step \"{run}\" removed")],
            "{run}"
        );
    }
}

#[test]
fn several_test_steps_changed_at_once() {
    let before = workflow("      - run: npm test\n      - run: npm run test:e2e\n");
    let after = workflow("      - run: npm test || true\n      - run: npm run test:e2e || true\n");
    assert_eq!(
        findings(WORKFLOW, &before, &after),
        [
            "high 7: job test, step \"npm test || true\": failures no longer fail it",
            "high 8: job test, step \"npm run test:e2e || true\": failures no longer fail it",
        ]
    );
    // Renamed as well.
    assert_eq!(
        findings(
            WORKFLOW,
            "jobs:\n  test:\n    steps:\n      - run: npm test\n",
            "jobs:\n  unit:\n    steps:\n      - run: npm test -- --passWithNoTests\n",
        ),
        [
            "high 4: job unit, step \"npm test -- --passWithNoTests\": passes when no tests are found"
        ]
    );
}

#[test]
fn which_tests_a_step_runs() {
    let step = |run: &str| workflow(&format!("      - run: {run}\n"));
    let cases = [
        // More paths or projects run more tests.
        (
            "npx vitest run test/unit",
            "npx vitest run test/unit test/api",
            None,
        ),
        (
            "npx playwright test --project chromium",
            "npx playwright test --project chromium --project firefox",
            None,
        ),
        (
            "npx vitest run test/unit test/api",
            "npx vitest run test/unit",
            Some("no longer runs test/api"),
        ),
        (
            "npx vitest run test/unit",
            "npx vitest run test/unit -t refunds",
            Some("now runs only -t refunds"),
        ),
    ];
    for (before, after, change) in cases {
        let expected: Vec<String> = change
            .map(|change| format!("medium 7: job test, step \"{after}\": {change}"))
            .into_iter()
            .collect();
        assert_eq!(
            findings(WORKFLOW, &step(before), &step(after)),
            expected,
            "{after}"
        );
    }
}

#[test]
fn conditions_that_still_run_the_step() {
    let before = workflow("      - run: npm test\n");
    for condition in ["always()", "${{ !cancelled() }}", "success() || failure()"] {
        assert_eq!(
            findings(
                WORKFLOW,
                &before,
                &workflow(&format!("      - run: npm test\n        if: {condition}\n"))
            ),
            [""; 0],
            "{condition}"
        );
    }
}

#[test]
fn a_job_is_reported_at_its_own_line() {
    let before = "jobs:\n  unit:\n    steps:\n      - run: npm run test:unit\n  test:\n    steps:\n      - run: npm test\n";
    let after = "jobs:\n  unit:\n    steps:\n      - run: npm run test:unit\n  test:\n    continue-on-error: true\n    steps:\n      - run: npm test\n";
    assert_eq!(
        findings(WORKFLOW, before, after),
        ["high 5: job test: continue-on-error lets failures through"]
    );
}

#[test]
fn settings_that_are_not_about_tests() {
    assert_eq!(
        findings(
            "vitest.config.ts",
            "export default defineConfig({ optimizeDeps: { include: ['vue', 'pinia'] }, test: { include: ['test/**'] } });",
            "export default defineConfig({
  optimizeDeps: { include: ['vue'], exclude: ['fsevents'] },
  server: { watch: { exclude: ['dist/**'] } },
  test: { include: ['test/**'], typecheck: { exclude: ['legacy/**'] } },
});",
        ),
        [""; 0]
    );
}

#[test]
fn a_vitest_workspace_losing_a_project() {
    let before = "export default defineWorkspace([
  'packages/web',
  { test: { name: 'api', include: ['api/**/*.test.ts'] } },
]);";
    assert_eq!(
        findings(
            "vitest.workspace.ts",
            before,
            "export default defineWorkspace([\n  'packages/web',\n]);"
        ),
        ["high -: projects[api] removed: its tests no longer run"]
    );
    assert_eq!(
        findings(
            "vitest.workspace.ts",
            before,
            "export default [\n  { test: { name: 'api', include: ['api/**/*.test.ts'] } },\n];"
        ),
        ["high -: projects[packages/web] removed: its tests no longer run"]
    );
    // vitest 3's `test.projects`, mixing folders and inline projects.
    assert_eq!(
        findings(
            "vitest.config.ts",
            "export default defineConfig({ test: { projects: ['packages/*', { test: { name: 'unit' } }] } });",
            "export default defineConfig({ test: { projects: [{ test: { name: 'unit' } }] } });",
        ),
        ["high -: test.projects[packages/*] removed: its tests no longer run"]
    );
}

#[test]
fn a_test_script_emptied_or_removed() {
    let before = r#"{ "scripts": { "test": "vitest run", "test:e2e": "playwright test" } }"#;
    assert_eq!(
        findings(
            "package.json",
            before,
            r#"{ "scripts": { "test": "echo skip" } }"#
        ),
        [
            "high 1: script test: no longer runs vitest",
            "medium -: script test:e2e removed: its tests (playwright test) no longer run from it",
        ]
    );
    // Another command may still run the tests, in a way assertrust doesn't follow.
    assert_eq!(
        findings(
            "package.json",
            before,
            r#"{ "scripts": { "test": "turbo test", "test:e2e": "playwright test" } }"#
        ),
        ["medium 1: script test: no longer runs vitest"]
    );
    // Renamed.
    assert_eq!(
        findings(
            "package.json",
            before,
            r#"{ "scripts": { "test": "vitest run", "e2e": "playwright test --retries 2" } }"#
        ),
        ["high 1: script e2e: retries raised from 0 to 2"]
    );
}
