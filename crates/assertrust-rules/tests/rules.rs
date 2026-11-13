//! The rules on small before/after files: at least one case each rule must flag and one it
//! must not, per `docs/architecture.md`, step 5.

use assertrust_ir::{FileIr, parse_file};
use assertrust_rules::{Side, TestFile, check};

/// Findings for one file before and after, as `rule confidence line: message`.
fn findings(before: &str, after: &str) -> Vec<String> {
    findings_in("a.test.ts", before, after)
}

fn findings_in(path: &str, before: &str, after: &str) -> Vec<String> {
    let before = parse_file(path, before).expect("before parses");
    let after = parse_file(path, after).expect("after parses");
    render(&[TestFile {
        before: Some(Side { path, ir: &before }),
        after: Some(Side { path, ir: &after }),
        renamed_out: None,
    }])
}

fn render(files: &[TestFile]) -> Vec<String> {
    check(files, &[])
        .iter()
        .map(|finding| {
            let line = finding.line.map_or("-".to_owned(), |line| line.to_string());
            format!(
                "{} {} {line}: {}",
                finding.rule, finding.confidence, finding.message
            )
        })
        .collect()
}

fn ir(source: &str) -> FileIr {
    parse_file("a.test.ts", source).expect("parses")
}

// check-removed

#[test]
fn a_removed_check_is_flagged() {
    assert_eq!(
        findings(
            "it('refunds', () => {\n  expect(res.status).toBe(200);\n  expect(res.body).toEqual({ ok: true });\n})",
            "it('refunds', () => {\n  expect(res.status).toBe(200);\n})",
        ),
        ["check-removed high 1: removed expect(res.body).toEqual({ ok: true })"]
    );
}

#[test]
fn a_removed_check_says_what_the_test_name_dropped() {
    // The shape of Mailyond's 038cf0b: the test renamed and refactored, one check gone.
    assert_eq!(
        findings(
            "it('puts the quoted reply into the text or preview', () => {
               const html = '<p>Hi there</p><blockquote>On Monday, Ada wrote</blockquote>';
               const fields = bodyFields({ HtmlBody: html });
               expect(fields.text).toBe('Hi there');
               expect(fields.preview).toBe('Hi there');
             })",
            "it('puts the quoted reply into the text', () => {
               const html = '<p>Hi there</p><blockquote>On Monday, Ada wrote</blockquote>';
               expect(textFromHtml(html)).toBe('Hi there');
             })",
        ),
        [
            "check-removed high 1: removed expect(fields.preview).toBe('Hi there'); the test name dropped \"or preview\""
        ]
    );
}

#[test]
fn a_test_left_without_checks_says_so() {
    assert_eq!(
        findings(
            "it('saves', () => { save(); expect(saved()).toBe(true) })",
            "it('saves', () => { save() })",
        ),
        ["check-removed high 1: removed expect(saved()).toBe(true); the test has no checks left"]
    );
}

#[test]
fn a_removed_check_in_a_rewritten_test_is_less_sure() {
    assert_eq!(
        findings(
            "it('loads', () => { expect(fetchMock).toHaveBeenCalledWith('/a') })",
            "it('loads', () => { expect(resolver.calls).toEqual(['/a']) })",
        ),
        ["check-removed medium 1: removed expect(fetchMock).toHaveBeenCalledWith('/a')"]
    );
}

#[test]
fn checks_that_moved_or_were_rewritten_are_not_removed() {
    // Moved to another test, written as the same fact, and reformatted.
    let before = "
it('checks the account', () => {
  expect(user.name).toBe('Ada');
  expect(user.plan).toBe('pro');
  expect(res.body.id).toBe(1);
});";
    let after = "
it('checks the account', () => {
  expect(user.name).toBe( 'Ada' );
  expect(res.body).toMatchObject({ id: 1 });
});
it('checks the plan', () => { expect(user.plan).toBe('pro') });";
    assert_eq!(findings(before, after), [""; 0]);
}

// test-disabled

#[test]
fn a_skipped_test_is_flagged() {
    assert_eq!(
        findings(
            "it('refunds', () => { expect(a).toBe(1) })",
            "it.skip('refunds', () => { expect(a).toBe(1) })",
        ),
        ["test-disabled high 1: now skipped (skip)"]
    );
}

#[test]
fn a_skipped_describe_is_one_finding() {
    assert_eq!(
        findings(
            "describe('billing', () => {\n  it('a', () => { expect(a).toBe(1) });\n  it('b', () => { expect(b).toBe(1) });\n});",
            "describe.skip('billing', () => {\n  it('a', () => { expect(a).toBe(1) });\n  it('b', () => { expect(b).toBe(1) });\n});",
        ),
        ["test-disabled high 2: 2 tests now skipped (skip)"]
    );
}

#[test]
fn modifiers_that_stop_a_test_sometimes_or_invert_it() {
    let before = "
it('a', () => { expect(a).toBe(1) });
it('b', () => { expect(b).toBe(1) });
it('c', () => { expect(c).toBe(1) });
it('d', () => { expect(d).toBe(1) });
it('e', () => {});";
    let after = "
it.skipIf(isCI)('a', () => { expect(a).toBe(1) });
it.runIf(hasDb)('b', () => { expect(b).toBe(1) });
it.fails('c', () => { expect(c).toBe(1) });
it.todo('d');
it.concurrent('e', () => {});";
    assert_eq!(
        findings(before, after),
        [
            "test-disabled high 4: now expected to fail (fails), so it passes only if it fails",
            "test-disabled high 5: now marked todo, so it doesn't run",
            "test-disabled medium 2: now skipped when isCI (skipIf)",
            "test-disabled medium 3: now skipped unless hasDb (runIf)",
        ]
    );
}

