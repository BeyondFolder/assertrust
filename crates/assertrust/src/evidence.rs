//! What the AI judge sees about a finding (`docs/architecture.md`, step 7, "What it sees"):
//! the finding; each test it names, before and after; diffs of the files the change
//! touched, its own file and what its test file imports first; the commits that made it;
//! the pull request. Everything is cut to size, and what's cut is listed, never dropped
//! silently.

use std::collections::HashMap;

use assertrust_git::{Comparison, FileChange, FileKind, Repo, Rev};
use assertrust_judge::{Commit, Evidence, PullRequest, Section};
use assertrust_rules::RuleId;
use similar::TextDiff;

use crate::output::{count, place};
use crate::report::Reported;

/// The most tests shown, before and after each.
const MAX_TESTS: usize = 3;
/// The most lines of one test.
const MAX_TEST_LINES: usize = 150;
/// The most lines of diff in all, and of one file.
const MAX_DIFF_LINES: usize = 600;
const MAX_FILE_DIFF_LINES: usize = 200;
/// The most files listed when not every diff fits.
const MAX_LISTED: usize = 400;
/// The most commits, and characters of one message or of the PR's body.
const MAX_COMMITS: usize = 8;
const MAX_MESSAGE: usize = 3000;
const MAX_BODY: usize = 4000;

/// Files read for the judge, kept for the next finding of the same change.
#[derive(Default)]
pub struct Files {
    texts: HashMap<(String, String), Option<String>>,
    /// Unified diffs, by the two sides and the file's path.
    diffs: HashMap<(String, String, String), String>,
}

impl Files {
    /// The unified diff of `file` in `comparison`, made once per change.
    fn diff(&mut self, repo: &Repo, comparison: &Comparison, file: &FileChange) -> &str {
        let key = (
            format!("{:?}", comparison.before),
            format!("{:?}", comparison.after),
            file.path().to_owned(),
        );
        if !self.diffs.contains_key(&key) {
            let side = |files: &mut Self, rev: &Rev, path: &Option<String>| {
                path.as_deref()
                    .and_then(|path| files.text(repo, rev, path))
                    .unwrap_or_default()
                    .to_owned()
            };
            let before = side(self, &comparison.before, &file.path_before);
            let after = side(self, &comparison.after, &file.path_after);
            let diff = unified(
                &before,
                &after,
                file.path_before.as_deref().unwrap_or("/dev/null"),
                file.path_after.as_deref().unwrap_or("/dev/null"),
            );
            self.diffs.insert(key.clone(), diff);
        }
        &self.diffs[&key]
    }

    /// The file at `path` on side `rev`, if it's there and is text.
    fn text(&mut self, repo: &Repo, rev: &Rev, path: &str) -> Option<&str> {
        let key = (format!("{rev:?}"), path.to_owned());
        self.texts
            .entry(key)
            .or_insert_with(|| repo.read_file(rev, path).ok().flatten())
            .as_deref()
    }
}

/// What the judge is shown about `reported`.
pub fn gather(
    repo: &Repo,
    reported: &Reported,
    pull_request: Option<&PullRequest>,
    files: &mut Files,
) -> Evidence {
    let finding = &reported.finding;
    let mut evidence = Evidence {
        rule: finding.rule.as_str().to_owned(),
        rule_meaning: meaning(finding.rule).to_owned(),
        confidence: finding.confidence.as_str().to_owned(),
        place: place(finding),
        tests: finding.all_tests().map(str::to_owned).collect(),
        message: finding.message.clone(),
        before: finding.before.clone(),
        after: finding.after.clone(),
        ..Evidence::default()
    };
    if let Some(comparison) = reported.comparison {
        let mut test_files = Vec::new();
        for id in finding.all_tests().take(MAX_TESTS) {
            let (path, id) = test_place(&finding.path, id);
            tests(repo, comparison, path, id, files, &mut evidence);
            if !test_files.contains(&path) {
                test_files.push(path);
            }
        }
        if finding.all_tests().count() > MAX_TESTS {
            evidence.cut.push(format!(
                "the code of {} more tests",
                finding.all_tests().count() - MAX_TESTS
            ));
        }
        diffs(
            repo,
            comparison,
            &finding.path,
            &test_files,
            files,
            &mut evidence,
        );
    }
    let mut commits = reported.commits.iter();
    for commit in commits.by_ref().take(MAX_COMMITS) {
        evidence.commits.push(Commit {
            short_sha: commit.short_sha().to_owned(),
            message: cut_text(&commit.message, MAX_MESSAGE),
        });
    }
    let more = commits.count();
    if more > 0 {
        evidence.cut.push(format!("{more} more commits"));
    }
    evidence.pull_request = pull_request.map(|pr| PullRequest {
        title: pr.title.clone(),
        body: cut_text(&pr.body, MAX_BODY),
    });
    evidence
}

