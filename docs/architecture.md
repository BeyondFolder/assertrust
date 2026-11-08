# assertrust architecture

> Status: **pre-alpha.** This is the design being built in small public PRs. `assertrust check` runs every rule (step 5); output formats, config, the GitHub Action and the judge are being built. When the code changes the design, the same PR updates this doc.

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

A renamed file keeps the stronger of its two kinds, so `a.test.ts` → `a.test.ts.disabled` is still a Test file: a test leaving the suite, not a new "other" file. Its new name isn't a test file name, so the runner won't pick it up. Loading treats it as gone after the change, and step 5 reports it.

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

**Built so far:** tests, hooks and checks in one file (crate `assertrust-ir`), shown by `assertrust tests` (see "Debug command" in step 3). Helper functions and the linked view come after the rules.

**What Mailyond's tests use** (35 test and test-support files, vitest plus one Playwright spec):
- **Tests:** 390 `it`/`test` calls, 23 of them `it.each`, plus `describe` blocks.
- **Checks:** 939. Matchers: `toBe` 389, `toEqual` 211, `toMatchObject` 119, `toContain` 75, then a long tail.
- **Other patterns:** `.not` ×54 and about 10 nested matchers such as `expect.objectContaining`.

The `expect(…).toX(…)` style covers essentially all of it.

### Parser

- **oxc** (0.155), with the language picked from the file extension. Regex literals are parsed too, so their alternatives can be compared.
- oxc frees its syntax tree after each file, so we copy what we need straight into our own structs. Nothing after step 2 depends on oxc, which means adding another language later (e.g. Python via tree-sitter) is just a new parser.
- **A file that doesn't parse on either side is skipped with a note**, e.g. `skipped, doesn't parse (line 3: Unexpected token)`. Work-in-progress commits often don't parse. oxc can partly recover a broken file, but guessing from half a file causes false alarms.

### Tests

- Calls to `describe`, `it`, `test` and `suite` (plus jest's `xit`, `xtest`, `xdescribe`, `fit`, `fdescribe`), and their variants: `.skip .only .todo .each .for .concurrent .sequential .shuffle .fails .fail .fixme .serial .parallel .skipIf(…) .runIf(…)`. The last two are conditional skips. Variants chain (`it.concurrent.each(…)`), and Playwright's `test.describe(…)`, `test.beforeEach(…)` and `test.step(…)` hang off `test`.
- **Skips decided at run time:** Playwright's `test.skip(cond)` / `test.fixme(cond)` and vitest's `ctx.skip()`. Inside a test they're an item in its body. Directly in a `describe` or at the top of a file (Playwright), they apply to every test in it, including the ones above them, so those tests get a `skip(cond)` modifier. One inside a function outside tests is ignored for now: that function may never be called (helpers are linked later).
- **Recognized by name, not by import.** Playwright projects usually import `test` from their own fixtures file (`base.extend(…)`), so checking imports would miss them.
- **Recorded for each test:**
  - the path of `describe` names
  - the name: string literals as-is, template literals with each `${…}` normalized, and the format string for `.each`
  - the variants
  - the line
  - a fingerprint: the distinct words in its source, which step 4 uses to pair renamed tests

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

- Small fixture `.ts` files, one per pattern (`crates/assertrust/tests/fixtures/parsing/`). The `assertrust tests` output for each is stored as a reviewable snapshot file, using the `insta` crate.
- All of Mailyond's test files, including `test/utils.ts` and `test/setup.ts`, must parse cleanly. They do: all 35 files, all 390 tests and all 939 checks are found, in about 0.1s. (Checked by hand for now; the backtest harness will run it automatically.)

## 3. Test IR

**In one sentence:** each test becomes a list of checks, written in a form that ignores formatting, so a before/after comparison only sees changes in what is being checked.

**Built so far:** the file IR, including each test's fingerprint and each check's facts (used by matching, step 4). Helpers and imports come with helper linking, gate facts with the gate rules.

**Example (Mailyond `10d8742`):**

```
test "emails a confirmation … and only forwards once confirmed"
  before:  check res.body  contains  { email: 'me@gmail.test', status: 'pending', domainIds: null }
  after:   check res.body  contains  { email: 'me@gmail.test' }
  → same test, same check, 2 fields fewer → weaker (dropped: domainIds, status)
