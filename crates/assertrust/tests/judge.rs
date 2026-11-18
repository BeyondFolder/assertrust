//! The AI judge (`docs/architecture.md`, step 7), against a fake model on localhost: what it's
//! sent, what its verdicts change, its cache, and that failing never fails the check.

mod common;

use common::{FakeModel, TestRepo};

const TEST: &str = "\
import { refund } from '../src/refund'

describe('refunds', () => {
  it('marks a refund done', () => {
    const r = refund(1)
    expect(r.status).toBe('done')
    expect(r.amount).toBe(1)
  })

  it('keeps the currency', () => {
    expect(refund(1).currency).toBe('EUR')
  })
})
";

/// A repo whose feature branch removes a check (`check-removed`, shown) and changes an
/// expected value (`outcome-changed`, left to the judge). Returns it and the range to check.
fn trimmed() -> (TestRepo, String) {
    let mut repo = TestRepo::new();
    repo.write(
        "src/refund.ts",
        "export const refund = (amount) => ({ amount, status: 'done', currency: 'EUR' })\n",
    );
    repo.write("test/refund.test.ts", TEST);
    let base = repo.commit("Add refunds");
    repo.branch("feature");
    repo.write(
        "test/refund.test.ts",
        &TEST
            .replace("    expect(r.status).toBe('done')\n", "")
            .replace("toBe('EUR')", "toBe('USD')"),
    );
    repo.commit("Trim refunds\n\nThe status check was flaky.");
    (repo, format!("{base}...HEAD"))
}

/// The status check lost coverage; the currency changed without a reason.
fn model() -> FakeModel {
    FakeModel::new(
        &[
            (
                "Where: test/refund.test.ts:4",
                "coverage-lost",
                "refunds still get a status, and nothing checks it",
            ),
            (
                "Where: test/refund.test.ts:10",
                "suspicious",
                "the currency changed with no code change",
            ),
        ],
        ("unsure", "can't tell"),
    )
}

