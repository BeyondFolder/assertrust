//! Matching on small before/after files: the cases listed in `docs/architecture.md`, step 4.

use assertrust_ir::{FileIr, ItemKind, parse_file};
use assertrust_match::{CheckMatch, FilePair, Matching, Side, TestMatch, match_files};

/// One file, before and after.
fn match_one(before: &str, after: &str) -> (Matching, FileIr, FileIr) {
    let before = parse_file("a.test.ts", before).expect("before parses");
    let after = parse_file("a.test.ts", after).expect("after parses");
    let matching = match_files(&[FilePair {
        before: Some(Side {
            path: "a.test.ts",
            ir: &before,
        }),
        after: Some(Side {
            path: "a.test.ts",
            ir: &after,
        }),
    }]);
    (matching, before, after)
}

/// Each test pair as `old id => new id (how)`.
fn test_pairs(matching: &Matching, before: &FileIr, after: &FileIr) -> Vec<String> {
    matching
        .pairs
        .iter()
        .map(|pair| {
            let how = match pair.how {
                TestMatch::SameId => "same id".to_owned(),
                TestMatch::SameName => "same name".to_owned(),
                TestMatch::Similar(_) => "similar".to_owned(),
            };
            format!(
                "{} => {} ({how})",
                before.tests[pair.before.test].id(),
                after.tests[pair.after.test].id()
            )
        })
        .collect()
}

/// The first pair's checks: `how: old checks => new checks`, then removed and added.
fn check_lines(matching: &Matching, before: &FileIr, after: &FileIr) -> Vec<String> {
    let pair = &matching.pairs[0];
    let text = |ir: &FileIr, test: usize, item: usize| match &ir.tests[test].body.items[item].kind {
        ItemKind::Assertion(assertion) => assertion.to_string(),
        _ => String::new(),
    };
    let mut lines = Vec::new();
    for check in &pair.checks.pairs {
        let old: Vec<String> = check
            .before
            .iter()
            .map(|&i| text(before, pair.before.test, i))
            .collect();
        let new: Vec<String> = check
            .after
            .iter()
            .map(|&i| text(after, pair.after.test, i))
            .collect();
        lines.push(format!(
            "{:?}: {} => {}",
            check.how,
            old.join(" + "),
            new.join(" + ")
        ));
    }
    for removed in &pair.checks.removed {
        lines.push(format!(
            "removed: {}",
            text(before, pair.before.test, removed.item)
        ));
    }
    for &added in &pair.checks.added {
        lines.push(format!("added: {}", text(after, pair.after.test, added)));
    }
    lines
}

#[test]
fn an_unchanged_name_pairs_by_id() {
    let source = "describe('d', () => { it('a', () => { expect(x).toBe(1) }) })";
    let (matching, before, after) = match_one(source, source);
    assert_eq!(
        test_pairs(&matching, &before, &after),
        ["d > a => d > a (same id)"]
    );
    assert_eq!(
        check_lines(&matching, &before, &after),
        ["Identical: expect(x).toBe(1) => expect(x).toBe(1)"]
    );
}

#[test]
fn a_rename_with_the_same_body_pairs_by_similarity() {
    let (matching, before, after) = match_one(
        "it('stores the pending forward', () => { expect(row.status).toBe('pending') })",
        "it('stores a pending forward', () => { expect(row.status).toBe('pending') })",
    );
    assert_eq!(
        test_pairs(&matching, &before, &after),
        ["stores the pending forward => stores a pending forward (similar)"]
    );
}

/// The `038cf0b` shape: the `describe` renamed, the test renamed, the subject refactored,
/// and one check gone.
#[test]
fn a_rename_plus_a_refactor_keeps_the_checks_that_survive() {
    let (matching, before, after) = match_one(
        "describe('inboundBodyFields', () => {
          it('never lets markup into the text or preview', () => {
            const fields = inboundBodyFields({ HtmlBody: '<b>Hi</b> there' });
            expect(fields.textBody).toBe('Hi there');
            expect(fields.preview).toBe('Hi there');
          });
        });",
        "describe('textFromHtmlBody', () => {
          it('never lets markup into the text', () => {
            expect(textFromHtmlBody('<b>Hi</b> there')).toBe('Hi there');
          });
        });",
    );
    assert_eq!(
        test_pairs(&matching, &before, &after),
        [
            "inboundBodyFields > never lets markup into the text or preview => textFromHtmlBody > never lets markup into the text (similar)"
        ]
    );
    assert_eq!(
        check_lines(&matching, &before, &after),
        [
            "SameExpected: expect(fields.textBody).toBe('Hi there') => expect(textFromHtmlBody('<b>Hi</b> there')).toBe('Hi there')",
            "removed: expect(fields.preview).toBe('Hi there')",
        ]
    );
}

