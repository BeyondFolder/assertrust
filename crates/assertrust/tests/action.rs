//! The GitHub Action's script, `action/run.sh` (`docs/architecture.md`, step 8): which
//! commits it checks for each event, and when it posts or edits the PR comment. A fake `gh`
//! stands in for GitHub's API.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use common::{Run, TestRepo};
use tempfile::TempDir;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../action/run.sh");
const ACTION: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const BINARY: &str = env!("CARGO_BIN_EXE_assertrust");

const TESTS: &str =
    "it('refunds', () => {\n  expect(res.status).toBe(200)\n  expect(res.body.ok).toBe(true)\n})\n";

/// A `gh` that logs each call, prints `$FAKE_GH_COMMENTS` (pages of comments) for the
/// listing of a PR's comments, and fails what it's asked to: the listing, or writes (a
/// fork's read-only token).
const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
case "$*" in
  *--paginate*)
    [ -z "${FAKE_GH_LIST_FAILS:-}" ] || exit 1
    printf '%s' "${FAKE_GH_COMMENTS:-[[]]}" ;;
  *--method*) [ -z "${FAKE_GH_READ_ONLY:-}" ] || exit 1 ;;
esac
"#;

/// The Action's surroundings for one run: the event, the job summary, and the fake `gh`.
struct Runner {
    dir: TempDir,
}

impl Runner {
    fn new(event: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        let gh = dir.path().join("bin/gh");
        fs::write(&gh, FAKE_GH).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.path().join("event.json"), event).unwrap();
        fs::write(dir.path().join("gh.log"), "").unwrap();
        fs::write(dir.path().join("summary.md"), "").unwrap();
        fs::write(dir.path().join("outputs"), "").unwrap();
        Self { dir }
    }

    /// Runs the script in `repo` for `event_name`, with the action's inputs and the fake
    /// `gh`'s settings in `env`.
    fn run(&self, repo: &TestRepo, event_name: &str, env: &[(&str, &str)]) -> Run {
        let path = |file: &str| self.dir.path().join(file).display().to_string();
        let search = format!("{}:{}", path("bin"), std::env::var("PATH").unwrap());
        let (event, summary, log) = (path("event.json"), path("summary.md"), path("gh.log"));
        let outputs = path("outputs");
        let mut vars: Vec<(&str, &str)> = vec![
            ("PATH", &search),
            ("GITHUB_EVENT_NAME", event_name),
            ("GITHUB_EVENT_PATH", &event),
            ("GITHUB_STEP_SUMMARY", &summary),
            ("GITHUB_OUTPUT", &outputs),
            ("GITHUB_REPOSITORY", "acme/shop"),
            ("GITHUB_ACTION_PATH", ACTION),
            ("GITHUB_RUN_ID", "42"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("INPUT_PATH", BINARY),
            ("FAKE_GH_LOG", &log),
        ];
        vars.extend_from_slice(env);
        repo.program_in(&repo.path(), "bash", &[SCRIPT], &vars)
    }

    fn summary(&self) -> String {
        fs::read_to_string(self.dir.path().join("summary.md")).unwrap()
    }

    fn outputs(&self) -> String {
        fs::read_to_string(self.dir.path().join("outputs")).unwrap()
    }

    /// The fake `gh`'s calls, the first line of each.
    fn gh_calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("gh.log"))
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("api"))
            .map(str::to_owned)
            .collect()
    }
}

/// A repo with a test on `main` and a `feature` branch that drops one of its checks.
/// Returns it, `main`'s commit and the branch's.
fn weakened() -> (TestRepo, String, String) {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TESTS);
    let base = repo.commit("Add tests");
    repo.branch("feature");
    repo.write(
        "test/a.test.ts",
        &TESTS.replace("  expect(res.body.ok).toBe(true)\n", ""),
    );
    let head = repo.commit("Trim refunds");
    (repo, base, head)
}

fn pull_request(base: &str, head: &str) -> String {
    format!(
        r#"{{ "pull_request": {{ "number": 7, "base": {{ "sha": "{base}" }}, "head": {{ "sha": "{head}" }} }} }}"#
    )
}

