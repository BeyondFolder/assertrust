//! `assertrust backtest` (`docs/architecture.md`, step 6, "Backtest"): replaying a branch's
//! history as CI would have checked it (merged PRs PR-style, squash-merged PRs against
//! their own commits when fetched, pushes on their own), with the config each had, and
//! the labels file that says how often assertrust is right.

mod common;

use std::fs;

use common::{Run, TestRepo};

const TESTS: &str = "\
it('refunds', () => {
  expect(a).toBe(1)
  expect(b).toBe(2)
})
it('charges', () => {
  expect(c).toBe(3)
  expect(d).toBe(4)
})
";

/// Runs `assertrust backtest`, with the time it took replaced by `Xs`.
fn backtest(repo: &TestRepo, args: &[&str]) -> Run {
    let mut all = vec!["backtest"];
    all.extend_from_slice(args);
    let mut run = repo.assertrust(&all);
    assert!(run.code == 0 || run.code == 1, "{}", run.stderr);
    run.stdout = run
        .stdout
        .lines()
        .map(|line| match line.rsplit_once(" · ") {
            Some((rest, time))
                if time
                    .strip_suffix('s')
                    .is_some_and(|n| n.parse::<f64>().is_ok()) =>
            {
                format!("{rest} · Xs\n")
            }
            _ => format!("{line}\n"),
        })
        .collect();
    run
}

/// Writes `TESTS` with one line replaced.
fn tests_with(from: &str, to: &str) -> String {
    assert!(TESTS.contains(from));
    TESTS.replace(from, to)
}

/// A history with one landing of each kind, each weakening a check:
/// - PR #1, merged with a merge commit: drops `d` and puts it back, then drops `a`;
/// - PR #2, squash-merged, its commits fetched: drops `c`, then edits the README;
/// - PR #3, squash-merged, its commits not fetched: skips `charges`;
/// - a push straight to `main`: loosens `b`.
///
/// Returns the repo and the SHAs worth naming: PR #1's last commit and merge, PR #2's first
/// commit and squash, PR #3's squash, the push.
fn history() -> (TestRepo, [String; 6]) {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TESTS);
    repo.commit("Add tests");

    repo.branch("feature");
    repo.write("test/a.test.ts", &tests_with("  expect(d).toBe(4)\n", ""));
    repo.commit("Drop d");
    repo.write("test/a.test.ts", TESTS);
    repo.commit("Put d back");
    repo.write("test/a.test.ts", &tests_with("  expect(a).toBe(1)\n", ""));
    let drop_a = repo.commit("Drop a");
    repo.switch("main");
    let merge = repo.merge(
        "feature",
        "Merge pull request #1 from org/feature\n\nTrim refunds",
    );

    repo.branch("pr2");
    let current = fs::read_to_string(repo.path().join("test/a.test.ts")).unwrap();
    repo.write(
        "test/a.test.ts",
        &current.replace("  expect(c).toBe(3)\n", ""),
    );
    let drop_c = repo.commit("Drop c");
    repo.write("README.md", "Docs\n");
    repo.commit("Docs");
    repo.switch("main");
    repo.git(&["merge", "--squash", "--quiet", "pr2"]);
    let squash = repo.commit("Trim charges (#2)");
    repo.git(&["update-ref", "refs/pull/2/head", "pr2"]);

    let current = fs::read_to_string(repo.path().join("test/a.test.ts")).unwrap();
    repo.write(
        "test/a.test.ts",
        &current.replace("it('charges'", "it.skip('charges'"),
    );
    let unfetched = repo.commit("Skip charges (#3)");

    let current = fs::read_to_string(repo.path().join("test/a.test.ts")).unwrap();
    repo.write(
        "test/a.test.ts",
        &current.replace("expect(b).toBe(2)", "expect(b).toBeTruthy()"),
    );
    let push = repo.commit("Loosen b");
    (repo, [drop_a, merge, drop_c, squash, unfetched, push])
}

fn short(sha: &str) -> &str {
    &sha[..7]
}

