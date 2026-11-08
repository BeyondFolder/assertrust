# assertrust

**Can you trust your green CI?**

assertrust catches changes that quietly make a test suite weaker: an assertion removed, a matcher loosened, a test skipped, a CI gate relaxed. Coding agents do this to get CI green. So do tired humans.

```
test/api/forwarding.test.ts:27  HIGH  expectation-loosened
  toMatchObject no longer checks status, domainIds · 10d8742
```

> **Status: pre-alpha. The check itself doesn't run yet.** Two debugging commands work: `assertrust changes` (which commits and files a check would look at) and `assertrust tests <file>` (the tests and checks it finds in a file). The design is in [docs/architecture.md](docs/architecture.md), and it's being built in small public PRs.

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