/// What a rule looks for, for the judge.
fn meaning(rule: RuleId) -> &'static str {
    match rule {
        RuleId::CheckRemoved => "a check disappeared from a test that still exists",
        RuleId::TestDisabled => {
            "a test no longer runs, or no longer runs everywhere: deleted, skipped, made conditional"
        }
        RuleId::CheckCantFail => "a check is still there but may no longer run or fail",
        RuleId::MatcherLoosened => "a check uses a weaker matcher, like toEqual(x) → toBeTruthy()",
        RuleId::MatcherSwapped => "a check's matcher swapped for one of about the same strength",
        RuleId::ExpectationLoosened => {
            "the same matcher accepts more: fields, alternatives or table rows dropped"
        }
        RuleId::OutcomeChanged => {
            "the expected value changed, neither looser nor stricter, like toBe(3) → toBe(4)"
        }
        RuleId::GateLoosened => "CI or test config runs fewer tests or lets more failures through",
    }
}

/// Where a finding's test is: a finding in a helper names it with its file in front
/// (`test/a.test.ts: refunds`).
fn test_place<'a>(finding_path: &'a str, id: &'a str) -> (&'a str, &'a str) {
    const SOURCE: [&str; 8] = [".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs"];
    match id.split_once(": ") {
        Some((path, rest))
            if !path.contains(char::is_whitespace)
                && SOURCE.iter().any(|extension| path.ends_with(extension)) =>
        {
            (path, rest)
        }
        _ => (finding_path, id),
    }
}

/// The change to `path` in `comparison`, if it changed.
fn change<'c>(comparison: &'c Comparison, path: &str) -> Option<&'c FileChange> {
    comparison.files.iter().find(|file| file.path() == path)
}

/// The test `id` in `path`, before and after the change: one section each, or one when it
/// didn't change. A side without it under that name (new, deleted, renamed) shows the
/// file's diff instead, among the diffs.
fn tests(
    repo: &Repo,
    comparison: &Comparison,
    path: &str,
    id: &str,
    files: &mut Files,
    evidence: &mut Evidence,
) {
    let changed = change(comparison, path);
    let before_path = changed.map_or(Some(path), |file| file.path_before.as_deref());
    let after_path = changed.map_or(Some(path), |file| file.path_after.as_deref());
    let mut source = |rev: &Rev, path: Option<&str>| -> Option<String> {
        let path = path?;
        let text = files.text(repo, rev, path)?;
        let ir = assertrust_ir::parse_file(path, text).ok()?;
        let test = ir.tests.iter().find(|test| test.id() == id)?;
        let lines: Vec<&str> = text
            .lines()
            .skip(test.line.saturating_sub(1) as usize)
            .take((test.end_line + 1).saturating_sub(test.line) as usize)
            .collect();
        Some(cut_lines(&lines, MAX_TEST_LINES))
    };
    let before = source(&comparison.before, before_path);
    let after = source(&comparison.after, after_path);
    let section = |title: String, text: String| Section {
        title,
        language: "ts".to_owned(),
        text,
    };
    match (before, after) {
        (Some(before), Some(after)) if before == after => evidence.sections.push(section(
            format!("The test \"{id}\", which this change didn't touch ({path})"),
            after,
        )),
        (before, after) => {
            if let Some(before) = before {
                let path = before_path.unwrap_or(path);
                evidence.sections.push(section(
                    format!("The test \"{id}\" before the change ({path})"),
                    before,
                ));
            }
            if let Some(after) = after {
                evidence.sections.push(section(
                    format!("The test \"{id}\" after the change ({path})"),
                    after,
                ));
            }
        }
    }
}