#[test]
fn replays_each_pr_and_push_as_ci_would_have() {
    let (repo, [drop_a, merge, drop_c, squash, unfetched, push]) = history();
    let run = backtest(&repo, &[]);
    assert_eq!(run.code, 0);
    assert_eq!(
        run.stdout,
        format!(
            "PR #1  {}  Trim refunds · 3 commits\n\
             test/a.test.ts:1  HIGH  check-removed\n\
             \x20 removed expect(a).toBe(1) · {}\n\
             \n\
             PR #2  {}  Trim charges (#2) · squashed, 2 commits\n\
             test/a.test.ts:4  HIGH  check-removed\n\
             \x20 removed expect(c).toBe(3) · {}\n\
             \n\
             PR #3  {}  Skip charges (#3) · squashed, checked as one commit\n\
             test/a.test.ts:4  HIGH  test-disabled\n\
             \x20 now skipped (skip) · {}\n\
             \n\
             push  {}  Loosen b\n\
             test/a.test.ts:2  HIGH  matcher-loosened\n\
             \x20 toBe(2) → toBeTruthy(): now any truthy value passes · {}\n\
             \n\
             note: 1 squash-merged PR checked as one commit, since their own commits aren't \
             fetched (git fetch origin refs/pull/N/head:refs/pull/N/head)\n\
             4 commits: 3 PRs, 1 push · 4 findings · 1 undone later hidden \
             (--show-transient shows them) · Xs\n",
            short(&merge),
            short(&drop_a),
            short(&squash),
            short(&drop_c),
            short(&unfetched),
            short(&unfetched),
            short(&push),
            short(&push),
        )
    );
}

#[test]
fn a_range_picks_the_commits_and_must_have_two_dots() {
    let (repo, [_, _, _, squash, _, push]) = history();
    let run = backtest(&repo, &["--range", &format!("{squash}..HEAD")]);
    assert!(
        run.stdout
            .contains(&format!("push  {}  Loosen b", short(&push)))
    );
    assert!(
        run.stdout
            .ends_with("2 commits: 1 PR, 1 push · 2 findings · Xs\n")
    );
    let run = repo.assertrust(&["backtest", "--range", "main...HEAD"]);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stderr,
        "assertrust: expected BASE..HEAD, got `main...HEAD`\n"
    );
    let run = backtest(&repo, &["--last", "1"]);
    assert!(
        run.stdout
            .ends_with("1 commit: 0 PRs, 1 push · 1 finding · Xs\n")
    );
}

#[test]
fn each_commit_is_checked_with_the_config_it_had() {
    // check-removed was off until the push that turned it on; the commit after it drops a
    // check, and is the only one reported.
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TESTS);
    repo.write("assertrust.toml", "[rules]\ncheck-removed = \"off\"\n");
    repo.commit("Add tests");
    repo.write("test/a.test.ts", &tests_with("  expect(a).toBe(1)\n", ""));
    repo.commit("Drop a");
    repo.remove("assertrust.toml");
    repo.commit("Turn every rule on");
    let current = fs::read_to_string(repo.path().join("test/a.test.ts")).unwrap();
    repo.write(
        "test/a.test.ts",
        &current.replace("  expect(c).toBe(3)\n", ""),
    );
    let drop_c = repo.commit("Drop c");
    let run = backtest(&repo, &[]);
    assert_eq!(
        run.stdout,
        format!(
            "push  {}  Drop c\n\
             test/a.test.ts:4  HIGH  check-removed\n\
             \x20 removed expect(c).toBe(3) · {}\n\
             \n\
             3 commits: 0 PRs, 3 pushes · 1 finding · Xs\n",
            short(&drop_c),
            short(&drop_c),
        )
    );
}

#[test]
fn a_pr_that_cannot_be_checked_is_reported() {
    // PR #4's fetched commits share no history with the branch.
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TESTS);
    repo.commit("Add tests");
    repo.git(&["switch", "--quiet", "--orphan", "elsewhere"]);
    repo.write("other.txt", "x\n");
    repo.commit("Unrelated");
    repo.git(&["update-ref", "refs/pull/4/head", "elsewhere"]);
    repo.switch("main");
    repo.write("README.md", "Docs\n");
    let squash = repo.commit("Docs (#4)");
    let run = backtest(&repo, &[]);
    assert_eq!(run.code, 0);
    assert!(
        run.stdout.starts_with(&format!(
            "PR #4  {}  Docs (#4)\ncouldn't check: ",
            short(&squash)
        )),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .ends_with("1 commit: 1 PR, 0 pushes · no findings · 1 couldn't be checked · Xs\n")
    );
}

