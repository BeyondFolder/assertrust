# assertrust architecture

> Status: **pre-alpha.** This is the design being built in small public PRs. Nothing runs yet. When the code changes the design, the same PR updates this doc.

assertrust checks that a change didn't quietly make the test suite weaker. Examples: an assertion removed, a matcher loosened, a test skipped, a CI gate relaxed. It reports what got weaker, where, and which commit did it.

## Scope

- **Module 1 only:** detecting assertion weakening. The other modules (dead-code detection, port parity, ticket intent, explore mode) come later.
- **Rust core.** Anything built on Playwright's TypeScript ecosystem later stays in TypeScript and calls the Rust binary.
- **Open source** (Apache-2.0).
- **Deterministic first.** An LLM judge is optional and used only for ambiguous cases (step 7).
- **The input layer is generic.** It knows nothing about tests or rules, so other tools can reuse it.

## Pipeline

```
trigger (PR / push / staged / worktree)
   ▼
1. input        → comparisons: before/after file pairs, plus the commits that produced them
2. parsing      → tests and assertions found in each file
3. test IR      → a language-neutral model of tests and assertions
4. matching     → pair each test before and after the change
5. rules        → deterministic findings with confidence
6. output       → terminal, JSON, SARIF, GitHub annotations, exit code
7. judge        → optional LLM check of ambiguous findings against stated intent
8. packaging    → CLI, GitHub Action, npm, Claude Code hook
```

## 1. Input

**Job:** turn a trigger into the before/after comparisons to analyze, and the commits that produced them.

### Why two levels: what lands, and which commit did it

Comparing only the PR's final diff misses attribution. Comparing only commit by commit reports problems that a later commit already fixed. Mailyond's history has one of each:

- `10d8742` dropped `status: 'pending'` from a `toMatchObject` on the `forward_email` branch, and `f8e4323` restored it two commits later. Only a per-commit check sees it, but by the end of the PR it was already fixed.
- `71110c0` ("Pipeline fix.") loosened a regex in a test and went straight to `main` with no PR. Only a per-commit check on push catches it.

So:

- **What gets reported is what lands.** In PR mode we compare the merge-base with the PR head. Only weakening still present at the head is reported.
- **Each finding names the commit that caused it.** For each surviving finding, we walk the PR's commits to find the one that introduced it, and attach that commit's message. Step 7 reads the message as the stated intent.
- **Transient findings are hidden by default.** These were weakened and then restored within the same PR. `--show-transient` shows them, which is useful for the benchmark.

### Modes

| Trigger | Mode | Compares |
|---|---|---|
| `pull_request` | PR | merge-base(base, head) → head, plus finding the commit for each finding |
| `push` with no PR | per commit | each pushed commit's first parent → the commit |
| pre-commit hook | staged | `HEAD` → git index |
| Claude Code hook | worktree | `HEAD` → files on disk |
| backtest | both | PR mode for each merge commit, per-commit for direct pushes to `main` |

On the command line, these map to:

| Flag | Mode |
|---|---|
| `--range BASE..HEAD` | per commit, every commit `HEAD` adds |
| `--range BASE...HEAD` | PR-style: merge-base → `HEAD`, plus every commit (the same meaning as `git diff A...B`) |
| no flag | PR-style, from the default branch (what `origin/HEAD` points to, else `main` or `master`) to `HEAD` |
| `--staged`, `--worktree` | staged, worktree (worktree compares with the files on disk, including untracked files; a file moved without `git add` still counts as a rename) |
| `--pr N` | becomes `base...head`; built with the GitHub Action (step 8) |

`assertrust changes` (same flags) prints what a check would look at: the commits, plus each changed file with its kind. It checks nothing, and it's for debugging the input layer.

- **Why the merge-base and not the base branch's latest commit:** if `main` moved on after the branch was cut, comparing against its latest commit would count `main`'s newer changes as part of the PR. GitHub's PR diff works the same way.
- **Merge commits are skipped** when walking commits. The commits a merge brings in are walked one by one instead, and checking the merge too would report everything twice. When `main` is merged into a branch halfway through, `main`'s commits aren't counted as part of the PR either.
  - **Known gap:** what a merge commit changes on its own (a conflict resolution, or an edit slipped into the merge) is covered by PR-style mode, because the net comparison includes it. Per-commit mode (`A..B`) doesn't see it yet. The fix would be `git show --remerge-diff`, which shows only what the merge changed compared with git's automatic merge. Until then, run PR-style on branches.
