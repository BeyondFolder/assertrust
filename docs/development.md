# Development

## Prerequisites

- Rust 1.97 or newer, installed with [rustup](https://rustup.rs). The minimum is set by `rust-version` in `Cargo.toml`, and CI checks it.
- git 2.28 or newer.

## Build and run

```sh
cargo build
cargo run -- --help
cargo run -- check --range origin/main..HEAD
```

## Checks (the same ones CI runs)

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace   # broken doc links
```

## Layout

```
Cargo.toml                    workspace: shared versions, dependencies, lints
crates/assertrust/            the binary
  src/main.rs                 entry point, exit codes
  src/cli.rs                  commands and flags (clap, derive API)
  src/check_command.rs        `assertrust check`: runs the rules, prints the findings
  src/changes.rs              `assertrust changes` output
  src/tests_command.rs        `assertrust tests` output
  src/pairs_command.rs        `assertrust pairs` output
  src/load.rs                 reading and parsing the test files a comparison touches
  tests/common/mod.rs         test harness: temp git repos, running the binary
  tests/*.rs                  integration tests
  tests/fixtures/parsing/     one test file per pattern the parser must handle
  tests/fixtures/matching/    a test file before and after a change, for `pairs`
  tests/fixtures/rules/       a test file before and after a change that weakens it, for `check`
  tests/snapshots/*.snap      expected outputs (insta)
crates/assertrust-git/        input layer (step 1): which commits and files to look at
  src/mode.rs                 --range / --staged / --worktree / default → revisions
  src/git.rs                  every git call (from the repo root, read-only, NUL-separated output)
  src/diff.rs                 parsing `git diff --name-status -z`
  src/classify.rs             file kinds: test, test-support, gate, source, other
  src/model.rs                the types handed to later steps
  src/lib.rs                  `Repo`: analyze a mode, read a file at a commit / the index / disk
crates/assertrust-ir/         parsing and the test IR (steps 2–3): tests and checks in a file
  src/model.rs                the IR types (language-neutral)
  src/value.rs                argument values, compared as structure
  src/matcher.rs              matcher name → kind
  src/js/mod.rs               the oxc walk: tests, hooks, checks and where they sit
  src/js/calls.rs             recognizing `describe` / `it` / hooks / `expect(…)` by shape
  src/js/values.rs            expressions → values and normalized source
  src/js/consts.rs            which `const`s can be filled in where they're used
  src/js/facts.rs             what a check states, however it's written (for matching)
crates/assertrust-match/      matching (step 4): which test and check after a change is which before
  src/test_pairing.rs         pairing tests: same id, same name, then similarity
  src/check_pairing.rs        pairing checks inside a test pair, and checks that moved
  src/similarity.rs           the scores and thresholds
  tests/matching.rs           the cases from step 4 of the architecture doc
crates/assertrust-rules/      rules (step 5): whether a change made a test weaker
  src/lib.rs                  findings, and running every rule on a change
  src/check_removed.rs        one file per rule
  src/test_disabled.rs
  src/check_cant_fail.rs
  src/changed_check.rs        the four rules that compare a check with what it became
  src/strength.rs             the matcher strength ladder
  src/looser.rs               which expected values accept more
  tests/rules.rs              cases each rule must flag and must not
```

New crates are added in the PR that gives them code (see `docs/architecture.md` for the planned pieces).

## Tests

Tests are integration tests. They run the real `assertrust` binary the way a user would. Cargo builds the binary and tells tests where it is through `CARGO_BIN_EXE_assertrust`, so no extra library is needed for that.

Most tests need a git history to check, which the harness in `tests/common/mod.rs` builds:

```rust
mod common;
use common::TestRepo;

#[test]
fn flags_a_loosened_matcher() {
    let mut repo = TestRepo::new();               // empty repo on `main`, in a temp dir
    repo.write("a.test.ts", "expect(x).toBe(1);\n");
    let base = repo.commit("Add a test");         // returns the commit SHA
    repo.write("a.test.ts", "expect(x).toBeTruthy();\n");
    repo.commit("Loosen it");

    let run = repo.assertrust(&["check", "--range", &format!("{base}..HEAD")]);
    assert_eq!(run.code, 0);                      // run.stdout / run.stderr hold the output
}
```

Other helpers: `branch`, `switch`, `merge` (always a merge commit), `stage`, `remove`, `git` (any git command), `assertrust_in` (run from a subdirectory), `assertrust_into_closed_pipe` (stdout closed early, like `| head`).

**Why the harness isolates git.** Tests must behave the same on every machine. So git runs:
- without your global or system config: commit signing, hooks or aliases would otherwise change or hang tests
- without git variables inherited from a surrounding git hook
- with fixed author, committer and dates, so the same steps always produce the same commit SHAs

## Snapshots

Some tests compare output against a stored file in `tests/snapshots/` instead of a string in the code, using [insta](https://insta.rs). Examples are the `--help` text, the `assertrust tests` output for each parsing fixture, and later findings output. A change in output shows up as a diff of the `.snap` file in the PR, which is easy to review.

Snapshots come in two forms:
- **Inline:** the expected text sits in the test itself, after `@r"`, next to the repo the test builds. Most tests use these because they're easy to read.
- **File:** stored in `tests/snapshots/*.snap`. Used for long output like `--help` and the parsing fixtures.

To cover a new parsing pattern, add a small `.ts` file to `tests/fixtures/parsing/` (written from scratch, never copied from a private codebase), add a test for it in `tests/tests_command.rs`, and review the new snapshot.

When output changes on purpose:

```sh
cargo install cargo-insta           # once
cargo insta test --review           # run tests, then accept or reject each changed snapshot
```

File snapshots can also be rewritten without cargo-insta, using `INSTA_UPDATE=always cargo test`. Then review them with `git diff`. CI never writes snapshots. A mismatch fails the build.

## Exit codes

- `0`: no findings, or nothing at or above `fail-on`
- `1`: findings at or above `fail-on`
- `2`: assertrust itself failed, including bad flags (clap uses 2 for usage errors too)