#[test]
fn a_skip_decided_while_running_is_flagged() {
    let before = "
test('a', async ({ page }) => { await expect(page).toHaveTitle('A') });
test('b', async ({ page }) => { await expect(page).toHaveTitle('B') });
it('c', (ctx) => { expect(c).toBe(1) });";
    let after = "
test('a', async ({ page, browserName }) => {
  test.skip(browserName === 'webkit', 'flaky');
  await expect(page).toHaveTitle('A');
});
test('b', async ({ page }) => { test.skip(); await expect(page).toHaveTitle('B') });
it('c', (ctx) => { if (!db) ctx.skip(); expect(c).toBe(1) });";
    assert_eq!(
        findings_in("a.spec.ts", before, after),
        [
            "test-disabled high 6: now skipped while running",
            "test-disabled medium 3: now skipped while running when browserName === 'webkit'",
            "test-disabled medium 7: now skipped while running (in if)",
        ]
    );
}

#[test]
fn a_committed_only_silences_the_rest_of_the_file() {
    let before = "it('a', () => {}); it('b', () => {}); it('c', () => {});";
    assert_eq!(
        findings(
            before,
            "it.only('a', () => {}); it('b', () => {}); it('c', () => {});"
        ),
        ["test-disabled high 1: only added: the other 2 tests in this file won't run"]
    );
    // Already there before: not this change's doing.
    let focused = "it.only('a', () => {}); it('b', () => {});";
    assert_eq!(
        findings(focused, &format!("{focused} it('c', () => {{}});")),
        [""; 0]
    );
}

#[test]
fn unskipping_or_keeping_a_skip_is_fine() {
    assert_eq!(
        findings(
            "it.skip('a', () => { expect(a).toBe(1) }); it.skipIf(isCI)('b', () => {});",
            "it('a', () => { expect(a).toBe(1) }); it.skipIf(isCI)('b', () => { expect(b).toBe(2) });",
        ),
        [""; 0]
    );
}

#[test]
fn a_deleted_test_is_flagged_even_without_checks() {
    assert_eq!(
        findings(
            "it('refunds', () => { expect(refund(1)).toBe(true) });\nit('renders', () => { render(app) });\nit('keeps', () => {});",
            "it('keeps', () => {});",
        ),
        [
            "test-disabled high -: deleted: refunds",
            "test-disabled high -: deleted: renders",
        ]
    );
}

#[test]
fn a_deleted_test_in_a_file_that_gained_tests_is_less_sure() {
    // Likely rewritten under another name, with a body too different to pair.
    assert_eq!(
        findings(
            "it('stores an email', () => { expect(res).toEqual({ ok: true }) })",
            "it('stores the html body and attachments', () => { expect(row.html).toBe('<p>hi</p>') })",
        ),
        ["test-disabled medium -: deleted: stores an email"]
    );
}

#[test]
fn a_test_split_in_two_is_not_deleted() {
    assert_eq!(
        findings(
            "it('checks the account', () => { expect(name).toBe('Ada'); expect(plan).toBe('pro') })",
            "it('checks the name', () => { expect(name).toBe('Ada') });
             it('checks the plan', () => { expect(plan).toBe('pro') })",
        ),
        [""; 0]
    );
}

#[test]
fn a_deleted_or_renamed_out_file_is_one_finding() {
    let before = ir("it('a', () => { expect(a).toBe(1) }); it('b', () => { expect(b).toBe(1) });");
    let side = Some(Side {
        path: "test/dca.test.ts",
        ir: &before,
    });
    assert_eq!(
        render(&[TestFile {
            before: side,
            after: None,
            renamed_out: None,
        }]),
        ["test-disabled high -: file deleted: its 2 tests no longer run"]
    );
    assert_eq!(
        render(&[TestFile {
            before: side,
            after: None,
            renamed_out: Some("test/dca.ts"),
        }]),
        [
            "test-disabled high -: renamed to test/dca.ts, which isn't a test file name: its 2 tests no longer run"
        ]
    );
}

// check-cant-fail

#[test]
fn a_check_moved_where_it_may_not_run_or_fail() {
    let before = "
it('x', () => {
  expect(a).toBe(1);
  expect(b).toBe(2);
  expect(c).toBe(3);
  expect(d).toBe(4);
  expect(e).toBe(5);
  expect(f).toBe(6);
});";
    let after = "
it('x', () => {
  if (ready) {
    expect(a).toBe(1);
  }
  items.forEach(() => expect(b).toBe(2));
  try { expect(c).toBe(3) } catch {}
  try { load() } catch (err) { expect(d).toBe(4) }
  ok && expect(e).toBe(5);
  for (const item of items) expect(f).toBe(6);
});";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail medium 4: expect(a).toBe(1) is now inside an if: it may not run",
            "check-cant-fail medium 6: expect(b).toBe(2) is now inside a callback: it may run after the test ends, or not at all",
            "check-cant-fail medium 7: expect(c).toBe(3) is now inside a try with a catch: its failure may be caught",
            "check-cant-fail medium 8: expect(d).toBe(4) is now inside a catch: it runs only if something throws",
            "check-cant-fail medium 9: expect(e).toBe(5) is now after &&, || or ??: it may not run",
            "check-cant-fail low 10: expect(f).toBe(6) is now inside a loop: it doesn't run if the loop runs zero times",
        ]
    );
}

#[test]
fn places_that_still_fail_the_test_are_fine() {
    let before = "it('x', async () => { expect(a).toBe(1); if (k) { expect(b).toBe(2) } expect(c).toBe(3) })";
    let after = "
it('x', async () => {
  try { expect(a).toBe(1) } finally { cleanup() }
  if (k) { expect(b).toBe(2) }
  await vi.waitFor(() => { expect(c).toBe(3) });
});";
    assert_eq!(findings(before, after), [""; 0]);
}