- **Renames are followed** (`git diff -M`), so a moved test file isn't reported as one test deleted and a new one added. A renamed file gets the stronger kind of its two paths (see the order below): `a.test.ts` → `a.test.ts.disabled` is still a test, one that just left the suite.

### Which files

Every changed path is recorded and labelled. Paths are relative to the repo root, wherever assertrust is run from. The kinds are checked in this order and the first match wins: Test, Gate, Test support, Source, Other.

| Kind | Default patterns (configurable) | Contents loaded |
|---|---|---|
| Test | `**/*.{test,spec}.{ts,tsx,js,jsx,mts,cts}`, `**/__tests__/**` | yes |
| Test support | code files in `test/`, `tests/`, `__tests__/`, `__mocks__/` or `e2e/`. Files elsewhere that a test imports through a relative path get added when helpers are linked (step 2). | yes |
| Gate | `.github/workflows/*.yml`, `{vitest,jest,playwright}.config.*`, `package.json` | yes |
| Source | other code files | only when asked (step 7) |
| Other | everything else | no |

Contents are read lazily, so the cost stays proportional to the test and gate files that changed. (Reading contents arrives with parsing, step 2.)

### Data handed to step 2

```rust
struct Commit     { sha: Sha, parents: Vec<Sha>, author: String, message: String }
enum   FileKind   { Test, TestSupport, Gate, Source, Other }
struct FileChange { kind: FileKind, path_before: Option<PathBuf>, path_after: Option<PathBuf> }
                  // None = added / deleted; different paths = renamed; contents fetched on demand
struct Comparison { before: Rev, after: Rev, files: Vec<FileChange> }
struct Analysis   { net: Comparison,                 // what lands
                    steps: Vec<(Commit, Comparison)> // per commit, used to find which commit caused a finding
                  }
```

_Sketch only. Exact types are settled in the implementation PR._

### Implementation notes

- **Shell out to `git`** (`log`, `merge-base`, `diff-tree -M`, `show rev:path`) rather than using the `gix` library. Git is always present in CI, and it's easier to read and debug. We can switch later if speed ever matters.
- **Read-only and safe next to an agent:** git runs with `GIT_OPTIONAL_LOCKS=0`, so it never takes the index lock while an editor or coding agent is using the repo. The worktree mode needs one write: untracked files are added as "intent to add" to a throwaway copy of the index in a temp dir, so a single `git diff` can compare with the disk. The real index is never touched.
- **No option injection:** user-supplied revisions are resolved to SHAs behind `--end-of-options`, so a "revision" like `--output=file` is rejected instead of being passed to git as an option.
- **GitHub Action gotcha:** `actions/checkout` fetches one commit by default. The Action needs `fetch-depth: 0`, or there's no history to walk. When the merge-base is missing in a shallow clone, assertrust says so and suggests the fix.
- **Recommended Action triggers:** `pull_request`, plus `push` to `main` only. Otherwise a push to a branch with an open PR is checked twice.

### Tests

Integration tests build throwaway git repos in a temp dir with real commits, branches and merges, then run the `assertrust` binary and check its output. There's at least one fixture per mode, plus the two Mailyond patterns:
- weakened, then restored within a PR → transient
- loosened directly on `main` → reported

## 2. Parsing

**Job:** read each before/after file and extract its tests, assertions and assertion helpers. This step only extracts. Judging happens in step 5.

**What Mailyond's tests use** (30 files, vitest plus one Playwright spec):
- **Test functions:** `it` ×363, `describe` ×68, `it.each` ×21.
- **Matchers:** `toBe` 386, `toEqual` 200, `toMatchObject` 119, `toContain` 75, then a long tail.
- **Other patterns:** `.not` ×25 and about 10 nested matchers such as `expect.objectContaining`.

The `expect(…).toX(…)` style covers essentially all of it.

### Parser

- **oxc**, with the language picked from the file extension.
- oxc frees its syntax tree after each file, so we copy what we need straight into our own structs. Nothing after step 2 depends on oxc, which means adding another language later (e.g. Python via tree-sitter) is just a new parser.
- **A file that doesn't parse on either side is skipped with a note.** Work-in-progress commits often don't parse. oxc can partly recover a broken file, but guessing from half a file causes false alarms.

