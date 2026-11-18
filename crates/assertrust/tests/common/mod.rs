//! Test harness: throwaway git repos, and a way to run the built `assertrust` binary.
//!
//! Every git command (and the binary, which will shell out to git) runs isolated:
//! - the user's global and system git config are switched off, so commit signing, hooks
//!   and aliases on the developer's machine can't change or hang a test;
//! - git variables inherited from a surrounding git hook (`GIT_DIR`, …) are removed;
//! - author, committer and dates are fixed, so the same steps give the same commit SHAs
//!   on every machine and every run, which keeps snapshots stable;
//! - model providers' keys and addresses are removed, so the judge never reaches a real
//!   model: tests point it at a [`FakeModel`] instead.

// Each test file uses a different subset of these helpers.
#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

/// The built binary. Cargo sets this variable for integration tests.
const BINARY: &str = env!("CARGO_BIN_EXE_assertrust");

/// 2026-01-01T00:00:00Z. Each commit is one minute after the previous one.
const START_TIMESTAMP: u64 = 1_767_225_600;

/// Git variables a surrounding git hook may set, which would point git at the wrong repo.
const LEAKY_GIT_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
];

/// Variables that would point the judge at a real model, or at a real pull request.
const MODEL_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "GEMINI_API_KEY",
    "OLLAMA_API_KEY",
    "OLLAMA_API_BASE_URL",
    "GITHUB_EVENT_NAME",
    "GITHUB_EVENT_PATH",
];

/// The result of running a command: exit code and captured output.
#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `assertrust` with `args` in an empty temp dir, outside any git repo (for `--help`,
/// `--version`, flag errors, and the "not inside a git repository" error).
pub fn assertrust(args: &[&str]) -> Run {
    let dir = tempfile::tempdir().expect("create temp dir");
    let parent = dir.path().parent().expect("temp dir has a parent");
    run(Command::new(BINARY)
        .args(args)
        .current_dir(dir.path())
        // Stop git from finding a repo in a parent directory.
        .env("GIT_CEILING_DIRECTORIES", parent))
}

/// A git repo in a temp dir, deleted when the value is dropped.
///
/// ```ignore
/// let mut repo = TestRepo::new();
/// repo.write("a.test.ts", "expect(x).toBe(1)");
/// let base = repo.commit("add test");
/// repo.branch("feature");
/// repo.write("a.test.ts", "expect(x).toBeTruthy()");
/// repo.commit("loosen it");
/// let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
/// ```
pub struct TestRepo {
    root: TempDir,
    commits: u64,
}

impl TestRepo {
    /// An empty repo on branch `main`.
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("create temp dir");
        fs::create_dir(root.path().join("home")).expect("create fake home");
        fs::write(root.path().join("home/.gitconfig"), "").expect("create empty git config");
        fs::create_dir(root.path().join("repo")).expect("create repo dir");