#[test]
fn loops_that_always_run_are_fine() {
    // The shapes of zod's from-json-schema test, tRPC's og routes test and Excalidraw's
    // export test: a loop over a written-out array, or with literal bounds, runs its body.
    let before = "
it('x', async () => {
  expect(a).toBe(1);
  expect(b).toBe(2);
  expect(c).toBe(3);
  expect(d).toBe(4);
  expect(e).toBe(5);
});";
    let after = "
it('x', async () => {
  for (const path of ['/a', '/b']) expect(a).toBe(1);
  for (let i = 0; i < 2; i++) { expect(b).toBe(2) }
  for (;;) { expect(c).toBe(3); break }
  while (true) { expect(d).toBe(4); break }
  do { expect(e).toBe(5) } while (next());
});";
    assert_eq!(findings(before, after), [""; 0]);
    // A `break` or `continue` may skip what comes after it; a `break` in a `switch` stays
    // there, a `continue` doesn't.
    let after = "
it('x', async () => {
  while (true) { if (await done()) break; expect(a).toBe(1) }
  for (const x of ['a', 'b']) { continue; expect(b).toBe(2) }
  for (const x of ['a', 'b']) { switch (x) { case 'a': break } expect(c).toBe(3) }
  for (const x of ['a', 'b']) { switch (x) { case 'a': continue } expect(d).toBe(4) }
  expect(e).toBe(5);
});";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail low 3: expect(a).toBe(1) is now inside a loop: it doesn't run if the loop runs zero times",
            "check-cant-fail low 4: expect(b).toBe(2) is now inside a loop: it doesn't run if the loop runs zero times",
            "check-cant-fail low 6: expect(d).toBe(4) is now inside a loop: it doesn't run if the loop runs zero times",
        ]
    );
    // Loops that may run zero times are still flagged.
    let after = "
it('x', async () => {
  for (const path of [...paths]) expect(a).toBe(1);
  for (let i = 0; i < n; i++) { expect(b).toBe(2) }
  for (let i = 2; i < 2; i++) { expect(c).toBe(3) }
  while (more()) { expect(d).toBe(4) }
  for (const path of []) expect(e).toBe(5);
});";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail low 3: expect(a).toBe(1) is now inside a loop: it doesn't run if the loop runs zero times",
            "check-cant-fail low 4: expect(b).toBe(2) is now inside a loop: it doesn't run if the loop runs zero times",
            "check-cant-fail low 5: expect(c).toBe(3) is now inside a loop: it doesn't run if the loop runs zero times",
            "check-cant-fail low 6: expect(d).toBe(4) is now inside a loop: it doesn't run if the loop runs zero times",
            "check-cant-fail low 7: expect(e).toBe(5) is now inside a loop: it doesn't run if the loop runs zero times",
        ]
    );
}

#[test]
fn a_function_the_test_surely_calls_runs_as_part_of_it() {
    // The shape of Excalidraw's tooltip and headless export tests.
    let before = "
it('x', async () => {
  expect(a).toBe(1);
  expect(b).toBe(2);
  expect(c).toBe(3);
});";
    let after = "
it('x', async () => {
  const hover = () => {
    expect(a).toBe(1);
    return tip;
  };
  const exported = async () => {
    expect(b).toBe(2);
  };
  function settle() {
    expect(c).toBe(3);
  }
  const tooltip = hover();
  await exported();
  settle();
});";
    assert_eq!(findings(before, after), [""; 0]);
    // Not surely called: only in an `if` (its checks are in the `if`), not awaited though
    // async, or called from a function that is never called.
    let after = "
it('x', async () => {
  const hover = () => {
    expect(a).toBe(1);
  };
  const exported = async () => {
    expect(b).toBe(2);
  };
  const outer = () => {
    inner();
  };
  const inner = () => {
    expect(c).toBe(3);
  };
  if (ready) hover();
  exported();
});";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail medium 4: expect(a).toBe(1) is now inside an if: it may not run",
            "check-cant-fail medium 7: expect(b).toBe(2) is now inside a callback: it may run after the test ends, or not at all",
            "check-cant-fail medium 13: expect(c).toBe(3) is now inside a callback: it may run after the test ends, or not at all",
        ]
    );
    // Its checks run where it's called: after a `return` before the call, inside a `try`
    // around it.
    let after = "
it('x', async () => {
  const first = () => {
    expect(a).toBe(1);
  };
  const second = () => {
    expect(b).toBe(2);
  };
  try { second() } catch {}
  if (skip) return;
  first();
  expect(c).toBe(3);
});";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail medium 7: expect(b).toBe(2) is now inside a try with a catch: its failure may be caught",
            "check-cant-fail medium 10: a return (in if) now comes before 2 checks: they may not run",
        ]
    );
    // A `let` may hold another function by the time it's called.
    let after = "
it('x', async () => {
  let check = () => {
    expect(a).toBe(1);
  };
  check = () => {};
  check();
  expect(b).toBe(2);
  expect(c).toBe(3);
});";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail medium 4: expect(a).toBe(1) is now inside a callback: it may run after the test ends, or not at all"
        ]
    );
}

#[test]
fn a_check_a_local_function_returns_is_awaited_with_the_call() {
    let before = "it('x', async () => { await expect.element(row).toBeVisible() })";
    assert_eq!(
        findings(
            before,
            "it('x', async () => {
  const visible = () => expect.element(row).toBeVisible();
  await visible();
})",
        ),
        [""; 0]
    );
    assert_eq!(
        findings(
            before,
            "it('x', async () => {
  const visible = () => expect.element(row).toBeVisible();
  visible();
})",
        ),
        [
            "check-cant-fail high 2: await dropped from expect.element(row).toBeVisible(): the test may end before it fails"
        ]
    );
}

#[test]
fn expect_element_is_a_check_that_must_be_awaited() {
    // The shape of immich's move to vitest browser mode.
    assert_eq!(
        findings(
            "it('x', () => { expect(screen.getByText('Hello test')).toBeInTheDocument() })",
            "it('x', async () => { await expect.element(screen.getByText('Hello test')).toBeInTheDocument() })",
        ),
        [""; 0]
    );
    assert_eq!(
        findings(
            "it('x', async () => { await expect.element(row).toBeVisible() })",
            "it('x', async () => { expect.element(row).toBeVisible() })",
        ),
        [
            "check-cant-fail high 1: await dropped from expect.element(row).toBeVisible(): the test may end before it fails"
        ]
    );
}

