# assertrust

**Can you trust your green CI?**

assertrust catches changes that quietly make a test suite weaker: an assertion removed, a matcher loosened, a test skipped, a CI gate relaxed. Coding agents do this to get CI green. So do tired humans.

```
test/api/forwarding.test.ts:27  HIGH  expectation-loosened
  toMatchObject no longer checks domainIds, status · 10d8742
```

> **Status: pre-alpha.** `assertrust check` runs, with rules for a check removed, a test disabled (skipped, `.only`, deleted), a check that can no longer fail (moved into an `if` or a callback, `await` dropped, …), a looser matcher (`toEqual` → `toBeTruthy`), an expectation that accepts more (fields, regex alternatives or `.each` rows dropped), and CI or test config that lets failures through (`|| true`, retries, a lower coverage threshold, …). Checks inside helper functions and hooks count for the tests that use them, including helpers imported from test folders, so a weakened `expectOk()` is caught even when no test file changed. It warns by default; `--fail-on high` makes it fail. Findings come as text, JSON or GitHub annotations, each named after the commit that made it, and a deliberate change can be acknowledged with a comment or commit trailer that gives a reason. `assertrust backtest` replays a branch's history the way CI would have checked it, PR by PR, to show what it would have flagged before you turn it on. The GitHub Action is next. Three debugging commands show what it sees: `assertrust changes` (which commits and files a check looks at), `assertrust tests <file>` (the tests and checks it finds in a file, or a CI or test config's gate facts) and `assertrust pairs` (which tests and checks before a change correspond to which after it). The design is in [docs/architecture.md](docs/architecture.md), and it's being built in small public PRs.

```sh
assertrust check                          # this branch, PR-style, against the default branch
assertrust check --range main..HEAD       # each commit on its own
assertrust check --worktree               # uncommitted changes
assertrust backtest --last 100            # what it would have flagged in the last 100 PRs and pushes
```

## What it will do

- Check every commit and each PR as a whole, and name the commit that weakened a test.
- Use deterministic rules first, for TS/JS tests written with vitest, jest or Playwright. No API key needed.
- Offer an optional AI judge for ambiguous cases, using your own key with any provider. It answers one question: does the test still check what it promises?
- Run in your terminal, as a GitHub Action, and inside Claude Code right after an agent edits a test.

## Roadmap

- [ ] v0.1: CLI and GitHub Action for TS/JS
- [ ] AI judge, npm package, Claude Code plugin
- [ ] More languages

## Contributing

See [docs/development.md](docs/development.md) for building and testing.

## License

[Apache-2.0](LICENSE)