```

### Two layers

1. **File IR:** one per file per revision, built from that file's content alone. It's cached by git blob hash, so unchanged files are parsed once during a backtest.
2. **Linked view:** for each test, its own assertions plus the ones that run on its behalf, from helpers (followed across relative imports) and from `beforeEach`/`afterEach` hooks. Each one is tagged with a `via` chain, e.g. `via: ["expectOk"]`.

### What gets recorded

- **Per file:** imports, suites (`describe` blocks and their modifiers), tests, hooks, helpers, and custom matchers (`expect.extend`).
- **Per test:** its id (`describe` path + name), its effective modifiers (`describe.skip` is inherited), its `.each` table as a value, and a body.
- **Per body** (shared by tests, helpers and hooks), in source order:
  - assertions
  - calls (resolved to helpers in the linked view)
  - early exits: a `return` before the test's last statement, possibly inside an `if`
  - runtime skips (`test.skip(cond)`, `ctx.skip()`)
  - `expect.assertions(n)` / `hasAssertions()`
- **Outside tests:** checks at the top level or directly in a `describe`. Checks inside functions declared outside tests belong to helpers.
- **Per assertion:**
  - the subject (normalized source)
  - `negated` (`.not`)
  - which `expect`: plain, `expect.soft` or `expect.poll`
  - promise modifier: `.resolves` or `.rejects`
  - `awaited`: whether the test waits for it: `await`ed, or returned from the test, hook, `test.step` or awaited `vi.waitFor` function (an arrow's body counts as returned). Returning it from another callback doesn't count: `setTimeout` or `forEach` drops what it returns.
  - the matcher: its raw name plus its kind
  - the arguments as values
  - its facts: what it states, in a form that doesn't depend on how it's written (see "Rewritten checks" in step 4)
  - context: inside an if (or `? :` or `switch` case), loop, try, catch, callback or short-circuit. "Try" means a `try` with a `catch`, or with a `finally` that returns (or `break`s / `continue`s out), which drops the failure. A plain `try { … } finally { cleanup() }` still lets a failure through. A `test.step` callback runs right away, so it isn't a callback. Neither is the function in an awaited `vi.waitFor`, `vi.waitUntil` or Testing Library `waitFor`: they retry it and fail the test if it never passes. Not awaited, the test may end first, so it's a callback.
  - a span, used for reporting and never compared
- **Values:** null, undefined, bool, number, string, template, regex (with its top-level alternatives split by oxc's regex parser), object (keys compared as a set), array, nested matcher (`expect.objectContaining(…)`), or opaque (any other expression, as normalized source).

### Matcher kinds

| Kind | Examples |
|---|---|
| Equal (identity / deep / deep-strict) | `toBe`, `toEqual`, `toStrictEqual` |
| Subset | `toMatchObject`, `toHaveProperty`, `objectContaining` |
| Contains | `toContain`, `toContainEqual`, `arrayContaining` |
| Pattern | `toMatch`, `stringMatching` |
| Truthiness | `toBeTruthy`, `toBeDefined`, `toBeNull`, … |
| Type, Compare, Size | `toBeInstanceOf`, `toBeGreaterThan`, `toHaveLength` |
| Throws, Called, Snapshot | `toThrow`, `toHaveBeenCalledWith`, `toMatchSnapshot`, `toHaveScreenshot` |
| UiState | Playwright's `toBeVisible`, `toHaveText`, … |
| Custom | from `expect.extend`, or unknown |

The IR only labels the kind. Which kind is stricter than which is decided by the rules (step 5). This keeps the rules language-neutral: a pytest `assert a == b` or a Rust `assert_eq!` would also map to Equal.

### Defaults

- **Formatting never counts:** whitespace, quotes, trailing commas, key order and line moves all produce the same IR.
- **Simple constants are filled in:** a `const` holding a literal value is inlined where it's used (13 Mailyond assertions compare to a variable). Names are resolved with oxc's scope analysis, so a parameter or inner variable with the same name is never mixed up with it. A `const` array or object can still change, so it's only inlined when every use just reads it (a matcher argument, an `expect(…)` subject, an `.each` table). After `expected.push(x)` or `seed({ expected })` it stays as written. Anything else stays opaque.
- **Template literals:** each `${…}` part is normalized like other source, so `${ a }` and `${a}` are equal, while the text between them is kept as written. A tagged `.each` table becomes one object per row, so lining up its columns differently changes nothing:

  ```text
  it.each`
    plan     | price
    ${'pro'} | ${10}
  `                      →  each([{ plan: 'pro', price: 10 }])
  ```
- **Normalized source:** subjects and opaque values are printed back by oxc in one style (single quotes, no trailing commas), with line breaks folded into spaces.
- **`.each` tables are tracked:** removing a row is weakening, even when no assertion changes.

### Gate facts

CI and test config files (step 1's Gate kind) become facts about which test failures get through. `assertrust_ir::parse_gate` reads one:

- **GitHub Actions workflows** (read with `yaml-rust2`; `serde_yaml` is archived): every job, with its `if:` and `continue-on-error`, and the steps that run tests. A test step is found by the command it runs, not its position: `npm test`, `npm run test:unit`, `pnpm exec playwright test`, `npx vitest run`, `node --test`, … For each one we record its `if:`, `continue-on-error`, and what the command does to failures:
  - swallows them: `|| true`, `|| echo …`, `; exit 0`, `set +e`
  - `--passWithNoTests`, and retries (`--retry 2`, `--retries=2`)
  - what narrows the run: file filters (`vitest run test/unit`), `-t`, `--grep`, `--project`
  - what skips tests: `--exclude`, `--testPathIgnorePatterns`, `--grep-invert`
- **vitest, jest and Playwright configs** (`.ts` / `.js` read with oxc, `.json` as JSON): every literal setting by its path, e.g. `test.coverage.thresholds.lines = 80`. The config object is found through `export default`, `module.exports =`, `defineConfig(…)`, `mergeConfig(…)`, a function returning it, or a `const`. An array of named objects is keyed by name: `projects[chromium].retries`, or vitest's `test.projects[unit].test.include`. Anything that isn't a literal stays as written (`process.env.CI ? 2 : 0`).
- **package.json:** the scripts that run tests (`test`, `test:unit`, `e2e`, not `test:build`), read like a workflow step, and the `jest` key's settings.

Lines are found by searching the file, which is enough for reporting.

### Debug command

`assertrust tests <file> [--rev <rev> | --staged]` prints a file's IR as plain text: from disk by default, at a commit with `--rev`, or from the index with `--staged`. The snapshot tests store this same output. Each line starts with the line number; checks show their matcher kind, the normalized check, and where they sit when it matters:

```
test/structure.test.ts: 9 tests, 3 hooks
  13  test billing > charges the card
  14        equal      expect(charge(10)).toBe(true)
  17  test billing > refunds the card  [skip]
  18        equal      expect(refund(10)).toBe(true)
  23  test billing > upgrades to %s  [each(['pro', 'agency'])]
  24        equal      expect(upgrade(plan)).toEqual({ ok: true, plan })