#[test]
fn an_early_return_before_checks_is_flagged() {
    let before = "
it('a', () => {
  expect(a).toBe(1);
  expect(b).toBe(2);
});
it('b', () => {
  expect(c).toBe(3);
});";
    let after = "
it('a', () => {
  expect(a).toBe(1);
  return;
  expect(b).toBe(2);
});
it('b', () => {
  if (!c) return;
  expect(c).toBe(3);
});";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail high 4: a return now comes before 1 check: it never runs",
            "check-cant-fail medium 8: a return (in if) now comes before 1 check: it may not run",
        ]
    );
    // The return was already there.
    assert_eq!(findings(after, after), [""; 0]);
}

#[test]
fn dropping_await_from_a_check_that_returns_a_promise() {
    let before = "
it('x', async () => {
  await expect(load()).resolves.toBe('done');
  await expect(a).toBe(1);
  await expect.poll(() => count()).toBe(2);
});";
    let after = "
it('x', async () => {
  expect(load()).resolves.toBe('done');
  expect(a).toBe(1);
  expect.poll(() => count()).toBe(2);
});";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail high 3: await dropped from expect(load()).resolves.toBe('done'): the test may end before it fails",
            "check-cant-fail high 5: await dropped from expect.poll(() => count()).toBe(2): the test may end before it fails",
        ]
    );
    // Playwright's web-first matchers wait too.
    assert_eq!(
        findings_in(
            "a.spec.ts",
            "test('x', async ({ page }) => { await expect(page.getByText('Saved')).toBeVisible() })",
            "test('x', async ({ page }) => { expect(page.getByText('Saved')).toBeVisible() })",
        ),
        [
            "check-cant-fail high 1: await dropped from expect(page.getByText('Saved')).toBeVisible(): the test may end before it fails"
        ]
    );
}

#[test]
fn a_bare_expect_checks_nothing() {
    assert_eq!(
        findings(
            "it('x', () => { expect(total).toBe(3) })",
            "it('x', () => { expect(total) })",
        ),
        ["check-cant-fail high 1: expect(total) has no matcher: it checks nothing"]
    );
}

#[test]
fn expect_assertions_removed_or_lowered() {
    let before = "
it('a', async () => { expect.assertions(2); await go(); expect(a).toBe(1); expect(b).toBe(2) });
it('b', async () => { expect.assertions(2); await go(); expect(a).toBe(1); expect(b).toBe(2) });
it('c', async () => { expect.assertions(2); await go(); expect(a).toBe(1); expect(b).toBe(2) });
it('d', async () => { expect.assertions(1); await go(); expect(a).toBe(1) });";
    let after = "
it('a', async () => { await go(); expect(a).toBe(1); expect(b).toBe(2) });
it('b', async () => { expect.assertions(1); await go(); expect(a).toBe(1); expect(b).toBe(2) });
it('c', async () => { expect.hasAssertions(); await go(); expect(a).toBe(1); expect(b).toBe(2) });
it('d', async () => { expect.assertions(2); await go(); expect(a).toBe(1); expect(b).toBe(2) });";
    assert_eq!(
        findings(before, after),
        [
            "check-cant-fail medium 2: expect.assertions(2) removed",
            "check-cant-fail medium 3: expect.assertions lowered from 2 to 1",
            "check-cant-fail medium 4: expect.assertions(2) replaced with expect.hasAssertions()",
        ]
    );
}

#[test]
fn reformatting_finds_nothing() {
    let before = "
describe('billing', () => {
  it.skipIf(isCI)('refunds', async () => {
    if (ready) { expect(res.status).toBe(200) }
    await expect(load()).resolves.toEqual({ ok: true, id: 1 })
  })
})";
    let after = "describe('billing',()=>{it.skipIf(isCI)('refunds',async()=>{if(ready){expect(res.status).toBe(200);}\nawait expect(load()).resolves.toEqual({id:1,ok:true});});});";
    assert_eq!(findings(before, after), [""; 0]);
}

// Found in code review

#[test]
fn a_test_that_did_not_run_stops_nothing() {
    // A placeholder deleted, and one skip swapped for another.
    assert_eq!(
        findings(
            "it.todo('handles refunds');\nit.skip('retries', () => { expect(a).toBe(1) });\nit('keeps', () => {});",
            "it.todo('retries');\nit('keeps', () => {});",
        ),
        [""; 0]
    );
    // A deleted file whose tests were all skipped.
    let before = ir("it.skip('a', () => { expect(a).toBe(1) });");
    assert_eq!(
        render(&[TestFile {
            before: Some(Side {
                path: "a.test.ts",
                ir: &before,
            }),
            after: None,
            renamed_out: None,
        }]),
        [""; 0]
    );
}

#[test]
fn a_test_that_skips_itself_while_running_is_one_finding() {
    assert_eq!(
        findings(
            "it('a', (ctx) => { expect(a).toBe(1); expect(b).toBe(2) });\nit('b', (ctx) => { expect(c).toBe(3) });",
            "it('a', (ctx) => { ctx.skip(); expect(a).toBe(1) });\nit.skip('b', (ctx) => { ctx.skip(); expect(c).toBe(3) });",
        ),
        [
            "test-disabled high 1: now skipped while running",
            "test-disabled high 2: now skipped (skip)",
        ]
    );
}

#[test]
fn a_return_that_is_no_longer_conditional() {
    assert_eq!(
        findings(
            "it('x', () => {\n  if (!res) return;\n  expect(res.status).toBe(200);\n});",
            "it('x', () => {\n  return;\n  expect(res.status).toBe(200);\n});",
        ),
        ["check-cant-fail high 2: a return now comes before 1 check: it never runs"]
    );
}

