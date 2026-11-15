//! The optional AI judge (`docs/architecture.md`, step 7): asks a model whether a test still
//! checks what it promises, after a change a rule found weaker.
//!
//! ```text
//! test/forwarding.test.ts:42  HIGH  check-removed
//!   removed 2 checks, including expect(address).toMatchObject({…}) · 10d8742
//!   judge: coverage lost: the title still promises "only forwards once confirmed", but
//!   nothing checks that a new address starts pending
//! ```
//!
//! The command line gathers what the judge sees ([`Evidence`]). This crate turns it into a
//! [`Prompt`], keeps verdicts in a [`Cache`] so the same change always gets the same one, and,
//! with the `llm` feature, asks a model through rig ([`ModelJudge`]). Nothing here runs
//! unless someone turns the judge on: the rules never need it.

mod cache;
mod evidence;
#[cfg(feature = "llm")]
mod model;

use std::fmt;
use std::ops::AddAssign;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub use crate::cache::Cache;
pub use crate::evidence::{Commit, Evidence, PullRequest, Section};
#[cfg(feature = "llm")]
pub use crate::model::ModelJudge;

/// The prompt's version: part of every cache key, so changing the prompt asks again.
pub const PROMPT_VERSION: u32 = 1;

/// What the judge is told about its job, the same for every finding.
pub const SYSTEM: &str = "\
You review one finding from assertrust, a tool that flags changes that may weaken a \
JavaScript or TypeScript test suite: a check removed, a test skipped, a looser matcher, a \
changed expected value, CI that lets failures through. Its rules only read test code, so \
many findings are false alarms or deliberate changes. Answer one question:

Does the test suite still check what it promised, after this change?

The question is about coverage, not intent. A deliberate, well-explained change can still \
lose coverage, and an unexplained one can still be harmless.

Pick one verdict:
- \"fine\": nothing the tests promised is checked less than before. The finding misreads the \
change (the same thing is still checked, elsewhere or another way; a check was rewritten or \
moved; a renamed test kept its checks), or the behaviour the check covered was removed or \
changed on purpose in this same change, so the old check no longer applies and any new \
behaviour is checked, or a new expected value follows a change to the code that produces it.
- \"coverage-lost\": behaviour that still exists, and that the test's title or the tests \
around it still promise, is no longer checked or is checked more loosely, even if the change \
explains why.
- \"suspicious\": an expected value, check or test was changed or removed so that a failing \
test passes, and nothing in the change explains it: no related change to the code under \
test, no reason in the commit messages or the pull request.
- \"unsure\": what you're shown isn't enough to tell. Prefer it to guessing.

How to decide:
- A test's title, with its describe blocks, is its promise. Compare it with what the checks \
verify after the change.
- When the code under test changed its behaviour in this change, a test that follows it is \
fine. When only the test changed, be skeptical.
- Commit messages and the pull request say what was meant: weigh them, but check them \
against the code.
- For CI and test-runner config, the promise is that the test runs happen and fail the \
build: a job or step that no longer runs or no longer fails is lost coverage unless \
something replaces it.
- Some context is cut for size, and marked as cut. Don't assume what you aren't shown.

Reply with JSON: {\"verdict\": \"...\", \"reason\": \"...\"}. The reason is one sentence of \
at most 30 words, for the developer who made the change, naming the check or behaviour.";

/// What the judge says about a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Verdict {
    /// Nothing the tests promised is checked less.
    Fine,
    /// Behaviour that still exists isn't checked any more, or less strictly.
    CoverageLost,
    /// A check changed to make a test pass, and nothing explains why.
    Suspicious,
    /// Not enough to tell.
    Unsure,
}

impl Verdict {
    pub const ALL: [Self; 4] = [
        Self::Fine,
        Self::CoverageLost,
        Self::Suspicious,
        Self::Unsure,
    ];