/// Diffs of the files the change touched, as many as fit: the finding's own file first,
/// then files of the same name elsewhere (a test moved or rewritten) and the files its
/// tests' files mention (what they import), then other tests, then the rest. Lock files and
/// files that aren't code are left out. When not every diff fits, every file the change
/// touched is listed with what happened to it, so a test deleted here and recreated there
/// still shows.
fn diffs(
    repo: &Repo,
    comparison: &Comparison,
    finding_path: &str,
    test_files: &[&str],
    files: &mut Files,
    evidence: &mut Evidence,
) {
    let mentioned: Vec<String> = test_files
        .iter()
        .filter_map(|path| files.text(repo, &comparison.after, path).map(str::to_owned))
        .collect();
    let names: Vec<String> = std::iter::once(finding_path)
        .chain(test_files.iter().copied())
        .map(|path| name(stem(path)))
        .filter(|name| !name.is_empty())
        .collect();
    let rank = |file: &FileChange| -> u8 {
        let path = file.path();
        if path == finding_path || test_files.contains(&path) {
            0
        } else if names.contains(&name(stem(path)))
            || mentioned.iter().any(|text| imports(text, stem(path)))
        {
            1
        } else if matches!(file.kind, FileKind::Test | FileKind::TestSupport) {
            2
        } else {
            3
        }
    };
    let mut candidates: Vec<&FileChange> = comparison
        .files
        .iter()
        .filter(|file| file.kind != FileKind::Other && !is_lock_file(file.path()))
        .collect();
    // Stable: within a rank, in the order git lists them.
    candidates.sort_by_key(|file| rank(file));
    let mut left = MAX_DIFF_LINES;
    let mut skipped = Vec::new();
    for file in candidates {
        if left == 0 {
            skipped.push(file.path().to_owned());
            continue;
        }
        let lines: Vec<&str> = files.diff(repo, comparison, file).lines().collect();
        if lines.is_empty() {
            continue;
        }
        let shown = lines.len().min(MAX_FILE_DIFF_LINES).min(left);
        left -= shown;
        evidence.sections.push(Section {
            title: format!("The change to {}", file.path()),
            language: "diff".to_owned(),
            text: cut_lines(&lines, shown),
        });
    }
    if !skipped.is_empty() {
        evidence.cut.push(format!(
            "the changes to {} (listed under \"Every file this change touched\")",
            count(skipped.len(), "more file")
        ));
        let mut listed: Vec<String> = comparison
            .files
            .iter()
            .take(MAX_LISTED)
            .map(|file| match (&file.path_before, &file.path_after) {
                (Some(before), Some(after)) if before != after => {
                    format!("{} {before} → {after}", file.status.letter())
                }
                _ => format!("{} {}", file.status.letter(), file.path()),
            })
            .collect();
        if comparison.files.len() > MAX_LISTED {
            listed.push(format!(
                "… and {} more",
                comparison.files.len() - MAX_LISTED
            ));
        }
        evidence.sections.push(Section {
            title: "Every file this change touched (A added, M changed, D deleted, R renamed)"
                .to_owned(),
            language: "text".to_owned(),
            text: listed.join("\n"),
        });
    }
}

/// A file name to compare across naming styles: `AlbumCard` and `album-card` → `albumcard`.
fn name(stem: &str) -> String {
    stem.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A unified diff, `a/` and `b/` in front of the paths as git writes them.
fn unified(before: &str, after: &str, path_before: &str, path_after: &str) -> String {
    let side = |prefix: &str, path: &str| {
        if path == "/dev/null" {
            path.to_owned()
        } else {
            format!("{prefix}{path}")
        }
    };
    TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&side("a/", path_before), &side("b/", path_after))
        .to_string()
}