        let repo = Self { root, commits: 0 };
        repo.git(&["init", "--quiet", "--initial-branch=main"]);
        repo
    }

    /// The repo's working directory.
    pub fn path(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    /// Writes a file (creating parent directories), relative to the repo root.
    pub fn write(&self, file: &str, contents: &str) {
        let path = self.path().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write file");
    }

    /// Deletes a file, relative to the repo root.
    pub fn remove(&self, file: &str) {
        fs::remove_file(self.path().join(file)).expect("remove file");
    }

    /// Stages one file, without committing.
    pub fn stage(&self, file: &str) {
        self.git(&["add", "--", file]);
    }

    /// Stages everything and commits it. Returns the new commit's SHA.
    pub fn commit(&mut self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.with_next_date(&["commit", "--quiet", "--allow-empty", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Creates a branch at the current commit and switches to it.
    pub fn branch(&self, name: &str) {
        self.git(&["switch", "--quiet", "--create", name]);
    }

    /// Switches to an existing branch.
    pub fn switch(&self, name: &str) {
        self.git(&["switch", "--quiet", name]);
    }

    /// Merges `branch` into the current branch with a merge commit (like GitHub's
    /// "Create a merge commit"). Returns the merge commit's SHA.
    pub fn merge(&mut self, branch: &str, message: &str) -> String {
        self.with_next_date(&["merge", "--quiet", "--no-ff", "-m", message, branch]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Runs git in the repo and returns its trimmed stdout. Panics if git fails.
    pub fn git(&self, args: &[&str]) -> String {
        let run = self.try_git(args);
        assert_eq!(run.code, 0, "git {args:?} failed:\n{}", run.stderr);
        run.stdout.trim_end().to_owned()
    }

    /// Runs git in the repo and returns the result, even if git fails.
    pub fn try_git(&self, args: &[&str]) -> Run {
        run(self.isolated(Command::new("git").args(args)))
    }

    /// Runs `assertrust` in the repo with the same isolation as git.
    pub fn assertrust(&self, args: &[&str]) -> Run {
        run(self.isolated(Command::new(BINARY).args(args)))
    }

    /// Runs `assertrust` in the repo with extra environment variables.
    pub fn assertrust_with_env(&self, args: &[&str], env: &[(&str, &std::path::Path)]) -> Run {
        let mut command = Command::new(BINARY);
        command.args(args);
        self.isolated(&mut command);
        run(command.envs(env.iter().copied()))
    }

    /// Runs `assertrust` in the repo with extra environment variables, given as text.
    pub fn assertrust_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut command = Command::new(BINARY);
        command.args(args);
        self.isolated(&mut command);
        run(command.envs(env.iter().copied()))
    }

    /// Runs any program with the same isolation as git, from `dir`, with extra environment
    /// variables.
    pub fn program_in(
        &self,
        dir: &std::path::Path,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Run {
        let mut command = Command::new(program);
        command.args(args);
        self.isolated(&mut command);
        run(command.current_dir(dir).envs(env.iter().copied()))
    }

    /// Runs `assertrust` from a subdirectory of the repo (relative to the repo root).
    pub fn assertrust_in(&self, subdir: &str, args: &[&str]) -> Run {
        run(self
            .isolated(Command::new(BINARY).args(args))
            .current_dir(self.path().join(subdir)))
    }

    /// Runs `assertrust` with its stdout closed before it writes anything, like
    /// `assertrust … | head -0`.
    pub fn assertrust_into_closed_pipe(&self, args: &[&str]) -> Run {
        let mut child = self
            .isolated(Command::new(BINARY).args(args))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start process");
        drop(child.stdout.take());
        to_run(child.wait_with_output().expect("wait for process"))
    }

    /// Runs a committing git command with the next fixed date.
    fn with_next_date(&mut self, args: &[&str]) {
        self.commits += 1;
        let date = format!("@{} +0000", START_TIMESTAMP + 60 * self.commits);
        let run = run(self
            .isolated(Command::new("git").args(args))
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date));
        assert_eq!(run.code, 0, "git {args:?} failed:\n{}", run.stderr);
    }

    fn isolated<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        let home = self.root.path().join("home");
        for var in LEAKY_GIT_VARS {
            cmd.env_remove(var);
        }
        // On GitHub Actions, a test must not write to the real job summary or step outputs.
        cmd.env_remove("GITHUB_STEP_SUMMARY");
        cmd.env_remove("GITHUB_OUTPUT");
        for var in MODEL_VARS {
            cmd.env_remove(var);
        }
        cmd.current_dir(self.path())
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.test")
            .env("GIT_COMMITTER_NAME", "Test Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.test")
    }
}

impl Default for TestRepo {
    fn default() -> Self {
        Self::new()
    }
}

/// A stand-in for Anthropic's Messages API on localhost, for the judge. Each request gets the
/// verdict of the first rule whose text is in its body (else the default), or the error
/// status it was made with; every body is kept, to look at.
pub struct FakeModel {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

/// One rule: text in the request → verdict and reason.
pub type Answer = (&'static str, &'static str, &'static str);

impl FakeModel {
    /// Answers with the first matching rule, else `default` (verdict, reason).
    pub fn new(rules: &[Answer], default: (&'static str, &'static str)) -> Self {
        Self::serve(rules.to_vec(), default, None)
    }

    /// Answers every request with HTTP `status` and an API error.
    pub fn failing(status: u16) -> Self {
        Self::serve(Vec::new(), ("unsure", ""), Some(status))
    }

    fn serve(
        rules: Vec<Answer>,
        default: (&'static str, &'static str),
        status: Option<u16>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        let url = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let rules = rules.clone();
                let kept = Arc::clone(&kept);
                std::thread::spawn(move || answer(stream, &rules, default, status, &kept));
            }
        });
        Self { url, requests }
    }

    /// The variables that point the judge at it.
    pub fn env(&self) -> [(&'static str, &str); 2] {
        [
            ("ANTHROPIC_BASE_URL", &self.url),
            ("ANTHROPIC_API_KEY", "test-key"),
        ]
    }

    /// The bodies of the requests it got.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests lock").clone()
    }
}

fn answer(
    stream: std::net::TcpStream,
    rules: &[Answer],
    default: (&str, &str),
    status: Option<u16>,
    kept: &Mutex<Vec<String>>,
) {
    let mut reader = BufReader::new(stream);
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            return;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    let (verdict, reason) = rules
        .iter()
        .find(|(text, _, _)| body.contains(text))
        .map_or(default, |(_, verdict, reason)| (*verdict, *reason));
    kept.lock().expect("requests lock").push(body);
    let (code, reply) = match status {
        Some(code) => (
            code,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad request"}}"#
                .to_owned(),
        ),
        None => {
            let text = serde_json::json!({ "verdict": verdict, "reason": reason }).to_string();
            let message = serde_json::json!({
                "id": "msg_test",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-5-5",
                "content": [{ "type": "text", "text": text }],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": { "input_tokens": 1000, "output_tokens": 50 },
            });
            (200, message.to_string())
        }
    };
    let mut stream = reader.into_inner();
    let _ = write!(
        stream,
        "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.flush();
}

fn run(cmd: &mut Command) -> Run {
    to_run(cmd.output().expect("start process"))
}

fn to_run(output: Output) -> Run {
    Run {
        code: output
            .status
            .code()
            .expect("process was killed by a signal"),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}