```

(An excerpt of the `structure.test.ts` fixture's output. Another fixture shows contexts, e.g. `expect(res.status).toBe(200)  (in if)`.)

Comparing the output at `10d8742~1` and `10d8742` shows exactly one changed line: the `toMatchObject` that lost `status` and `domainIds`. A JSON format can be added when another tool needs it.

For a CI or test config file, `assertrust tests` prints its gate facts instead:

```
.github/workflows/ci.yml: 1 job runs tests
   5  job test  [continue-on-error: true]
   9    npm test -- --retry=2 || true  (swallows failures; retries 2)
```

### Mailyond is private

Mailyond is a private codebase, so its code is never committed here. This doc quotes only a few short snippets as examples. The backtest runs against a local clone (path set by an env var) and publishes only counts and findings. Test fixtures are written from scratch.

## 4. Matching

**In one sentence:** before comparing checks, work out which test after the change is the same test as before it, and which check inside it is the same check, even when the test was renamed, moved or refactored.

**Built so far:** pairing tests and checks (crate `assertrust-match`), shown by `assertrust pairs` (see "Debug command" below). Hooks and checks outside tests are paired once helpers are linked.

**Example (Mailyond `038cf0b`, "Keep stored text bodies as they were; derive HTML text only in MCP"):**

```
before: describe('inboundBodyFields') > it('never lets markup or script content into the text or preview')
          check fields.textBody  equals 'Hi scriptbad() <3'
          check fields.preview   equals 'Hi scriptbad() <3'
after:  describe('textFromHtmlBody') > it('never lets markup or script content into the text')
          check textFromHtmlBody(…) equals 'Hi scriptbad() <3'