/// Whether `text` imports a module named `stem`: `from '../src/api'`, `require("./api.js")`,
/// `import('./api')`. A URL (`fetch('/api/users')`) or a word inside another isn't one.
fn imports(text: &str, stem: &str) -> bool {
    if stem.is_empty() {
        return false;
    }
    ["from", "require(", "import("].iter().any(|keyword| {
        text.match_indices(keyword).any(|(at, _)| {
            let rest = text[at + keyword.len()..].trim_start();
            let Some(quote) = rest
                .chars()
                .next()
                .filter(|c| matches!(c, '\'' | '"' | '`'))
            else {
                return false;
            };
            let specifier = rest[1..].split(quote).next().unwrap_or_default();
            let last = specifier.rsplit('/').next().unwrap_or(specifier);
            self::stem(last) == stem
        })
    })
}

/// `src/forwarding.ts` → `forwarding`: what an import of it says.
fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    // An `index` file is imported by its directory.
    if stem == "index" {
        path.rsplit('/').nth(1).unwrap_or(stem)
    } else {
        stem
    }
}

fn is_lock_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "bun.lockb" | "bun.lock"
    )
}

/// The first `max` of `lines`, then a line saying how many were cut.
fn cut_lines(lines: &[&str], max: usize) -> String {
    let mut text = lines[..lines.len().min(max)].join("\n");
    if lines.len() > max {
        text.push_str(&format!(
            "\n… ({} cut)",
            count(lines.len() - max, "more line")
        ));
    }
    text
}

/// `text`, at most `max` characters, then a note that the rest was cut.
fn cut_text(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}\n… (cut)", &text[..at]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_helper_findings_test() {
        assert_eq!(
            test_place("test/utils.ts", "test/a.test.ts: refunds"),
            ("test/a.test.ts", "refunds")
        );
        assert_eq!(
            test_place("test/a.test.ts", "refunds > note: partial"),
            ("test/a.test.ts", "refunds > note: partial")
        );
        // A describe block named like a version isn't a file.
        assert_eq!(
            test_place("test/a.test.ts", "v2.0: migration > keeps ids"),
            ("test/a.test.ts", "v2.0: migration > keeps ids")
        );
    }

    #[test]
    fn imports_are_paths_not_words() {
        let test =
            "import { refund } from '../src/api'\nimport x from \"./util.js\"\nfetch('/api/users')";
        assert!(imports(test, "api"));
        assert!(imports(test, "util"));
        assert!(!imports(test, "users"));
        assert!(!imports(test, "ref"));
        // A dotfile has no stem: it's never one.
        assert!(!imports(test, ""));
    }

    #[test]
    fn names_match_across_styles() {
        assert_eq!(name(stem("web/AlbumCard.spec.ts")), "albumcard");
        assert_eq!(name(stem("e2e/album-card.browser.spec.ts")), "albumcard");
    }

    #[test]
    fn names_what_an_import_says() {
        assert_eq!(stem("src/forwarding.ts"), "forwarding");
        assert_eq!(stem("src/mail/index.ts"), "mail");
        assert_eq!(stem("a.config.ts"), "a");
    }

    #[test]
    fn cuts_say_so() {
        assert_eq!(cut_lines(&["a", "b", "c"], 2), "a\nb\n… (1 more line cut)");
        assert_eq!(cut_lines(&["a"], 2), "a");
        assert_eq!(cut_text("abcdef", 3), "abc\n… (cut)");
    }

    #[test]
    fn diffs_like_git() {
        let diff = unified("a\nb\n", "a\nc\n", "x.ts", "x.ts");
        assert_eq!(
            diff,
            "--- a/x.ts\n+++ b/x.ts\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n"
        );
        assert!(unified("", "new\n", "/dev/null", "y.ts").starts_with("--- /dev/null\n+++ b/y.ts"));
    }
}