#[test]
fn labels_say_how_often_it_is_right_and_what_is_new_or_gone() {
    let (repo, [_, merge, _, squash, unfetched, push]) = history();
    let labels = repo.path().join("labels.toml");

    // Written from scratch: every finding, not labelled yet, and the range it replayed.
    let run = backtest(
        &repo,
        &["--labels", labels.to_str().unwrap(), "--update-labels"],
    );
    assert_eq!(run.code, 1);
    assert!(run.stdout.ends_with(
        "labels: none of the findings are labelled yet\n4 not labelled yet (marked above)\n"
    ));
    assert_eq!(run.stderr, format!("wrote {}: 4 new\n", labels.display()));
    let root = repo.git(&["rev-list", "--max-parents=0", "HEAD"]);
    let written = fs::read_to_string(&labels).unwrap();
    let entry =
        |sha: &str, pr: &str, rule: &str, test: &str, before: &str, after: &str, message: &str| {
            format!(
                "\n[[finding]]\nunit = \"{}\"\n{pr}rule = \"{rule}\"\npath = \"test/a.test.ts\"\n\
             test = \"{test}\"\nbefore = {before}\nafter = {after}\nmessage = \"{message}\"\n\
             label = \"\"\nnote = \"\"\n",
                &sha[..12]
            )
        };
    assert_eq!(
        written,
        format!(
            "# Labels for `assertrust backtest --labels`: a verdict on each finding.\n\
             #   wrong        nothing got weaker: assertrust misread the change\n\
             #   intended     weaker on purpose: the code the test covered went too, or a commit says why\n\
             #   unexplained  weaker, and nothing in the change says why\n\
             # \"\" is a finding not labelled yet. `--update-labels` rewrites this file: it adds new\n\
             # findings and drops false alarms no longer found. Keep remarks in `note`.\n\
             \n\
             range = \"{root}..{}\"\n{}{}{}{}",
            repo.git(&["rev-parse", "HEAD"]),
            entry(
                &merge,
                "pr = 1\n",
                "check-removed",
                "refunds",
                "[\"expect(a).toBe(1)\"]",
                "[]",
                "removed expect(a).toBe(1)"
            ),
            entry(
                &squash,
                "pr = 2\n",
                "check-removed",
                "charges",
                "[\"expect(c).toBe(3)\"]",
                "[]",
                "removed expect(c).toBe(3)"
            ),
            entry(
                &unfetched,
                "pr = 3\n",
                "test-disabled",
                "charges",
                "[\"charges []\"]",
                "[\"charges [skip]\"]",
                "now skipped (skip)"
            ),
            entry(
                &push,
                "",
                "matcher-loosened",
                "refunds",
                "[\"expect(b).toBe(2)\"]",
                "[\"expect(b).toBeTruthy()\"]",
                "toBe(2) → toBeTruthy(): now any truthy value passes"
            ),
        )
    );

    // Labelled by hand: three right, one wrong. Then two gone findings: a right one (missed)
    // and a false alarm (fixed). The skip's entry recorded its code differently, so its label
    // is carried over.
    let gone = |label: &str, message: &str| {
        format!(
            "\n[[finding]]\nunit = \"{}\"\nrule = \"check-removed\"\npath = \"test/a.test.ts\"\n\
             test = \"gone\"\nbefore = []\nafter = []\nmessage = \"{message}\"\nlabel = \"{label}\"\n\
             note = \"\"\n",
            &push[..12]
        )
    };
    let mut labelled = written
        .replacen("label = \"\"", "label = \"unexplained\"", 2)
        .replacen("label = \"\"", "label = \"intended\"", 1)
        .replacen("label = \"\"", "label = \"wrong\"", 1)
        .replace(
            "after = [\"charges [skip]\"]",
            "after = [\"charges [it.skip]\"]",
        );
    labelled.push_str(&gone("unexplained", "removed a check that's still found"));
    labelled.push_str(&gone("wrong", "a false alarm that's gone"));
    fs::write(&labels, &labelled).unwrap();
    let run = backtest(&repo, &["--labels", labels.to_str().unwrap()]);
    assert_eq!(run.code, 1, "a right finding is gone");
    assert!(run.stdout.contains(&format!(
        "PR #3  {}  Skip charges (#3) · squashed, checked as one commit\n\
         test/a.test.ts:4  HIGH  test-disabled  [intended, carried over]\n",
        short(&unfetched)
    )));
    assert!(
        run.stdout
            .contains("test/a.test.ts:2  HIGH  matcher-loosened  [wrong]\n"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.ends_with(&format!(
        "labels: 2 unexplained, 1 intended, 1 wrong · precision 75% (3 of 4 right) · high 3 of 4\n\
         by rule: check-removed 2 of 2, matcher-loosened 0 of 1, test-disabled 1 of 1\n\
         no longer found, though labelled right:\n\
         \x20 {}  test/a.test.ts  check-removed  removed a check that's still found  [unexplained]\n\
         false alarms no longer found:\n\
         \x20 {}  test/a.test.ts  check-removed  a false alarm that's gone  [wrong]\n",
        &push[..12],
        &push[..12],
    )));

    // Updated: labels kept (the carried one with its new code), the gone false alarm
    // dropped, the gone right finding kept until someone looks at it.
    let run = backtest(
        &repo,
        &["--labels", labels.to_str().unwrap(), "--update-labels"],
    );
    assert_eq!(run.code, 1);
    assert_eq!(
        run.stderr,
        format!("wrote {}: 0 new, 1 dropped\n", labels.display())
    );
    let updated = fs::read_to_string(&labels).unwrap();
    assert_eq!(
        updated,
        labelled
            .replace(
                "after = [\"charges [it.skip]\"]",
                "after = [\"charges [skip]\"]"
            )
            .replace(&gone("wrong", "a false alarm that's gone"), "")
    );

    // Once the gone one is looked at and removed, everything's labelled: exit 0.
    fs::write(
        &labels,
        updated.replace(
            &gone("unexplained", "removed a check that's still found"),
            "",
        ),
    )
    .unwrap();
    let run = backtest(&repo, &["--labels", labels.to_str().unwrap()]);
    assert_eq!(run.code, 0);
    assert!(run.stdout.ends_with(
        "labels: 2 unexplained, 1 intended, 1 wrong · precision 75% (3 of 4 right) · high 3 of 4\n\
         by rule: check-removed 2 of 2, matcher-loosened 0 of 1, test-disabled 1 of 1\n"
    ));
}

#[test]
fn the_labels_file_says_which_commits_to_replay() {
    let (repo, [_, _, _, squash, _, push]) = history();
    let labels = repo.path().join("labels.toml");
    fs::write(&labels, format!("range = \"{squash}..{push}\"\n")).unwrap();
    let run = backtest(&repo, &["--labels", labels.to_str().unwrap()]);
    assert!(
        run.stdout
            .contains("2 commits: 1 PR, 1 push · 2 findings · Xs\n")
    );
    // A flag wins over the file.
    let run = backtest(
        &repo,
        &["--labels", labels.to_str().unwrap(), "--last", "1"],
    );
    assert!(
        run.stdout
            .contains("1 commit: 0 PRs, 1 push · 1 finding · Xs\n")
    );
    let run = repo.assertrust(&["backtest", "--labels", "missing.toml"]);
    assert_eq!(run.code, 2);
    assert!(
        run.stderr
            .starts_with("assertrust: couldn't read missing.toml: ")
    );
}

#[test]
fn json_lists_every_commit_with_its_findings_and_labels() {
    let (repo, [_, _, _, _, unfetched, _]) = history();
    let labels = repo.path().join("labels.toml");
    backtest(
        &repo,
        &["--labels", labels.to_str().unwrap(), "--update-labels"],
    );
    let text =
        fs::read_to_string(&labels)
            .unwrap()
            .replacen("label = \"\"", "label = \"unexplained\"", 1);
    fs::write(&labels, text).unwrap();
    let run = backtest(
        &repo,
        &["--labels", labels.to_str().unwrap(), "--format", "json"],
    );
    assert_eq!(run.code, 1);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["schema"], 1);
    let units = json["units"].as_array().unwrap();
    assert_eq!(units.len(), 4);
    let landings: Vec<&str> = units
        .iter()
        .map(|unit| unit["landing"].as_str().unwrap())
        .collect();
    assert_eq!(landings, ["merge", "squash", "squash", "push"]);
    assert_eq!(units[1]["fetched"], true);
    assert_eq!(units[2]["fetched"], false);
    assert_eq!(units[2]["sha"], unfetched.as_str());
    assert_eq!(units[0]["commits"], 3);
    // PR #1's undone check is there too, with its status, but has no label.
    let statuses: Vec<(&str, Option<&str>)> = units[0]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| {
            (
                finding["status"].as_str().unwrap(),
                finding["label"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [("shown", Some("unexplained")), ("transient", None)]
    );
    assert_eq!(units[1]["findings"][0]["label"], serde_json::Value::Null);
    assert_eq!(json["labels"]["unexplained"], 1);
    assert_eq!(json["labels"]["not_labelled"], 3);
}