```

- The `describe` and the name both changed, so pairing by name fails.
- But the names share most of their words and the bodies use the same HTML input, so the two tests are paired (score 0.67).
- Inside the pair, `textBody` and the new check expect the same value. Both old checks do, but `fields.textBody` shares more words with `textFromHtmlBody(…)`, so that one is paired.
- The `preview` check has no partner, so it's **removed**. The title also dropped "or preview", which suggests the removal was deliberate.

### Pairing tests, in order

Within each file first, then across the files of the change (a test moved to another file):

1. **Same `describe` path and name** → paired. This covers most tests. Duplicate names in a file are paired in order.
2. **Same name under another `describe` path** (the `describe` was renamed, or the test moved into another one) → paired, if the name is unique on both sides.
3. **Leftovers** → paired by a similarity score from 0 to 1:
   - 40% the words in the names. An old name kept whole inside a longer new one counts as close: "is red with more than one record" → "is red with more than one record, suggesting a merge".
   - 30% the words in the bodies (a fingerprint of each test's source)
   - 30% the checks: identical checks and distinctive expected values they share

   A pair needs a score of at least 0.4, must be each other's best candidate, and must beat the runner-up on both sides by 0.1.
4. **Still unpaired:**
   - old tests count as deleted
   - new tests count as added, which is never a problem

### Pairing checks inside a paired test, in order

1. **Identical** → unchanged. Checks in the same place pair first, so a check that moved into an `if`, `try` or callback is the one paired with a changed context. `pairs` shows it as `context`, and the rules judge it.
2. **Same subject and matcher kind** → paired, then their values are compared (`10d8742`: same `res.body`, same matcher, fewer fields).
3. **Same subject, another matcher** → paired: `toEqual({ id: 1 })` → `toBeTruthy()`.
4. **Same fact, written differently** → paired, see "Rewritten checks" below. This can pair groups: two property checks folded into one object check.
5. **The same distinctive expected value** (a string of 4+ characters, a regex, a long number, an expression), with the subject refactored → paired: `expect(fields.textBody).toBe('Hi there')` → `expect(textOf(html)).toBe('Hi there')`.
6. **Same matcher and arguments on a refactored subject, in order**, when the subjects share a word or input: `normalize('a.com').invalid` → `parse('a.com').skipped`, `hasDmarcEnforcement({ dmarc: result })` → `result.enforced`.
7. **Same matcher kind, scored:** subjects that share a word, plus expected values that mostly match (or subjects that mostly match). Same margin rule as tests.
8. **Leftover old checks** → removed. **Leftover new checks** → added.

When one check could pair with several, the closest wins if it's clearly ahead: by subject words, then expected value, then the same `.not` and the same matcher. So `toBe(200)` goes to `toBe(201)` rather than `.not.toBe(500)` when all three check `res.status`. Otherwise none are paired. Subjects are compared by their words, splitting camelCase (`parseURLValue` → parse, url, value), with each string literal counted as one word: the same input is a strong sign, but a long one mustn't swamp the rest.

### Rewritten checks

The same check can be written many ways, and refactors and linters move between them. So each check also records its **facts**: what it states about a value, in one form. Two checks that state a fact in common are the same check rewritten:

| Written as | Fact |
|---|---|
| `expect(r.text).toBe(v)`, `toMatchObject({ text: v })`, `toHaveProperty('text', v)`, `toEqual(expect.objectContaining({ text: v }))`, `r?.text`, `r['text']` | `r.text == v` |
| nested: `expect(res.body.user.id).toBe(7)`, `toMatchObject({ user: { id: 7 } })` on `res.body`, `toHaveProperty('body.user.id', 7)` | `res.body.user.id == 7` |
| `expect(list.length).toBe(2)`, `expect(list).toHaveLength(2)` | `list.length == 2` |
| `expect(list.includes(v)).toBe(true)`, `expect(list).toContain(v)` | `list toContain(v)` |
| `expect(a === b).toBe(true)`, `expect(a).toBe(b)`; `!==` or `toBe(false)` for `.not` | `a == b` |
| `expect(n > 5).toBe(true)`, `expect(n).toBeGreaterThan(5)` (and `>=`, `<`, `<=`) | `n toBeGreaterThan(5)` |
| `expect(typeof x).toBe('string')`, `expect(x).toBeTypeOf('string')` | `x toBeTypeOf('string')` |
| `expect(e instanceof E).toBe(true)`, `expect(e).toBeInstanceOf(E)` | `e toBeInstanceOf(E)` |
| `toBe(true)` / `toBeTruthy()`, `toBe(false)` / `toBeFalsy()` / `expect(!x).toBe(true)`, `toBe(null)` / `toBeNull()`, `toEqual(undefined)` / `toBeUndefined()` | the same fact |
| `await expect(p).resolves.toEqual(v)`, `expect(await p).toEqual(v)` | `await p == v` |
| Playwright: `expect(await page.title()).toBe(t)` / `toHaveTitle(t)`, `textContent()` / `innerText()` / `toHaveText`, `toContainText`, `count()` / `toHaveCount`, `url()` / `toHaveURL`, `inputValue()` / `toHaveValue`, `getAttribute(a)` / `toHaveAttribute(a, v)`, `isVisible()` / `toBeVisible()` (and hidden, checked, enabled, disabled, editable), `ok()` / `toBeOK()` | `page.title() == t`, … (a call stays apart from a property: `store.count()` isn't `store.count`) |
| mocks: `expect(fn.mock.calls.length).toBe(2)` / `toHaveBeenCalledTimes(2)`, `fn.mock.calls[0]` / `toHaveBeenNthCalledWith(1, …)` | `fn.mock.calls.length == 2`, … |
| `expect(() => f()).toThrow(e)`, `expect(f).toThrow(e)`, and the same for a method, `() => obj.parse()` | `f toThrow(e)` |

A negated check of several fields at once (`.not.toMatchObject({ a: 1, b: 2 })` means "not both") has no facts, and neither does `.rejects`. A negated equality isn't split into fields at all: `.not.toEqual({ a: 1 })` passes for `{ a: 1, b: 2 }`, so it doesn't state `not a == 1`.

Pairing only says the checks correspond. The rules (step 5) decide whether the new form is weaker: folding `expect(r.text).toBe(v)` into `toMatchObject({ text: v, html })` adds a field, which is fine.

### Moves and splits aren't removals

A removed check that shows up unchanged, and in the same place (not newly inside an `if` or callback), in another test of the same change moved there. Each added check can account for one removed check. A deleted test whose checks all moved was split or merged into others. Neither produces findings. A deleted test with no checks wasn't moved: it's a deletion.

### Defaults

- **When unsure, don't pair.** Missing a weakening is better than a false alarm, because a noisy tool gets turned off.
- **Renames are information, not findings.** They appear in the output, with the words the title dropped. When a title drops words in the same change that a check disappears, that's noted for the reviewer and the LLM judge (step 7).
- **Thresholds are set on the Mailyond history** (below), not guessed.

### Checked on Mailyond's history

`assertrust pairs` over all 313 non-merge commits takes 2 seconds. Every pairing that isn't by id was read by hand:
- **21 renamed tests, all correct.** Every candidate above 0.35 that was the other's clear best was a real rename or rewrite. The lowest real one scored 0.37, so the threshold is 0.4.
- **Rewritten checks:** of 27 tests that still had a check removed next to an added one, 22 are paired now, by steps 4–7. The other 5 are real behaviour changes or have nothing to pair on, e.g. `expect(overLimit).toEqual([…])` → `expect(skipped).toEqual(['You pasted 12 domains…'])`.
- **20 deleted tests:** a deleted test file, removed functions and removed behaviour. Three were near misses (0.27–0.37), and one was a split with two close candidates. All four were left unpaired on purpose.

### Debug command

`assertrust pairs [--range <range> | --staged | --worktree]` prints how tests and checks pair up, for the net change and each commit, the same way `assertrust changes` lists them. Unchanged tests are only counted. A test file that doesn't parse on either side is skipped for that change with a note: comparing against nothing would make every test in it look deleted. A test file renamed out of the suite shows as `test/dca.test.ts → test/dca.ts (not a test file name)`, with its tests deleted. From the `billing` fixture:

```
test/billing.test.ts: 7 tests paired (1 unchanged), 1 deleted, 1 added
  same      invoices > stores the pending invoice
    1 check identical
    changed      12  expect(res.body).toMatchObject({ amount: 10, currency: 'EUR', status: 'pending' })
             →   12  expect(res.body).toMatchObject({ amount: 10 })
  renamed (0.57) invoices > marks an invoice as paid
         →  invoices > marks a paid invoice  (dropped: "an as")
    1 check identical
    rewritten    17  expect(invoice.status).toBe('paid')
             →   17  expect(invoice).toMatchObject({ method: 'card', status: 'paid' })
