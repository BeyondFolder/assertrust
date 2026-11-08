//! `assertrust pairs`: shows how the tests and checks before a change pair up with the ones
//! after it, without judging anything. It prints what matching (step 4) sees, for debugging.
//!
//! ```text
//! per-commit: 1a2b3c4..9f8e7d6
//! 9f8e7d6 Loosen the test
//!   test/a.test.ts: 2 tests paired (1 unchanged)
//!     same      forwarding > stores the pending forward
//!       changed      5  expect(row).toMatchObject({ domainIds: null, status: 'pending' })
//!                 →  5  expect(row).toMatchObject({ status: 'pending' })
//! ```

use std::fmt::Write as _;

use assertrust_git::{Analysis, Classifier, Comparison, ModeKind, Repo};
use assertrust_ir::{ItemKind, Test};
use assertrust_match::{CheckMatch, FilePair, Matching, TestMatch, TestPair, TestRef, match_files};

use crate::cli::PairsArgs;
use crate::load::{self, Loaded};

pub fn run(args: &PairsArgs) -> Result<(), String> {
    let mode = args.mode.to_input()?;
    let dir = std::env::current_dir().map_err(|err| format!("can't read current dir: {err}"))?;
    let repo = Repo::open(&dir).map_err(|err| err.to_string())?;
    let analysis = repo
        .analyze(&mode, &Classifier::default())
        .map_err(|err| err.to_string())?;
    let output = render(&repo, &analysis)?;
    crate::print_stdout(&output)
}

fn render(repo: &Repo, analysis: &Analysis) -> Result<String, String> {
    let mut out = String::new();
    let mode = match analysis.kind {
        ModeKind::PerCommit => "per-commit",
        ModeKind::PrStyle => "pr-style",
        ModeKind::Staged => "staged",
        ModeKind::WorkTree => "worktree",
    };
    if analysis.label.is_empty() {
        line(&mut out, mode);
    } else {
        line(&mut out, &format!("{mode}: {}", analysis.label));
    }

    if let Some(net) = &analysis.net {
        line(&mut out, &format!("compare {} → {}", net.before, net.after));
        comparison(&mut out, repo, net, "  ")?;
    }
    for step in &analysis.steps {
        let commit = &step.commit;
        line(
            &mut out,
            &format!("{} {}", commit.short_sha(), commit.subject()),
        );
        comparison(&mut out, repo, &step.comparison, "  ")?;
    }
    if analysis.is_empty() {
        line(&mut out, "nothing to check");
    }
    Ok(out)
}

fn comparison(
    out: &mut String,
    repo: &Repo,
    comparison: &Comparison,
    indent: &str,
) -> Result<(), String> {
    let mut loaded = load::load(repo, comparison)?;
    loaded.notes.dedup();
    for note in &loaded.notes {
        line(out, &format!("{indent}{note}"));
    }
    let files = loaded.file_pairs();
    if files.is_empty() {
        if loaded.notes.is_empty() {
            line(out, &format!("{indent}no test files changed"));
        }
        return Ok(());
    }
    let matching = match_files(&files);
    for (index, file) in files.iter().enumerate() {
        render_file(
            out,
            &View {
                files: &files,
                loaded: &loaded,
                matching: &matching,
            },
            index,
            file,
            indent,
        );
    }
    Ok(())
}

struct View<'v, 'a> {
    files: &'v [FilePair<'a>],
    loaded: &'v Loaded,
    matching: &'v Matching,
}

impl View<'_, '_> {
    fn before(&self, test_ref: TestRef) -> &Test {
        &self.loaded.files[test_ref.file]
            .before
            .as_ref()
            .expect("exists")
            .ir
            .tests[test_ref.test]
    }

    fn after(&self, test_ref: TestRef) -> &Test {
        &self.loaded.files[test_ref.file]
            .after
            .as_ref()
            .expect("exists")
            .ir
            .tests[test_ref.test]
    }

    /// `path: test id`, or just the test id when it's in `file`.
    fn where_after(&self, test_ref: TestRef, file: usize) -> String {
        let id = self.after(test_ref).id();
        if test_ref.file == file {
            id
        } else {
            let path = self.files[test_ref.file].after.map_or("", |side| side.path);
            format!("{path}: {id}")
        }
    }
}

/// One file: its pairs (listed under the file the test was in before), deleted and added
/// tests.
fn render_file(out: &mut String, view: &View, index: usize, file: &FilePair, indent: &str) {
    let path = match (file.before, file.after) {
        (Some(before), Some(after)) if before.path != after.path => {
            format!("{} → {}", before.path, after.path)
        }
        (Some(side), _) | (None, Some(side)) => side.path.to_owned(),
        (None, None) => return,
    };
    let pairs: Vec<&TestPair> = view
        .matching
        .pairs
        .iter()
        .filter(|pair| pair.before.file == index)
        .collect();
    let deleted: Vec<_> = view
        .matching
        .deleted
        .iter()
        .filter(|deleted| deleted.test.file == index)
        .collect();
    let added: Vec<TestRef> = view
        .matching
        .added
        .iter()
        .copied()
        .filter(|test_ref| test_ref.file == index)
        .collect();
    let unchanged = pairs.iter().filter(|pair| is_unchanged(view, pair)).count();

    let mut summary = Vec::new();
    if !pairs.is_empty() {
        summary.push(if unchanged > 0 {
            format!(
                "{} paired ({unchanged} unchanged)",
                count(pairs.len(), "test")
            )
        } else {
            format!("{} paired", count(pairs.len(), "test"))
        });
    }
    if !deleted.is_empty() {
        summary.push(format!("{} deleted", deleted.len()));
    }
    if !added.is_empty() {
        summary.push(format!("{} added", added.len()));
    }
    if summary.is_empty() {
        summary.push("no tests".to_owned());
    }
    line(out, &format!("{indent}{path}: {}", summary.join(", ")));

    let indent = format!("{indent}  ");
    for pair in pairs {
        if !is_unchanged(view, pair) {
            render_pair(out, view, pair, &indent);
        }
    }
    for deleted in deleted {
        let test = view.before(deleted.test);
        let label = if deleted.all_moved() {
            "split"
        } else {
            "deleted"
        };
        line(out, &format!("{indent}{label:<9} {}", test.id()));
        for removed in &deleted.checks {
            removed_line(
                out,
                view,
                test,
                removed.item,
                removed.moved_to,
                index,
                &indent,
            );
        }
    }
    for test_ref in added {
        line(
            out,
            &format!("{indent}{:<9} {}", "added", view.after(test_ref).id()),
        );
    }
}

