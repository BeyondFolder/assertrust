//! Asking a model, through rig (the `llm` feature): Anthropic, OpenAI, Gemini or Ollama,
//! named `provider/model` (`anthropic/claude-sonnet-5-5`). The key comes from the provider's
//! usual environment variable, and so does another address (`ANTHROPIC_BASE_URL`, …).
//!
//! The answer comes back as structured output matching [`schema`](crate::schema). The
//! newest Claude models reject a forced tool call, which is how many libraries ask for
//! JSON; rig sends the schema as `output_config.format` instead.

use std::time::Duration;

use futures::StreamExt as _;
use rig_core::completion::{CompletionRequest, Effort};
use rig_core::operation::Completion;
use rig_core::providers::{anthropic, gemini, ollama, openai};
use rig_core::schemars::Schema;
use rig_core::wire::{Body, Encoded, Mode, Wire, WireFrame};
use rig_core::{DynModel, Model};
use serde_json::Value;

use crate::{Answer, Judge, Judgment, Prompt, Usage, schema};

/// Room for the model's thinking and its one-sentence answer.
const MAX_TOKENS: u64 = 16_000;

/// How long one answer may take before it counts as failed.
const TIMEOUT: Duration = Duration::from_secs(180);

/// Requests sent at once.
const CONCURRENCY: usize = 4;

/// The key a model built only to show what it would send gets.
const PLACEHOLDER: &str = "dry-run";

type Encoder = Box<dyn Fn(CompletionRequest) -> Result<Encoded, String> + Send + Sync>;

/// A model behind one provider's API.
pub struct ModelJudge {
    spec: String,
    model: DynModel<Completion>,
    /// Builds the HTTP request rig would send, for `--judge-dry-run`.
    encode: Encoder,
    effort: Option<Effort>,
    runtime: tokio::runtime::Runtime,
}

impl ModelJudge {
    /// The model `spec` names (`provider/model`), with its key from the environment.
    /// `effort` is how hard it thinks (`low`, `medium`, `high`), or its own default.
    pub fn connect(spec: &str, effort: Option<&str>) -> Result<Self, String> {
        Self::build(spec, effort, true)
    }

    /// The same model without its key, to show what would be sent: nothing can be asked.
    pub fn offline(spec: &str, effort: Option<&str>) -> Result<Self, String> {
        Self::build(spec, effort, false)
    }

    fn build(spec: &str, effort: Option<&str>, keyed: bool) -> Result<Self, String> {
        let (provider, name) = split(spec)?;
        let effort = effort.map(parse_effort).transpose()?;
        let env = |name: &str| format!("{name} isn't set");
        // Without a key, a placeholder: what's shown never holds the real one, even where a
        // provider puts it in the address (Gemini's `?key=`). The address is the real one.
        let address = |name: &str| std::env::var(name).ok().filter(|url| !url.is_empty());
        let (model, encode) = match provider {
            "anthropic" => {
                let config = if keyed {
                    anthropic::AnthropicConfig::from_env().map_err(|_| env("ANTHROPIC_API_KEY"))?
                } else {
                    let config = anthropic::AnthropicConfig::new(PLACEHOLDER);
                    match address("ANTHROPIC_BASE_URL") {
                        Some(url) => config.with_base_url(url),
                        None => config,
                    }
                };
                erase(config.client().completion(name))
            }
            "openai" => {
                let config = if keyed {
                    openai::OpenAIConfig::from_env().map_err(|_| env("OPENAI_API_KEY"))?
                } else {
                    let config = openai::OpenAIConfig::new(PLACEHOLDER);
                    match address("OPENAI_BASE_URL") {
                        Some(url) => config.with_base_url(url),
                        None => config,
                    }
                };
                erase(config.client().completion(name))
            }
            "gemini" => {
                let config = if keyed {
                    gemini::GeminiConfig::from_env().map_err(|_| env("GEMINI_API_KEY"))?
                } else {
                    gemini::GeminiConfig::new(PLACEHOLDER)
                };
                erase(config.client().completion(name))
            }
            "ollama" => {
                let config = if keyed {
                    ollama::OllamaConfig::from_env().map_err(|err| err.to_string())?
                } else {
                    let config = ollama::OllamaConfig::new();
                    match address("OLLAMA_API_BASE_URL") {
                        Some(url) => config.with_base_url(url),
                        None => config,
                    }
                };
                erase(config.client().completion(name))
            }
            other => {
                return Err(format!(
                    "unknown model provider `{other}`: use anthropic, openai, gemini or ollama, as in anthropic/claude-sonnet-5-5"
                ));
            }
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| format!("couldn't start the judge's runtime: {err}"))?;
        Ok(Self {
            spec: format!("{provider}/{name}"),
            model,
            encode,
            effort,
            runtime,
        })
    }

