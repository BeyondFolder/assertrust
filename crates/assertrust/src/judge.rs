//! The AI judge in a check (`docs/architecture.md`, step 7): which findings it's asked about,
//! its verdicts (from the cache, else from the model), and what they change.
//!
//! ```text
//! test/forwarding.test.ts:42  HIGH  check-removed
//!   removed 2 checks, including expect(address).toMatchObject({…}) · 10d8742
//!   judge: coverage lost: nothing checks that a new address starts pending
//! ```
//!
//! - It's asked about shown findings and changed outcomes (left to it), most sure first, up
//!   to `max-findings`. Allowed findings, those undone later and those on code the PR added
//!   can't fail the check, so it isn't asked about them.
//! - A changed outcome it doesn't call fine is shown. One it calls fine stays hidden.
//! - A shown finding it calls fine is listed apart, folded away. It's never dropped.
//! - Failing is soft: no key, no network, a bad answer, and findings are shown without
//!   verdicts, with a note. A judge that didn't answer never fails the check.

use std::env;
use std::fs;
use std::path::Path;

use assertrust_git::{Analysis, Comparison, Rev};
use assertrust_judge::{Cache, Judge, Judgment, Prompt, PullRequest, Usage, Verdict};
use serde_json::Value;

use crate::config::JudgeConfig;
use crate::evidence::{self, Files};
use crate::output::count;
use crate::report::{Report, Reported, Status};

/// Where verdicts are kept, from the repo root.
pub const CACHE_DIR: &str = ".assertrust/cache/judge";

/// Which findings go to the judge.
pub struct Scope {
    /// Also those on code a PR added (a backtest, to measure the judge on them).
    pub pr_added: bool,
    /// The most it's asked about, or no limit.
    pub cap: Option<usize>,
}

/// What the judge did in one check, for its summary line.
#[derive(Debug, Default)]
pub struct Summary {
    /// `provider/model`.
    pub model: String,
    /// How hard it thought, when set.
    pub effort: Option<String>,
    /// Findings given a verdict, from the cache or not.
    pub judged: usize,
    /// Of those, verdicts from the cache.
    pub cached: usize,
    /// Findings it was asked about and gave no verdict for.
    pub failed: usize,
    /// Findings the cap kept from it.
    pub capped: usize,
    pub usage: Usage,
}

impl Summary {
    /// `judge: anthropic/claude-haiku-5-5 · 3 judged, 1 from the cache · 12.3K tokens in,
    /// 0.4K out · $0.01`
    pub fn line(&self) -> String {
        let mut parts = vec![match &self.effort {
            Some(effort) => format!("judge: {} (effort {effort})", self.model),
            None => format!("judge: {}", self.model),
        }];
        let mut judged = format!("{} judged", self.judged);
        if self.cached > 0 {
            judged.push_str(&format!(", {} from the cache", self.cached));
        }
        parts.push(judged);
        if self.failed > 0 {
            parts.push(format!("{} without a verdict", self.failed));
        }
        if self.capped > 0 {
            parts.push(format!("{} not judged (max-findings)", self.capped));
        }
        if self.usage.input_tokens + self.usage.output_tokens > 0 {
            parts.push(format!(
                "{} tokens in, {} out",
                thousands(self.usage.input_tokens),
                thousands(self.usage.output_tokens)
            ));
        }
        match self.usage.cost {
            Some(cost) if cost >= 0.005 => parts.push(format!("${cost:.2}")),
            Some(cost) if cost > 0.0 => parts.push("<$0.01".to_owned()),
            _ => {}
        }
        parts.join(" · ")
    }
}

/// `1234` → `1.2K`.
fn thousands(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}K", n as f64 / 1000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Whether a finding is about code the PR added: added by the PR, or hidden and found in one
/// of its commits but not in the whole PR (in a PR-style check, whose whole change is
/// `net`), which a check keeps only when it's still weaker at the head.
pub fn pr_added(reported: &Reported, net: Option<&Comparison>) -> bool {
    match reported.status {
        Status::PrAdded => true,
        Status::Hidden => net.is_some_and(|net| {
            reported
                .comparison
                .is_some_and(|comparison| !std::ptr::eq(comparison, net))
        }),
        _ => false,
    }
}