/// Same id, same file, same modifiers and an identical body.
fn is_unchanged(view: &View, pair: &TestPair) -> bool {
    let (before, after) = (view.before(pair.before), view.after(pair.after));
    pair.how == TestMatch::SameId
        && pair.before.file == pair.after.file
        && before.modifiers == after.modifiers
        && before.body.items.len() == after.body.items.len()
        && before
            .body
            .items
            .iter()
            .zip(&after.body.items)
            .all(|(a, b)| a.kind == b.kind && a.context == b.context)
}

fn render_pair(out: &mut String, view: &View, pair: &TestPair, indent: &str) {
    let (before, after) = (view.before(pair.before), view.after(pair.after));
    let label = match pair.how {
        TestMatch::SameId => "same".to_owned(),
        TestMatch::SameName => "moved".to_owned(),
        TestMatch::Similar(score) => format!("renamed ({score:.2})"),
    };
    line(out, &format!("{indent}{label:<9} {}", before.id()));
    if before.id() != after.id() || pair.before.file != pair.after.file {
        let dropped = dropped_words(&before.name, &after.name);
        let mut text = format!(
            "{indent}       →  {}",
            view.where_after(pair.after, pair.before.file)
        );
        if !dropped.is_empty() {
            write!(text, "  (dropped: \"{}\")", dropped.join(" ")).expect("writing to a String");
        }
        line(out, &text);
    }
    if before.modifiers != after.modifiers {
        line(
            out,
            &format!(
                "{indent}  modifiers  [{}] → [{}]",
                modifiers(before),
                modifiers(after)
            ),
        );
    }

    let identical = pair
        .checks
        .pairs
        .iter()
        .filter(|check| check.how == CheckMatch::Identical)
        .count();
    if identical > 0 {
        line(
            out,
            &format!("{indent}  {} identical", count(identical, "check")),
        );
    }
    for check in &pair.checks.pairs {
        let label = match check.how {
            CheckMatch::Identical => continue,
            CheckMatch::SameSubjectAndKind => "changed",
            CheckMatch::SameSubject => "matcher",
            CheckMatch::SameFact => "rewritten",
            CheckMatch::SameExpected => "same value",
            CheckMatch::Refactored => "refactored",
        };
        for (i, &item) in check.before.iter().enumerate() {
            let label = if i == 0 { label } else { "" };
            line(
                out,
                &format!("{indent}  {label:<10} {}", check_text(before, item)),
            );
        }
        for &item in &check.after {
            line(
                out,
                &format!("{indent}  {:>10} {}", "→", check_text(after, item)),
            );
        }
    }
    for removed in &pair.checks.removed {
        removed_line(
            out,
            view,
            before,
            removed.item,
            removed.moved_to,
            pair.before.file,
            indent,
        );
    }
    for &item in &pair.checks.added {
        line(
            out,
            &format!("{indent}  {:<10} {}", "added", check_text(after, item)),
        );
    }
}

fn removed_line(
    out: &mut String,
    view: &View,
    test: &Test,
    item: usize,
    moved_to: Option<TestRef>,
    file: usize,
    indent: &str,
) {
    let text = check_text(test, item);
    match moved_to {
        Some(target) => line(
            out,
            &format!(
                "{indent}  {:<10} {text}  → {}",
                "moved",
                view.where_after(target, file)
            ),
        ),
        None => line(out, &format!("{indent}  {:<10} {text}", "removed")),
    }
}

/// `12  expect(x).toBe(1)`, with the context when it matters.
fn check_text(test: &Test, item: usize) -> String {
    let item = &test.body.items[item];
    let ItemKind::Assertion(assertion) = &item.kind else {
        return String::new();
    };
    let mut text = format!("{:>4}  {assertion}", item.line);
    if !item.context.is_plain() {
        write!(text, "  ({})", item.context).expect("writing to a String");
    }
    text
}

fn modifiers(test: &Test) -> String {
    let modifiers: Vec<String> = test.modifiers.iter().map(ToString::to_string).collect();
    modifiers.join(", ")
}

/// Words in the old name that the new one doesn't have, in order, ignoring punctuation.
fn dropped_words(before: &str, after: &str) -> Vec<String> {
    let bare = |word: &str| {
        word.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let kept: Vec<String> = after.split_whitespace().map(bare).collect();
    before
        .split_whitespace()
        .filter(|word| !bare(word).is_empty() && !kept.contains(&bare(word)))
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
        .collect()
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn line(out: &mut String, text: &str) {
    writeln!(out, "{text}").expect("writing to a String can't fail");
}
