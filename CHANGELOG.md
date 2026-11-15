# Changelog

All notable changes to assertrust. It follows [semver](https://semver.org), staying on 0.x while the rules settle.

## 0.1.0

The first release: a CLI and a GitHub Action that flag changes weakening a TypeScript or JavaScript test suite (vitest, jest, Playwright).

- `assertrust check` finds removed checks, looser matchers, checks that accept more, disabled tests, checks that can no longer fail, and CI or test config that lets failures through. Checks in helper functions, hooks and helper class methods count for the tests that use them.
- PR-style checks name the commit behind each finding and hide weakening undone later in the PR, or done to tests the PR itself added.
- Output as text, JSON (`"schema": 1`) or GitHub annotations with a job summary; `fail-on` decides the exit code, warn-only by default.
- Allow comments and `Assertrust-Allow` commit trailers acknowledge a deliberate change, with a reason.
- `assertrust.toml` sets `fail-on`, which files are tests, helpers and gates, and which rules run. A change can't loosen its own check.
- `assertrust backtest` replays a branch's history PR by PR, and measures precision against labels.
- `assertrust changes`, `tests` and `pairs` show what a check sees.
- The GitHub Action (`BeyondFolder/assertrust@v0`) checks pull requests and pushes, annotates the diff, and keeps one PR comment once there are findings.
- Builds for Linux and macOS (x64 and arm64), with a shell installer.