### Tests

- Calls to `describe`, `it`, `test` and `suite`, plus their variants: `.skip .only .todo .each .concurrent .fails .fixme .serial .skipIf(…) .runIf(…)`. The last two are conditional skips.
- **Recognized by name, not by import.** Playwright projects usually import `test` from their own fixtures file (`base.extend(…)`), so checking imports would miss them.
- **Recorded for each test:**
  - the path of `describe` names
  - the name: string literals as-is, template literals with `${…}` kept as text, and the format string for `.each`
  - the variants
  - the line
  - the body text, which step 4 uses for matching

### Assertions

- **Shape:** `expect(subject)` (optionally `expect.soft` or `expect.poll`), then optionally `.not` / `.resolves` / `.rejects`, then `.toX(args)`.
- **Recorded for each one:**
  - the subject's source text
  - negation (`.not`)
  - the async modifiers
  - the matcher name
  - the arguments **as structure**: object keys, array items, regex alternatives, literals, and nested matchers such as `expect.objectContaining(…)`
  - where it sits: in a test, a hook or top level, and whether it's inside an `if`, a loop, a `try`/`catch` or a callback
- **Also recorded:**
  - `expect.assertions(n)` and `expect.hasAssertions()`
  - a bare `expect(x)` with no matcher, which always passes
- **Custom matchers** (`expect.extend`) are tracked if removed, but never judged weaker or stronger.

### Helper functions

Helpers can change what a test checks without the test file changing, so they're followed.

- **Asserting helper:** a function that contains an assertion, either directly or by calling another asserting helper. Call cycles are guarded.
- **Where helpers are found:** in the same file, and in files imported through a relative path (`./utils`, `../helpers/api`). Imports from packages aren't followed. tsconfig path aliases come later.
- **A call to an asserting helper inside a test counts as that helper's assertions,** marked "via `helperName()`". If the test stops calling it, those assertions count as removed.
- **A change inside a helper is checked like a change to a test.** The finding lists every test that calls the helper.
- **Helpers that don't assert still shape what a test does:** setup, seeding and request wrappers, like everything in Mailyond's `test/utils.ts`. The swallowed-error and early-return checks run on them too. Subtler changes go to the LLM judge (step 7), such as a seed helper's default switching from `'connected'` to `'pending'`.
- This is why step 1 has a **Test support** file kind.

### Not in v0.1

- chai style (`expect(x).to.equal(y)`) and `node:assert`
- `.snap` snapshot files (inline snapshots are covered as arguments)
- helpers imported from packages

### Tests

- Small fixture `.ts` files, one per pattern. The extracted structure is compared against expected output stored as reviewable snapshot files, using the `insta` crate.
- All of Mailyond's test files, including `test/utils.ts` and `test/setup.ts`, must parse cleanly.

## 3. Test IR

**In one sentence:** each test becomes a list of checks, written in a form that ignores formatting, so a before/after comparison only sees changes in what is being checked.

**Example (Mailyond `10d8742`):**

```
test "emails a confirmation … and only forwards once confirmed"
  before:  check res.body  contains  { email: 'me@gmail.test', status: 'pending', domainIds: null }
  after:   check res.body  contains  { email: 'me@gmail.test' }
  → same test, same check, 2 fields fewer → weaker (dropped: status, domainIds)
```

### Two layers

1. **File IR:** one per file per revision, built from that file's content alone. It's cached by git blob hash, so unchanged files are parsed once during a backtest.
2. **Linked view:** for each test, its own assertions plus the ones that run on its behalf, from helpers (followed across relative imports) and from `beforeEach`/`afterEach` hooks. Each one is tagged with a `via` chain, e.g. `via: ["expectOk"]`.

### What gets recorded

- **Per file:** imports, suites (`describe` blocks and their modifiers), tests, hooks, helpers, and custom matchers (`expect.extend`).
- **Per test:** its id (`describe` path + name), its effective modifiers (`describe.skip` is inherited), its `.each` table as a value, and a body.
- **Per body** (shared by tests, helpers and hooks):
  - assertions
  - calls (resolved to helpers in the linked view)
  - early exits (`return`, possibly inside an `if`)
  - runtime skips (`test.skip(cond)`, `ctx.skip()`)
  - `expect.assertions(n)` / `hasAssertions()`
  - a normalized fingerprint for step 4
