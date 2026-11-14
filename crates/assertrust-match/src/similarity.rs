//! Scores for pairing things that aren't identical.

use std::collections::BTreeSet;

use assertrust_ir::Test;

/// Two tests need at least this score...
///
/// Set on Mailyond's history: every candidate above 0.35 that was the other's best (and
/// clear of the runner-up) was a real rename or rewrite, and the lowest real one scored 0.37.
pub(crate) const MIN_SCORE: f64 = 0.4;
/// ...and must beat every other candidate for either side by this much. When unsure, don't
/// pair.
pub(crate) const MARGIN: f64 = 0.1;

/// What a test is compared by, worked out once per test rather than once per candidate
/// pair: a big rewrite can leave hundreds of tests on each side.
pub(crate) struct TestFeatures<'a> {
    name: BTreeSet<String>,
    body: BTreeSet<&'a str>,
    checks: BTreeSet<String>,
}

impl<'a> TestFeatures<'a> {
    pub(crate) fn new(test: &'a Test) -> Self {
        Self {
            name: words(&test.name),
            body: test.fingerprint.iter().map(String::as_str).collect(),
            checks: check_keys(test),
        }
    }
}

/// How alike two tests are, from 0 to 1:
/// - 40% their names: the words they share, counting an old name kept whole inside a longer
///   new one ("is red with more than one record" → "…, suggesting a merge") as close
/// - 30% their source: the words in their bodies
/// - 30% their checks: identical checks and distinctive expected values they share
pub(crate) fn test_score(before: &TestFeatures, after: &TestFeatures) -> f64 {
    let names = f64::midpoint(
        jaccard(&before.name, &after.name),
        containment(&before.name, &after.name),
    );
    let bodies = jaccard(&before.body, &after.body);
    let checks = jaccard(&before.checks, &after.checks);
    0.4 * names + 0.3 * bodies + 0.3 * checks
}

/// A test's checks as text, plus the distinctive expected values in them.
fn check_keys(test: &Test) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for assertion in test.body.assertions() {
        keys.insert(assertion.checked_text());
        keys.extend(crate::check_pairing::expected_values(assertion));
    }
    keys
}

/// The share of the smaller set that's in the other one.
fn containment<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> f64 {
    let smaller = a.len().min(b.len());
    if smaller == 0 {
        return 0.0;
    }
    #[expect(clippy::cast_precision_loss, reason = "word counts are small")]
    let score = a.intersection(b).count() as f64 / smaller as f64;
    score
}

/// How alike two subjects are, from 0 to 1, by the words in them: `fields.textBody` and
/// `textFromHtmlBody(html)` share `text` and `body`.
pub(crate) fn subject_score(before: &str, after: &str) -> f64 {
    jaccard(&subject_words(before), &subject_words(after))
}

/// True when two subjects have a word in common.
pub(crate) fn subjects_overlap(before: &str, after: &str) -> bool {
    !subject_words(before).is_disjoint(&subject_words(after))
}

