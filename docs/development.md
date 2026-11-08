# Development

## Prerequisites

- Rust 1.85 or newer, installed with [rustup](https://rustup.rs). The minimum is set by `rust-version` in `Cargo.toml`, and CI checks it.
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
```

## Layout

```
Cargo.toml                    workspace: shared versions, dependencies, lints
crates/assertrust/            the binary
  src/main.rs                 entry point, exit codes
  src/cli.rs                  commands and flags (clap, derive API)
  tests/common/mod.rs         test harness: temp git repos, running the binary
  tests/*.rs                  integration tests
  tests/snapshots/*.snap      expected outputs (insta)
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

Other helpers: `branch`, `switch`, `merge` (always a merge commit), `stage`, `remove`, `git` (any git command).

**Why the harness isolates git.** Tests must behave the same on every machine. So git runs:
- without your global or system config: commit signing, hooks or aliases would otherwise change or hang tests
- without git variables inherited from a surrounding git hook
- with fixed author, committer and dates, so the same steps always produce the same commit SHAs

## Snapshots

Some tests compare output against a stored file in `tests/snapshots/` instead of a string in the code, using [insta](https://insta.rs). Examples are the `--help` text and, later, findings output. A change in output shows up as a diff of the `.snap` file in the PR, which is easy to review.

When output changes on purpose:

```sh
INSTA_UPDATE=always cargo test   # rewrite the snapshots
git diff crates/*/tests/snapshots # review what changed
```

Alternatively, run `cargo install cargo-insta`, then `cargo insta review` to accept or reject each change interactively. CI never writes snapshots. A mismatch fails the build.

## Exit codes

- `0`: no findings, or nothing at or above `fail-on`
- `1`: findings at or above `fail-on`
- `2`: assertrust itself failed, including bad flags (clap uses 2 for usage errors too)