#[test]
fn a_test_moved_to_another_describe_pairs_by_name() {
    let (matching, before, after) = match_one(
        "describe('old', () => { it('charges the card', () => { expect(a).toBe(1) }) })",
        "describe('new', () => { it('charges the card', () => { expect(a).toBe(1) }) })",
    );
    assert_eq!(
        test_pairs(&matching, &before, &after),
        ["old > charges the card => new > charges the card (same name)"]
    );
}

#[test]
fn a_test_moved_to_another_file_pairs_across_files() {
    let a_before = parse_file(
        "a.test.ts",
        "it('refunds', () => { expect(refund(1)).toBe(true) }); it('keeps', () => {})",
    )
    .unwrap();
    let a_after = parse_file("a.test.ts", "it('keeps', () => {})").unwrap();
    let b_after = parse_file(
        "b.test.ts",
        "it('refunds', () => { expect(refund(1)).toBe(true) })",
    )
    .unwrap();
    let matching = match_files(&[
        FilePair {
            before: Some(Side {
                path: "a.test.ts",
                ir: &a_before,
            }),
            after: Some(Side {
                path: "a.test.ts",
                ir: &a_after,
            }),
        },
        FilePair {
            before: None,
            after: Some(Side {
                path: "b.test.ts",
                ir: &b_after,
            }),
        },
    ]);
    let moved = matching
        .pairs
        .iter()
        .find(|pair| pair.before.file != pair.after.file)
        .expect("a cross-file pair");
    assert_eq!(a_before.tests[moved.before.test].name, "refunds");
    assert_eq!(moved.after.file, 1);
    assert!(matching.deleted.is_empty() && matching.added.is_empty());
}

#[test]
fn a_test_split_in_two_is_not_a_deletion() {
    let (matching, ..) = match_one(
        "it('checks the account', () => { expect(name).toBe('Ada'); expect(plan).toBe('pro') })",
        "it('checks the name', () => { expect(name).toBe('Ada') });
         it('checks the plan', () => { expect(plan).toBe('pro') })",
    );
    // Whichever half it pairs with, the other check moved rather than disappeared.
    let removed: Vec<_> = matching
        .pairs
        .iter()
        .flat_map(|pair| &pair.checks.removed)
        .chain(matching.deleted.iter().flat_map(|deleted| &deleted.checks))
        .collect();
    assert!(
        removed.iter().all(|check| check.moved_to.is_some()),
        "{removed:?}"
    );
}

#[test]
fn near_identical_tests_are_not_guessed() {
    let (matching, ..) = match_one(
        "it('one', () => { expect(sum(1, 2)).toBe(3) }); it('two', () => { expect(sum(1, 2)).toBe(3) })",
        "it('uno', () => { expect(sum(1, 2)).toBe(3) }); it('dos', () => { expect(sum(1, 2)).toBe(3) })",
    );
    assert!(matching.pairs.is_empty());
    assert_eq!((matching.deleted.len(), matching.added.len()), (2, 2));
}

#[test]
fn duplicate_names_pair_in_order() {
    let (matching, before, after) = match_one(
        "it('works', () => { expect(a).toBe(1) }); it('works', () => { expect(b).toBe(2) })",
        "it('works', () => { expect(a).toBe(1) }); it('works', () => { expect(b).toBe(3) })",
    );
    assert_eq!(matching.pairs.len(), 2);
    assert_eq!(
        check_lines(&matching, &before, &after),
        ["Identical: expect(a).toBe(1) => expect(a).toBe(1)"]
    );
    let second = &matching.pairs[1];
    assert_eq!(second.checks.pairs[0].how, CheckMatch::SameSubjectAndKind);
}

/// Each check step, on its own.
#[test]
fn checks_pair_by_the_first_step_that_fits() {
    let cases = [
        // 10d8742: the same check with fields dropped.
        (
            "expect(res.body).toMatchObject({ email: 'a@b.test', status: 'pending' })",
            "expect(res.body).toMatchObject({ email: 'a@b.test' })",
            "SameSubjectAndKind",
        ),
        (
            "expect(user).toEqual({ id: 1 })",
            "expect(user).toBeTruthy()",
            "SameSubject",
        ),
        // Rewritten: a property check folded into an object check.
        (
            "expect(reply.text).toBe('hi')",
            "expect(reply).toMatchObject({ text: 'hi', containsHtml: true })",
            "SameFact",
        ),
        (
            "expect(fields.text).toBe('Hello there')",
            "expect(textOf(html)).toBe('Hello there')",
            "SameExpected",
        ),
        (
            "expect(hasEnforcement({ dmarc: result })).toBe(false)",
            "expect(result.enforced).toBe(false)",
            "Refactored",
        ),
    ];
    for (before, after, how) in cases {
        let (matching, b, a) = match_one(
            &format!("it('t', () => {{ {before} }})"),
            &format!("it('t', () => {{ {after} }})"),
        );
        let lines = check_lines(&matching, &b, &a);
        assert!(
            lines.len() == 1 && lines[0].starts_with(&format!("{how}:")),
            "{before} → {after}: {lines:?}"
        );
    }
}

