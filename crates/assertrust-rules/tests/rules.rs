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
    check(files)
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
            "expect(body).toContain('Hello there')",
            "expect(body).toContain('Hello')",
            "expectation-loosened medium 1: toContain now only needs 'Hello'",
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
    ];
    for (before, after, expected) in cases {
        assert_eq!(one_check(before, after), [expected], "{before} → {after}");
    }
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
