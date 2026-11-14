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
| backtest | both | each merged PR PR-style (a squash-merged one against its own commits, when fetched), each direct push on its own; see "Backtest" in step 6 |

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
| Test support | code files in `test/`, `tests/`, `__tests__/`, `__mocks__/` or `e2e/`. Helpers are followed into these files and Test files only (step 2, "Helper functions"). | yes |
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

**Built so far:** tests, hooks, checks, helper functions and imports in one file (crate `assertrust-ir`), shown by `assertrust tests` (see "Debug command" in step 3), and helpers followed across files for the linked view (step 3).

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
- **Skips decided at run time:** Playwright's `test.skip(cond)` / `test.fixme(cond)` and vitest's `ctx.skip()`. Inside a test they're an item in its body. Directly in a `describe` or at the top of a file (Playwright), they apply to every test in it, including the ones above them, so those tests get a `skip(cond)` modifier. One inside a helper is ignored for now.
- **Recognized by name, not by import.** Playwright projects usually import `test` from their own fixtures file (`base.extend(…)`), so checking imports would miss them.
- **Recorded for each test:**
  - the path of `describe` names
  - the name: string literals as-is, template literals with each `${…}` normalized, and the format string for `.each`
  - the variants
  - the line
  - a fingerprint: the distinct words in its source, which step 4 uses to pair renamed tests

### Assertions