/// The findings the judge is asked about, by index, most sure first, and how many more the
/// cap kept from it. `net` is a PR-style check's whole change.
pub fn select(report: &Report, scope: &Scope, net: Option<&Comparison>) -> (Vec<usize>, usize) {
    let wanted = |reported: &Reported| match reported.status {
        _ if pr_added(reported, net) => scope.pr_added,
        Status::Shown | Status::Hidden => true,
        Status::PrAdded | Status::Allowed { .. } | Status::Transient => false,
    };
    let mut picked: Vec<usize> = (0..report.findings.len())
        .filter(|&i| wanted(&report.findings[i]))
        .collect();
    // Shown ones first, each kind most sure first (stable: then in report order).
    picked.sort_by_key(|&i| {
        let reported = &report.findings[i];
        (
            reported.status == Status::Hidden,
            reported.finding.confidence,
        )
    });
    let capped = match scope.cap {
        Some(cap) if picked.len() > cap => picked.split_off(cap).len(),
        _ => 0,
    };
    (picked, capped)
}

/// What the judge is sent about each picked finding.
pub fn prompts(
    repo: &assertrust_git::Repo,
    report: &Report,
    picked: &[usize],
    pull_request: Option<&PullRequest>,
) -> Vec<Prompt> {
    let mut files = Files::default();
    picked
        .iter()
        .map(|&i| {
            Prompt::new(&evidence::gather(
                repo,
                &report.findings[i],
                pull_request,
                &mut files,
            ))
        })
        .collect()
}

/// What the judge said about each prompt, and notes on what went wrong.
pub struct Asked {
    pub judgments: Vec<Option<Judgment>>,
    pub summary: Summary,
    pub notes: Vec<String>,
}

/// Who answers, as the cache keys it: the model, and how hard it thinks when that's set
/// (`anthropic/claude-haiku-5-5 (effort high)`).
pub fn judge_name(judge: &JudgeConfig) -> String {
    let model = model_name(&judge.model);
    match judge.effort() {
        Some(effort) => format!("{model} (effort {effort})"),
        None => model,
    }
}

/// The verdict on each prompt: from the cache under `root`, else from the model, which is
/// only reached when something isn't cached.
pub fn ask(root: &Path, judge: &JudgeConfig, prompts: &[Prompt]) -> Asked {
    let cache = Cache::new(root.join(CACHE_DIR));
    let name = judge_name(judge);
    let keys: Vec<String> = prompts.iter().map(|prompt| prompt.key(&name)).collect();
    let mut judgments: Vec<Option<Judgment>> = keys.iter().map(|key| cache.get(key)).collect();
    let mut summary = Summary {
        model: model_name(&judge.model),
        effort: judge.effort().map(str::to_owned),
        cached: judgments.iter().flatten().count(),
        ..Summary::default()
    };
    let mut notes = Vec::new();
    let missing: Vec<usize> = (0..prompts.len())
        .filter(|&i| judgments[i].is_none())
        .collect();
    if !missing.is_empty() {
        match connect(judge) {
            Err(err) => {
                notes.push(format!(
                    "the judge didn't run ({err}), so {} have no verdict",
                    count(missing.len(), "finding")
                ));
                summary.failed = missing.len();
            }
            Ok(model) => {
                let asked: Vec<Prompt> = missing.iter().map(|&i| prompts[i].clone()).collect();
                let mut first_error = None;
                for (&i, answer) in missing.iter().zip(model.judge(&asked)) {
                    match answer {
                        Ok(answer) => {
                            summary.usage += answer.usage;
                            // Not keeping it only means asking again next time.
                            let _ = cache.put(&keys[i], &answer.judgment);
                            judgments[i] = Some(answer.judgment);
                        }
                        Err(err) => {
                            summary.failed += 1;
                            first_error.get_or_insert(err);
                        }
                    }
                }
                if let Some(err) = first_error {
                    notes.push(format!(
                        "the judge gave no verdict on {} ({err})",
                        count(summary.failed, "finding")
                    ));
                }
            }
        }
    }
    summary.judged = judgments.iter().flatten().count();
    Asked {
        judgments,
        summary,
        notes,
    }
}

/// Puts the verdicts on the findings: a changed outcome it doesn't call fine is shown, or
/// added by the PR when it's about code the PR added (`net` as in [`select`]).
pub fn apply(
    report: &mut Report,
    picked: &[usize],
    judgments: Vec<Option<Judgment>>,
    net: Option<&Comparison>,
) {
    for (&i, judgment) in picked.iter().zip(judgments) {
        let reported = &mut report.findings[i];
        if let Some(judgment) = &judgment
            && reported.status == Status::Hidden
            && judgment.verdict != Verdict::Fine
        {
            reported.status = if pr_added(reported, net) {
                Status::PrAdded
            } else {
                Status::Shown
            };
        }
        reported.judgment = judgment;
    }
}