#[test]
fn a_plain_check_folded_into_one_inside_an_if() {
    assert_eq!(
        findings(
            "it('x', () => {\n  expect(r.a).toBe(1);\n  if (x) { expect(r.b).toBe(2) }\n});",
            "it('x', () => {\n  if (y) {\n    expect(r).toMatchObject({ a: 1, b: 2 });\n  }\n});",
        ),
        [
            "check-cant-fail medium 3: expect(r).toMatchObject({ a: 1, b: 2 }) is now inside an if: it may not run"
        ]
    );
}

// matcher-loosened

/// Findings when one check in a test changes from `before` to `after`.
fn one_check(before: &str, after: &str) -> Vec<String> {
    findings(
        &format!("it('x', async () => {{ {before} }})"),
        &format!("it('x', async () => {{ {after} }})"),
    )
}

#[test]
fn a_matcher_down_the_ladder_is_looser() {
    let cases = [
        (
            "expect(user).toEqual({ id: 1 })",
            "expect(user).toBeTruthy()",
            "matcher-loosened high 1: toEqual({ id: 1 }) → toBeTruthy(): now any truthy value passes",
        ),
        (
            "expect(send).toHaveBeenCalledWith('a@b.com')",
            "expect(send).toHaveBeenCalled()",
            "matcher-loosened high 1: toHaveBeenCalledWith('a@b.com') → toHaveBeenCalled(): now any call passes",
        ),
        (
            "expect(ok).toBe(true)",
            "expect(ok).toBeTruthy()",
            "matcher-loosened high 1: toBe(true) → toBeTruthy(): now any truthy value passes",
        ),
        (
            "expect(order).toEqual(expected)",
            "expect(order).toMatchObject(expected)",
            "matcher-loosened medium 1: toEqual(expected) → toMatchObject(expected): now extra fields pass",
        ),
        (
            "expect(title).toBe('Invoice 42')",
            "expect(title).toContain('Invoice')",
            "matcher-loosened medium 1: toBe('Invoice 42') → toContain('Invoice'): now a partial match passes",
        ),
        (
            "expect(total).toBe(1200)",
            "expect(total).toBeGreaterThan(0)",
            "matcher-loosened medium 1: toBe(1200) → toBeGreaterThan(0): now a range of values passes",
        ),
    ];
    for (before, after, expected) in cases {
        assert_eq!(one_check(before, after), [expected], "{before} → {after}");
    }
    assert_eq!(
        findings_in(
            "a.spec.ts",
            "test('x', async () => { await expect(link).toHaveAttribute('href', '/billing') })",
            "test('x', async () => { await expect(link).toHaveAttribute('href') })",
        ),
        ["expectation-loosened high 1: toHaveAttribute no longer checks the value of 'href'"]
    );
    assert_eq!(
        findings_in(
            "a.spec.ts",
            "test('x', async ({ page }) => { await expect(page.getByRole('status')).toHaveText('Saved') })",
            "test('x', async ({ page }) => { await expect(page.getByRole('status')).toBeVisible() })",
        ),
        [
            "matcher-loosened high 1: toHaveText('Saved') → toBeVisible(): now only the element's state is checked"
        ]
    );
}

#[test]
fn the_same_check_written_another_way_is_fine() {
    let cases = [
        // Stricter.
        (
            "expect(user).toBeTruthy()",
            "expect(user).toEqual({ id: 1 })",
        ),
        (
            "expect(order).toEqual(expected)",
            "expect(order).toStrictEqual(expected)",
        ),
        // The same check.
        ("expect(n).toBe(5)", "expect(n).toEqual(5)"),
        ("expect(x).toBe(null)", "expect(x).toBeNull()"),
        ("expect(x).not.toBeUndefined()", "expect(x).toBeDefined()"),
        ("expect(fn).toBeCalled()", "expect(fn).toHaveBeenCalled()"),
        ("expect(f).toThrowError('bad')", "expect(f).toThrow('bad')"),
        // Part of the check moved into the subject, or out of it.
        (
            "expect(reply.text).toBe('hi')",
            "expect(reply).toMatchObject({ text: 'hi' })",
        ),
        (
            "expect(list.length).toBe(2)",
            "expect(list).toHaveLength(2)",
        ),
        (
            "await expect(load()).resolves.toEqual({ ok: true })",
            "expect(await load()).toEqual({ ok: true })",
        ),
        ("expect(a !== b).toBe(true)", "expect(a).not.toBe(b)"),
        // Fields added, an alternative removed.
        (
            "expect(r).toMatchObject({ a: 1 })",
            "expect(r).toMatchObject({ a: 1, b: 2 })",
        ),
        ("expect(s).toMatch(/a|b/)", "expect(s).toMatch(/a/)"),
    ];
    for (before, after) in cases {
        assert_eq!(one_check(before, after), [""; 0], "{before} → {after}");
    }
}

// matcher-swapped

#[test]
fn a_matcher_swapped_at_the_same_level() {
    let cases = [
        (
            "expect(user).toBe(admin)",
            "expect(user).toEqual(admin)",
            "matcher-swapped low 1: swapped toBe(admin) for toEqual(admin)",
        ),
        (
            "expect(log).toContain('sent')",
            "expect(log).toMatch(/sent/)",
            "matcher-swapped low 1: swapped toContain('sent') for toMatch(/sent/)",
        ),
        // Under `.not`, a looser matcher makes a stricter check, so nothing is ranked.
        (
            "expect(a).not.toEqual(b)",
            "expect(a).not.toBe(b)",
            "matcher-swapped low 1: swapped .not.toEqual(b) for .not.toBe(b)",
        ),
    ];
    for (before, after, expected) in cases {
        assert_eq!(one_check(before, after), [expected], "{before} → {after}");
    }
}

// expectation-loosened