```

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
- each check-pairing step, groups of folded checks, and every form in the rewritten-checks table

## 5. Rules

**In one sentence:** each rule looks at what matching paired up (or one gate file) before and after the change, and says whether a test got weaker, how, and how confident it is.

**Built so far:** every rule (crate `assertrust-rules`), run by `assertrust check`.

**Example (Mailyond `71110c0`, "Pipeline fix."):**

```
before: expect(source).not.toMatch(/support@|support\.your|resend|inboxes\.net|…/)
after:  expect(source).not.toMatch(/support@|support\.your|inboxes\.net|…/)
→ expectation-loosened (high): .not.toMatch no longer rules out resend
```

`.not.toMatch` means "must not contain any of these". Taking a word off that list means the test now allows it, so the test is weaker. Without `.not`, removing an alternative would make it stricter. That's why polarity is part of the IR (step 3).

### The rules

| Rule id | What it catches | Examples | Default confidence |
|---|---|---|---|
| `check-removed` | A check disappeared from a test that still exists, and didn't reappear elsewhere in the change | an `expect` line deleted; a test left with zero checks | high; medium when the test also gained checks that didn't pair (it was rewritten) |
| `matcher-loosened` | A matcher moved down the strength ladder (below) | `toEqual({ id: 1 })` → `toBeTruthy()`; `toHaveBeenCalledWith(a)` → `toHaveBeenCalled()` | high when it ends at the bottom level, else medium |
| `matcher-swapped` | A matcher swapped for one at the same level, or any matcher change under `.not` | `toBe(obj)` → `toEqual(obj)` (identity becomes deep equality); `toContain('x')` → `toMatch(/x/)`; `.not.toEqual(a)` → `.not.toBe(a)` | low, shown by default |
| `expectation-loosened` | The same matcher accepts more than before | object fields dropped (`10d8742`); an alternative dropped under `.not` (`71110c0`); array items or `.each` rows dropped; `toThrow('msg')` → `toThrow()`; a wider numeric bound or `toBeCloseTo` precision | high for fields, rows and alternatives; medium for numbers, substrings and a reshaped `.each` table |
| `test-disabled` | A test no longer runs, or no longer runs everywhere | `.skip`, `.todo`, `.fails`, `.skipIf(…)`, a runtime `test.skip(cond)`; `.only` committed (silences the rest of the file); a test deleted and not moved; a test file deleted, or renamed to a name the runner doesn't pick up | high; medium when it skips only under a condition, or the deleted test's file gained tests that didn't pair |
| `check-cant-fail` | A check is still there but may no longer run or fail | moved into a `try` with a `catch`, an `if` or an unawaited callback; an early `return` added before it; `await` dropped from `.resolves`/`.rejects`; a bare `expect(x)` with no matcher; `expect.assertions(n)` removed or `n` lowered | high for a bare `expect`, a dropped `await` and an unconditional `return`; low for a loop; else medium |
| `gate-loosened` | CI or test config lets more failures through | `\|\| true`, `continue-on-error: true`, `if: false` on a test step; the test step removed; `retries` raised; coverage threshold lowered; a pattern added to `exclude`/`testIgnore`; `passWithNoTests` added | high; medium for an `if:` with a condition, a narrowed run, files left out of coverage, and `.only` settings |
| `outcome-changed` | The expected outcome changed: not looser, just different | `toBe(3)` → `toBe(4)`; `.not` added or removed | low; shown only with `-v` until the LLM judge exists |

### What the first three check, in detail

**`check-removed`**, one finding per test, listing what it lost:

```
test/unit/inbound-body.test.ts:41  HIGH  check-removed
  removed expect(fields.preview).toBe('Hi scriptbad() <3'); the test name dropped "preview" · 038cf0b