/// Lowercase words, split at anything that isn't a letter or digit.
pub(crate) fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The words in a subject: identifiers split at camelCase (`textFromHtmlBody` → text,
/// from, html, body; `parseURLValue` → parse, url, value), numbers, and each string literal
/// as one word. The same input string is a strong sign of the same check, but a long one
/// mustn't outweigh everything else.
fn subject_words(text: &str) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    let mut word = String::new();
    let mut previous: Option<char> = None;
    let mut chars = text.chars().peekable();
    let flush = |word: &mut String, words: &mut BTreeSet<String>| {
        if !word.is_empty() {
            words.insert(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        if c == '\'' || c == '`' {
            flush(&mut word, &mut words);
            let mut literal = String::from(c);
            while let Some(next) = chars.next() {
                literal.push(next);
                if next == '\\' {
                    literal.extend(chars.next());
                } else if next == c {
                    break;
                }
            }
            words.insert(literal);
        } else if c.is_alphanumeric() {
            // A new word starts at an uppercase letter after a lowercase one (`textFrom`),
            // or at the last capital of a run followed by a lowercase letter (`URLValue`).
            let after_lowercase = previous.is_some_and(char::is_lowercase);
            let ends_capitals = previous.is_some_and(char::is_uppercase)
                && chars.peek().is_some_and(|next| next.is_lowercase());
            if c.is_uppercase() && (after_lowercase || ends_capitals) {
                flush(&mut word, &mut words);
            }
            word.extend(c.to_lowercase());
        } else {
            flush(&mut word, &mut words);
        }
        previous = Some(c);
    }
    flush(&mut word, &mut words);
    words
}

pub(crate) fn jaccard<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(b).count();
    let all = a.union(b).count();
    #[expect(clippy::cast_precision_loss, reason = "word counts are small")]
    let score = shared as f64 / all as f64;
    score
}

/// Pairs items by score: each pair must be the other's best candidate, score at least
/// `min_score`, and beat the runner-up on both sides by [`MARGIN`]. Returns index pairs
/// into `before` and `after`, with their scores.
pub(crate) fn best_pairs(scores: &[Vec<f64>], min_score: f64) -> Vec<(usize, usize, f64)> {
    let rows = scores.len();
    let cols = scores.first().map_or(0, Vec::len);
    let mut pairs = Vec::new();
    for row in 0..rows {
        let Some((col, score, runner_up)) = best(scores[row].iter().copied()) else {
            continue;
        };
        let column = (0..rows).map(|r| scores[r][col]);
        let Some((best_row, _, column_runner_up)) = best(column) else {
            continue;
        };
        if best_row == row
            && score >= min_score
            && score - runner_up >= MARGIN
            && score - column_runner_up >= MARGIN
        {
            pairs.push((row, col, score));
        }
    }
    debug_assert!(pairs.iter().all(|&(_, col, _)| col < cols));
    pairs
}

/// The index and value of the highest score, and the second-highest value (0 if none).
fn best(scores: impl Iterator<Item = f64>) -> Option<(usize, f64, f64)> {
    let mut top: Option<(usize, f64)> = None;
    let mut runner_up = 0.0_f64;
    for (index, score) in scores.enumerate() {
        match top {
            Some((_, best)) if score <= best => runner_up = runner_up.max(score),
            Some((_, best)) => {
                runner_up = best;
                top = Some((index, score));
            }
            None => top = Some((index, score)),
        }
    }
    top.map(|(index, score)| (index, score, runner_up))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_share_words_across_camel_case() {
        assert!(subject_score("fields.textBody", "textFromHtmlBody(html)") > 0.3);
        assert!(subject_score("fields.preview", "textFromHtmlBody(html)") < 0.01);
        // All-caps names stay whole words: `res.URL` and `u.id` have nothing in common.
        assert!(!subjects_overlap("res.URL", "u.id"));
        assert_eq!(
            subject_words("parseURLValue(res.JSON)"),
            ["json", "parse", "res", "url", "value"]
                .map(String::from)
                .into()
        );
        // A string literal is one word: the same input matches, a long one doesn't swamp.
        let long = "textFromHtmlBody('<p>preview of the fields</p>', false)";
        assert!(subject_score("fields.preview", long) < 0.01);
        assert!(
            subject_score(
                "normalize('https://a.com').domains",
                "parse('https://a.com').domains"
            ) > subject_score(
                "normalize('https://a.com').domains",
                "parse('b.com').domains"
            )
        );
    }

    #[test]
    fn a_close_second_means_no_pair() {
        // Row 0 likes both columns about equally: no pair for it. Row 1 clearly prefers
        // column 1, and column 1 clearly prefers row 1.
        let scores = vec![vec![0.80, 0.75], vec![0.10, 0.90]];
        assert_eq!(best_pairs(&scores, MIN_SCORE), vec![(1, 1, 0.90)]);
        // Now column 1 likes row 0 almost as much as row 1: no pair at all.
        let scores = vec![vec![0.80, 0.85], vec![0.10, 0.90]];
        assert_eq!(best_pairs(&scores, MIN_SCORE), vec![]);
    }
}