#[test]
fn verdicts_go_under_findings_and_fail_the_check() {
    let (repo, range) = trimmed();
    let model = model();
    let run = repo.assertrust_env(&["check", "--range", &range, "--judge"], &model.env());
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    insta::assert_snapshot!(run.stdout, @r"
    test/refund.test.ts:4  HIGH  check-removed
      removed expect(r.status).toBe('done') · 75f0ded
      judge: coverage lost: refunds still get a status, and nothing checks it
    test/refund.test.ts:10  LOW  outcome-changed
      toBe('EUR') → toBe('USD') · 75f0ded
      judge: suspicious: the currency changed with no code change

    2 findings · 2 judged coverage lost or suspicious (exit 1)
    judge: anthropic/claude-sonnet-5-5 (effort medium) · 2 judged · 2.0K tokens in, 100 out · $0.01
    ");
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let removed = requests
        .iter()
        .find(|body| body.contains("Where: test/refund.test.ts:4"))
        .unwrap();
    // The test before and after, the commit message, and the answer's schema.
    assert!(
        removed.contains("expect(r.status).toBe('done')"),
        "{removed}"
    );
    assert!(removed.contains("The status check was flaky."), "{removed}");
    assert!(removed.contains("\"json_schema\""), "{removed}");
    assert!(removed.contains("claude-sonnet-5-5"), "{removed}");
}

#[test]
fn a_finding_judged_fine_is_folded_away() {
    let (repo, range) = trimmed();
    let model = FakeModel::new(&[], ("fine", "the status is checked by the next test"));
    let run = repo.assertrust_env(&["check", "--range", &range, "--judge"], &model.env());
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    insta::assert_snapshot!(run.stdout, @r"
    judged fine (1):
      test/refund.test.ts:4  check-removed: the status is checked by the next test

    no findings · 1 judged fine · 1 changed outcome hidden (-v shows them) · none judged coverage lost or suspicious (exit 0)
    judge: anthropic/claude-sonnet-5-5 (effort medium) · 2 judged · 2.0K tokens in, 100 out · $0.01
    ");
}

#[test]
fn a_finding_judged_fine_that_fails_fail_on_stays_in_view() {
    let (repo, range) = trimmed();
    let model = FakeModel::new(&[], ("fine", "the status is checked by the next test"));
    let args = ["check", "--range", &range, "--judge", "--fail-on", "high"];
    let run = repo.assertrust_env(&args, &model.env());
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    // It fails the check, so it isn't folded away: it says why the check failed.
    assert!(
        run.stdout.starts_with(
            "test/refund.test.ts:4  HIGH  check-removed\n  removed expect(r.status).toBe('done') · 75f0ded\n  judge: fine: the status is checked by the next test\n"
        ),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains("judged fine ("), "{}", run.stdout);
    assert!(
        run.stdout.contains("1 finding · 1 changed outcome hidden (-v shows them) · 1 finding at high or above (exit 1)"),
        "{}",
        run.stdout
    );
    let github = repo.assertrust_env(&[&args[..], &["--format", "github"]].concat(), &model.env());
    assert!(
        github
            .stdout
            .contains("::error file=test/refund.test.ts,line=4"),
        "{}",
        github.stdout
    );
}

#[test]
fn verbose_counts_hidden_findings_judged_fine_as_judged_fine() {
    let (repo, range) = trimmed();
    let model = FakeModel::new(&[], ("fine", "nothing got weaker"));
    let run = repo.assertrust_env(&["check", "--range", &range, "--judge", "-v"], &model.env());
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("judged fine (2):"), "{}", run.stdout);
    assert!(
        run.stdout.contains(
            "\nno findings · 2 judged fine · none judged coverage lost or suspicious (exit 0)\n"
        ),
        "{}",
        run.stdout
    );
}

#[test]
fn the_same_change_is_answered_from_the_cache() {
    let (repo, range) = trimmed();
    let model = model();
    let args = ["check", "--range", &range, "--judge"];
    let first = repo.assertrust_env(&args, &model.env());
    assert_eq!(first.code, 1);
    let second = repo.assertrust_env(&args, &model.env());
    assert_eq!(second.code, 1);
    assert_eq!(model.requests().len(), 2, "nothing more was asked");
    assert!(
        second
            .stdout
            .contains("judge: coverage lost: refunds still get a status"),
        "{}",
        second.stdout
    );
    assert!(
        second.stdout.ends_with(
            "judge: anthropic/claude-sonnet-5-5 (effort medium) · 2 judged, 2 from the cache\n"
        ),
        "{}",
        second.stdout
    );
    // Even without a key: nothing needs asking.
    let offline = repo.assertrust(&args);
    assert_eq!(offline.code, 1, "{}", offline.stdout);
    // The cache stays out of git.
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
}

#[test]
fn without_a_key_findings_have_no_verdicts() {
    let (repo, range) = trimmed();
    let run = repo.assertrust(&["check", "--range", &range, "--judge"]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    insta::assert_snapshot!(run.stdout, @r"
    test/refund.test.ts:4  HIGH  check-removed
      removed expect(r.status).toBe('done') · 75f0ded

    note: the judge didn't run (ANTHROPIC_API_KEY isn't set), so 2 findings have no verdict
    1 finding · 1 changed outcome hidden (-v shows them) · none judged coverage lost or suspicious (exit 0)
    judge: anthropic/claude-sonnet-5-5 (effort medium) · 0 judged · 2 without a verdict
    ");
}

#[test]
fn a_model_that_fails_never_fails_the_check() {
    let (repo, range) = trimmed();
    let model = FakeModel::failing(400);
    let run = repo.assertrust_env(&["check", "--range", &range, "--judge"], &model.env());
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout
            .contains("note: the judge gave no verdict on 2 findings ("),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains("judge: coverage"), "{}", run.stdout);
}

#[test]
fn a_dry_run_prints_what_would_be_sent() {
    let (repo, range) = trimmed();
    let run = repo.assertrust(&["check", "--range", &range, "--judge-dry-run"]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.starts_with(
            "== test/refund.test.ts:4  check-removed\nPOST https://api.anthropic.com/v1/messages\n"
        ),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("# Finding"), "{}", run.stdout);
    assert!(
        run.stdout
            .contains("== test/refund.test.ts:10  outcome-changed"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .ends_with("2 requests would be sent to anthropic/claude-sonnet-5-5 (effort medium)\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn the_config_sets_the_cap_and_what_fails() {
    let (mut repo, _) = trimmed();
    repo.switch("main");
    repo.write(
        "assertrust.toml",
        "[judge]\nmodel = \"claude-haiku-5-5\"\nmax-findings = 1\nfail-on = \"never\"\n",
    );
    let base = repo.commit("Turn the judge on");
    repo.git(&["rebase", "--quiet", "main", "feature"]);
    let model = model();
    // `[judge]` turns it on, without --judge.
    let run = repo.assertrust_env(
        &["check", "--range", &format!("{base}...feature")],
        &model.env(),
    );
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(model.requests().len(), 1, "the shown finding first");
    assert!(model.requests()[0].contains("claude-haiku-5-5"));
    assert!(
        run.stdout
            .contains("judge: coverage lost: refunds still get a status"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.ends_with(
            "1 finding · 1 changed outcome hidden (-v shows them) · warn-only (exit 0)\njudge: anthropic/claude-haiku-5-5 · 1 judged · 1 not judged (max-findings) · 1.0K tokens in, 50 out · <$0.01\n"
        ),
        "{}",
        run.stdout
    );
}

#[test]
fn an_effort_is_sent_and_kept_apart_in_the_cache() {
    let (repo, range) = trimmed();
    let model = model();
    // The default model thinks at the effort it was measured at.
    let args = ["check", "--range", &range, "--judge"];
    assert_eq!(repo.assertrust_env(&args, &model.env()).code, 1);
    assert!(model.requests()[0].contains("\"effort\":\"medium\""));
    let high = [&args[..], &["--judge-effort", "high"]].concat();
    let run = repo.assertrust_env(&high, &model.env());
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    // Asked again, not answered with the verdicts of another effort.
    assert_eq!(model.requests().len(), 4);
    assert!(model.requests()[3].contains("\"effort\":\"high\""));
    assert!(
        run.stdout
            .contains("judge: anthropic/claude-sonnet-5-5 (effort high) · 2 judged ·"),
        "{}",
        run.stdout
    );
    // Another model thinks at its own default.
    let haiku = [&args[..], &["--judge-model", "claude-haiku-5-5"]].concat();
    assert_eq!(repo.assertrust_env(&haiku, &model.env()).code, 1);
    assert_eq!(model.requests().len(), 6);
    assert!(!model.requests()[5].contains("\"effort\""));
}

#[test]
fn json_and_github_carry_the_verdicts() {
    let (repo, range) = trimmed();
    let model = model();
    let run = repo.assertrust_env(
        &["check", "--range", &range, "--judge", "--format", "json"],
        &model.env(),
    );
    assert_eq!(run.code, 1);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["exit"], 1);
    assert_eq!(json["findings"][0]["judge"]["verdict"], "coverage-lost");
    assert_eq!(json["findings"][1]["status"], "shown");
    assert_eq!(json["findings"][1]["judge"]["verdict"], "suspicious");
    assert_eq!(json["judge"]["model"], "anthropic/claude-sonnet-5-5");
    assert_eq!(json["judge"]["judged"], 2);
    assert_eq!(json["judge"]["input_tokens"], 2000);

    let summary = repo.path().join("summary.md");
    std::fs::write(&summary, "").unwrap();
    let mut env = model.env().to_vec();
    let summary_path = summary.to_str().unwrap().to_owned();
    env.push(("GITHUB_STEP_SUMMARY", &summary_path));
    let run = repo.assertrust_env(
        &["check", "--range", &range, "--judge", "--format", "github"],
        &env,
    );
    assert_eq!(run.code, 1);
    assert!(
        run.stdout.contains(
            "::error file=test/refund.test.ts,line=4,title=assertrust%3A check-removed (high)::removed expect(r.status).toBe('done') (75f0ded) · judge: coverage lost: refunds still get a status, and nothing checks it"
        ),
        "{}",
        run.stdout
    );
    let summary = std::fs::read_to_string(&summary).unwrap();
    assert!(
        summary.contains("<br>**judge: suspicious**: the currency changed with no code change"),
        "{summary}"
    );
    assert!(
        summary.contains("<sub>judge: anthropic/claude-sonnet-5-5"),
        "{summary}"
    );
}

#[test]
fn the_pull_request_comes_from_the_github_event() {
    let (repo, range) = trimmed();
    let head = repo.git(&["rev-parse", "HEAD"]);
    let event = repo.path().join("event.json");
    std::fs::write(
        &event,
        serde_json::json!({
            "pull_request": {
                "title": "Trim the refund response",
                "body": "Clients only read the amount now.",
                "head": { "sha": head },
            }
        })
        .to_string(),
    )
    .unwrap();
    let model = model();
    let mut env = model.env().to_vec();
    let event_path = event.to_str().unwrap().to_owned();
    env.push(("GITHUB_EVENT_NAME", "pull_request"));
    env.push(("GITHUB_EVENT_PATH", &event_path));
    let run = repo.assertrust_env(&["check", "--range", &range, "--judge"], &env);
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    let body = &model.requests()[0];
    assert!(body.contains("Trim the refund response"), "{body}");
    assert!(body.contains("Clients only read the amount now."), "{body}");
}

#[test]
fn a_backtest_matches_verdicts_with_labels() {
    let (mut repo, _) = trimmed();
    repo.switch("main");
    repo.merge(
        "feature",
        "Merge pull request #7 from org/feature\n\nTrim refunds",
    );
    let labels = repo.path().join("labels.toml");
    std::fs::write(&labels, "").unwrap();
    let labels_path = labels.to_str().unwrap();
    // Write the entries, then label them: the removed check unexplained, the currency wrong.
    repo.assertrust(&[
        "backtest",
        "--last",
        "1",
        "--labels",
        labels_path,
        "--update-labels",
    ]);
    let text = std::fs::read_to_string(&labels).unwrap();
    let mut labelled = text.replacen("label = \"\"", "label = \"unexplained\"", 1);
    labelled = labelled.replacen("label = \"\"", "label = \"wrong\"", 1);
    std::fs::write(&labels, labelled).unwrap();
    let model = model();
    let run = repo.assertrust_env(
        &[
            "backtest",
            "--last",
            "1",
            "--labels",
            labels_path,
            "--judge",
        ],
        &model.env(),
    );
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains(
            "judge on shown findings:\n  unexplained (1): 1 coverage lost · 1 would fail (100%)\n"
        ),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(
            "judge on changed outcomes:\n  wrong (1): 1 suspicious · 1 would fail (100%)\n"
        ),
        "{}",
        run.stdout
    );
    // The PR's title from the merge commit.
    assert!(
        model
            .requests()
            .iter()
            .all(|body| body.contains("Pull request: Trim refunds"))
    );
}

#[test]
fn a_changed_outcome_on_code_the_pr_added_is_not_judged() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('adds', () => { expect(add(1, 1)).toBe(2) })\n",
    );
    let base = repo.commit("Add a test");
    repo.branch("feature");
    repo.write(
        "test/a.test.ts",
        "it('adds', () => { expect(add(1, 1)).toBe(2) })\nit('doubles', () => { expect(double(2)).toBe(4) })\n",
    );
    repo.commit("Test doubling");
    repo.write(
        "test/a.test.ts",
        "it('adds', () => { expect(add(1, 1)).toBe(2) })\nit('doubles', () => { expect(double(2)).toBe(5) })\n",
    );
    repo.commit("Fix the expected value");
    let model = FakeModel::new(&[], ("suspicious", "changed to match"));
    let range = format!("{base}...HEAD");
    let run = repo.assertrust_env(&["check", "--range", &range, "--judge", "-v"], &model.env());
    // The main branch lost nothing, so it can't fail the check, and isn't worth asking about.
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(model.requests().is_empty(), "{}", run.stdout);
    assert!(run.stdout.contains("outcome-changed"), "{}", run.stdout);
}

#[test]
fn a_dry_run_never_shows_a_key() {
    let (repo, range) = trimmed();
    // Gemini takes its key in the address, which the dry run prints.
    let run = repo.assertrust_env(
        &[
            "check",
            "--range",
            &range,
            "--judge-dry-run",
            "--judge-model",
            "gemini/gemini-3.8-flash",
        ],
        &[("GEMINI_API_KEY", "secret-gemini-key")],
    );
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout
            .contains("POST https://generativelanguage.googleapis.com/v1beta/models/gemini-3.8-flash:generateContent\n"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("responseJsonSchema"), "{}", run.stdout);
    assert!(!run.stdout.contains("secret-gemini-key"), "{}", run.stdout);
}

#[test]
fn verdicts_committed_in_a_change_are_never_read() {
    let (mut repo, range) = trimmed();
    let model = model();
    let args = ["check", "--range", &range, "--judge"];
    assert_eq!(repo.assertrust_env(&args, &model.env()).code, 1);
    // The verdicts live in the git directory, out of reach of a commit.
    let cache = repo.path().join(".git/assertrust/judge");
    let kept: Vec<_> = std::fs::read_dir(&cache).unwrap().flatten().collect();
    assert_eq!(kept.len(), 2);
    // A change that commits "fine" verdicts under every key the judge would look up, where
    // a cache in the working tree would be, and wipes the real one.
    for entry in &kept {
        repo.write(
            &format!(
                ".assertrust/cache/judge/{}",
                entry.file_name().to_string_lossy()
            ),
            "{\"verdict\": \"fine\", \"reason\": \"trust me\"}\n",
        );
    }
    repo.git(&["add", "--force", ".assertrust"]);
    repo.commit("Add verdicts");
    std::fs::remove_dir_all(&cache).unwrap();
    let run = repo.assertrust_env(&args, &model.env());
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(!run.stdout.contains("trust me"), "{}", run.stdout);
    assert_eq!(model.requests().len(), 4, "asked again");
}

#[test]
fn a_backtest_shows_a_judged_outcome_on_pr_added_code_with_show_pr_added() {
    let mut repo = TestRepo::new();
    repo.write(
        "test/a.test.ts",
        "it('adds', () => { expect(add(1, 1)).toBe(2) })\n",
    );
    repo.commit("Add a test");
    repo.branch("feature");
    let doubles = "it('adds', () => { expect(add(1, 1)).toBe(2) })\nit('doubles', () => { expect(double(2)).toBe(4) })\n";
    repo.write("test/a.test.ts", doubles);
    repo.commit("Test doubling");
    repo.write("test/a.test.ts", &doubles.replace("toBe(4)", "toBe(5)"));
    repo.commit("Fix the expected value");
    repo.switch("main");
    repo.merge(
        "feature",
        "Merge pull request #3 from org/feature\n\nDoubling",
    );
    let model = FakeModel::new(&[], ("suspicious", "changed to match"));
    let args = ["backtest", "--last", "1", "--judge"];
    let run = repo.assertrust_env(&args, &model.env());
    assert!(!run.stdout.contains("outcome-changed"), "{}", run.stdout);
    let run = repo.assertrust_env(&[&args[..], &["--show-pr-added"]].concat(), &model.env());
    assert!(
        run.stdout.contains("outcome-changed  (added by this PR)"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("judge: suspicious: changed to match"),
        "{}",
        run.stdout
    );
}