- **Per assertion:**
  - the subject (normalized source)
  - `negated` (`.not`)
  - modifier: resolves, rejects, soft or poll
  - the matcher: its raw name plus its kind
  - the arguments as values
  - context: inside an if, loop, try, catch, callback or short-circuit
  - a span, used for reporting and never compared
- **Values:** null, undefined, bool, number, string, template, regex (with its top-level alternatives split by oxc's regex parser), object (keys compared as a set), array, nested matcher (`expect.objectContaining(…)`), or opaque (any other expression, as normalized source).

### Matcher kinds

| Kind | Examples |
|---|---|
| Equal (identity / deep / deep-strict) | `toBe`, `toEqual`, `toStrictEqual` |
| Subset | `toMatchObject`, `objectContaining` |
| Contains | `toContain`, `toContainEqual`, `arrayContaining` |
| Pattern | `toMatch`, `stringMatching` |
| Truthiness | `toBeTruthy`, `toBeDefined`, `toBeNull`, … |
| Type, Compare, Size | `toBeInstanceOf`, `toBeGreaterThan`, `toHaveLength`, `toHaveProperty` |
| Throws, Called, Snapshot | `toThrow`, `toHaveBeenCalledWith`, `toMatchSnapshot`, `toHaveScreenshot` |
| UiState | Playwright's `toBeVisible`, `toHaveText`, … |
| Custom | from `expect.extend`, or unknown |

The IR only labels the kind. Which kind is stricter than which is decided by the rules (step 5). This keeps the rules language-neutral: a pytest `assert a == b` or a Rust `assert_eq!` would also map to Equal.

### Defaults

- **Formatting never counts:** whitespace, quotes, trailing commas, key order and line moves all produce the same IR.
- **Simple constants are filled in:** a `const` holding a literal value in the same test or file is inlined (13 Mailyond assertions compare to a variable). Anything else stays opaque.
- **`.each` tables are tracked:** removing a row is weakening, even when no assertion changes.

### Gate facts

CI and test config files become a list of keyed facts:

- **Workflow steps that run tests:** identified by the command they run, not their position. For each one we record `continue-on-error`, `if:`, and commands that swallow failures (`|| true`, `; exit 0`).
- **Test config:** `retries`, `passWithNoTests`, `bail`, coverage thresholds, and `include` / `exclude` / `testIgnore`. Config `.ts` files are read with oxc, literal values only.
- **package.json:** the `test*` scripts.

### Debug command

`assertrust ir <file> [--rev <rev>]` prints a file's IR as JSON. The snapshot tests store this same output.

### Mailyond is private

Mailyond is a private codebase, so its code is never committed here. This doc quotes only a few short snippets as examples. The backtest runs against a local clone (path set by an env var) and publishes only counts and findings. Test fixtures are written from scratch.

## 4. Matching

**In one sentence:** before comparing checks, work out which test after the change is the same test as before it, and which check inside it is the same check, even when the test was renamed, moved or refactored.

**Example (Mailyond `038cf0b`, "Keep stored text bodies as they were; derive HTML text only in MCP"):**

```
before: it('never lets markup or script content into the text or preview')
          check fields.textBody  equals 'Hi scriptbad() <3'
          check fields.preview   equals 'Hi scriptbad() <3'
after:  it('never lets markup or script content into the text')
          check textFromHtmlBody(…) equals 'Hi scriptbad() <3'
```

- The name changed, so pairing by name fails.
- But the names share most of their words, and both bodies use the same long HTML input, so the two tests are paired.
- Inside the pair, `textBody` and the new check expect the same value, so they're paired even though the subject changed with the refactor.
- The `preview` check has no partner, so it's reported as **removed**. The note says the title also dropped "or preview", which suggests the removal was deliberate.

### Pairing tests, in order

1. **Same `describe` path and name** → paired. This covers most tests. Duplicate names in a file are told apart by their order.
2. **Leftovers in the same file** → paired by a similarity score. The score combines:
   - shared words in the name
   - shared literal values (strings, numbers, regexes)
   - shared expected values in checks
   - the normalized body fingerprint

   A pair needs a high score that's clearly better than any alternative.
3. **Leftovers across files in the same change** → the same two passes, to catch tests moved to another file.
4. **Still unpaired:**
   - old tests count as deleted
   - new tests count as added, which is never a problem

### Pairing checks inside a paired test, in order

1. Identical checks → unchanged.
2. Same subject and same matcher kind → paired, then their values are compared (`10d8742`: same `res.body`, same matcher, fewer fields).
3. Same expected value and same matcher kind → paired, which handles subjects renamed by a refactor (`038cf0b`).
4. Leftover old checks → removed. Leftover new checks → added.

### Moves and splits aren't removals

A deleted test or a removed check is reported only if it doesn't reappear anywhere else in the same change. Splitting one test into two, or moving tests between files, produces no findings.

### Defaults

- **When unsure, don't pair.** Missing a weakening is better than a false alarm, because a noisy tool gets turned off.
- **Renames are information, not findings.** They appear in the output. When a title drops words in the same change that a check disappears, that's noted for the reviewer and the LLM judge (step 7).
- **Similarity thresholds are tuned on the Mailyond backtest,** not guessed. Each pairing pass is visible in debug output, so a wrong pairing can be traced.

### Tests

Fixtures cover:
- an unchanged name
- a rename with the same body
- a rename plus a refactor (the `038cf0b` shape)
- a test moved to another describe block
- a test moved to another file
- one test split into two
- two tests with near-identical bodies (must not be mis-paired)
- duplicate names in the same file

## 5. Rules

**In one sentence:** each rule looks at one paired test (or one gate file) before and after the change and says whether it got weaker, how, and how confident it is.

**Example (Mailyond `71110c0`, "Pipeline fix."):**

```
before: expect(source).not.toMatch(/support@|support\.your|resend|inboxes\.net|…/)
after:  expect(source).not.toMatch(/support@|support\.your|inboxes\.net|…/)
→ expectation-loosened (high): the page may now mention "resend"
```

`.not.toMatch` means "must not contain any of these". Taking a word off that list means the test now allows it, so the test is weaker. Without `.not`, removing an alternative would make it stricter. That's why polarity is part of the IR (step 3).

### The rules

| Rule id | What it catches | Examples | Default confidence |
|---|---|---|---|
| `check-removed` | A check disappeared from a test that still exists, and didn't reappear elsewhere in the change | an `expect` line deleted; a test left with zero checks | high |
| `matcher-loosened` | A matcher moved down the strength ladder (below) | `toEqual({ id: 1 })` → `toBeTruthy()`; `toHaveBeenCalledWith(a)` → `toHaveBeenCalled()` | high when it ends at the bottom level, else medium |
| `matcher-swapped` | A matcher swapped for one at the same level, or any matcher change under `.not` | `toBe(obj)` → `toEqual(obj)` (identity becomes deep equality); `toContain('x')` → `toMatch(/x/)`; `.not.toEqual(a)` → `.not.toBe(a)` | low, shown by default |
| `expectation-loosened` | The same matcher accepts more than before | object fields dropped (`10d8742`); an alternative dropped under `.not` (`71110c0`); array items or `.each` rows dropped; `toThrow('msg')` → `toThrow()`; a wider numeric bound or `toBeCloseTo` precision | high for fields, rows and alternatives; medium for numbers |
| `test-disabled` | A test no longer runs, or no longer runs everywhere | `.skip`, `.todo`, `.fails`, `.skipIf(…)`, a runtime `test.skip(cond)`; `.only` committed (silences the rest of the file); a test deleted and not moved | high |
| `check-cant-fail` | A check is still there but may no longer run or fail | moved into `try/catch`, an `if` or an unawaited callback; an early `return` added before it; `await` dropped from `.resolves`/`.rejects`; a bare `expect(x)` with no matcher; `expect.assertions(n)` removed or `n` lowered | high for the bare `expect` and dropped `await`, else medium |
| `gate-loosened` | CI or test config lets more failures through | `\|\| true`, `continue-on-error: true`, `if: false` on a test step; the test step removed; `retries` raised; coverage threshold lowered; a pattern added to `exclude`/`testIgnore`; `passWithNoTests` added | high |
| `outcome-changed` | The expected outcome changed (not looser, just different) in the same commit as the code it tests | `toBe(3)` → `toBe(4)`; `.not` added or removed | low; hidden unless the LLM judge is on |

### Matcher strength ladder (positive checks)

1. **Exact:** Equal (deep-strict ≥ deep ≥ identity), Snapshot, Throws with a specific message or class
2. **Structured:** Subset, CalledWith, exact Size (`toHaveLength`)
3. **Partial:** Contains, Pattern, Compare (`toBeGreaterThan`), Throws (any), CalledTimes
4. **Type:** `toBeInstanceOf`, `expect.any`
5. **Presence:** Truthiness, Defined, Called at all

- Moves down the ladder → `matcher-loosened`.
- Sideways swaps (same level) → `matcher-swapped`, at low confidence. One exception: swaps that behave identically aren't flagged, e.g. `toBe(5)` ↔ `toEqual(5)` with a primitive literal.
- Moves up the ladder are stricter and aren't flagged.
- Matcher changes under `.not` aren't ranked in v0.1. They're reported as `matcher-swapped`.

### Which direction is looser

| Matcher kind | Positive check: looser when… | Under `.not`: looser when… |
|---|---|---|
| Subset (`toMatchObject`) | keys are removed | keys are added |
| Pattern (`toMatch`) | alternatives are added | alternatives are removed |
| Contains | fewer items are required | more items are listed |
| Equal | n/a (a different value is `outcome-changed`) | n/a |

### Shape

- Each rule is one file implementing a `Rule` trait. It takes the paired test (or gate facts) and returns findings.
- A finding has:
  - the rule id and confidence
  - the file, line, test and `via` chain
  - a one-line message, plus the before and after snippets
  - the commit that caused it, filled in later by step 1's attribution
- All v0.1 rules are deterministic, so no AI or API key is needed to run them.
- Rules can be turned off one at a time in config (step 6).

### Defaults

- **Sideways matcher swaps are flagged too,** at low confidence. Changes that behave identically are not.
- **`outcome-changed` is hidden unless the judge is on.** Legitimate behaviour changes do this constantly, so on its own it would be mostly noise. It's still recorded in the backtest report for tuning.
- **Confidence levels are a first guess** and get adjusted using the Mailyond backtest.

### Tests

There's at least one positive fixture (must flag) and one negative fixture (must not flag) per rule, plus:
- both polarity directions for Pattern and Subset
- reformat-only changes, which must produce no findings
- the three Mailyond shapes: `10d8742` (fields dropped), `71110c0` (an alternative dropped under `.not`) and `038cf0b` (a check removed in a renamed, refactored test)

## 6. Output

**In one sentence:** show each finding where the person or agent will see it, in the terminal, on the PR, or as JSON for tools, and never block anything unless the repo opts in.

### Terminal (default): short, two lines per finding

```
test/api/forwarding.test.ts:27  HIGH  expectation-loosened
  toMatchObject no longer checks status, domainIds · 10d8742

1 finding · warn-only (exit 0)
```

- `-v` adds the test name, the before/after snippet and the full commit message.
- Findings are sorted most severe first. Allowed findings come last, marked `ALLOWED` with their reason. Transient findings appear only with `--show-transient`.
- **On the PR**, an inline annotation already sits on the line, so it says only the finding itself: title `assertrust: expectation-loosened (high)`, message `toMatchObject no longer checks status, domainIds (10d8742)`. The summary comment is a short table: file:line, rule, message.

Messages are short by design: one line that says what got weaker. Detail goes behind `-v`, never in the default view.

### Formats (`--format`)

| Format | For | v0.1 |
|---|---|---|
| `terminal` | people, Claude Code hook output | yes |
| `json` | the backtest, the benchmark, other tools. Versioned (`"schema": 1`) | yes |
| `github` | inline annotations on the PR diff (`::warning file=…,line=…::`), plus one summary comment per PR, edited in place on every push instead of re-posted | yes |
| `sarif` | GitHub code scanning (the Security tab) | later |

### Exit codes

- `0`: no findings, or findings below `fail-on`. The default is `fail-on = "never"` (warn-only).
- `1`: at least one finding at or above `fail-on`, or, with the judge on, a verdict listed in `[judge] fail-on` (step 7).
- `2`: assertrust itself failed (no git, bad config, bad range). This is never confused with "found something".

**Blocking.** Deterministic findings warn by default. Set `fail-on` to block. This default may change once the backtests have proven precision.

### Acknowledging a deliberate change

Sometimes weakening a test is the right call. There are two ways to say so, and both need a reason:

- **In the test:** `// assertrust-allow: check-removed preview is tested in MCP now`, anywhere inside the test or on the line above it. This works for changes to checks that survive or were removed from a test that still exists.
- **In the commit message:** a trailer, `Assertrust-Allow: test-disabled flaky on CI, tracked in #123`. This works when there's no test left to put a comment in, e.g. a deleted test.

Allowed findings still appear, marked `ALLOWED`, so a reviewer sees what was waived and why. An allow with no reason is itself a finding.

### Config: `assertrust.toml` (optional)

```toml
fail-on = "never"            # never | high | medium | low
show-transient = false

[files]                      # defaults from step 1; override only if needed
test         = ["**/*.{test,spec}.{ts,tsx,js,jsx,mts,cts}", "**/__tests__/**"]
test-support = ["test/**", "tests/**", "e2e/**"]
gate         = [".github/workflows/*.yml", "{vitest,jest,playwright}.config.*", "package.json"]

[rules]                      # every rule is on by default except outcome-changed
outcome-changed = "judge"    # off | on | judge
matcher-swapped = "on"
```

assertrust works with no config file. CLI flags override the file.

### Tests

- Snapshot tests of the terminal and JSON output for a fixed fixture repo.
- Exit-code tests for each `fail-on` level.
- Allow comment and commit trailer, each with and without a reason.
- The GitHub format parsed back to check annotation syntax and that the summary comment is edited in place (its marker comment is found).

## 7. LLM judge (planned after v0.1)

**In one sentence:** an optional AI pass that reads a finding together with the stated intent and answers one question: does the test still check what it promises?

The question is about coverage, not intent. `10d8742` was intentional (the API response was trimmed), but it still lost coverage: nothing checked that a new forwarding address starts pending until `f8e4323` added that back.

### Example verdicts

| Finding | Verdict | Reason (one line) |
|---|---|---|
| `10d8742`: dropped `status`, `domainIds` | **coverage lost** | The title still promises "only forwards once confirmed", but nothing checks that a new address starts pending. |
| `038cf0b`: `preview` check removed | **fine** | The commit moves HTML-to-text out of storage, and the title was updated to match. |
| hypothetical: `toBe(3)` → `toBe(4)`, commit "fix failing test", no related source change | **suspicious** | The expected value was changed to match new output, and nothing in the change says the behaviour should change. |

Verdicts: `fine`, `coverage-lost`, `suspicious`, `unsure`, each with a one-line reason.

### When it runs

- **Opt-in only:** `--judge` or `[judge]` in config, with the user's own API key. Off by default because it sends code to a model provider.
- **On which findings:** `outcome-changed` (hidden unless the judge is on), low- and medium-confidence findings, and anything with a rename note. High-confidence findings are judged too when the judge is on, but the verdict is only added as a note.
- **What the judge can change:**
  - It adds a verdict and a reason to findings.
  - It decides whether an `outcome-changed` finding is shown. `fine` stays hidden, anything else is shown.
  - It never deletes a deterministic finding.
- **Judge verdicts block by default.** When the judge is on, a finding it marks `suspicious` or `coverage-lost` fails the run (exit 1). This is configurable: `[judge] fail-on = ["suspicious", "coverage-lost"]` by default, or `"never"`. So deterministic findings warn, and judge-confirmed findings block. Allowed findings (`assertrust-allow`) never block.
- **Failure is soft.** No key, a network error or rate limits mean findings are shown without verdicts, plus one note. A judge that didn't run never blocks and never exits 2.

### What it sees

- the test before and after, plus the helpers involved
- the commit message(s) that caused the finding (from step 1's attribution)
- the PR title and body, in PR mode
- the diff of product code the test imports, with a size cap; anything cut is marked as cut, never silently dropped
- the test's title, which acts as its promise

`--judge-dry-run` prints exactly what would be sent, without calling anything.

### Model and library

- **rig-core** behind our own `Judge` trait, as an optional cargo feature (`llm`), so the deterministic core has no LLM dependencies. Any provider works: Anthropic, OpenAI, Gemini, or Ollama for local and free.
- **Default model chosen by measurement, not guessed.** Run the Mailyond backtest findings through Claude Haiku 5.5, Sonnet 5.5 and Opus 5.5, compare against hand labels, and pick the cheapest model that matches the labels well. Rough cost per judged finding (~5K tokens in, short answer, some thinking), at Anthropic list prices cached 2026-10-06:
  - Haiku 5.5 (`claude-haiku-5-5`, $0.10/$0.50 per M tokens): about $0.001
  - Sonnet 5.5 (`claude-sonnet-5-5`, $2/$10): about $0.02
  - Opus 5.5 (`claude-opus-5-5`, $4/$20): about $0.04
  
  A whole Mailyond backtest costs a few dollars at most, even on Opus.
- **Structured answer:** the verdict comes back as JSON matching a schema. **Check in the judge PR:** if rig's structured-output helper forces a tool call, the newest Claude models (Opus 5.5, Sonnet 5.5) reject it with a 400. If so, use the API's structured outputs (`output_config.format`) or `tool_choice: auto` plus `strict: true`. As a last resort, a thin HTTP client behind the same `Judge` trait.
- **Repeatability comes from the cache, not sampling settings.** The current Claude API rejects `temperature` and similar parameters. Verdicts are cached by a hash of model, prompt version and inputs, in `.assertrust/cache/` locally and via `actions/cache` in CI, so the same change always gets the same verdict and CI never flips.
- **Cost guard:** a cap on judged findings per run (default 20). Token usage is printed at the end.

### Tests

- The `Judge` trait has a fake implementation that returns scripted verdicts. All pipeline tests use it, with no network.
- The real-model eval (hand-labelled Mailyond findings) runs manually, not in CI, and records agreement and cost per model.

## 8. Packaging

**In one sentence:** one Rust binary, delivered wherever the check should run: a terminal, CI, git hooks, and inside a coding agent's loop.

- **CLI framework:** `clap`, using its derive API.
- **Repo:** `github.com/BeyondFolder/assertrust`.
- **License:** Apache-2.0.

### Ways in

| Channel | What it is | When |
|---|---|---|
| **CLI binary** | Linux x64/arm64, macOS x64/arm64 and Windows x64 builds on GitHub Releases, plus shell and PowerShell installers, built by **cargo-dist** (v0.34, maintained) | v0.1 |
| `cargo install assertrust` | for Rust users | v0.1 |
| **GitHub Action** | `BeyondFolder/assertrust@v0`: downloads the matching release binary and runs it with `--format github` | v0.1 |
| **npm** | `npx assertrust` / `npm i -D assertrust`, generated by cargo-dist. Most target projects are JS, so this is the most natural install for them | before the public launch |
| **Claude Code plugin** | a hook that runs after the agent edits a test file and feeds findings straight back to the agent (below) | before the public launch |
| **pre-commit** | a documented snippet (`assertrust check --staged`) for husky, lefthook and pre-commit | docs only |

### GitHub Action usage

```yaml
on:
  pull_request:
  push:
    branches: [main]
permissions:
  contents: read
  pull-requests: write          # for the summary comment
jobs:
  assertrust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with: { fetch-depth: 0 }  # full history, required (step 1)
      - uses: BeyondFolder/assertrust@v0
        # with: { judge: true }
        # env:  { ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }} }
```

- **PR events** run PR mode: merge-base → head, with commit attribution.
- **Push events** run per-commit mode over `before..after`. A brand-new branch (`before` is all zeros) falls back to the default branch's merge-base.
- **PRs from forks** get a read-only token and no secrets. The summary comment then falls back to the job summary (`$GITHUB_STEP_SUMMARY`), and the judge is skipped (soft, step 7).
- The Action is a small composite action: a shell script, no Node runtime.

### Claude Code plugin

- A `PostToolUse` hook on `Edit|Write|MultiEdit` runs `assertrust check --worktree` when the edited file is a test, test-support or gate file.
- Findings go back to the agent through the hook's feedback channel, so it sees "you dropped `status` from this assertion" right after the edit. It can then fix the test or add an `assertrust-allow` with a reason.
- **Check in the plugin PR:** the exact hook feedback format (exit code vs JSON output), against the current Claude Code hooks docs.
- Other agents with hooks (Codex, Cursor, …) can call the same command. Docs for those come later.

### Releases

- semver, staying on `0.x` until the backtests are solid
- a `CHANGELOG.md`
- tagging `vX.Y.Z` triggers cargo-dist, and the Action's `v0` tag moves to the newest release
- assertrust runs on its own PRs (dogfooding)

### Tests

- The hook gets sample hook JSON on stdin, and the test checks the output and exit code.
- The Action runs in this repo's CI against a small fixture repo, for both a PR event and a push event.
- The release build is checked on each target by running `assertrust --version` on the built binary.