```

- Checks that moved to another test in the same change (step 4) don't count.
- The message adds "the test has no checks left" when that's the case, and the words the test's name dropped.
- **Medium** when the test also gained checks matching couldn't pair: it was rewritten, and the new checks may cover the old ones in a way matching can't see (a mock renamed from `fetch` to `resolver`).
- A test whose checks no longer run (skipped for good with `skip`, `todo` or `fixme`, or skipping itself with no condition, like `ctx.skip()` outside any `if`) isn't looked into: `test-disabled` reports the skip.

**`test-disabled`:**

- **A modifier that stops the test:** `skip`, `fixme`, `todo`, and `fails` / `fail` (the test passes only if it fails) are high. `skipIf(cond)`, `runIf(cond)` and Playwright's `test.skip(cond)` in a `describe` skip only sometimes, so they're medium. Tests in one file that gain the same one (a `describe.skip`) make one finding.
- **A skip added inside the test** (`test.skip(cond)`, `ctx.skip()`): high when unconditional, medium with a condition or inside an `if`.
- **A `.only`** in a file that had none: one finding saying how many other tests stop running. A new file doesn't count, since none of its tests ran before.
- **A deleted test** whose checks didn't all move to other tests. A deleted test without checks counts too: a smoke test still fails when the code throws. **Medium** when the file gained tests that didn't pair, since the test may have been rewritten under another name (Mailyond `49a44d2`).
- **A test that didn't run before** (skipped, or skipping itself) stops nothing: deleting it, or swapping one skip for another (`skip` → `todo`), isn't flagged.
- **A deleted test file**, or one renamed to a name that isn't a test file name (`dca.test.ts` → `dca.ts`, `a.test.ts` → `a.test.ts.disabled`): one finding for the file. Step 1 keeps a renamed file's stronger kind, so the file is still loaded, and its tests count as gone after the change.

**`check-cant-fail`:**

- **A check in a new place** where it may not run or fail (when checks are folded into one, any of them that wasn't there before counts): a callback, a `catch`, a `try` with a `catch`, an `if` or `? :`, after `&&`/`||`/`??` (medium), or a loop that may run zero times (low). Places that still fail the test aren't flagged: `try { … } finally { … }`, `test.step`, and an awaited `vi.waitFor` / `waitFor`, which retries and then fails the test (step 3).
- **A `return` added before checks,** or one moved out of an `if`: high when it's unconditional (they never run), medium inside an `if`.
- **`await` dropped from a check that returns a promise:** `.resolves` / `.rejects`, `expect.poll`, and Playwright's web-first matchers (`toBeVisible`, `toHaveText`, …). The test may end before it fails. On a plain `toBe`, `await` did nothing, so dropping it is fine.
- **A matcher dropped:** `expect(total).toBe(3)` → `expect(total)` checks nothing.
- **`expect.assertions(n)`** removed, lowered, or replaced with `expect.hasAssertions()` (medium).

### Comparing a check with what it became

`matcher-loosened`, `matcher-swapped`, `expectation-loosened` and `outcome-changed` each compare a check pair from step 4, and a pair gets at most one of them. In order:

1. **A matcher is first written one way:** aliases folded (`toBeCalled` → `toHaveBeenCalled`), and matchers that behave the same written the same (`toBeNull()` = `toBe(null)`, `.not.toBeUndefined()` = `toBeDefined()`, `toEqual(5)` = `toBe(5)`).
2. **The subject changed** (a rewrite): what the checks state (their facts, step 4) decides. All of it still stated: fine, as with `expect(r.text).toBe(v)` → `expect(r).toMatchObject({ text: v })` or `await expect(p).resolves.toBe(v)` → `expect(await p).toBe(v)`. Only part of it: `expectation-loosened`, e.g. `no longer checks r.b == 2`. None of it: go on.
3. **`.not` added or removed,** or `.resolves` ↔ `.rejects`: `outcome-changed`.
4. **Another matcher:** the ladder below. Down is `matcher-loosened`, across is `matcher-swapped`, up is fine. Within one family, a different value is `outcome-changed`: `toEqual({ preview })` → `toBe('Hi')`, `toBeGreaterThan(t)` → `toBeLessThanOrEqual(t)`.
5. **The same matcher:** the expected values, by the table under "Which direction is looser". Looser is `expectation-loosened`; different but not looser is `outcome-changed`; stricter is fine.

Several checks folded into one, or one split into several, are compared by their facts the same way as in step 2. A test's `.each` table is compared too, row for row: rows dropped are `expectation-loosened` (a duplicate counts). When rows were also edited or reshaped (a column added or removed), only the count is compared, at medium: `.each now runs 4 rows instead of 5`.

### What `gate-loosened` checks

It compares the gate facts (step 3) of each CI or test config file before and after:

```
.github/workflows/ci.yml:7  HIGH  gate-loosened
  job test, step "npm test || true": failures no longer fail it · 1a2b3c4