/// What the judge would be sent about each finding not in the cache under `root`, for
/// `--judge-dry-run`.
pub fn dry_run(
    root: &Path,
    judge: &JudgeConfig,
    report: &Report,
    picked: &[usize],
    prompts: &[Prompt],
) -> Result<String, String> {
    let model = judge_name(judge);
    let cache = Cache::new(root.join(CACHE_DIR));
    let mut out = String::new();
    let mut cached = 0;
    let preview = previewer(judge)?;
    for (&i, prompt) in picked.iter().zip(prompts) {
        if cache.get(&prompt.key(&model)).is_some() {
            cached += 1;
            continue;
        }
        let finding = &report.findings[i].finding;
        out.push_str(&format!(
            "== {}  {}\n{}\n\n",
            crate::output::place(finding),
            finding.rule,
            preview(prompt)?
        ));
    }
    let sent = prompts.len() - cached;
    out.push_str(&format!(
        "{} would be sent to {model}",
        count(sent, "request")
    ));
    if cached > 0 {
        out.push_str(&format!(" ({cached} more answered from the cache)"));
    }
    out.push('\n');
    Ok(out)
}

/// The model `provider/model` names; a Claude model without a provider is Anthropic's.
pub fn model_name(model: &str) -> String {
    if !model.contains('/') && model.starts_with("claude-") {
        format!("anthropic/{model}")
    } else {
        model.to_owned()
    }
}

#[cfg(feature = "llm")]
fn connect(judge: &JudgeConfig) -> Result<Box<dyn Judge>, String> {
    let model = assertrust_judge::ModelJudge::connect(&judge.model, judge.effort())?;
    Ok(Box::new(model))
}

#[cfg(not(feature = "llm"))]
fn connect(_judge: &JudgeConfig) -> Result<Box<dyn Judge>, String> {
    Err(NOT_BUILT.to_owned())
}

type Preview = Box<dyn Fn(&Prompt) -> Result<String, String>>;

#[cfg(feature = "llm")]
fn previewer(judge: &JudgeConfig) -> Result<Preview, String> {
    let model = assertrust_judge::ModelJudge::offline(&judge.model, judge.effort())?;
    Ok(Box::new(move |prompt| model.preview(prompt)))
}

#[cfg(not(feature = "llm"))]
fn previewer(_judge: &JudgeConfig) -> Result<Preview, String> {
    Err(NOT_BUILT.to_owned())
}

#[cfg(not(feature = "llm"))]
const NOT_BUILT: &str = "this assertrust was built without the judge (cargo feature `llm`)";

/// The pull request a check in GitHub Actions runs for, from the event that started it: its
/// title and description, when its head is the commit being checked.
pub fn pull_request(analysis: &Analysis) -> Option<PullRequest> {
    let name = env::var("GITHUB_EVENT_NAME").ok()?;
    if !name.starts_with("pull_request") {
        return None;
    }
    let event: Value =
        serde_json::from_str(&fs::read_to_string(env::var("GITHUB_EVENT_PATH").ok()?).ok()?)
            .ok()?;
    let pr = &event["pull_request"];
    let head = match analysis.net.as_ref().map(|net| &net.after) {
        Some(Rev::Commit(sha)) => sha.as_str(),
        _ => analysis.steps.last()?.commit.sha.as_str(),
    };
    (pr["head"]["sha"].as_str()? == head).then(|| PullRequest {
        title: pr["title"].as_str().unwrap_or_default().to_owned(),
        body: pr["body"].as_str().unwrap_or_default().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_tokens_briefly() {
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(12_345), "12.3K");
        assert_eq!(thousands(3_210_000), "3.2M");
    }

    #[test]
    fn names_models_with_their_provider() {
        assert_eq!(model_name("claude-haiku-5-5"), "anthropic/claude-haiku-5-5");
        assert_eq!(model_name("openai/gpt-5.2"), "openai/gpt-5.2");
    }

    #[test]
    fn summarizes_what_it_cost() {
        let summary = Summary {
            model: "anthropic/claude-haiku-5-5".to_owned(),
            effort: None,
            judged: 3,
            cached: 1,
            failed: 0,
            capped: 2,
            usage: Usage {
                input_tokens: 12_300,
                output_tokens: 400,
                cost: Some(0.0145),
            },
        };
        assert_eq!(
            summary.line(),
            "judge: anthropic/claude-haiku-5-5 · 3 judged, 1 from the cache · 2 not judged (max-findings) · 12.3K tokens in, 400 out · $0.01"
        );
    }
}