#[test]
fn the_same_matcher_accepting_more() {
    let cases = [
        // The shape of Mailyond's 10d8742.
        (
            "expect(res.body).toMatchObject({ email: 'me@x.com', status: 'pending', domainIds: null })",
            "expect(res.body).toMatchObject({ email: 'me@x.com' })",
            "expectation-loosened high 1: toMatchObject no longer checks domainIds, status",
        ),
        (
            "expect(r).toMatchObject({ user: { id: 1, name: 'Ada' } })",
            "expect(r).toMatchObject({ user: { id: 1 } })",
            "expectation-loosened high 1: toMatchObject no longer checks user.name",
        ),
        (
            "expect(r).toMatchObject({ id: 7, status: 'paid' })",
            "expect(r).toMatchObject({ id: expect.any(Number), status: 'paid' })",
            "expectation-loosened high 1: toMatchObject now accepts any id (expect.any(Number))",
        ),
        (
            "expect(r).toEqual({ id: 7 })",
            "expect(r).toEqual(expect.objectContaining({ id: 7 }))",
            "expectation-loosened high 1: toEqual now allows extra fields",
        ),
        (
            "expect(tables).toEqual(expect.arrayContaining(['users', 'emails']))",
            "expect(tables).toEqual(expect.arrayContaining(['users']))",
            "expectation-loosened high 1: toEqual no longer checks 'emails'",
        ),
        // The shape of Mailyond's 71110c0.
        (
            "expect(page).not.toMatch(/support@|resend|inboxes\\.net/)",
            "expect(page).not.toMatch(/support@|inboxes\\.net/)",
            "expectation-loosened high 1: .not.toMatch no longer rules out resend",
        ),
        (
            "expect(kind).toMatch(/^(inbox|sent)$|draft/)",
            "expect(kind).toMatch(/^(inbox|sent)$|draft|spam/)",
            "expectation-loosened high 1: toMatch now also accepts spam",
        ),
        (
            "expect(() => pay()).toThrow('card declined')",
            "expect(() => pay()).toThrow()",
            "expectation-loosened high 1: toThrow no longer checks which error",
        ),
        (
            "expect(r).toHaveProperty('user.id', 1)",
            "expect(r).toHaveProperty('user.id')",
            "expectation-loosened high 1: toHaveProperty no longer checks the value of 'user.id'",
        ),
        (
            "expect(r).not.toMatchObject({ role: 'admin' })",
            "expect(r).not.toMatchObject({ role: 'admin', active: true })",
            "expectation-loosened high 1: .not.toMatchObject now fails only if active match too",
        ),
        (
            "expect(body).toMatch('Hello there')",
            "expect(body).toMatch('Hello')",
            "expectation-loosened medium 1: toMatch now only needs 'Hello'",
        ),
        (
            "expect(count).toBeGreaterThan(5)",
            "expect(count).toBeGreaterThan(3)",
            "expectation-loosened medium 1: toBeGreaterThan lower bound lowered from 5 to 3",
        ),
        (
            "expect(ms).toBeLessThanOrEqual(100)",
            "expect(ms).toBeLessThanOrEqual(500)",
            "expectation-loosened medium 1: toBeLessThanOrEqual upper bound raised from 100 to 500",
        ),
        (
            "expect(sum).toBeCloseTo(0.3, 5)",
            "expect(sum).toBeCloseTo(0.3, 1)",
            "expectation-loosened medium 1: toBeCloseTo precision lowered from 5 to 1 digits",
        ),
        // Split, with part of the check left behind.
        (
            "expect(r).toMatchObject({ a: 1, b: 2 })",
            "expect(r.a).toBe(1)",
            "expectation-loosened high 1: no longer checks r.b == 2",
        ),
        // Compared to fewer than one decimal digit, it's not the same number anymore.
        (
            "expect(y).toBe(239)",
            "expect(y).toBeCloseTo(239, 0)",
            "matcher-loosened medium 1: toBe(239) → toBeCloseTo(239, 0): now a range of values passes",
        ),
    ];
    for (before, after, expected) in cases {
        assert_eq!(one_check(before, after), [expected], "{before} → {after}");
    }
}

#[test]
fn a_number_compared_closely_is_the_same_check() {
    for (before, after) in [
        ("expect(y).toBe(239)", "expect(y).toBeCloseTo(239, 1)"),
        ("expect(y).toEqual(0.3)", "expect(y).toBeCloseTo(0.3)"),
        (
            "expect(r).toMatchObject({ y: 239 })",
            "expect(r.y).toBeCloseTo(239, 1)",
        ),
    ] {
        assert_eq!(one_check(before, after), [""; 0], "{before} → {after}");
    }
}

#[test]
fn a_key_checked_on_its_own_line_is_still_checked() {
    // The shape of Excalidraw's transform test: placement became computed, so y moved out of
    // the object into a close comparison.
    assert_eq!(
        findings(
            "it('x', () => {
  expect(arrow).toMatchObject({ type: 'arrow', x: 255.5, y: 239 });
  expect(text).toMatchObject({ type: 'text', x: 240, y: 226.5 });
})",
            "it('x', () => {
  expect(arrow.y).toBeCloseTo(239, 1);
  expect(arrow).toMatchObject({ type: 'arrow', x: 255.5 });
  expect(text.x).toBe(240);
  expect(text.y).toBeCloseTo(226.5, 1);
  expect(text).toMatchObject({ type: 'text' });
})",
        ),
        [""; 0]
    );
    // Not by a check that can't fail: a promise check the test doesn't wait for, or one
    // after a `return` the object's check doesn't come after.
    assert_eq!(
        findings(
            "it('x', async () => {
  await expect(p).resolves.toMatchObject({ a: 1, y: 2 });
  expect(r).toMatchObject({ a: 1, y: 2 });
})",
            "it('x', async () => {
  await expect(p).resolves.toMatchObject({ a: 1 });
  expect(p).resolves.toHaveProperty('y', 2);
  expect(r).toMatchObject({ a: 1 });
  if (!ok) return;
  expect(r.y).toBe(2);
})",
        ),
        [
            "expectation-loosened high 2: toMatchObject no longer checks y",
            "expectation-loosened high 4: toMatchObject no longer checks y",
        ]
    );
    // Only where it runs whenever the object's check does; the rest is still reported.
    assert_eq!(
        findings(
            "it('x', () => {
  expect(r).toMatchObject({ a: 1, b: 2, c: { d: 3, e: 4 } });
})",
            "it('x', () => {
  if (ready) expect(r.b).toBe(2);
  expect(r.c.d).toBe(3);
  expect(r).toMatchObject({ a: 1 });
})",
        ),
        ["expectation-loosened high 4: toMatchObject no longer checks b, c"]
    );
}