#[test]
fn a_pull_request_gets_annotations_a_summary_and_one_comment() {
    let (repo, base, head) = weakened();
    let runner = Runner::new(&pull_request(&base, &head));
    let run = runner.run(&repo, "pull_request", &[]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains(&format!(
            "::warning file=test/a.test.ts,line=1,title=assertrust%3A check-removed (high)::removed expect(res.body.ok).toBe(true) ({})",
            &head[..7]
        )),
        "{}",
        run.stdout
    );
    assert!(
        runner
            .summary()
            .starts_with("<!-- assertrust -->\n### assertrust: 1 finding\n")
    );
    assert_eq!(runner.outputs(), "findings=1\n");
    let calls = runner.gh_calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].starts_with("api --paginate --slurp repos/acme/shop/issues/7/comments"));
    assert!(
        calls[1].starts_with(
            "api --method POST repos/acme/shop/issues/7/comments -f body=<!-- assertrust -->"
        ),
        "{calls:?}"
    );
    let log = fs::read_to_string(runner.dir.path().join("gh.log")).unwrap();
    assert!(log.contains("[run](https://github.com/acme/shop/actions/runs/42)"));
}

#[test]
fn a_clean_pull_request_gets_a_comment_only_to_update_one() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TESTS);
    let base = repo.commit("Add tests");
    repo.write("README.md", "Docs\n");
    let head = repo.commit("Docs");
    let runner = Runner::new(&pull_request(&base, &head));
    let run = runner.run(&repo, "pull_request", &[]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("assertrust: no findings"),
        "{}",
        run.stdout
    );
    // No comment yet: none is posted.
    assert_eq!(runner.gh_calls().len(), 1);
    // One from an earlier push: it now says there's nothing. A person's comment quoting
    // it isn't the one.
    let comments = r#"[[{ "id": 1111, "user": { "type": "User" }, "body": "<!-- assertrust -->\nquoted" },
        { "id": 1234, "user": { "type": "Bot" }, "body": "<!-- assertrust -->\n### assertrust: 1 finding" }],
        [{ "id": 5678, "user": { "type": "Bot" }, "body": "<!-- assertrust -->\nanother" }]]"#;
    let runner = Runner::new(&pull_request(&base, &head));
    runner.run(&repo, "pull_request", &[("FAKE_GH_COMMENTS", comments)]);
    let calls = runner.gh_calls();
    assert!(
        calls[1].starts_with(
            "api --method PATCH repos/acme/shop/issues/comments/1234 -f body=<!-- assertrust -->"
        ),
        "{calls:?}"
    );
}

#[test]
fn allowed_findings_alone_get_no_comment() {
    let mut repo = TestRepo::new();
    repo.write("test/a.test.ts", TESTS);
    let base = repo.commit("Add tests");
    repo.write(
        "test/a.test.ts",
        &TESTS.replace(
            "  expect(res.body.ok).toBe(true)\n",
            "  // assertrust-allow: check-removed the body is checked in the API tests\n",
        ),
    );
    let head = repo.commit("Trim refunds");
    let runner = Runner::new(&pull_request(&base, &head));
    let run = runner.run(&repo, "pull_request", &[]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        runner.summary().contains("**1 allowed**"),
        "{}",
        runner.summary()
    );
    assert_eq!(runner.outputs(), "findings=0\n");
    assert_eq!(runner.gh_calls().len(), 1);
}

