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
