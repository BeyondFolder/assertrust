//! What the judge sees about one finding, and how it's written into the prompt.
//!
//! ~~~text
//! # Finding
//! Rule: check-removed (a check disappeared from a test that still exists)
//! Confidence: high
//! Where: test/forwarding.test.ts:42
//! Test: forwarding > creates a pending address
//! What got weaker: removed 2 checks, including expect(address).toMatchObject({…})
//! Before:
//!     expect(address).toMatchObject({ status: 'pending' })
//!
//! # The test before the change (test/forwarding.test.ts)
//! ```ts
//! it('creates a pending address', async () => { … })
//! ```
//!
//! # Commit 10d8742
//! Trim the forwarding response
//! ~~~

use std::fmt::Write as _;

/// The finding, and the code and messages around it. The command line fills it in; every
/// part is already cut to size, and `cut` says what was left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evidence {
    /// `check-removed`.
    pub rule: String,
    /// What the rule looks for, in a few words.
    pub rule_meaning: String,
    /// `high`, `medium` or `low`.
    pub confidence: String,
    /// `test/a.test.ts:12`.
    pub place: String,
    /// The tests it's about, as `describe > test`.
    pub tests: Vec<String>,
    /// One line saying what got weaker.
    pub message: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
    /// Code: the tests before and after, diffs of the other files the change touched.
    pub sections: Vec<Section>,
    /// The commits that made it, oldest first.
    pub commits: Vec<Commit>,
    pub pull_request: Option<PullRequest>,
    /// What was left out for size, one line each.
    pub cut: Vec<String>,
}

/// A titled block of code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    /// The code fence's language: `ts`, `diff`, `yaml`.
    pub language: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub short_sha: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub title: String,
    pub body: String,
}

impl Evidence {
    /// The prompt's text about the finding: markdown, finding first.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut put = |text: &str| {
            out.push_str(text);
            out.push('\n');
        };
        put("# Finding");
        put(&format!("Rule: {} ({})", self.rule, self.rule_meaning));
        put(&format!("Confidence: {}", self.confidence));
        put(&format!("Where: {}", self.place));
        match self.tests.as_slice() {
            [] => {}
            [test] => put(&format!("Test: {test}")),
            tests => {
                put("Tests:");
                for test in tests {
                    put(&format!("    {test}"));
                }
            }
        }
        put(&format!("What got weaker: {}", self.message));
        for (label, lines) in [("Before", &self.before), ("After", &self.after)] {
            if lines.is_empty() {
                put(&format!("{label}: (nothing)"));
            } else {
                put(&format!("{label}:"));
                for line in lines {
                    put(&format!("    {line}"));
                }
            }
        }
        for section in &self.sections {
            put("");
            put(&format!("# {}", section.title));
            put(&fenced(&section.language, &section.text));
        }
        for commit in &self.commits {
            put("");
            put(&format!("# Commit {}", commit.short_sha));
            put(commit.message.trim_end());
        }
        if let Some(pr) = &self.pull_request {
            put("");
            put(&format!("# Pull request: {}", pr.title.trim()));
            if !pr.body.trim().is_empty() {
                put(pr.body.trim_end());
            }
        }
        if !self.cut.is_empty() {
            put("");
            put("# Left out for size");
            for cut in &self.cut {
                put(&format!("- {cut}"));
            }
        }
        out
    }
}

/// `text` in a code fence longer than any run of backticks inside it.
fn fenced(language: &str, text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let mut out = String::new();
    write!(out, "{fence}{language}\n{}\n{fence}", text.trim_end()).expect("writing to a String");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_finding_first() {
        let evidence = Evidence {
            rule: "check-removed".to_owned(),
            rule_meaning: "a check disappeared from a test that still exists".to_owned(),
            confidence: "high".to_owned(),
            place: "test/a.test.ts:3".to_owned(),
            tests: vec!["refunds > marks it done".to_owned()],
            message: "removed expect(refund.status).toBe('done')".to_owned(),
            before: vec!["expect(refund.status).toBe('done')".to_owned()],
            after: Vec::new(),
            sections: vec![Section {
                title: "The test before the change".to_owned(),
                language: "ts".to_owned(),
                text: "it('marks it done', () => {\n  expect(refund.status).toBe('done')\n})\n"
                    .to_owned(),
            }],
            commits: vec![Commit {
                short_sha: "1a2b3c4".to_owned(),
                message: "Trim refunds\n\nThe status moved to the event.\n".to_owned(),
            }],
            pull_request: Some(PullRequest {
                title: "Refund events".to_owned(),
                body: String::new(),
            }),
            cut: vec!["src/big.ts (900 lines)".to_owned()],
        };
        insta::assert_snapshot!(evidence.render(), @r"
        # Finding
        Rule: check-removed (a check disappeared from a test that still exists)
        Confidence: high
        Where: test/a.test.ts:3
        Test: refunds > marks it done
        What got weaker: removed expect(refund.status).toBe('done')
        Before:
            expect(refund.status).toBe('done')
        After: (nothing)

        # The test before the change
        ```ts
        it('marks it done', () => {
          expect(refund.status).toBe('done')
        })
        ```

        # Commit 1a2b3c4
        Trim refunds

        The status moved to the event.

        # Pull request: Refund events

        # Left out for size
        - src/big.ts (900 lines)
        ");
    }

    #[test]
    fn fences_are_longer_than_what_they_hold() {
        assert_eq!(fenced("md", "a ``` b"), "````md\na ``` b\n````");
        assert_eq!(fenced("ts", "x"), "```ts\nx\n```");
    }
}