- **Shape:** `expect(subject)` (optionally `expect.soft`, `expect.poll`, or vitest browser mode's `expect.element`), then optionally `.not` / `.resolves` / `.rejects`, then `.toX(args)`.
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

```
test/helpers.ts:     export function expectOk(res) { expect(res.status).toBe(200); expect(res.body.ok).toBe(true) }
test/refund.test.ts: it('refunds', async () => { expectOk(await refund()) })
→ refunds checks expect(res.status).toBe(200) and expect(res.body.ok).toBe(true), via expectOk(await refund())
```

- **A helper** is a named function outside tests and hooks: `function expectOk(res) { … }` or `const expectOk = (res) => { … }`, at the top of a file or in a `describe` (then only tests in that `describe` see it). Its body is recorded like a test's: checks, calls and where they sit. A function a helper puts on an object it builds (`return { sendChunk() { expect(…) } }`) runs when a test calls that method, not with the helper, so its checks aren't the helper's.
- **A method is a helper too** when its class or object is defined outside tests: `class SyncTestContext { assertSyncResponse(response, records) { … } }`, a class field `expectOpen = () => { … }`, or `const api = { expectOk(res) { … } }`. Constructors, getters and setters aren't. immich's sync tests get their context from a `setup()` function, so what a test calls a method on (`ctx.assertSyncResponse(…)`) can't be traced to its class. A method call is linked by the method's name instead: to the one method by that name that checks something, in the file and the files it imports (and what they pass on, or build objects from: `export const page = new Page()` in a fixtures file). With two such methods, the call isn't followed. Methods that check nothing never count, so `ctx.get(…)` and `page.click()` stay ordinary lines. An instance method counts only when one of those files builds or extends its class: nothing can call it on an object nobody builds. A static method or an object's method is only called on its owner by name: `API.clearSelection()` links, `h.app.clearSelection()` doesn't. Inside a method, `this.other()` is its own class's method, a helper if it checks something, and `this` stands for what the call was made on. A name matched to the wrong method is the price of this: a library's `checkbox.check()` in a file whose page object has a checking `check()`.
- **A call** to a function by its plain name, from a test, hook or helper, is recorded when the name is a helper in the file or something the file imports. Every method call (`ctx.assertSyncResponse(…)`, `this.syncStream(…)`) is recorded too, since its method may be in any file the test imports; linking drops the ones that lead to no helper. Arguments are recorded as values.
- **Where helpers are found:** in the same file, and in files imported through a relative path, but only files in test folders (step 1's Test and Test support kinds). In the six repos we checked, every helper with checks lives in a test folder, and following imports into source files would read and parse many files per commit for nothing. Imports are resolved the way bundlers do: the path with each extension (`.ts`, `.tsx`, `.mts`, `.cts`, `.js`, `.jsx`, `.mjs`, `.cjs`), then an `index` file in it, and `./helpers.js` may name `helpers.ts`. Files that pass helpers on are followed (`export { expectOk } from './http'`, `export * from './http'`), and so are default exports. Only imports a file uses that way are followed: ones it calls, calls methods on, builds objects from (`new SyncTestContext(db)`) or extends. Files that aren't UTF-8 text are left out.
- **Aliases** go through the nearest `tsconfig.json` above the importing file: its `paths` (the pattern with the longest prefix wins, `@/test/*` over `@/*`), then its `baseUrl`, including what it `extends` from files in the repo. Packages it extends (`@tsconfig/node20`) aren't read. Aliases that live outside git can't be followed: Nuxt's `~~/…` and `#shared/…` come from a generated `.nuxt/tsconfig.json`, and vite's `resolve.alias` is code. Imports from packages aren't followed either.
- **A helper that moved out of test folders** (`test/helpers.ts` → `src/testing.ts`, with the import changed) is followed into its new file: it was followed before the change, so the comparison would otherwise lose it.
- **A call that can't be followed** (an alias that can't be resolved, a package, a file outside test folders) stays in the test's view, marked so. The checks it ran before aren't reported as removed: what it checks now can't be seen. Dropping the call still counts.
- **A test file that didn't change** is loaded too when a helper file it calls changed. A search for the helper file's name in import paths (`git grep`) finds candidates, and resolving their imports confirms them, also through files that pass helpers on. This only happens when a helper that checks something (itself or through helper files it calls) changed, not just its lines, or an import such a helper uses did. A file of factories or mocks doesn't start a search, and neither does a page object whose `click()` changed next to an `expectOpen()` that didn't, or a call in a helper that leads to no helper (`act(() => …)` from a package): changing those doesn't change what a test checks. All the changed helper files of a change are searched for at once, one `git grep` per step away from them, and a test file is kept only when what its tests check changed.
- **A helper file that doesn't parse** skips the tests that call it, with a note: comparing without its checks would make them look removed.
- **Helpers that don't assert still shape what a test does:** setup, seeding and request wrappers, like everything in Mailyond's `test/utils.ts`. Changes there go to the LLM judge (step 7), such as a seed helper's default switching from `'connected'` to `'pending'`.
- **Not covered yet:** a runtime skip inside a helper, arguments for parameters that aren't plain names (`{ strict }`, `status = 200`), and values read from a page object rather than methods called on it (immich's `app.dialog`, built by a function the tests' fixture calls).

### Not in v0.1

- chai style (`expect(x).to.equal(y)`) and `node:assert`
- `.snap` snapshot files (inline snapshots are covered as arguments)
- helpers imported from packages, or through aliases that aren't in a committed `tsconfig.json`

### Tests

- Small fixture `.ts` files, one per pattern (`crates/assertrust/tests/fixtures/parsing/`). The `assertrust tests` output for each is stored as a reviewable snapshot file, using the `insta` crate.
- All of Mailyond's test files, including `test/utils.ts` and `test/setup.ts`, must parse cleanly. They do: all 35 files, all 390 tests and all 939 checks are found, in about 0.1s. The backtest (step 6) notes every file it can't parse, in every PR and push it replays, so a parsing regression shows up there.

## 3. Test IR

**In one sentence:** each test becomes a list of checks, written in a form that ignores formatting, so a before/after comparison only sees changes in what is being checked.

**Built so far:** the file IR, including each test's fingerprint, each check's facts (used by matching, step 4), helpers and imports, the linked view, and gate facts.

**Example (Mailyond `10d8742`):**

```
test "emails a confirmation … and only forwards once confirmed"
  before:  check res.body  contains  { email: 'me@gmail.test', status: 'pending', domainIds: null }
  after:   check res.body  contains  { email: 'me@gmail.test' }
  → same test, same check, 2 fields fewer → weaker (dropped: domainIds, status)
```

### Two layers

1. **File IR:** one per file per revision, built from that file's content alone. Caching it by git blob hash, so unchanged files are parsed once during a backtest, is planned.
2. **Linked view** (`assertrust_ir::link`): for each test, its own checks plus the ones that run on its behalf, in the order they run:
   - `beforeAll` and `beforeEach` hooks, outermost `describe` first
   - the test's own items, each call to a helper followed by the helper's items (helpers calling helpers too)
   - `afterEach` and `afterAll` hooks, innermost `describe` first

   Each item from a helper or hook is tagged with how it gets there, its `via` chain: `expect(res.status).toBe(200)  via expectOk(res) > expectStatus(res, 200)`.
   - **Arguments are filled in** for plain parameters, in the checks' subjects and expected values: `function expectStatus(res, status) { expect(res.status).toBe(status) }` called as `expectStatus(refund(), 200)` checks `refund().status` with `toBe(200)`, so moving a check into a helper isn't a change. A constant the helper reads from its arguments is filled in too (`const items = response.slice(0, -1)`), and so is `this` in a method. In an expected value only a whole parameter is replaced: `schema.safeParse(input).data` stays as written, so a test that passes the same input under another name (`proto` for `JSON.parse(…)`) doesn't look like a new outcome. Messages show a check as its helper writes it (`expect(res.status).toBe(200)`), so one finding about the helper covers every test that calls it.
   - **Where a call sits counts for every check it runs:** a helper called in an `if` puts all of its checks in one.
   - **An async helper that isn't awaited** marks its checks "not awaited": they may run after the test ends. A helper that isn't async runs its checks before it returns, so `await` doesn't matter there.
   - **A `return` in a hook or helper** stays in the view, tagged like the rest: it ends only the items of that call. A `return` in the test stops everything after it but `afterEach` and `afterAll`.
   - **A helper that never runs a check** (a factory, a mock, a request wrapper), itself or through the helpers it calls, isn't expanded: its call is all the rules need. Without this, a test calling `newTestService()`, which builds 40 mocks, carried hundreds of items.
   - **The call itself stays** in the view too, with where its helper is defined, so the rules can tell a call removed from a test from a helper that lost a check, and both from a call they can't see into. Each hook starts with a call to it. Call cycles are guarded, and calls are followed 8 deep.

### What gets recorded

- **Per file:** imports, suites (`describe` blocks and their modifiers), tests, hooks, helpers (methods with their class or object), the names it builds objects from or extends, and custom matchers (`expect.extend`).
- **Per test:** its id (`describe` path + name), its effective modifiers (`describe.skip` is inherited), its `.each` table as a value, and a body.
- **Per body** (shared by tests, helpers and hooks), in source order:
  - assertions
  - calls to helpers, with their arguments, and whether they're awaited (resolved in the linked view)
  - early exits: a `return` before the test's last statement, possibly inside an `if`
  - runtime skips (`test.skip(cond)`, `ctx.skip()`)
  - `expect.assertions(n)` / `hasAssertions()`
- **Outside tests:** checks at the top level or directly in a `describe`. Checks inside functions declared outside tests belong to helpers.
- **Per assertion:**
  - the subject (normalized source)
  - `negated` (`.not`)
  - which `expect`: plain, `expect.soft`, `expect.poll` or `expect.element`
  - promise modifier: `.resolves` or `.rejects`
  - `awaited`: whether the test waits for it: `await`ed, or returned from the test, hook, `test.step` or awaited `vi.waitFor` function (an arrow's body counts as returned). Returning it from another callback doesn't count: `setTimeout` or `forEach` drops what it returns.
  - the matcher: its raw name plus its kind
  - the arguments as values
  - its facts: what it states, in a form that doesn't depend on how it's written (see "Rewritten checks" in step 4)
  - context: inside an if (or `? :` or `switch` case), loop, try, catch, callback or short-circuit. "Loop" means one that may run zero times: not a `for…of` over an array written out with at least one item (`for (const path of ['/a', '/b'])`), a `for` whose counter starts inside its literal bound (`for (let i = 0; i < 2; i++)`) or that has no condition, a `while (true)`, or a `do … while`, which all run their body at least once. In those, the statements from the first one that may `break` or `continue` (`if (done) break;`) on are in the loop. "Try" means a `try` with a `catch` that doesn't throw again (`catch (err) { tidy(err); throw err }` lets the failure through), or with a `finally` that returns (or `break`s / `continue`s out), which drops the failure. A plain `try { … } finally { cleanup() }` still lets a failure through. A `test.step` callback runs right away, so it isn't a callback. Neither is the function in an awaited `vi.waitFor`, `vi.waitUntil` or Testing Library `waitFor`: they retry it and fail the test if it never passes. Not awaited, the test may end first, so it's a callback. A function declared in the test (or hook, or helper) and called by its name, `const hover = () => { … }` or `function hover() { … }`, runs as part of the call: it's read where it's first called, so its checks take the call's place (an `if`, a `try`, a callback), come after any `return` before the call, and what it returns is awaited when the call is. An async one the test doesn't await is a callback, and so is one it never calls (handed to an event listener, say). A `let` isn't followed, since it may hold another function by the time it's called.
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

- **GitHub Actions workflows** (read with `yaml-rust2`; `serde_yaml` is archived): every job, with its `if:` and `continue-on-error`, and the steps that run tests. A test step is found by the command it runs, not its position: `npm test`, `npm run test:unit`, `pnpm --filter web test`, `pnpm exec playwright test`, `npx vitest run`, `node --test`, …, also behind a wrapper like `xvfb-run` or `timeout 600`. Other JS tools count when they're used the same way: monorepo runners (`turbo run test`, `nx test web`), `ni`'s shortcuts (`nr test:unit`), Vite+ (`vp test`), `deno test`, `nub exec --node vitest run`. Only tools on that list count: guessing at any tool also took `docker run --name test-db` for a test run. Steps nested inside another step (`- parallel: [{ run: … }]`) are read too, under the wrapper's `if:` and `continue-on-error`. For each one we record its `if:`, `continue-on-error`, and what the command does to failures:
  - swallows them: `|| true`, `|| echo …`, `npm test && echo ok || true`, or a later command deciding the result once `set +e` is on. The script is followed the way the shell runs it. A step runs under `bash -e`, so `npm test; exit 0` still fails, and so does a handler that exits non-zero (`|| { cat log; exit 1; }`). Redirects (`2>&1`, `> out.log`) are ignored. A pipe (`npm test | tee log`) is taken to pass a failure on, as it does with `pipefail`.
  - `--passWithNoTests`, and retries (`--retry 2`, `--retries=2`, or the run repeated: `npm test || npm test`)
  - what narrows the run: file filters (`vitest run test/unit`), `-t`, `--grep`, `--project`, and which `--config` picks the tests
  - what skips tests: `--exclude`, `--testPathIgnorePatterns`, `--grep-invert`
- **vitest, jest and Playwright configs** (`.ts` / `.js` read with oxc, `.json` as JSON): every literal setting by its path, e.g. `test.coverage.thresholds.lines = 80`. The config object is found through `export default`, `module.exports =`, `defineConfig(…)`, `mergeConfig(…)`, a function returning it, or a `const`. An array of named objects is keyed by name: `projects[chromium].retries`, or vitest's `test.projects[unit].test.include`. A vitest workspace (`defineWorkspace([…])`) is read as its projects, and a project given by folder or glob is keyed by it: `projects[packages/*]`. Anything that isn't a literal stays as written (`process.env.CI ? 2 : 0`).
- **package.json:** the scripts that run tests (`test`, `tests`, `test:unit`, `test-unit`, `e2e`), and the `jest` key's settings. Not scripts that build or set up for tests (`test:build`), watch (`test:watch`), or check types (`test:types`, `test:tsd`, `test:attw`): type checks aren't tests. A script is read like a workflow step, but npm runs it with `sh -c`, which goes on after a failure: `vitest run; exit 0` passes there.

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

Helpers are listed like tests, with their checks, and so are imports. Calls show as written; `assertrust pairs` shows the checks they add (from the `helpers.test.ts` fixture):

```
test/helpers.test.ts: 2 tests, 4 helpers
      imports seed from ./support
   6  helper expectOk(res, …)
   7        equal      expect(res.status).toBe(status)
  24  test rows > lists
  25        call       expectOk(await list())
```

Comparing the output at `10d8742~1` and `10d8742` shows exactly one changed line: the `toMatchObject` that lost `status` and `domainIds`. A JSON format can be added when another tool needs it.

For a CI or test config file, `assertrust tests` prints its gate facts instead:

```
.github/workflows/ci.yml: 1 job runs tests
   5  job test  [continue-on-error: true]
   9    npm test -- --retry=2 || true  (swallows failures; retries 2)
```

### Mailyond is private

Mailyond is a private codebase, so its code is never committed here. This doc quotes only a few short snippets as examples. Its backtest labels quote its code, so they live in a local file outside this repo, and this doc publishes only counts and findings. Test fixtures are written from scratch.

## 4. Matching

**In one sentence:** before comparing checks, work out which test after the change is the same test as before it, and which check inside it is the same check, even when the test was renamed, moved or refactored.

**Built so far:** pairing tests and checks (crate `assertrust-match`), shown by `assertrust pairs` (see "Debug command" below). Tests are paired with their linked view (step 3), so checks from hooks and helpers pair like the test's own. Checks outside tests aren't paired yet.

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
4. **A list checked in parts** → paired as a group, when the new checks state every item of the old list again on parts of one list: `expect(response).toEqual([user, done])` → `expect(response.at(-1)).toEqual(done)`, `expect(response.slice(0, -1)).toEqual(expect.arrayContaining([user]))` and `toHaveLength(1)` on the slice, the shape of immich's `assertSyncResponse`. Parts are `at(i)`, `[i]` and `slice(…)`, each stating the items at its place when the old list was exact; an old `toHaveLength` of the list goes with it. The list should be the same (`response`, or `(await p)` for `expect(p).resolves`); one stored under another name first (`const response = await …`) counts when one of its items is distinctive (step 6), since all of them must match.
5. **Same fact, written differently** → paired, see "Rewritten checks" below. This can pair groups: two property checks folded into one object check.
6. **The same distinctive expected value** (a string of 4+ characters, a regex, a long number, an expression), with the subject refactored → paired: `expect(fields.textBody).toBe('Hi there')` → `expect(textOf(html)).toBe('Hi there')`.
7. **Same matcher and arguments on a refactored subject, in order**, when the subjects share a word or input: `normalize('a.com').invalid` → `parse('a.com').skipped`, `hasDmarcEnforcement({ dmarc: result })` → `result.enforced`.
8. **Same matcher kind, scored:** subjects that share a word, plus expected values that mostly match (or subjects that mostly match). Same margin rule as tests.
9. **Leftover old checks** → removed. **Leftover new checks** → added.

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
| `expect(y).toBe(239)`, `expect(y).toBeCloseTo(239, 1)`: close to at least one decimal digit (the default is two). Exact equality on a computed number is brittle, so the switch is usually a fix; `toBeCloseTo(239, 0)` passes anything from 238.5 to 239.5, so it doesn't count | `y == 239` |
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

A check from a helper or hook says how it gets there: `changed  2  expect(res.status).toBe(200)  via expectStatus(refund(), 200)`.

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
- each check-pairing step, groups of folded checks, a list checked in parts, and every form in the rewritten-checks table

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
- **A deleted test** whose checks didn't all move to other tests. Four or more deleted from one file in one change make one finding (`deleted 32 tests, including parses dates`, all of them listed with `-v`): a feature removed with its tests is one event, not 32 lines. A deleted test without checks counts too: a smoke test still fails when the code throws. **Medium** when the file gained tests that didn't pair, since the test may have been rewritten under another name (Mailyond `49a44d2`).
- **A test that didn't run before** (skipped, or skipping itself) stops nothing: deleting it, or swapping one skip for another (`skip` → `todo`), isn't flagged.
- **A deleted test file**, or one renamed to a name that isn't a test file name (`dca.test.ts` → `dca.ts`, `a.test.ts` → `a.test.ts.disabled`): one finding for the file. Step 1 keeps a renamed file's stronger kind, so the file is still loaded, and its tests count as gone after the change.

**`check-cant-fail`:**

- **A check in a new place** where it may not run or fail (when checks are folded into one, any of them that wasn't there before counts): a callback, a `catch`, a `try` with a `catch`, an `if` or `? :`, after `&&`/`||`/`??` (medium), or a loop that may run zero times (low). Places that still fail the test aren't flagged: `try { … } finally { … }`, `test.step`, and an awaited `vi.waitFor` / `waitFor`, which retries and then fails the test (step 3).
- **A `return` added before checks,** or one moved out of an `if`: high when it's unconditional (they never run), medium inside an `if`.
- **`await` dropped from a check that returns a promise:** `.resolves` / `.rejects`, `expect.poll`, `expect.element`, and Playwright's web-first matchers (`toBeVisible`, `toHaveText`, …). The test may end before it fails. On a plain `toBe`, `await` did nothing, so dropping it is fine.
- **A matcher dropped:** `expect(total).toBe(3)` → `expect(total)` checks nothing.
- **`expect.assertions(n)`** removed, lowered, or replaced with `expect.hasAssertions()` (medium).

### Checks that run through helpers and hooks

The rules see each test's linked view (step 3), so a check from a helper or hook is judged like the test's own. Where it's reported depends on where the change happened:

```
test/helpers.ts:1  HIGH  check-removed
  removed expect(res.body.ok).toBe(true) from expectOk (in 2 tests)
```

- **The change is in the helper** (a check removed, loosened or moved into an `if` there, or a `return` added before checks): reported in its file, at the check, the `return`, or for a removed check the helper itself as it is now. Tests calling it make one finding, ending `(in 2 tests)`, and `-v` names each test with its file (`test/refund.test.ts: refunds`). A `return` added in a helper is `a return in expectOk now comes before 2 checks: they never run`. It's unconditional when nothing inside the helper guards it, even if the call to the helper sits in an `if`. A check whose new place comes from inside the helper (the helper wrapped it in a `try`) is reported in the helper, even when the calls changed too.
- **The change is at the call** (its arguments changed, it moved into an `if`, its `await` was dropped, or the test stopped calling it): reported at the call, in the test.
  - `expectFast(load(), 100)` → `expectFast(load(), 500)`, where the helper checks `toBeLessThan(limit)`: `toBeLessThan upper bound raised from 100 to 500`.
  - A call that moved is one finding, not one per check: `expectOk(res) is now inside an if, with its 2 checks: it may not run`. An async helper's call that lost its `await`: `expectSaved(1) is now not awaited, with its check: it may run after the test ends`.
  - A call removed: `removed expectOk(res), which ran 2 checks`. As with a test's own checks, it's medium when the test also gained checks that didn't pair (`expect(h.history)` replaced by a helper snapshotting the undo and redo stacks). When some of its checks still have a counterpart (it was swapped for a weaker helper): `removed expectOk(refund()); 1 of the 2 checks it ran is gone`.
- **A call inside a helper** changed the same way is reported there: `removed the call to expectStatus from expectOk, which ran 1 check`, or an argument raised at the call in `expectOk`. The first call that differs on the way to a check decides where.
- **Hooks** count like helpers called once per test: a check removed from a `beforeEach` is `removed expect(db.open).toBe(true) from beforeEach (in 2 tests)`. An early `return` in a test doesn't stop `afterEach` and `afterAll`, which run however the test ends. `expect.assertions(n)` is the test's own: one in a hook doesn't stand in for it.

### Comparing a check with what it became

`matcher-loosened`, `matcher-swapped`, `expectation-loosened` and `outcome-changed` each compare a check pair from step 4, and a pair gets at most one of them. In order:

1. **A matcher is first written one way:** aliases folded (`toBeCalled` → `toHaveBeenCalled`), and matchers that behave the same written the same (`toBeNull()` = `toBe(null)`, `.not.toBeUndefined()` = `toBeDefined()`, `toEqual(5)` = `toBe(5)`).
2. **The subject changed** (a rewrite): what the checks state (their facts, step 4) decides. All of it still stated: fine, as with `expect(r.text).toBe(v)` → `expect(r).toMatchObject({ text: v })` or `await expect(p).resolves.toBe(v)` → `expect(await p).toBe(v)`. Only part of it: `expectation-loosened`, e.g. `no longer checks r.b == 2`. None of it: go on.
3. **`.not` added or removed,** or `.resolves` ↔ `.rejects`: `outcome-changed`.
4. **Another matcher:** the ladder below. Down is `matcher-loosened`, across is `matcher-swapped`, up is fine. Within one family, a different value is `outcome-changed`: `toEqual({ preview })` → `toBe('Hi')`, `toBeGreaterThan(t)` → `toBeLessThanOrEqual(t)`. A number compared closely (step 4's facts) is the same check: `toBe(239)` → `toBeCloseTo(239, 1)`.
5. **The same matcher:** the expected values, by the table under "Which direction is looser". Looser is `expectation-loosened`; different but not looser is `outcome-changed`; stricter is fine.

A list checked in parts (step 4) states every item again, so only two things can be lost, both `expectation-loosened`: the order, when the old list was exact and the parts take two different items in any order (low: `expect(response).toEqual([…]) is now checked in parts, with its 3 items in any order`), and the count, when the old checks fixed it and the parts don't (medium: `… which pass with more than its 3 items`). Only one slice whose count is known (an exact list, or a `toHaveLength` on it) with single items covering the rest fixes the count: `slice(0, -1)` and `at(-1)`, or `[0]` and `slice(1)`. Single items alone (`[0]`, `[1]`) pass for a longer list too.

What another check in the test still states isn't lost, if that check runs whenever the pair's new one does and can fail: it's in the same place or where nothing can stop it, no `return` comes before it that doesn't come before the new one too, and it isn't a promise check the test doesn't await. Placement became computed in Excalidraw, so `y` moved out of an object check into one of its own:

```text
before: expect(arrow).toMatchObject({ type: 'arrow', x: 261, y: 239 })
after:  expect(arrow.y).toBeCloseTo(239, 1)
        expect(arrow).toMatchObject({ type: 'arrow', x: 261 })
→ nothing: arrow.y == 239 is still stated
```

That holds for a key dropped from an object (every fact under it must be stated), a rewrite in step 2, and folded checks.

Several checks folded into one, or one split into several, are compared by their facts the same way as in step 2. A test's `.each` table is compared too, row for row: rows dropped are `expectation-loosened` (a duplicate counts). When rows were also edited or reshaped (a column added or removed), only the count is compared, at medium: `.each now runs 4 rows instead of 5`. A test in a `describe.each` with an `it.each` of its own runs every row of one with every row of the other, so the tables are compared level by level, from the test's own outward. A level only one side has isn't compared: a test moved into a `describe.each` still has its own table compared. A table that isn't written out (`name === 'Checkbox' ? [a, b] : [a]`) isn't compared.

### What `gate-loosened` checks

It compares the gate facts (step 3) of each CI or test config file before and after:

```
.github/workflows/ci.yml:7  HIGH  gate-loosened
  job test, step "npm test || true": failures no longer fail it · 1a2b3c4
```

- **Workflows:** jobs are paired by id (or, when renamed, by how they run tests). Test steps are paired by their script, then their name, then how they run tests (`npm test` → `npm test || true`), then the script they run through another tool (`pnpm test` → `nub run test`), then the one step left on each side. Only jobs that run tests are checked, not `lint` or `publish`. A job counts when it has a test step, or when its id or name says test or e2e, or it calls a reusable workflow that does (`uses: ./.github/workflows/test.yml`). Then:
  - a test job or step removed (high)
  - `continue-on-error` turned on (high; medium for an expression like `${{ matrix.experimental }}`)
  - an `if:` added or changed: `if: false` is high, any other condition medium. One that still runs the step (`always()`, `!cancelled()`) isn't flagged.
  - the command now swallows failures, passes with no tests, retries more, or skips tests (high), or runs fewer of them (medium). Each kind of filter is judged on its own. Fewer means a kind the run didn't have (a first path, `--project`, `-t`, `--grep` or `--changed`), one path or project dropped from several, or a path inside a dropped one (`tests` → `tests/smoke`). Another path or project runs more, so it isn't flagged. Neither is a filter swapped for another of its kind, since the two can't be compared (`test/unit` → `test/api`, `-t refunds` → `-t charges`), nor a run with another `--config` (`--config fastly.config.ts` → `--project fastly`).
- **Test config settings,** by key:
  - `retry` / `retries` raised (high), or set to an expression (medium)
  - `passWithNoTests: true` added (high)
  - a coverage threshold (`lines`, `functions`, `branches`, `statements`) lowered or removed (high)
  - a pattern added to vitest's `test.exclude`, `testIgnore` or `testPathIgnorePatterns` (high), or to a coverage `exclude` (medium: coverage, not tests)
  - a pattern removed from vitest's `test.include`, `testMatch` or `testRegex`, a project removed, including a workspace entry (high), `testDir` changed (medium)
  - only patterns that pick test files count: Vite's `optimizeDeps.exclude` or `server.watch.exclude` don't
  - patterns are compared for what they mean. A negated include (`'!src/legacy/**'`) is an exclude, and `{a,b}` alternatives are spelled out. In vitest's `exclude`, a pattern vitest excludes anyway (`**/node_modules/**`, `...configDefaults.exclude`) skips nothing new. Any other spread (`...flakyTests`) may skip anything, so it counts. A file excluded from one project that another project now includes moved; it wasn't skipped.
  - a project is removed only if no config in the same folder lists it now: vitest 3 moves `vitest.workspace.ts`'s projects into `vitest.config.ts`'s `test.projects` (or `test.workspace`). A moved project is compared with what it was, so a `retry` raised on the way is flagged. A new project's settings aren't checked, since it only adds tests.
  - `forbidOnly` turned off or `allowOnly: true` (medium: a committed `.only` passes)
- **package.json:** each test script's command, like a workflow step's, and the `jest` settings. A script that calls another script of the same file is read through it, with its own flags added to that script's: with `"test:unit": "vitest --project unit"`, changing `"test": "nr test:unit"` to `"nr test:unit --project=server"` runs one more project, not only `server`. Only npm, pnpm, yarn, `bun run` and `nr` are followed this way, not tools that run other packages' scripts (`turbo run test`, `pnpm -r test`). A test script that no longer runs a test runner is high when it does nothing (`echo skip`). It's medium when it runs something else, since `node scripts/ci.js` may still run the tests. A test script removed is medium, because CI calling it would now fail loudly.
- **`assertrust.toml` itself** (step 6, "Config"): a rule turned `off` or left to the `judge`, `fail-on` lowered (high), or a `test` or `gate` file pattern dropped (medium, since it's often rewritten to match the same files).

A deleted workflow or config counts as all of it removed. So does one renamed or moved where it no longer runs (`ci.yml` → `ci.yml.disabled`). Raising a threshold, removing a skip, or adding a step is stricter and never flagged. Not covered yet: triggers (`on:`), matrix entries, a step's own `shell:`, and which script a workflow step's `npm run …` runs (only scripts calling scripts in the same `package.json` are followed).

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
| Subset (`toMatchObject` at every level; `expect.objectContaining` at its own level, since its fields are compared exactly) | keys are removed. A key whose value moved to a new key that looks alike (one contains the other, or they start with the same 4 letters) was renamed with the code (`statusCode: 422` → `status: 422`): `outcome-changed` | keys are added |
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

### Checked on public repos

The last 600 commits of hono, h3, zod, nuxt/ui and vueuse, one by one: 3 to 5 seconds each, nothing fails to parse. The first run had 71 `gate-loosened` findings, mostly false. Each pattern behind them is now handled and has a test:
- vitest 3 moving projects from `vitest.workspace.ts` into `vitest.config.ts` (zod, vueuse)
- a new tool running the same scripts: `pnpm test` → `nub run test` (zod), `vitest` → `vp test` (hono)
- steps nested under `parallel:` (nuxt/ui)
- type checks taken for tests: `test:types` renamed `typecheck` (h3)
- `--config x` → `--project x` read as a narrower run (hono)
- conditions on jobs that run no tests (zod's `lint` and publish jobs)
- negated includes moved to `exclude`, default excludes listed, and files moved to a new project (vueuse)

What's left is 23 findings. 19 are correct: a CI job dropped with its tests (hono's lambda jobs), a reverted workflow that ran tests, conditions added to test jobs, a coverage script now running one project, and coverage now ignoring more files. The other 4 come from vueuse splitting or merging CI steps whose scripts point to other scripts. Seeing those needs following `npm run X` into `package.json`, which is listed under "Not covered yet".

In the test rules, 90 of h3's 94 `expectation-loosened` are bundle-size budgets raised (`toBeLessThanOrEqual(5200)` → `5300`). They're correct and stay at medium; a repo that finds them noisy can ignore the path once config lands (step 6). 2 came from fields renamed along with an API, now `outcome-changed`. zod's biggest group of `test-disabled`, 32 of 73, is one revert that deleted a feature with its tests: correct, and now one finding (4 or more tests deleted from one file in one change are grouped). On the 10 public repos and Mailyond, grouping turns 499 deleted-test lines into 234 (immich 116 → 25, cal.com 153 → 72, zod 70 → 28) and changes nothing else.

**With helpers linked**, checked on 10 public repos (the five above plus TanStack Query, tRPC, Excalidraw, cal.com and immich, 600 commits each) and on Mailyond:
- **False findings gone:** 37 in cal.com, where one commit moved 37 tests' checks into two new helpers (`expectResponsesToBe`, `expectParsingToFail`), so each test seemed to have no checks left. One in tRPC (a check moved into `renderThrowing()`). Duplicate lines for the same deleted test in immich and Excalidraw.
- **New correct findings:** three Excalidraw test files whose `checkpoint` helper, run after every test, stopped snapshotting `h.history` (replaced by snapshots of the undo and redo stacks, so medium). cal.com's fork removing `expectNoShowAuditToBeDone(…)` calls from its no-show tests. h3's `rejects(promise, pattern)` helper accepting one more error message. In zod, three tests swapping `expectOwnProp(…)` for `expectStripped(…)` when `__proto__` stopped being kept, and one `expectMatch(…)` call moved into a loop (low).
- **Still wrong:** Excalidraw's `reconcile` helper moving from chai's `expect(x).deep.equal(y)` to `assert.deepEqual(x, y)`, neither of which is read yet ("Not in v0.1").
- **Fixed along the way:** a `catch` that throws again (`catch (err) { tidy(err); throw err }`), which Excalidraw's `getTextEditor` added, made 40 checks look swallowed; and tRPC's `testContext()`, whose returned methods' checks were counted as the helper's.

Linking costs 0–15% more time on 9 of the 11 (Excalidraw 55%, Mailyond 33% of 1.2s): each helper file is read once per run unless a commit changes it, helpers that never check aren't expanded, and the search for tests that call a changed helper file runs only when its checks may have changed. A whole history takes about as long as before: hono's 2,835 commits in 22s (20s before), nuxt/ui's 6,019 in 29s (28s before).

### Tests

There's at least one fixture that must be flagged and one that must not, per rule (`crates/assertrust-rules/tests/rules.rs`), plus:
- checks through helpers and hooks, linked (`crates/assertrust-rules/tests/helpers.rs`), including methods found by their name and immich's `assertSyncResponse` shape (a list checked in parts, its order or count lost), and helper files followed in real repos, including staged and unsaved changes, `index` files that pass helpers on or are imported by dots alone, helpers outside test folders and helpers moved there, tsconfig aliases, files that aren't text, a helper file that stops parsing, a class the tests build with `new`, and a page object whose methods that check nothing changed (`crates/assertrust/tests/helpers.rs`)
- a fixture with one change of each kind, run through `assertrust check` (`crates/assertrust/tests/fixtures/rules/`)
- both polarity directions for Pattern and Subset
- rewrites that state the same thing, and matchers that behave the same, which must produce no findings, including a number compared closely and a key checked on its own line
- loops that always run, functions a test surely calls, `expect.element`, `.each` tables at two levels, and a script calling another, each in the shape the backtest found
- reformat-only changes, which must produce no findings
- tests and checks that moved to another test or file, which must produce no findings
- the three Mailyond shapes: `10d8742` (fields dropped), `71110c0` (an alternative dropped under `.not`) and `038cf0b` (a check removed in a renamed, refactored test)

## 6. Output

**In one sentence:** show each finding where the person or agent will see it, in the terminal, on the PR, or as JSON for tools, and never block anything unless the repo opts in.

**Built so far:** all of it except SARIF and the PR comment, which the GitHub Action posts. The terminal, JSON and GitHub formats; each finding named after the commits that made it, also in a PR-style check; weakening undone later in the PR hidden, and so is weakening of code the PR itself added; allow comments and trailers; `fail-on` and its exit codes; `assertrust.toml`, which a PR can't loosen for its own check; and `assertrust backtest`, which replays a branch's history PR by PR and, with labels, measures how often findings are right. Changed outcomes are hidden without `-v`, and the summary line says how many: `no findings · 2 changed outcomes hidden (-v shows them)`. Files that couldn't be checked are listed as notes before the summary, e.g. `note: test/a.test.ts (1a2b3c4): skipped, doesn't parse (…)`, and so are merge commits a per-commit check skipped. In a PR-style check, a file one commit couldn't parse is noted only when the whole PR can't parse it either: a work-in-progress commit that a later one fixed never lands.

### Which commit, and weakening undone later

A per-commit check runs the rules on each commit, so each finding has its commit. A PR-style check runs them on the whole PR (what lands) and on each commit, then pairs them up:

```
c1  remove flaky check   → removed expect(res.body).toEqual({…})
c2  restore check        → puts it back
c3  skip slow test       → now skipped (skip)
whole PR                 → now skipped (skip) · c3
```

- **A finding in the whole PR names every commit with a finding about the same thing:** the same file (or the one the PR renamed it to), the same test when both name theirs (or the one the PR renamed it to), and then the same rule, the same message, or the same code lost. So a matcher loosened in one commit and the check removed in the next still pair, but one commit skipping `charges` doesn't pair with another skipping `refunds`, though both say `now skipped`. A finding about many tests (`deleted 4 tests`) pairs with each commit's finding about one of them. At most three are named, then `+2 more`; `-v` lists each with its subject.
- **A commit's finding the whole PR doesn't have was undone later** (transient): c1's above. It's hidden, and the summary says how many: `1 undone later hidden (--show-transient shows them)`. With `--show-transient` it's marked `(undone later)`.
- **Unless it's about code the PR itself added, and still weaker at the PR's head.** A new test written in one commit and loosened in the next (`toBe(30)` → `toBeGreaterThan(0)`) lands as just a new test, so nothing the main branch had got weaker. It's how an agent gets its own new test green, but in the backtest such findings are rarely worth a reviewer's time (7 of 202 unexplained), and they were half of everything reported. So they're **added by this PR**: hidden, counted in the summary (`1 on code this PR added hidden (--show-pr-added shows them)`), and never failing the check. `--show-pr-added`, or `show-pr-added = true` in `assertrust.toml`, shows them, marked `(added by this PR)`. "Added" means the file wasn't there before the PR, or the test wasn't in it; a file or test the PR only renamed was there. "Still weaker" means no later commit touches its file (or, for deleted tests, any test file), or else the rules, run again from before that commit to the PR's head across every file between them, find the same test (followed through renames and moves to other files) with code both lost, or deleted or skipped; or its file gone with its tests. Otherwise it was undone: a test the PR added, deleted and put back, or moved whole into another file, lands as it was. On 100-commit ranges of the public repos, most of what the whole PR doesn't have is this kind (tRPC 19, Excalidraw 12, Mailyond 11, where it includes the reference cases `71110c0`, `038cf0b` and `8454255` in files that range created); weakening truly undone later is rarer (hono 4, zod 3, Mailyond 2).
- Staged and unsaved changes have no commits, so their findings name none.

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
- Findings are sorted most severe first. Allowed findings come last, marked `ALLOWED`, with a line saying why: `allowed: preview is tested in MCP now (comment)`. Transient findings appear only with `--show-transient`, marked `(undone later)`. A finding whose rule is left to the judge (`outcome-changed`) needs `-v` whatever its status, allowed or undone later too, and isn't counted without it.
- **On the PR**, an inline annotation already sits on the line, so it says only the finding itself: title `assertrust: expectation-loosened (high)`, message `toMatchObject no longer checks domainIds, status (10d8742)`. A finding that reaches `fail-on` is an `error`, the rest are warnings; allowed, hidden, undone-later and added-by-the-PR ones aren't annotated (the last only with `show-pr-added`). The last line and the summary count findings as the terminal does: `assertrust: no findings · 1 on code this PR added hidden · warn-only (exit 0)`. The summary is a short table (file:line, rule, message, then the allowed ones with their reasons), written to the job summary (`$GITHUB_STEP_SUMMARY`) and starting with `<!-- assertrust -->`, so the GitHub Action can post it as one PR comment and edit it in place on every push.

Messages are short by design: one line that says what got weaker. Detail goes behind `-v`, never in the default view.

### Formats (`--format`)

| Format | For | v0.1 |
|---|---|---|
| `terminal` | people, Claude Code hook output | yes |
| `json` | the backtest, the benchmark, other tools. Versioned (`"schema": 1`; until v0.1 is released, it can still change). Every finding is there with its `status` (`shown`, `hidden`, `allowed`, `transient`, `pr-added`), its commits, and for an allowed one the reason | yes |
| `github` | inline annotations on the PR diff (`::warning file=…,line=…::`) and a summary table in the job summary; the Action posts that as one PR comment, edited in place on every push | yes |
| `sarif` | GitHub code scanning (the Security tab) | later |

### Exit codes

- `0`: no findings, or findings below `fail-on`. The default is `fail-on = "never"` (warn-only). `--fail-on high` (or `medium`, `low`) sets it for one run.
- `1`: at least one finding at or above `fail-on`, or, with the judge on, a verdict listed in `[judge] fail-on` (step 7). Hidden, allowed, undone-later and added-by-the-PR findings never count. The summary line says it: `2 findings · 1 finding at high or above (exit 1)`.
- `2`: assertrust itself failed (no git, bad config, bad range). This is never confused with "found something".

**Blocking.** Deterministic findings warn by default. Set `fail-on` to block. This default may change once the backtests prove precision: they don't yet (42% right, 61% at high; see "Backtest" below).

### Acknowledging a deliberate change

Sometimes weakening a test is the right call. There are two ways to say so, and both need a reason:

- **In the code:** `// assertrust-allow: check-removed preview is tested in MCP now`, anywhere inside the test, hook or helper the finding is in, or on the line above it. Several rules go comma-separated: `check-removed,matcher-loosened`. In CI and test config, `# assertrust-allow: gate-loosened …` (or `//` in a JS config) on the line of the step or setting that changed, or in the comment lines right above it.
- **In the commit message:** a trailer, `Assertrust-Allow: test-disabled flaky on CI, tracked in #123`. It counts for the findings its commit made, which is the way when there's nothing left to put a comment in (a deleted test) or no comments at all (`package.json`).

**A comment counts only in the change that adds it** (the commit, or the PR in a PR-style check): a line the change added, as `git diff` shows it. One left from an earlier change can't waive a new weakening of the same test: someone allowed a removed check in March, another check is removed in June, and June's is reported. A comment that only moved with its code is still the old one, but a copy of an existing comment put in another test is new.

Allowed findings still appear, marked `ALLOWED`, so a reviewer sees what was waived and why. An allow with no reason doesn't count: the finding stays, ending `; allow ignored: it gives no reason`.

### Config: `assertrust.toml` (optional)

```toml
fail-on = "never"            # never | high | medium | low
show-transient = false
show-pr-added = false        # weakening of code the PR itself added

[files]                      # defaults from step 1; a kind given replaces its defaults
test         = ["**/*.{test,spec}.{ts,tsx,js,jsx,mts,cts}", "**/__tests__/**"]
test-support = ["test/**", "tests/**", "e2e/**"]
gate         = [".github/workflows/*.yml", "{vitest,jest,playwright}.config.*", "package.json"]

[rules]                      # every rule is on by default except outcome-changed
outcome-changed = "judge"    # off | on | judge
matcher-swapped = "on"
```

assertrust works with no config file. It lives at the repo root. CLI flags override the file.

**A change can't loosen its own check.** A range or PR-style check reads the file as it is at its base (the start of the range, or where the branch split off), so a change to it applies once it lands:

```
- fail-on = "high"
+ fail-on = "never"
+ [rules]
+ check-removed = "off"
→ the check still fails on the removed check, and reports both settings:
assertrust.toml:1  HIGH  gate-loosened
  lowered fail-on from high to never · 1a2b3c4
assertrust.toml:4  HIGH  gate-loosened
  turned check-removed off: its findings are no longer reported · 1a2b3c4
```

Like a loosened CI config, a deliberate change says why with `# assertrust-allow: gate-loosened <reason>` above the setting. When the file on disk differs from the one at the base, a note says the check uses the base's. Staged and worktree checks read the file on disk, and so does `assertrust tests`; `changes` and `pairs` read it the way `check` does. An unknown key, rule or value is an error (exit 2), so a typo (`fail_on`) doesn't silently do nothing. `off` drops a rule's findings; `judge` leaves them to the judge (step 7), which for now means they're shown only with `-v`, as `outcome-changed` is by default.

### Backtest: what it would have flagged, and how often it's right

`assertrust backtest` replays a branch's history the way CI would have checked it, PR by PR. On Mailyond:

```
PR #44  8fd84d8  Free tool: /tools/dmarc-checker (DMARC, SPF and MX for up to 10 domains) · 11 commits
test/unit/rate-limit.test.ts  HIGH  test-disabled  (added by this PR)
  file deleted: its 4 tests no longer run · 11c140f

push  71110c0  Pipeline fix.
test/unit/mx-checker.test.ts:290  HIGH  expectation-loosened
  .not.toMatch no longer rules out resend · 71110c0

181 commits: 90 PRs, 10 other merges, 81 pushes · 1 finding · 40 changed outcomes hidden (-v shows them) · 23 on code its PR added hidden (--show-pr-added shows them) · …
```

Each commit on the branch's first-parent line got there one way, and is checked the way CI would have checked it then, with the `assertrust.toml` it had (at the PR's base):
- **A merge commit** merged a PR with its commits: checked PR-style, `C^1...C^2`. Findings name the PR's commits, and weakening undone within the PR is hidden.
- **A squash merge** (`Fix refunds (#12)`, how most GitHub repos merge) leaves one commit on the branch. When the PR's own commits are there (`git fetch origin refs/pull/12/head:refs/pull/12/head`), it's checked PR-style against them, as the GitHub Action will see it. Otherwise it's checked as one commit, and a note says how many were. The PR's commits must add up to the squash: each file it changed is the same at the PR's last commit, unless the branch changed that file too after the PR split off. A commit that only cites a PR (`Address review comments from (#12)`) fails that, and is checked on its own, with a note.
- **Any other commit** was pushed straight to the branch: checked on its own. A merge that isn't a PR's (`Merge branch 'main' of …`, from a pull on the branch) is checked PR-style like a merged PR, and counted as an "other merge".

The repo's first commit isn't checked: there's nothing before it. A commit that can't be checked (say, a PR whose commits share no history with a shallow clone) is listed with the reason. assertrust never touches the network: fetching is up to whoever runs it, and `backtest/run.sh` does it for the repos below (`docs/development.md`, "Backtests").

**Labels** (`--labels FILE`) are a person's verdict on each finding a PR check would report (shown, allowed, or added by its PR, though that's hidden by default):
- `wrong`: nothing got weaker; assertrust misread the change (the check moved, the new matcher accepts no more, …). A test rewritten for a deliberate behaviour change, still checking the same input against the new result, is `wrong` too: that's a changed outcome, not a weaker test.
- `intended`: weaker on purpose: the change also removed or rewrote the code the test covered (and the test now checks less of what's left), or a commit message or the PR's title says why.
- `unexplained`: weaker, and nothing in the change says why. These are the findings a reviewer should look at.

**Precision** is the share labelled `intended` or `unexplained`: the findings that are right. A run prints it overall, per confidence level and per rule. Telling `intended` from `unexplained` is what the judge (step 7) will be scored on, from the same labels.

With labels, a run exits with 1 when a finding isn't labelled yet, a finding labelled right is no longer found, or a commit couldn't be checked. So after a change to the rules, it says exactly which findings appeared or went. `--update-labels` adds new findings to the file, unlabelled, and drops false alarms no longer found. A finding is the same one when its commit, rule, file, test and code are the same; the message isn't part of it, so rewording one keeps its label. If its code is printed differently, it keeps the label of the one left-over entry about the same commit, rule, file and test, marked `carried over`.

**First results (2026-11-11).** The last 100 PRs and pushes of each of the 10 public repos below, plus Mailyond's whole history (181), replayed and every finding labelled: 424 findings, **42% right** (179). At high confidence 61% (134 of 219), at medium 22% (45 of 196), at low none of 9. That's well short of the 70% that blocking by default would need, so `fail-on` stays `never`.

| Repo | Findings | Right | High: right |
|---|---|---|---|
| cal.com | 13 | 13 (100%) | 11 of 11 |
| Mailyond | 24 | 17 (70%) | 11 of 12 |
| h3 | 22 | 11 (50%) | 0 of 2 |
| zod | 37 | 18 (48%) | 16 of 16 |
| Excalidraw | 87 | 38 (43%) | 25 of 38 |
| vueuse | 12 | 5 (41%) | 5 of 9 |
| hono | 13 | 5 (38%) | 4 of 7 |
| immich | 84 | 32 (38%) | 31 of 73 |
| nuxt/ui | 12 | 4 (33%) | 3 of 4 |
| TanStack Query | 25 | 8 (32%) | 5 of 8 |
| tRPC | 95 | 28 (29%) | 23 of 39 |

By rule: `test-disabled` 117 of 197 (59%), `expectation-loosened` 17 of 29, `check-removed` 40 of 164 (24%), `gate-loosened` 4 of 20, `check-cant-fail` 0 of 11, `matcher-swapped` 0 of 2, `matcher-loosened` 1 of 1. Almost half the findings (202) were about tests the PR itself added and then weakened, and they were right about as often as the rest (42%, against 41% for findings about what the main branch had).

Most right findings are `intended` (137): tests deleted or cut down along with the code they covered. 42 are `unexplained`, most of them in one immich PR ("refactor: web tests") that dropped tests for components that still exist, and Mailyond's `71110c0`.

The 245 wrong ones, most common first:
- **A test rewritten for a deliberate behaviour change**, still checking the same input against the new result, reported as a removed check or a deleted test instead of a changed outcome. Often the test was renamed too, or split into new tests (hono's percent-encoded paths now giving 404, a tRPC formula changed from square root to linear).
- **A check rewritten into an equivalent or stronger form that matching doesn't pair:** an `it.each` table replacing a list of tests, a new API or state field asserting the same value, a check moved into a helper the test calls through an object (immich's `ctx.assertSyncResponse`, 22 findings; fixed since) or into a fixture (`app.dialog`).
- **Tests or checks moved to another file**, which matching doesn't look for: immich moved component tests into page specs and Playwright specs, hono moved adapter tests into their own packages and CI workflow.
- **Smaller mistakes, each with a clear fix:**
  - `await expect.element(…)` (vitest browser mode) isn't read as a check (7 in immich). Fixed since.
  - `check-cant-fail` fires on a loop over a literal non-empty array, a loop with a constant bound, and a local helper the test always calls: all 11 of its findings. Fixed since, except for an `if` in a loop that skips one entry (tRPC), which only running the code would tell.
  - Weakening of code the PR added was decided without looking at the PR's head: a test the PR added, deleted and put back, or moved to another file, was still reported. Fixed since (below).
  - `.each` rows are counted from the outer `describe.each` when the table is an expression (`name === 'Checkbox' ? […] : […]`). Fixed since.
  - A field taken out of `toMatchObject` but asserted on its own line (`toBeCloseTo(239, 1)`) is reported as no longer checked. Fixed since; a plain `toBe(239)` on its own line was reported too.
  - An added `--project` in a vitest command is read as replacing the others. Fixed since: a script calling another script is read through it.
  - Stacked PRs: after the PR below is squash-merged, the PR above still holds its commits, so their findings are reported again (7 in tRPC). GitHub's own diff shows those commits too until a rebase, so the Action would as well. The backtest checks a stacked PR on its own only when its commits no longer merge cleanly into what landed.
  - A few removals are reported twice, once against the main branch and once as added by the PR under the file's old name.

The next PRs fix these, one group at a time, each measured with the backtest.

**Since then.** Weakening of code the PR added is hidden by default and decided against the PR's head. 8 such findings turned out undone: 6 false alarms, a check that came back, and a test rewritten so the rules find it no weaker. The labels now hold 416 findings:
- shown by default: 222, **41% right** (53% at high confidence, 68 of 128);
- added by their PR, hidden: 194, 43% right.

What a PR check shows by default halved; its precision didn't move, since that comes from the rule fixes. Still reported: a PR that puts the same tests in two files and later deletes one copy (vueuse), because the other file didn't change after that commit. The extra comparisons cost time: Excalidraw's 100 PRs take 6.4s instead of 4.0s. Most of it is parsing the same files again, which caching parsed files by git blob would remove.

Then the smaller mistakes above were fixed: `expect.element`, loops that always run, functions a test declares and calls, keys checked on their own line, `.each` tables level by level, and scripts calling scripts. 21 false alarms went, and nothing right did. The labels now hold 395 findings:
- shown by default: 211, **44% right** (58% at high confidence, 68 of 118);
- added by their PR, hidden: 184, 46% right.

Two immich findings stay: checks rewritten into `expect.element` on new locators, which don't pair with the old ones (the rewrites above).

Then methods became helpers. immich had moved its sync checks into `ctx.assertSyncResponse(response, records)`, a method of a context class its tests get from a `setup()` function, which checks the last record, then the others in any order, and how many there are. Following methods by name, filling arguments into subjects and pairing a list with checks on its parts took away those 22 false alarms, all at high confidence. In their place are 4 true findings that two records are now checked in any order (low), and one false alarm in Excalidraw: a test that stopped calling `mouse.select()`, whose `API.clearSelection()` checks that the selection is empty, a setup check. Nothing right was lost. The labels now hold 378 findings:
- shown by default: 194, **50% right** (70% at high confidence, 68 of 97);
- added by their PR, hidden: 184, 46% right.

Following methods costs time where tests call many of them: Excalidraw's 100 PRs take 5.6s instead of 4.5s, immich's 2.9s instead of 1.9s (most of it in the sync refactor and a 139-file rewrite of its web tests), the whole backtest 10–20% more. What's left of the wrong findings is mostly tests rewritten for a behaviour change (renamed, new expected values on the same inputs) and tests moved into other files as different tests. To matching, those look exactly like a behaviour removed on purpose with its tests, so they're for the LLM judge (step 7).

### Tests

In `crates/assertrust/tests/output.rs`, on temporary repos:
- a PR-style finding naming each commit, weakening undone later shown only with `--show-transient`, and a new test loosened by its PR hidden, shown with `--show-pr-added` or `show-pr-added`, and never failing
- a test the PR added, deleted and put back, or moved into another file, undone later; one it loosened and then deleted, renamed as it weakened it and then deleted, weakened as its file was renamed, or weakened and then moved into another file, still added by it; and the same line lost by another test not keeping an undone weakening alive
- allow comments in a test, a helper and a CI workflow, a trailer for a deleted test, an allow without a reason, and an allow left from an earlier change
- a PR-style finding naming only commits about its own test, a grouped deletion naming the commits that deleted its tests, weakening undone in a renamed file or test, and a file broken in a middle commit left without a note
- grouped deletions, exit codes for each `fail-on` level, and `assertrust.toml` (`fail-on`, `[rules]`, `[files]`, a typo, a PR loosening it, an allow comment in it)
- an allow comment copied into another test, and changed outcomes needing `-v` even when allowed or undone later
- a snapshot of the JSON output, the GitHub annotations and job summary, and both counting findings as the terminal does

In `crates/assertrust/tests/backtest.rs`: a history with a PR merged with a merge commit, a squash-merged PR with its commits fetched and one without, and a push, each checked the way CI would have; a range and the labels file picking the commits; each commit checked with the `assertrust.toml` it had; a PR that can't be checked; labels giving precision, carried over to a finding printed differently, and what's new or gone, then rewritten by `--update-labels`; and the JSON output. `labels.rs` has unit tests for reading back what it writes, typos, and pairing.

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