#[test]
fn comments_that_can_not_be_listed_are_left_alone() {
    // Posting could add a second one.
    let (repo, base, head) = weakened();
    let runner = Runner::new(&pull_request(&base, &head));
    let run = runner.run(&repo, "pull_request", &[("FAKE_GH_LIST_FAILS", "1")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("couldn't read the PR's comments"),
        "{}",
        run.stdout
    );
    assert_eq!(runner.gh_calls().len(), 1);
}

#[test]
fn a_read_only_token_leaves_the_findings_in_the_job_summary() {
    let (repo, base, head) = weakened();
    let runner = Runner::new(&pull_request(&base, &head));
    let run = runner.run(&repo, "pull_request", &[("FAKE_GH_READ_ONLY", "1")]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .contains("::notice title=assertrust::couldn't post the PR comment"),
        "{}",
        run.stdout
    );
    assert!(runner.summary().contains("check-removed"));
    // Turned off, nothing is posted.
    let runner = Runner::new(&pull_request(&base, &head));
    runner.run(&repo, "pull_request", &[("INPUT_COMMENT", "false")]);
    assert!(runner.gh_calls().is_empty());
}

#[test]
fn fail_on_fails_the_job() {
    let (repo, base, head) = weakened();
    let runner = Runner::new(&pull_request(&base, &head));
    let run = runner.run(&repo, "pull_request", &[("INPUT_FAIL_ON", "high")]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("::error file=test/a.test.ts"),
        "{}",
        run.stdout
    );
    // A --fail-on in args wins over the input.
    let env = [("INPUT_FAIL_ON", "high"), ("INPUT_ARGS", "--fail-on=never")];
    let run = runner.run(&repo, "pull_request", &env);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    // The format is the action's.
    let run = runner.run(&repo, "pull_request", &[("INPUT_ARGS", "--format json")]);
    assert_eq!(run.code, 2);
    assert!(
        run.stdout.contains("leave --format out of args"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_push_checks_each_commit_pushed() {
    let (mut repo, base, _) = weakened();
    repo.write("README.md", "Docs\n");
    let head = repo.commit("Docs");
    let runner = Runner::new(&format!(
        r#"{{ "before": "{base}", "after": "{head}", "repository": {{ "default_branch": "main" }} }}"#
    ));
    let run = runner.run(&repo, "push", &[]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("check-removed"), "{}", run.stdout);
    // Pushes never comment.
    assert!(runner.gh_calls().is_empty());
}

#[test]
fn a_new_branch_is_checked_from_where_it_split_off() {
    let (repo, _, head) = weakened();
    let zeros = "0".repeat(40);
    let runner = Runner::new(&format!(
        r#"{{ "before": "{zeros}", "after": "{head}", "repository": {{ "default_branch": "main" }} }}"#
    ));
    let run = runner.run(&repo, "push", &[]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("check-removed"), "{}", run.stdout);
    // The default branch's first push has nothing to compare with.
    let main = repo.git(&["rev-parse", "main"]);
    let runner = Runner::new(&format!(
        r#"{{ "before": "{zeros}", "after": "{main}", "repository": {{ "default_branch": "main" }} }}"#
    ));
    let run = runner.run(&repo, "push", &[]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("no commits to check"), "{}", run.stdout);
}

#[test]
fn other_events_need_a_range() {
    let (repo, base, head) = weakened();
    let runner = Runner::new("{}");
    let run = runner.run(&repo, "workflow_dispatch", &[]);
    assert_eq!(run.code, 2);
    assert!(
        run.stdout.contains("run it on pull_request or push events"),
        "{}",
        run.stdout
    );
    // Written over several lines (`args: |`), every line counts.
    let args = format!("--show-transient\n--range {base}...{head}\n");
    let run = runner.run(&repo, "workflow_dispatch", &[("INPUT_ARGS", &args)]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("check-removed"), "{}", run.stdout);
    // A pull_request_target checkout doesn't have a fork's commits.
    let runner = Runner::new(&pull_request(&base, &head));
    let run = runner.run(&repo, "pull_request_target", &[]);
    assert_eq!(run.code, 2);
    assert!(
        run.stdout.contains("run it on pull_request or push events"),
        "{}",
        run.stdout
    );
}

/// A `curl` that serves `$FAKE_RELEASES/<tag>/<file>` for a release download URL, and logs
/// each URL.
const FAKE_CURL: &str = r#"#!/bin/sh
out=""
while [ $# -gt 1 ]; do
  case "$1" in -o) out="$2"; shift ;; esac
  shift
done
printf '%s\n' "$1" >> "$FAKE_CURL_LOG"
cp "$FAKE_RELEASES/${1##*/download/}" "$out"
"#;

/// A release for `target`, as the Release workflow publishes it: the archive and its
/// checksum, under `<dir>/v<version>/`.
fn release(dir: &std::path::Path, target: &str, tamper: bool) {
    let version = env!("CARGO_PKG_VERSION");
    let folder = dir.join(format!("v{version}"));
    let staging = dir.join(format!("assertrust-{target}"));
    fs::create_dir_all(&folder).unwrap();
    fs::create_dir_all(&staging).unwrap();
    // A stand-in that runs this build: compressing the build itself takes a minute.
    let binary = staging.join("assertrust");
    fs::write(&binary, format!("#!/bin/sh\nexec '{BINARY}' \"$@\"\n")).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let archive = format!("assertrust-{target}.tar.xz");
    let made = std::process::Command::new("tar")
        .args(["-cJf", &folder.join(&archive).display().to_string()])
        .args([
            "-C",
            &dir.display().to_string(),
            &format!("assertrust-{target}"),
        ])
        .status()
        .unwrap();
    assert!(made.success());
    let sum = std::process::Command::new("shasum")
        .args(["-a", "256", &folder.join(&archive).display().to_string()])
        .output()
        .unwrap();
    let mut hash = String::from_utf8(sum.stdout).unwrap()[..64].to_owned();
    if tamper {
        hash = "0".repeat(64);
    }
    fs::write(
        folder.join(format!("{archive}.sha256")),
        format!("{hash} *{archive}\n"),
    )
    .unwrap();
}

#[test]
fn the_release_for_the_runner_is_downloaded_and_checked() {
    let (repo, base, head) = weakened();
    let runner = Runner::new(&pull_request(&base, &head));
    let curl = runner.dir.path().join("bin/curl");
    fs::write(&curl, FAKE_CURL).unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
    let releases = tempfile::tempdir().unwrap();
    release(releases.path(), "aarch64-apple-darwin", false);
    let temp = tempfile::tempdir().unwrap();
    let log = runner.dir.path().join("curl.log").display().to_string();
    let (releases_dir, temp_dir) = (
        releases.path().display().to_string(),
        temp.path().display().to_string(),
    );
    let env = [
        ("INPUT_PATH", ""),
        ("RUNNER_OS", "macOS"),
        ("RUNNER_ARCH", "ARM64"),
        ("RUNNER_TEMP", &temp_dir),
        ("FAKE_RELEASES", &releases_dir),
        ("FAKE_CURL_LOG", &log),
    ];
    let run = runner.run(&repo, "pull_request", &env);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("check-removed"), "{}", run.stdout);
    // The version this copy of the action was released with.
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        format!(
            "https://github.com/BeyondFolder/assertrust/releases/download/v{version}/assertrust-aarch64-apple-darwin.tar.xz\n\
             https://github.com/BeyondFolder/assertrust/releases/download/v{version}/assertrust-aarch64-apple-darwin.tar.xz.sha256\n"
        )
    );

    // An archive that doesn't match its checksum isn't run.
    let releases = tempfile::tempdir().unwrap();
    release(releases.path(), "x86_64-unknown-linux-gnu", true);
    let releases_dir = releases.path().display().to_string();
    let temp = tempfile::tempdir().unwrap();
    let temp_dir = temp.path().display().to_string();
    let env = [
        ("INPUT_PATH", ""),
        ("RUNNER_OS", "Linux"),
        ("RUNNER_ARCH", "X64"),
        ("RUNNER_TEMP", &temp_dir),
        ("FAKE_RELEASES", &releases_dir),
        ("FAKE_CURL_LOG", &log),
    ];
    let run = runner.run(&repo, "pull_request", &env);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert!(
        run.stdout.contains("doesn't match its checksum"),
        "{}",
        run.stdout
    );

    // No build for Windows yet.
    let env = [
        ("INPUT_PATH", ""),
        ("RUNNER_OS", "Windows"),
        ("RUNNER_ARCH", "X64"),
    ];
    let run = runner.run(&repo, "pull_request", &env);
    assert_eq!(run.code, 2);
    assert!(
        run.stdout.contains("only Linux and macOS"),
        "{}",
        run.stdout
    );
}
