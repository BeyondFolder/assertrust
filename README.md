# assertrust

**Can you trust your green CI?**

assertrust catches changes that quietly make a test suite weaker: an assertion removed, a matcher loosened, a test skipped, a CI gate relaxed. Coding agents do this to get CI green. So do tired humans.

```
test/api/forwarding.test.ts:27  HIGH  expectation-loosened
  toMatchObject no longer checks domainIds, status · 10d8742
```

> **Status: pre-alpha.** `assertrust check` runs, with rules for a check removed, a test disabled (skipped, `.only`, deleted), a check that can no longer fail (moved into an `if` or a callback, `await` dropped, …), a looser matcher (`toEqual` → `toBeTruthy`), an expectation that accepts more (fields, regex alternatives or `.each` rows dropped), and CI or test config that lets failures through (`|| true`, retries, a lower coverage threshold, …). It only warns for now. Output formats and the GitHub Action are next. Three debugging commands show what it sees: `assertrust changes` (which commits and files a check looks at), `assertrust tests <file>` (the tests and checks it finds in a file, or a CI or test config's gate facts) and `assertrust pairs` (which tests and checks before a change correspond to which after it). The design is in [docs/architecture.md](docs/architecture.md), and it's being built in small public PRs.

```sh
assertrust check                          # this branch, PR-style, against the default branch
assertrust check --range main..HEAD       # each commit on its own
assertrust check --worktree               # uncommitted changes
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