    /// The name in answers, config and JSON: `coverage-lost`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fine => "fine",
            Self::CoverageLost => "coverage-lost",
            Self::Suspicious => "suspicious",
            Self::Unsure => "unsure",
        }
    }

    /// The verdict named `name`, as [`Self::as_str`] writes it.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|verdict| verdict.as_str() == name)
    }

    /// How it reads in a sentence: `coverage lost`.
    pub fn words(self) -> &'static str {
        match self {
            Self::CoverageLost => "coverage lost",
            other => other.as_str(),
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A verdict and the one sentence that says why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgment {
    pub verdict: Verdict,
    pub reason: String,
}

impl Judgment {
    /// Reads a model's answer: the JSON object [`schema`] describes, maybe in a code fence.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        // A model without structured outputs may wrap it anyway.
        let json = match (text.find('{'), text.rfind('}')) {
            (Some(start), Some(end)) if start < end => &text[start..=end],
            _ => text,
        };
        let value: Value = serde_json::from_str(json)
            .map_err(|_| format!("the answer isn't the JSON asked for: {}", shorten(text)))?;
        let verdict = value["verdict"]
            .as_str()
            .and_then(Verdict::parse)
            .ok_or_else(|| format!("the answer has no known verdict: {}", shorten(text)))?;
        let reason = value["reason"]
            .as_str()
            .map(|reason| reason.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        Ok(Self { verdict, reason })
    }

    /// The JSON the cache keeps.
    pub fn to_json(&self) -> Value {
        json!({ "verdict": self.verdict.as_str(), "reason": self.reason })
    }
}

/// The JSON schema of an answer, sent as the model's structured output.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": {
                "type": "string",
                "enum": Verdict::ALL.map(Verdict::as_str),
            },
            "reason": { "type": "string" },
        },
        "required": ["verdict", "reason"],
        "additionalProperties": false,
    })
}

/// What's sent about one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: &'static str,
    /// The finding and what the judge sees about it ([`Evidence::render`]).
    pub user: String,
}

impl Prompt {
    pub fn new(evidence: &Evidence) -> Self {
        Self {
            system: SYSTEM,
            user: evidence.render(),
        }
    }

    /// Its key in the cache for `model` (`provider/model`): a hash of everything that
    /// decides the answer.
    pub fn key(&self, model: &str) -> String {
        let input = json!([PROMPT_VERSION, model, self.system, self.user]).to_string();
        Sha256::digest(input.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// What asking cost: tokens, and dollars when the model's prices are known.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: Option<f64>,
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cost = match (self.cost, other.cost) {
            (Some(a), Some(b)) => Some(a + b),
            (cost, None) | (None, cost) => cost,
        };
    }
}

/// A model's answer about one prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub judgment: Judgment,
    pub usage: Usage,
}

/// Something that answers prompts: a model ([`ModelJudge`]), or a stand-in in tests.
pub trait Judge: Sync {
    /// Who answers, as `provider/model`: part of the cache key.
    fn model(&self) -> &str;

    /// Asks about each prompt, several at once: an answer, or why there's none, in order.
    fn judge(&self, prompts: &[Prompt]) -> Vec<Result<Answer, String>>;
}

/// `text`, cut to a line's worth for an error message.
fn shorten(text: &str) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(120) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_answers() {
        let judgment =
            Judgment::parse(r#"{"verdict": "coverage-lost", "reason": "nothing checks\n it"}"#)
                .unwrap();
        assert_eq!(judgment.verdict, Verdict::CoverageLost);
        assert_eq!(judgment.reason, "nothing checks it");
        // Wrapped in a fence by a model without structured outputs.
        let fenced = "```json\n{\"verdict\": \"fine\", \"reason\": \"moved\"}\n```";
        assert_eq!(Judgment::parse(fenced).unwrap().verdict, Verdict::Fine);
        assert!(Judgment::parse("probably fine").is_err());
        assert!(Judgment::parse(r#"{"verdict": "great", "reason": ""}"#).is_err());
    }

    #[test]
    fn verdict_names_round_trip() {
        for verdict in Verdict::ALL {
            assert_eq!(Verdict::parse(verdict.as_str()), Some(verdict));
        }
        assert_eq!(Verdict::CoverageLost.words(), "coverage lost");
    }

    #[test]
    fn keys_change_with_the_model_and_the_prompt() {
        let prompt = Prompt {
            system: SYSTEM,
            user: "finding".to_owned(),
        };
        let key = prompt.key("anthropic/claude-haiku-5-5");
        assert_eq!(key.len(), 64);
        assert_eq!(key, prompt.key("anthropic/claude-haiku-5-5"));
        assert_ne!(key, prompt.key("anthropic/claude-sonnet-5-5"));
        let other = Prompt {
            system: SYSTEM,
            user: "another finding".to_owned(),
        };
        assert_ne!(key, other.key("anthropic/claude-haiku-5-5"));
    }

    #[test]
    fn usage_adds_up() {
        let mut total = Usage::default();
        total += Usage {
            input_tokens: 10,
            output_tokens: 2,
            cost: Some(0.5),
        };
        total += Usage {
            input_tokens: 5,
            output_tokens: 1,
            cost: None,
        };
        assert_eq!(total.input_tokens, 15);
        assert_eq!(total.output_tokens, 3);
        assert_eq!(total.cost, Some(0.5));
    }
}