```

- **Workflows:** jobs are paired by id (or, when renamed, by a test command they share), and test steps by their command. When one step's command changed, it's paired with the one new step. Then:
  - a test job or step removed (high)
  - `continue-on-error` turned on (high; medium for an expression like `${{ matrix.experimental }}`)
  - an `if:` added or changed: `if: false` is high, any other condition medium
  - the command now swallows failures, passes with no tests, retries more, or skips tests (high), or runs fewer of them (medium)
- **Test config settings,** by key:
  - `retry` / `retries` raised (high), or set to an expression (medium)
  - `passWithNoTests: true` added (high)
  - a coverage threshold (`lines`, `functions`, `branches`, `statements`) lowered or removed (high)
  - a pattern added to `exclude`, `testIgnore` or `testPathIgnorePatterns` (high), or to a coverage `exclude` (medium: coverage, not tests)
  - a pattern removed from `include`, `testMatch` or `testRegex`, a project removed (high), `testDir` changed (medium)
  - `forbidOnly` turned off or `allowOnly: true` (medium: a committed `.only` passes)
- **package.json:** each test script's command, like a workflow step's, and the `jest` settings.

A deleted workflow or config counts as all of it removed. Raising a threshold, removing a skip, or adding a step is stricter and never flagged. Not covered yet: triggers (`on:`), matrix entries, and which script a renamed `npm run …` step now runs.

### Matcher strength ladder (positive checks)

1. **Exact:** Equal (deep-strict ≥ deep ≥ identity, plus `toBeNull`, `toBeUndefined`, `toBeNaN`), Snapshot, Throws with a specific message or class, Playwright's exact text and values (`toHaveText`, `toHaveValue`, `toHaveURL`, `toHaveCount`, `toHaveAttribute(name, value)`, …)
2. **Structured:** Subset (`toMatchObject`, `toHaveProperty(path, value)`), CalledWith, exact Size (`toHaveLength`)
3. **Partial:** Contains, Pattern, Compare (`toBeGreaterThan`), Throws (any), CalledTimes, `toContainText`
4. **Type:** `toBeInstanceOf`, `toBeTypeOf`
5. **Presence:** `toBeTruthy`, `toBeFalsy`, `toBeDefined`, `toHaveProperty(path)`, called at all, element states (`toBeVisible`, `toBeEnabled`, …)

- Moves down the ladder → `matcher-loosened`.
- Sideways swaps (same level) → `matcher-swapped`, at low confidence. Swaps that behave identically aren't flagged, e.g. `toBe(5)` ↔ `toEqual(5)` with a primitive literal, and neither is `toEqual(x)` → `toStrictEqual(x)`.
- Custom matchers aren't ranked: any change to one is `matcher-swapped`.
- Moves up the ladder are stricter and aren't flagged.
- Matcher changes under `.not` aren't ranked in v0.1. They're reported as `matcher-swapped`.

### Which direction is looser

| Matcher kind | Positive check: looser when… | Under `.not`: looser when… |
|---|---|---|
| Subset (`toMatchObject` at every level; `expect.objectContaining` at its own level, since its fields are compared exactly) | keys are removed | keys are added |
| Any expected value | it becomes `expect.any(…)` or `expect.anything()` that still accepts the old value (`expect.anything()` rejects `null`, `expect.any(String)` a number: those are `outcome-changed`); an exact object becomes `expect.objectContaining(…)` | n/a (it's stricter) |
| Pattern (`toMatch`, `expect.stringMatching`) | alternatives are added (at least one kept), or a flag that matches more (`i`, `s`, `m`) | alternatives or such a flag are removed |
| Text (`toMatch` with a string, `toContainText`, `expect.stringContaining`) | the text is shortened | the text is lengthened |
| Contains (`expect.arrayContaining`) | items are removed. A different `toContain` value is `outcome-changed`: on an array it needs a whole item | n/a |
| Compare | a bound moves outward, also between `>` and `>=` (`> 5` → `>= 5` at low confidence); `toBeCloseTo` keeps fewer digits, or becomes `>` / `<` | n/a |
| Throws, `toHaveProperty`, `toHaveAttribute` | the message or value is dropped | n/a |
| Equal, Size, CalledWith, Snapshot | n/a (a different value is `outcome-changed`) | n/a |

### Shape

- Each rule is one file with a `check` function. It reads the whole change, not one test pair at a time: `test-disabled` needs deleted tests and whole files, and a `.only` affects the rest of its file.
- The four rules that compare a check with what it became share one file (`changed_check.rs`), since a pair gets at most one of them. The ladder is in `strength.rs`, and the comparison of expected values in `looser.rs`.
- A finding has:
  - the rule id and confidence
  - the file and line after the change (no line when what it's about is gone: a deleted test or file)
  - the test, and later its `via` chain
  - a one-line message, plus the code before and after, shown with `-v`
  - the commit that caused it. Per-commit checks know it already; PR-style checks get it from step 1's attribution, which comes with the output PR.
- All v0.1 rules are deterministic, so no AI or API key is needed to run them.
- Rules can be turned off one at a time in config (step 6).

### Defaults

- **Sideways matcher swaps are flagged too,** at low confidence. Changes that behave identically are not.
- **`outcome-changed` is hidden unless the judge is on.** Legitimate behaviour changes do this constantly, so on its own it would be mostly noise (43 of them on Mailyond's history, below). Until the judge exists, `-v` shows them, and the summary line says how many were hidden. It's still recorded in the backtest report for tuning.
- **Confidence levels are a first guess** and get adjusted using the Mailyond backtest. They already were once: a deleted test is medium when its file gained tests, after `49a44d2` (below).

### Checked on Mailyond's history

`assertrust check` over the same 313 commits, one by one, takes 2 seconds and shows 32 findings, plus 43 hidden changed outcomes. All were read by hand:
- **11 `check-removed`** (5 high, 6 medium) and **17 `test-disabled`** (8 high, 9 medium). No `check-cant-fail`: the one check that moved, into `await vi.waitFor(…)` in `8b2cb57`, still fails the test.
- **No `gate-loosened`, rightly:** the 8 commits that changed the workflow or a test config only added jobs, steps and settings (the e2e job, `isolate: false`, a Chrome channel), and `package.json` only gained test scripts. All of them parse.
- **4 `expectation-loosened`, all correct:** `71110c0` (the example above), `8454255` dropped `wildcard: false` from two `toMatchObject`s, and `e689765` dropped `'lifecycleEmails'` from an `arrayContaining` list of tables. No `matcher-loosened` or `matcher-swapped`.
- **All 32 describe the change correctly:** the check or the test is gone, or accepts more.
- **Most are deliberate.** Tests deleted with the behaviour they tested (`581fb66`: "drop the double-prefix rule and resolveSetupTarget"), refactors that dropped a field the checks read (`11c140f`). Only the commit message says so. That's the judge's job (step 7), or an allow trailer (step 6).
- **One false alarm, now medium:** `49a44d2` rewrote "stores an inbound email" into a bigger test under a new name. The bodies differ too much to pair, but the file gained tests, so the deletion is medium.
- **Borderline ones worth a look:** `038cf0b` dropped the `preview` check (the example in step 4), and `11c140f` dropped `toMatchObject({ subdomainPolicy, nonexistentPolicy, adkim, aspf })` from "surfaces sp, np and strict alignment", which still checks the findings for them.
- **The 43 changed outcomes are legitimate behaviour changes:** new prices in `.each` tables, renamed constants, `toBe(false)` → `toBe('skipped')` when a result became a status. That's why they're hidden.
- **Other branches:** `10d8742` on `forward_email` is found commit by commit (`toMatchObject no longer checks domainIds, status`), and a PR-style check of the branch finds nothing, since `f8e4323` restored the fields. On `plan/audiences`, `6913cd6` removed a column and a row from an `.each` table: `.each now runs 4 rows instead of 5`, at medium.

### Tests

There's at least one fixture that must be flagged and one that must not, per rule (`crates/assertrust-rules/tests/rules.rs`), plus:
- a fixture with one change of each kind, run through `assertrust check` (`crates/assertrust/tests/fixtures/rules/`)
- both polarity directions for Pattern and Subset
- rewrites that state the same thing, and matchers that behave the same, which must produce no findings
- reformat-only changes, which must produce no findings
- tests and checks that moved to another test or file, which must produce no findings
- the three Mailyond shapes: `10d8742` (fields dropped), `71110c0` (an alternative dropped under `.not`) and `038cf0b` (a check removed in a renamed, refactored test)

## 6. Output

**In one sentence:** show each finding where the person or agent will see it, in the terminal, on the PR, or as JSON for tools, and never block anything unless the repo opts in.

**Built so far:** the terminal format and `-v`, always warn-only (exit 0). Changed outcomes are hidden without `-v`, and the summary line says how many: `no findings · 2 changed outcomes hidden (-v shows them)`. A per-commit check names each finding's commit. A PR-style check doesn't yet: tracing a finding back to its commit, hiding transient findings, the other formats, allow comments and config come with the output PR. Until then `--format json`, `--format github` and `--show-transient` exit with 2. Files that couldn't be checked are listed as notes before the summary, e.g. `note: test/a.test.ts (1a2b3c4): skipped, doesn't parse (…)`, and so are merge commits a per-commit check skipped.

### Terminal (default): short, two lines per finding

```
test/api/forwarding.test.ts:27  HIGH  expectation-loosened
  toMatchObject no longer checks domainIds, status · 10d8742

1 finding · warn-only (exit 0)
```

- `-v` adds the test name, the before/after snippet and the full commit message:

  ```
  test/a.test.ts:1  HIGH  check-removed
    removed 2 checks, including expect(b).toBe(2) · 1a2b3c4
      test    refunds
      before  expect(b).toBe(2)
              expect(c).toBe(3)
      commit  Trim the refund test
  ```
- A finding about something that's gone (a deleted test or file) has no line: `test/a.test.ts  HIGH  test-disabled` / `deleted: billing > refunds`.
- Findings are sorted most severe first. Allowed findings come last, marked `ALLOWED` with their reason. Transient findings appear only with `--show-transient`.
- **On the PR**, an inline annotation already sits on the line, so it says only the finding itself: title `assertrust: expectation-loosened (high)`, message `toMatchObject no longer checks domainIds, status (10d8742)`. The summary comment is a short table: file:line, rule, message.

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