#[test]
fn each_tables_are_compared_level_by_level() {
    // The shape of nuxt/ui's Tooltip test: the test moved into a `describe.each`, and its own
    // table became an expression.
    assert_eq!(
        findings(
            "describe('Tooltip', () => {
  it.each([undefined, 'list', 'card'])('opens with variant %s', (variant) => { expect(open(variant)).toBe(true) })
})",
            "describe('Tooltip', () => {
  describe.each([{ name: 'Checkbox' }, { name: 'Switch' }])('with $name', ({ name }) => {
    it.each(name === 'Checkbox' ? [undefined, 'list', 'card'] : [undefined])('opens with variant %s', (variant) => { expect(open(variant)).toBe(true) })
  })
})",
        ),
        [""; 0]
    );
    // A test moved into a `describe.each`: its own table is still compared.
    assert_eq!(
        findings(
            "it.each(['pro', 'agency'])('upgrades to %s', (plan) => { expect(upgrade(plan)).toBe(true) })",
            "describe.each(['a'])('%s', () => {
  it.each(['pro'])('upgrades to %s', (plan) => { expect(upgrade(plan)).toBe(true) })
})",
        ),
        ["expectation-loosened high 2: .each no longer runs 1 row: 'agency'"]
    );
    // A row dropped from the test's own table, under a suite table that didn't change.
    assert_eq!(
        findings(
            "describe.each(['a', 'b'])('%s', () => {
  it.each(['pro', 'agency'])('upgrades to %s', (plan) => { expect(upgrade(plan)).toBe(true) })
})",
            "describe.each(['a', 'b'])('%s', () => {
  it.each(['pro'])('upgrades to %s', (plan) => { expect(upgrade(plan)).toBe(true) })
})",
        ),
        ["expectation-loosened high 2: .each no longer runs 1 row: 'agency'"]
    );
}

#[test]
fn each_rows_dropped() {
    // The shape of Mailyond's billing test: one plan no longer tested.
    assert_eq!(
        findings(
            "it.each(['pro', 'agency'])('upgrades to %s', (plan) => { expect(upgrade(plan)).toBe(true) })",
            "it.each(['pro'])('upgrades to %s', (plan) => { expect(upgrade(plan)).toBe(true) })",
        ),
        ["expectation-loosened high 1: .each no longer runs 1 row: 'agency'"]
    );
    // A column removed along with a row: only the count can be compared.
    assert_eq!(
        findings(
            "it.each([[50, 500, 100], [150, 500, 100], [2000, 51, 51]])('canary for %i', (n, max, size) => { expect(canary(n)).toBe(size) })",
            "it.each([[50, 100], [150, 100]])('canary for %i', (n, size) => { expect(canary(n)).toBe(size) })",
        ),
        ["expectation-loosened medium 1: .each now runs 2 rows instead of 3"]
    );
    // A row added is fine.
    assert_eq!(
        findings(
            "it.each(['pro'])('upgrades to %s', (plan) => { expect(upgrade(plan)).toBe(true) })",
            "it.each(['pro', 'agency'])('upgrades to %s', (plan) => { expect(upgrade(plan)).toBe(true) })",
        ),
        [""; 0]
    );
}

// outcome-changed

#[test]
fn fields_renamed_with_their_values_are_a_changed_outcome() {
    // The shape of h3's d684744: an API renamed its fields, the test followed.
    assert_eq!(
        one_check(
            "expect(await res.json()).toMatchObject({ statusCode: 422, statusMessage: 'Bad' })",
            "expect(await res.json()).toMatchObject({ status: 422, statusText: 'Bad' })",
        ),
        [
            "outcome-changed low 1: toMatchObject({ statusCode: 422, statusMessage: 'Bad' }) → toMatchObject({ status: 422, statusText: 'Bad' })"
        ]
    );
    // Another value under the new name: the old field is no longer checked.
    assert_eq!(
        one_check(
            "expect(r).toMatchObject({ statusCode: 422 })",
            "expect(r).toMatchObject({ status: 500 })",
        ),
        ["expectation-loosened high 1: toMatchObject no longer checks statusCode"]
    );
    // The same value under a key that doesn't look alike: not a rename.
    assert_eq!(
        one_check(
            "expect(user).toMatchObject({ id: 1, verified: true })",
            "expect(user).toMatchObject({ id: 1, active: true })",
        ),
        ["expectation-loosened high 1: toMatchObject no longer checks verified"]
    );
}

#[test]
fn a_different_expected_outcome() {
    let cases = [
        (
            "expect(total).toBe(3)",
            "expect(total).toBe(4)",
            "outcome-changed low 1: toBe(3) → toBe(4)",
        ),
        (
            "expect(x).not.toBe(1)",
            "expect(x).toBe(1)",
            "outcome-changed low 1: .not removed: .not.toBe(1) → toBe(1)",
        ),
        (
            "expect(sentAt).toBeGreaterThan(cutoff)",
            "expect(sentAt).toBeLessThanOrEqual(cutoff)",
            "outcome-changed low 1: toBeGreaterThan(cutoff) → toBeLessThanOrEqual(cutoff)",
        ),
        (
            "expect(fields(html)).toEqual({ preview: 'Hello there', text: 'Hello there' })",
            "expect(previewFor(html)).toBe('Hello there')",
            "outcome-changed low 1: toEqual({ preview: 'Hello there', text: 'Hello there' }) → toBe('Hello there')",
        ),
        (
            "expect(r).toEqual({ a: 1, b: 2 })",
            "expect(r).toEqual({ a: 1 })",
            "outcome-changed low 1: toEqual({ a: 1, b: 2 }) → toEqual({ a: 1 })",
        ),
        // Under `.not`, accepting anything rules out more: not looser.
        (
            "expect(send).not.toHaveBeenCalledWith('a@b.com')",
            "expect(send).not.toHaveBeenCalledWith(expect.anything())",
            "outcome-changed low 1: .not.toHaveBeenCalledWith('a@b.com') → .not.toHaveBeenCalledWith(expect.anything())",
        ),
    ];
    for (before, after, expected) in cases {
        assert_eq!(one_check(before, after), [expected], "{before} → {after}");
    }
}

