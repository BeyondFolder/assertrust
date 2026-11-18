# assertrust

**Can you trust your green CI?**

assertrust catches changes that quietly make a test suite weaker: an assertion removed, a matcher loosened, a test skipped, a CI gate relaxed. Coding agents do this to get CI green. So do tired humans.

```
test/api/forwarding.test.ts:27  HIGH  expectation-loosened
  toMatchObject no longer checks domainIds, status · 10d8742
```

It reads TypeScript and JavaScript tests written with vitest, jest or Playwright, plus the CI and test config that decides whether they fail the build. It runs on every commit and on each PR as a whole, names the commit that weakened a test, and needs no API key: the rules are deterministic and never touch the network. An optional [AI judge](#the-ai-judge), with your own key, gives each finding a verdict.

> **Status: v0.2, early.** On the history of 10 public repos and one private one, half of what it shows is right, and 70% at high confidence ("How well it works" below). So it warns by default and never fails a build unless you ask it to. With the optional AI judge on, the findings it confirms fail the build, and 88% of those are right.

- [Quick start](#quick-start)
- [What it catches](#what-it-catches)
- [The GitHub Action](#the-github-action)
- [Commands](#commands)
- [Output and exit codes](#output-and-exit-codes)
- [Saying a change is deliberate](#saying-a-change-is-deliberate)
- [Configuration](#configuration-assertrusttoml)
- [Git hooks and coding agents](#git-hooks-and-coding-agents)
- [The AI judge](#the-ai-judge)
- [How well it works](#how-well-it-works)

## Quick start

**On GitHub**, add `.github/workflows/assertrust.yml`:

```yaml
name: assertrust
on:
  pull_request:
  push:
    branches: [main]
permissions:
  contents: read
  pull-requests: write # for the summary comment
jobs:
  assertrust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0 # the whole history: it checks each commit
      - uses: BeyondFolder/assertrust@v0
```

**In a terminal**, install it (Linux and macOS, x64 and arm64):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/BeyondFolder/assertrust/releases/latest/download/assertrust-installer.sh | sh
```

Or download a build from [Releases](https://github.com/BeyondFolder/assertrust/releases), or build it with Rust 1.97.1 or newer: `cargo install --git https://github.com/BeyondFolder/assertrust --tag v0.2.0 assertrust`. Windows builds and npm come later.

Then, in your repo:

```sh
assertrust check                     # this branch, as a PR, against the default branch
assertrust check --worktree          # uncommitted changes
assertrust backtest --last 100       # what it would have flagged in the last 100 PRs and pushes
```

## What it catches

| Rule | What got weaker | Example | Confidence |
|---|---|---|---|
| `check-removed` | A check is gone from a test that's still there, and didn't move to another test | an `expect` line deleted | high; medium when the test was also rewritten |
| `matcher-loosened` | A weaker matcher | `toEqual({ id: 1 })` → `toBeTruthy()`, `toHaveBeenCalledWith(a)` → `toHaveBeenCalled()` | high when it ends at a presence check (`toBeTruthy`, `toBeDefined`, `toBeVisible`), else medium |
| `matcher-swapped` | A different matcher that isn't clearly weaker or stronger | `toBe(obj)` → `toEqual(obj)`, `toContain('x')` → `toMatch(/x/)` | low |
| `expectation-loosened` | The same matcher accepts more | fields dropped from `toMatchObject`, a word dropped from a `.not.toMatch` regex, `.each` rows dropped, `toThrow('msg')` → `toThrow()`, a wider bound | high; medium for numbers and substrings |
| `test-disabled` | A test no longer runs | `.skip`, `.todo`, `.skipIf(…)`, a committed `.only`, a test or test file deleted | high; medium when it skips only sometimes |
| `check-cant-fail` | A check is still there but may not run or fail | moved into a `try`/`catch`, an `if` or a callback, an early `return` before it, `await` dropped from `.resolves`, `expect(x)` with no matcher | high or medium; low inside a loop |
| `gate-loosened` | CI or test config lets more failures through | `\|\| true` or `continue-on-error` on a test step, the step removed, more `retries`, a lower coverage threshold, `passWithNoTests` | high; medium for a condition or a narrower run |
| `outcome-changed` | The expected result changed, neither looser nor stricter | `toBe(3)` → `toBe(4)` | low, shown only with `-v` |

A few things make the findings worth reading:

- **Checks in helpers count.** Checks inside helper functions, hooks and the methods of helper classes (page objects, `ctx.assertSyncResponse(…)`) count for every test that uses them, also when imported from another file in a test folder. A weakened `expectOk()` is caught even when no test file changed, and reported once, in the helper.
- **Rewrites aren't removals.** A check moved to another test, rewritten (`expect(r.text).toBe(v)` → `expect(r).toMatchObject({ text: v })`), split into several or folded into one isn't reported when it still states the same thing. Renamed tests are followed.
- **Only what lands counts.** On a PR, a check removed in one commit and put back in the next isn't reported, and neither is weakening of a test the PR itself added (it's how a new test gets fixed, and nothing that was on `main` got weaker). Both can be shown with a flag.
- **CI config is read too:** GitHub Actions workflows, `vitest`, `jest` and `playwright` configs, the test scripts in `package.json`, and `assertrust.toml` itself.

**Which files.** Tests are `*.test.*` and `*.spec.*` files (`.ts`, `.tsx`, `.js`, `.jsx`, `.mts`, `.cts`) and anything in `__tests__/`. Helpers are followed into code in `test/`, `tests/`, `__tests__/`, `__mocks__/` and `e2e/`. All of it can be changed in `assertrust.toml`.

**Not yet:** other languages, `assert`-style checks (`node:test`, chai), and helpers outside test folders.

## The GitHub Action

`BeyondFolder/assertrust@v0` downloads the release for the runner and checks the event that started the job:

- **A pull request** is checked as a whole (what it adds since it split off from its base), with each finding named after the commit that made it. Findings show up as annotations on the diff and as a table in the job summary.
- **A push** checks each commit pushed. A new branch is checked from where it split off from the default branch.
- **The PR comment:** once a PR has findings, the table is also posted as one comment, edited in place on every push, also to say "no findings" once they're fixed. A PR with no findings (or only allowed ones) gets no comment. A PR from a fork gets a read-only token, so its findings stay in the job summary.

It needs `fetch-depth: 0`, since it reads each commit, and runs on Linux and macOS runners.

| Input | Default | What it does |
|---|---|---|
| `fail-on` | `never`, or `fail-on` from `assertrust.toml` | fail the job on findings this sure or surer: `high`, `medium` or `low` |
| `comment` | `true` | post the summary as a PR comment once there are findings |
| `args` | | more arguments for `assertrust check`, like `--show-pr-added`, on one line or several. A `--range` here replaces the one from the event, which also makes it work on other events; a `--fail-on` here wins over the input |
| `judge` | `false` | `true` asks the [AI judge](#the-ai-judge) about each finding, with the key you set on the step (below). `[judge]` in `assertrust.toml` turns it on too |
| `judge-model` | `assertrust.toml`'s, or the built-in one | the judge's model, like `anthropic/claude-haiku-5-5` |
| `judge-effort` | `assertrust.toml`'s, else `medium` for the built-in model and the model's own for others | how hard the judge's model thinks: `low`, `medium` or `high` |
| `version` | the release this tag points to | the assertrust version to run, like `0.2.0` |
| `path` | | run this binary instead of downloading one |
| `github-token` | `github.token` | the token that posts the comment. A bot's token (this one, or a GitHub App's), since the comment is found again by its author |

The step's `findings` output is how many findings it listed, for later steps (`steps.<id>.outputs.findings`).

**With the judge**, pass your key from a secret. Its verdicts are kept between runs with `actions/cache`, so a change is judged once. A PR from a fork gets no secrets: its findings show without verdicts.

```yaml
      - uses: BeyondFolder/assertrust@v0
        with:
          judge: true
        env:
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
```

**To block merges**, set `fail-on: high` and make the job a required check. `@v0` always runs the newest 0.x release; pin `@v0.2.0` to stay on one. A branch or commit that isn't a release (`@main`) needs `version`, since it downloads the release its `Cargo.toml` names.

## Commands

### `assertrust check`

Checks a change and prints what got weaker.

| What to check | Flag |
|---|---|
| this branch as a PR: everything since it split off from the default branch (`origin/HEAD`, else `main` or `master`) | none |
| a PR-style range: what `HEAD` adds since it split off from `BASE` | `--range BASE...HEAD` |
| each commit on its own | `--range BASE..HEAD` |
| staged changes, for a pre-commit hook | `--staged` |
| uncommitted changes, including new files, for a coding agent | `--worktree` |

A PR-style check also checks each commit, to name the one that weakened a test and to hide weakening that a later commit undid. Merge commits are skipped: the commits they bring in are checked one by one.

| Flag | What it does |
|---|---|
| `--format terminal\|json\|github` | the output format (below) |
| `--fail-on high\|medium\|low\|never` | exit with 1 when a finding is this sure or surer |
| `--show-transient` | also show weakening undone later in the same PR |
| `--show-pr-added` | also show weakening of tests the PR itself added |
| `-v`, `--verbose` | add the test name, the code before and after, the full commit message, and changed outcomes |
| `--judge` | ask the [AI judge](#the-ai-judge) about each finding (sends the code to the model's provider, with your key) |
| `--judge-model MODEL` | the judge's model, like `anthropic/claude-haiku-5-5` (turns it on) |
| `--judge-effort low\|medium\|high` | how hard the judge's model thinks (default: `[judge] effort`, else `medium` for the built-in model and the model's own for others) |
| `--judge-dry-run` | print what the judge would be sent, and send nothing |

### `assertrust backtest`

Replays a branch's history the way CI would have checked it: each merged PR as a PR, each direct push on its own. Run it before turning assertrust on, to see what it would have said.

```
push  71110c0  Pipeline fix.
test/unit/mx-checker.test.ts:290  HIGH  expectation-loosened
  .not.toMatch no longer rules out resend · 71110c0

181 commits: 90 PRs, 10 other merges, 81 pushes · 1 finding · …
```

- `--last N` replays the last N commits on the branch's first-parent line (default 100); `--range BASE..HEAD` picks them.
- A squash-merged PR is checked against its own commits when they've been fetched (`git fetch origin 'refs/pull/*/head:refs/pull/*/head'`), else as one commit.
- `--labels FILE` reads your verdict on each finding (`wrong`, `intended` or `unexplained`) and prints how often it's right; `--update-labels` adds new findings to the file. This is how assertrust itself is measured.
- `--judge` (and `--judge-model`, `--judge-effort`) sends every finding to the judge, changed outcomes and findings on code a PR added too, with no cap. With `--labels`, it prints how the verdicts fall on each label. This is how the judge is measured.
- `--format json`, `--show-transient`, `--show-pr-added` and `-v` work as in `check`.

### Debugging commands

- `assertrust changes` lists the commits and files a check would look at, with each file's kind (test, test support, gate, …). It takes the same flags as `check`.
- `assertrust tests FILE` shows the tests, checks and helpers it finds in a file, or a CI or test config's gate facts. `--rev main` reads the file as it was at a commit, `--staged` from the index.
- `assertrust pairs` shows which tests and checks before a change it paired with which after it, and how. Same flags as `check`.

## Output and exit codes

**Terminal** (the default): two lines per finding, the most severe first.

```
test/a.test.ts:1  HIGH  check-removed
  removed expect(res.body.ok).toBe(true) · 83d6b1c

1 finding · warn-only (exit 0)
```

`-v` adds the details:

```
test/a.test.ts:1  HIGH  check-removed
  removed expect(res.body.ok).toBe(true) · 83d6b1c
    test    refunds
    before  expect(res.body.ok).toBe(true)
    commit  Trim refunds
```

Hidden findings are counted on the last line, with the flag that shows them: `no findings · 2 changed outcomes hidden (-v shows them)`. Files it couldn't read (say, a syntax error) are listed as notes. With the judge on, each finding gets its verdict on a third line, those it calls fine are listed apart at the end, and a last line says what the judge did and cost.

**JSON** (`--format json`) has every finding, hidden ones too, each with its `status`: `shown`, `hidden`, `allowed`, `transient` (undone later) or `pr-added`, and its `judge` verdict when there is one. The format is versioned (`"schema": 1`) and stays stable within it.

**GitHub** (`--format github`) prints annotations for GitHub Actions and writes a table to the job summary. The Action uses it.

| Exit code | Meaning |
|---|---|
| `0` | no findings, or none at or above `fail-on` |
| `1` | a shown finding at or above `fail-on`, or one the judge calls `coverage-lost` or `suspicious` (allowed, hidden and PR-added ones never count) |
| `2` | assertrust couldn't run: not a git repo, a bad range, a bad `assertrust.toml` |

## Saying a change is deliberate

Sometimes weakening a test is right. Say why, and the finding is marked `ALLOWED` instead of counting:

- **A comment** in the test, hook or helper, or on the line above the change: `// assertrust-allow: check-removed preview is tested in MCP now`. In YAML or other config, `# assertrust-allow: gate-loosened <reason>` on or above the changed line. Several rules go comma-separated.
- **A commit trailer**, for the findings of that commit: `Assertrust-Allow: test-disabled flaky on CI, tracked in #123`. It works where a comment can't go, like a deleted test or `package.json`.

A reason is required: an allow without one is ignored, and the finding says so. A comment counts only in the change that adds it, so an old one can't wave through a new weakening of the same test.

## Configuration: `assertrust.toml`

Optional, at the repo root. Flags override it.

```toml
fail-on = "never"            # never | high | medium | low
show-transient = false       # weakening undone later in the same PR
show-pr-added = false        # weakening of tests the PR itself added

[files]                      # a kind given replaces its defaults
test         = ["**/*.{test,spec}.{ts,tsx,js,jsx,mts,cts}", "**/__tests__/**"]
test-support = ["test/**", "tests/**", "e2e/**"]
gate         = [".github/workflows/*.yml", "{vitest,jest,playwright}.config.*", "package.json"]

[rules]                      # off | on | judge
matcher-swapped = "on"
outcome-changed = "judge"    # judge: shown when the AI judge says it isn't fine, else with -v

[judge]                      # the AI judge: on when this table is here
model = "anthropic/claude-sonnet-5-5"       # provider/model
effort = "medium"            # low | medium | high: how hard the model thinks
fail-on = ["coverage-lost", "suspicious"]   # verdicts that fail the check, or "never"
max-findings = 20            # the most findings one check sends
```

**A change can't loosen its own check.** A range or PR check reads `assertrust.toml` as it was at the base, so lowering `fail-on`, turning a rule off or turning the judge down applies once it lands, and is itself reported as `gate-loosened`. A typo (`fail_on`) is an error, not silently ignored.

## Git hooks and coding agents

**Before each commit**, check what's staged. With [husky](https://typicode.github.io/husky/) (`.husky/pre-commit`):

```sh
assertrust check --staged --fail-on high
```

With [lefthook](https://lefthook.dev/) (`lefthook.yml`):

```yaml
pre-commit:
  commands:
    assertrust:
      run: assertrust check --staged --fail-on high
```

With [pre-commit](https://pre-commit.com/) (`.pre-commit-config.yaml`):

```yaml
repos:
  - repo: local
    hooks:
      - id: assertrust
        name: assertrust
        entry: assertrust check --staged --fail-on high
        language: system
        pass_filenames: false
```

**For a coding agent**, `assertrust check --worktree` checks the files on disk, so the agent hears "you dropped `status` from this assertion" right after the edit, while it can still fix the test or say why with an allow comment. A Claude Code plugin that runs it after each edit to a test is coming.

## The AI judge

The rules only read test code, so a test deleted along with the feature it tested looks just like a check quietly dropped to get CI green. The judge tells them apart. It reads each finding with the change around it and answers one question: **does the test still check what it promises?**

```
build/validate-exports.test.ts  HIGH  test-disabled
  file deleted: its test no longer runs · fcd351b
  judge: coverage lost: The validateExports logic was moved verbatim into a vite.config.ts build plugin that still runs in the build and throws on mismatch; the old unit test was deleted with its module and not replaced, though the behavior stays.
```

That's hono moving a build check into its Vite config and deleting the check's test without a replacement.

| Verdict | Means | Fails the check |
|---|---|---|
| `fine` | nothing the tests promised is checked less: a false alarm, or the behaviour changed on purpose and the test follows it | no, and it's listed apart, folded away (unless `fail-on` fails it anyway) |
| `coverage-lost` | behaviour that still exists isn't checked any more, even if the change says why | yes |
| `suspicious` | a check changed to make a test pass, and nothing explains why | yes |
| `unsure` | not enough to tell | no |

It's off by default, and runs with your own key:

```sh
export ANTHROPIC_API_KEY=sk-ant-…
assertrust check --judge
assertrust check --judge-model anthropic/claude-haiku-5-5   # another model
assertrust check --judge-dry-run                            # print what would be sent, send nothing
```

For every check, add `[judge]` to `assertrust.toml` ([Configuration](#configuration-assertrusttoml)), or `judge: true` to [the Action](#the-github-action).

**Models and providers.** Any model of these providers works, named `provider/model`, with the provider's usual key:

| Provider | Example | Key |
|---|---|---|
| Anthropic (the default) | `anthropic/claude-sonnet-5-5` | `ANTHROPIC_API_KEY` |
| OpenAI | `openai/gpt-5.2` | `OPENAI_API_KEY` |
| Google Gemini | `gemini/gemini-3.8-flash` | `GEMINI_API_KEY` |
| Ollama, local and free | `ollama/qwen3:8b` | none (`OLLAMA_API_BASE_URL` for a daemon elsewhere) |

Each one gets the answer's schema in its own API's structured-output format, and `ANTHROPIC_BASE_URL` or `OPENAI_BASE_URL` point it at another address. The default, Sonnet 5.5 at `medium` effort, was picked by measuring Claude models on the labelled backtests ("How well it works" below). Models of other providers work the same way, but haven't been measured. `--judge-dry-run` shows exactly what a model would be sent, and needs no key.

- **What it's asked about:** the shown findings, most sure first, up to `max-findings` (20), and changed outcomes, which stay hidden unless it says they aren't fine. Findings that can't fail the check (allowed, undone later, on code the PR added) aren't sent.
- **What it's sent:** the finding; each test it's about, before and after; diffs of the files the change touched, cut to size; the commit messages; in GitHub Actions, the PR's title and description. `--judge-dry-run` prints exactly that.
- **Cost:** about 2 cents a finding with the default (a tenth of a cent on Haiku 5.5, 4 cents on Opus 5.5), and a run sends at most `max-findings` (20). The judge's line after the summary says what a run cost.
- **Same change, same verdict:** verdicts are cached in the repo's git directory (`.git/assertrust/judge/`), where no commit can add one of its own, so a change is judged once, CI never flips, and a cached verdict needs no key.
- **It never gets in the way:** with no key, no network or a bad answer, findings show without verdicts, plus a note. A judge that didn't answer never fails the check.

**Coming next:** a Claude Code plugin that runs assertrust after the agent edits a test and hands the findings straight back to it.

## How well it works

assertrust is measured on real history: the last 100 PRs and pushes of 10 public repos (cal.com, Excalidraw, h3, hono, immich, nuxt/ui, TanStack Query, tRPC, VueUse, zod) plus a private app's 181, replayed with `assertrust backtest` and every finding labelled by hand. Of the 194 findings a PR check would show, 50% are right; at high confidence, 70% (68 of 97).

Most wrong ones are tests rewritten for a deliberate change, which only the commit message explains. That's what the judge is for. Until precision is higher, `fail-on` defaults to `never`.

**The judge** is measured on the same labels, plus the 341 changed outcomes a check hides, of which 338 are legitimate and 3 aren't (719 findings in all, `backtest --labels --judge`). With the default, Sonnet 5.5 at `medium` effort, on the findings a PR check shows:

- of the 97 false alarms, it folds away 80 as fine, and fails the check on 5 (5%);
- of the 35 weakenings nothing explains, it catches 23 (66%);
- of the 41 findings it fails the check on, 36 are right (88%);
- of the 338 legitimate changed outcomes it brings up and fails 3 (1%), and it catches both real ones.

Other models, on the same findings (measured with the evidence of an earlier draft, so the numbers differ a little):

| Model | False alarms it fails | Weakenings it catches | Of its failures, right | Cost per finding |
|---|---|---|---|---|
| Sonnet 5.5, `medium` effort (the default, earlier evidence) | 9% | 74% | 81% | $0.021 |
| Sonnet 5.5, `high` effort | 5% | 37% | 81% | $0.021 |
| Opus 5.5, `medium` effort | 0% | 49% | 100% | $0.043 |
| Haiku 5.5, `high` effort | 2% | 31% | 94% | $0.0013 |
| Haiku 5.5, `medium` effort | 3% | 29% | 88% | $0.001 |

Opus never failed a false alarm. Haiku is nearly free and almost as careful, but catches less than a third. Sonnet at `medium` catches the most. Every model folds away 74–86% of the false alarms. Most of the weakenings no model catches are test files deleted in one big test migration, where the judge can't always tell whether a test moved. The labels are in [`backtest/`](backtest), and the method in [docs/architecture.md](docs/architecture.md#backtest-what-it-would-have-flagged-and-how-often-its-right).

## Roadmap

- [x] v0.1: CLI and GitHub Action for TS/JS
- [x] v0.2: AI judge
- [ ] npm package, crates.io, Claude Code plugin
- [ ] Windows builds
- [ ] More languages

## Contributing

The design is in [docs/architecture.md](docs/architecture.md), and building and testing in [docs/development.md](docs/development.md). Changes land as small PRs.

## License

[Apache-2.0](LICENSE)