    /// The HTTP request that asking about `prompt` sends: method, address and JSON body,
    /// without headers or the address's query (they carry the key).
    pub fn preview(&self, prompt: &Prompt) -> Result<String, String> {
        let encoded = (self.encode)(self.request(prompt))?;
        let request = encoded.request;
        let body = match request.body() {
            Body::Bytes(bytes) => serde_json::from_slice::<Value>(bytes)
                .map(|json| serde_json::to_string_pretty(&json).expect("JSON values serialize"))
                .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned()),
            Body::Multipart(_) => "(a multipart form)".to_owned(),
        };
        let uri = request.uri();
        let address = format!(
            "{}://{}{}",
            uri.scheme_str().unwrap_or("https"),
            uri.authority().map_or("", |authority| authority.as_str()),
            uri.path()
        );
        Ok(format!("{} {address}\n{body}", request.method()))
    }

    fn request(&self, prompt: &Prompt) -> CompletionRequest {
        let schema: Schema = schema()
            .try_into()
            .expect("the answer's schema is a JSON object");
        let mut request = CompletionRequest::new(prompt.user.clone())
            .preamble(prompt.system)
            .output_schema(schema)
            .max_tokens(MAX_TOKENS);
        if let Some(effort) = self.effort {
            request = request.reasoning(effort);
        }
        request
    }

    async fn ask(&self, prompt: &Prompt) -> Result<Answer, String> {
        let call = self.model.call(self.request(prompt));
        let response = tokio::time::timeout(TIMEOUT, call)
            .await
            .map_err(|_| format!("no answer within {}s", TIMEOUT.as_secs()))?
            .map_err(|err| err.to_string())?;
        if let Some(error) = &response.error {
            return Err(error.clone());
        }
        let usage = Usage {
            input_tokens: response.usage.input_tokens.unwrap_or(0),
            output_tokens: response.usage.output_tokens.unwrap_or(0),
            cost: response.usage.cost.as_ref().map(|cost| cost.total),
        };
        let judgment = Judgment::parse(&response.text())?;
        Ok(Answer { judgment, usage })
    }
}

impl Judge for ModelJudge {
    fn model(&self) -> &str {
        &self.spec
    }

    fn judge(&self, prompts: &[Prompt]) -> Vec<Result<Answer, String>> {
        self.runtime.block_on(async {
            let mut answers: Vec<(usize, Result<Answer, String>)> =
                futures::stream::iter(prompts.iter().enumerate())
                    .map(|(i, prompt)| async move { (i, self.ask(prompt).await) })
                    .buffer_unordered(CONCURRENCY)
                    .collect()
                    .await;
            answers.sort_by_key(|(i, _)| *i);
            answers.into_iter().map(|(_, answer)| answer).collect()
        })
    }
}

/// A provider's model, erased so every provider looks the same, and how to build what it
/// sends.
fn erase<W>(model: Model<W>) -> (DynModel<Completion>, Encoder)
where
    W: Wire<Op = Completion, Payload = Encoded, Frame = WireFrame>,
{
    let wire = model.wire.clone();
    let encode: Encoder = Box::new(move |request| {
        wire.encode(request, Mode::Unary)
            .map_err(|err| err.to_string())
    });
    (model.erase(), encode)
}

/// `anthropic/claude-sonnet-5-5` → (`anthropic`, `claude-sonnet-5-5`). A Claude model
/// without a provider is Anthropic's.
fn split(spec: &str) -> Result<(&str, &str), String> {
    match spec.split_once('/') {
        Some((provider, name)) if !provider.is_empty() && !name.is_empty() => Ok((provider, name)),
        None if spec.starts_with("claude-") => Ok(("anthropic", spec)),
        _ => Err(format!(
            "the model `{spec}` should be provider/model, as in anthropic/claude-sonnet-5-5"
        )),
    }
}

fn parse_effort(name: &str) -> Result<Effort, String> {
    match name {
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        other => Err(format!(
            "the judge's effort is `{other}`; it can be low, medium or high"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_provider_and_a_model() {
        assert_eq!(
            split("anthropic/claude-haiku-5-5").unwrap(),
            ("anthropic", "claude-haiku-5-5")
        );
        assert_eq!(
            split("claude-haiku-5-5").unwrap(),
            ("anthropic", "claude-haiku-5-5")
        );
        assert_eq!(split("ollama/qwen3:8b").unwrap(), ("ollama", "qwen3:8b"));
        assert!(split("gpt-5").is_err());
        assert!(split("/x").is_err());
    }

    #[test]
    fn shows_what_it_would_send_without_a_key() {
        let judge = ModelJudge::offline("anthropic/claude-haiku-5-5", Some("low")).unwrap();
        let prompt = Prompt {
            system: crate::SYSTEM,
            user: "# Finding".to_owned(),
        };
        let preview = judge.preview(&prompt).unwrap();
        assert!(preview.starts_with("POST "), "{preview}");
        assert!(preview.contains("/v1/messages"), "{preview}");
        assert!(preview.contains("\"json_schema\""), "{preview}");
        assert!(preview.contains("# Finding"), "{preview}");
        assert!(!preview.contains("dry-run"), "no key in it: {preview}");
    }
}

#[cfg(test)]
mod providers {
    use super::*;

    /// Every provider gets the answer's schema as structured output, in its own API's shape.
    #[test]
    fn every_provider_is_sent_the_schema() {
        let prompt = Prompt {
            system: crate::SYSTEM,
            user: "# Finding".to_owned(),
        };
        for spec in [
            "openai/gpt-5.2",
            "gemini/gemini-3.8-flash",
            "ollama/qwen3:8b",
        ] {
            let preview = ModelJudge::offline(spec, None)
                .unwrap()
                .preview(&prompt)
                .unwrap();
            assert!(preview.contains("coverage-lost"), "{spec}: {preview}");
            assert!(preview.contains("# Finding"), "{spec}: {preview}");
            // Gemini's key goes in the address; not in what's shown.
            assert!(!preview.contains(PLACEHOLDER), "{spec}: {preview}");
        }
    }
}