#[test]
fn several_checks_folded_into_one_pair_as_a_group() {
    let (matching, before, after) = match_one(
        "it('t', () => { expect(user.name).toBe('Ada'); expect(user.plan).toBe('pro') })",
        "it('t', () => { expect(user).toMatchObject({ name: 'Ada', plan: 'pro' }) })",
    );
    assert_eq!(
        check_lines(&matching, &before, &after),
        [
            "SameFact: expect(user.name).toBe('Ada') + expect(user.plan).toBe('pro') => expect(user).toMatchObject({ name: 'Ada', plan: 'pro' })"
        ]
    );
}

#[test]
fn refactored_subjects_with_the_same_check_pair_in_order() {
    let (matching, before, after) = match_one(
        "it('t', () => { expect(normalize('a.com').invalid).toHaveLength(1); expect(normalize('b.com').invalid).toHaveLength(1) })",
        "it('t', () => { expect(parse('a.com').skipped).toHaveLength(1); expect(parse('b.com').skipped).toHaveLength(1) })",
    );
    assert_eq!(
        check_lines(&matching, &before, &after),
        [
            "Refactored: expect(normalize('a.com').invalid).toHaveLength(1) => expect(parse('a.com').skipped).toHaveLength(1)",
            "Refactored: expect(normalize('b.com').invalid).toHaveLength(1) => expect(parse('b.com').skipped).toHaveLength(1)",
        ]
    );
}

#[test]
fn unrelated_checks_are_not_paired() {
    // Same matcher and value, but nothing else in common: a removal plus an addition.
    let (matching, before, after) = match_one(
        "it('t', () => { expect(hasEnforcement(reject)).toBe(false) })",
        "it('t', () => { expect(quarantine.enforced).toBe(false) })",
    );
    assert_eq!(
        check_lines(&matching, &before, &after),
        [
            "removed: expect(hasEnforcement(reject)).toBe(false)",
            "added: expect(quarantine.enforced).toBe(false)",
        ]
    );
}

#[test]
fn a_check_moved_to_another_test_is_not_removed() {
    let (matching, ..) = match_one(
        "it('a', () => { expect(x).toBe(1); expect(y).toBe(2) }); it('b', () => {})",
        "it('a', () => { expect(x).toBe(1) }); it('b', () => { expect(y).toBe(2) })",
    );
    let removed = &matching.pairs[0].checks.removed;
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].moved_to.map(|test| test.test), Some(1));
}

#[test]
fn a_check_moved_into_an_if_still_pairs_and_its_twin_stays_put() {
    let (matching, before, after) = match_one(
        "it('t', () => { expect(x).toBe(1); if (flag) { expect(x).toBe(1) } })",
        "it('t', () => { if (flag) { expect(x).toBe(1) } })",
    );
    let pair = &matching.pairs[0];
    // The checks in the same place pair; the plain one is what was removed.
    assert_eq!(pair.checks.pairs.len(), 1);
    let check = &pair.checks.pairs[0];
    let context = |ir: &FileIr, item: usize| ir.tests[0].body.items[item].context;
    assert_eq!(
        context(&before, check.before[0]),
        context(&after, check.after[0])
    );
    assert_eq!(pair.checks.removed.len(), 1);
    assert!(context(&before, pair.checks.removed[0].item).is_plain());

    // On its own, a check that moved into an `if` pairs as identical, context changed.
    let (matching, before, after) = match_one(
        "it('t', () => { expect(x).toBe(1) })",
        "it('t', () => { if (flag) { expect(x).toBe(1) } })",
    );
    let check = &matching.pairs[0].checks.pairs[0];
    assert_eq!(check.how, CheckMatch::Identical);
    assert!(context(&before, check.before[0]).is_plain());
    assert!(context(&after, check.after[0]).in_if);
}

#[test]
fn one_check_against_several_with_the_same_subject_goes_to_the_closest() {
    let (matching, before, after) = match_one(
        "it('t', () => { expect(res.status).toBe(200) })",
        "it('t', () => { expect(res.status).toBe(201); expect(res.status).not.toBe(500) })",
    );
    assert_eq!(
        check_lines(&matching, &before, &after),
        [
            "SameSubjectAndKind: expect(res.status).toBe(200) => expect(res.status).toBe(201)",
            "added: expect(res.status).not.toBe(500)",
        ]
    );
}

#[test]
fn a_check_that_moved_into_an_if_in_another_test_was_not_just_moved() {
    let (matching, ..) = match_one(
        "it('a', () => { expect(x).toBe(1); expect(role).toBe('admin') }); it('b', () => {})",
        "it('a', () => { expect(x).toBe(1) }); it('b', () => { if (ci) expect(role).toBe('admin') })",
    );
    let removed = &matching.pairs[0].checks.removed;
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].moved_to, None);
}
