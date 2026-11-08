# assertrust: notes for agents

assertrust is a Rust CLI that detects changes that weaken a test suite. Read `README.md` for what it is and `docs/architecture.md` for how it works.

## How we work

- **One PR at a time.** Plan only the next PR in detail. Don't open several at once.
- **Size:** aim for fewer than ~50 files changed (soft limit). 1–3k lines is fine, including tests.
- **Every PR includes:**
  - a description: what changed, why, and how to verify it
  - updated docs: when the code changes the design, update `docs/architecture.md` in the same PR so the doc always matches the code
  - tests, preferably integration tests that build temporary git repos and run the `assertrust` binary
- **Explaining a design choice:** start with one concrete before/after example, then the details.

## Code

- Rust workspace in `crates/`. The CLI uses `clap` with its derive API. New crates are added in the PR that gives them code.
- LLM dependencies stay behind the optional `llm` cargo feature. The deterministic core never needs an API key.
- Before pushing, these must pass: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- Tests use the harness in `crates/assertrust/tests/common/mod.rs` (temp git repos, isolated from the developer's git config). Snapshots use insta. See `docs/development.md`.

## Private code

- Mailyond, the backtest target, is a private, proprietary codebase. Never commit its code or fixtures copied from it. A few short snippets quoted in docs are fine. The backtest reads a local clone, with the path set by an env var.