// Found in code review of the matcher and value rules

#[test]
fn an_unchanged_negated_subset_in_an_edited_test_is_fine() {
    assert_eq!(
        findings(
            "it('x', () => { expect(r).not.toMatchObject({ a: 1 }); expect(x).toBe(1) })",
            "it('x', () => { expect(r).not.toMatchObject({ a: 1 }); expect(x).toBe(2) })",
        ),
        ["outcome-changed low 1: toBe(1) → toBe(2)"]
    );
}

#[test]
fn review_cases_that_are_not_looser() {
    let cases = [
        // `expect.objectContaining` compares nested fields exactly.
        (
            "expect(r).toEqual(expect.objectContaining({ a: { x: 1, y: 2 } }))",
            "expect(r).toEqual(expect.objectContaining({ a: { x: 1 } }))",
            "outcome-changed low 1: toEqual(expect.objectContaining({ a: { x: 1, y: 2 } })) → toEqual(expect.objectContaining({ a: { x: 1 } }))",
        ),
        // On an array, `toContain` needs a whole item.
        (
            "expect(list).toContain('abc')",
            "expect(list).toContain('ab')",
            "outcome-changed low 1: toContain('abc') → toContain('ab')",
        ),
        // `expect.anything()` rejects `null`, `expect.any(String)` a number.
        (
            "expect(r).toEqual({ a: null })",
            "expect(r).toEqual({ a: expect.anything() })",
            "outcome-changed low 1: toEqual({ a: null }) → toEqual({ a: expect.anything() })",
        ),
        (
            "expect(r).toEqual({ n: 5 })",
            "expect(r).toEqual({ n: expect.any(String) })",
            "outcome-changed low 1: toEqual({ n: 5 }) → toEqual({ n: expect.any(String) })",
        ),
    ];
    for (before, after, expected) in cases {
        assert_eq!(one_check(before, after), [expected], "{before} → {after}");
    }
}

#[test]
fn review_cases_that_are_looser() {
    let cases = [
        (
            "expect(r).toEqual(expect.objectContaining({ a: 1, b: 2 }))",
            "expect(r).toEqual(expect.objectContaining({ a: 1 }))",
            "expectation-loosened high 1: toEqual no longer checks b",
        ),
        // The value is still checked: only extra fields pass now.
        (
            "expect(r).toEqual({ a: 1 })",
            "expect(r).toHaveProperty('a', 1)",
            "matcher-loosened medium 1: toEqual({ a: 1 }) → toHaveProperty('a', 1): now extra fields pass",
        ),
        (
            "expect(count).toBeGreaterThan(5)",
            "expect(count).toBeGreaterThanOrEqual(1)",
            "expectation-loosened medium 1: toBeGreaterThan(5) → toBeGreaterThanOrEqual(1): the bound now lets more through",
        ),
        (
            "expect(count).toBeGreaterThan(5)",
            "expect(count).toBeGreaterThanOrEqual(5)",
            "expectation-loosened low 1: toBeGreaterThan(5) → toBeGreaterThanOrEqual(5): the bound now lets more through",
        ),
        (
            "expect(total).toBeCloseTo(5)",
            "expect(total).toBeGreaterThan(0)",
            "matcher-loosened medium 1: toBeCloseTo(5) → toBeGreaterThan(0): now a range of values passes",
        ),
        (
            "expect(name).toMatch(/foo/)",
            "expect(name).toMatch(/foo/i)",
            "expectation-loosened high 1: toMatch now matches more with the i flag",
        ),
        (
            "expect(name).not.toMatch(/foo/i)",
            "expect(name).not.toMatch(/foo/)",
            "expectation-loosened high 1: .not.toMatch no longer rules out what only the i flag matched",
        ),
    ];
    for (before, after, expected) in cases {
        assert_eq!(one_check(before, after), [expected], "{before} → {after}");
    }
    // Stricter.
    for (before, after) in [
        (
            "expect(count).toBeGreaterThanOrEqual(5)",
            "expect(count).toBeGreaterThan(5)",
        ),
        (
            "expect(name).toMatch(/foo/i)",
            "expect(name).toMatch(/foo/)",
        ),
    ] {
        assert_eq!(one_check(before, after), [""; 0], "{before} → {after}");
    }
}

#[test]
fn each_rows_are_counted_one_for_one() {
    assert_eq!(
        findings(
            "it.each(['a', 'a', 'b'])('%s', (x) => { expect(f(x)).toBe(true) })",
            "it.each(['a', 'b'])('%s', (x) => { expect(f(x)).toBe(true) })",
        ),
        ["expectation-loosened high 1: .each no longer runs 1 row: 'a'"]
    );
    // One row dropped and another edited: which is which can't be told.
    assert_eq!(
        findings(
            "it.each([[1, 2], [3, 4], [5, 6]])('%i', (a, b) => { expect(f(a)).toBe(b) })",
            "it.each([[1, 2], [3, 9]])('%i', (a, b) => { expect(f(a)).toBe(b) })",
        ),
        ["expectation-loosened medium 1: .each now runs 2 rows instead of 3"]
    );
}
