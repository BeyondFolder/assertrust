# Changelog

All notable changes to assertrust. It follows [semver](https://semver.org), staying on 0.x while the rules settle.

## 0.2.0

The optional AI judge: with your own key, a model reads each finding with the change around it and says whether the test still checks what it promises.

- `assertrust check --judge` gives each shown finding a verdict: `fine`, `coverage-lost`, `suspicious` or `unsure`. Findings judged fine are folded away, and `coverage-lost` and `suspicious` fail the check (`[judge] fail-on`). Changed expected values, hidden until now, show up when the judge doesn't call them fine.
- Sonnet 5.5 at `medium` effort by default. `--judge-model` and `--judge-effort`, or `[judge]` in `assertrust.toml`, pick another: models of Anthropic, OpenAI, Google Gemini and Ollama work, named `provider/model`.
- Verdicts are cached in `.git/assertrust/judge/`, so a change is judged once and CI never flips. With no key, no network or a bad answer, findings show without verdicts and the check doesn't fail.
- `--judge-dry-run` prints exactly what a model would be sent, and needs no key.
- Verdicts show in the terminal, in JSON (`"judge"`, still `"schema": 1`), and in the GitHub annotations and job summary.
- The Action takes `judge`, `judge-model` and `judge-effort`, and keeps verdicts between runs with `actions/cache`.
- `assertrust backtest --judge` measures the judge against labels. On the labelled history of 11 repos, with the default model, 88% of the findings it fails are right, and it folds away 80 of the 97 false alarms.
- Building from source needs Rust 1.97.1.

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
