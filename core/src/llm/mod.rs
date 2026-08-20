//! Optional LLM layer.
//!
//! The model is an accessory, never the source of musical fact. It is used in
//! exactly three places:
//!
//! 1. **Natural-language → [`PlaylistFilter`]** — the highest-value use. The
//!    model only produces the *filter*; execution is SQL over the local cache,
//!    and the filter is validated against `genre_canonical` first, so a
//!    hallucinated genre is rejected rather than silently returning nothing.
//! 2. **Orphan tag normalisation** — mapping unknown raw tags onto the canonical
//!    vocabulary. Answers are written to `genre_alias`, so each tag is decided
//!    once in the lifetime of the app and is a dictionary lookup thereafter.
//! 3. **Playlist naming** — cosmetic.
//!
//! It is deliberately **not** used to determine the genre, origin or year of a
//! track from parametric knowledge. That is exactly where a small model
//! hallucinates confidently, and the result would be a "Brazilian soul"
//! playlist with a track from Detroit. Facts come from an API with a traceable
//! row in `tag_signal`.

pub mod anthropic;
pub mod ollama;
pub mod prompts;

use crate::config::{LlmProviderKind, Settings};
use crate::error::{CoreError, Result};
use serde_json::Value;

/// A provider that can return JSON conforming to a supplied schema.
pub trait LlmProvider {
    /// Complete `prompt`, constrained to `schema`.
    ///
    /// `schema` is a JSON Schema object. Implementations must reject or repair
    /// output that does not parse; callers still validate the *content*.
    fn complete_json(
        &self,
        schema: &Value,
        system: &str,
        prompt: &str,
    ) -> impl std::future::Future<Output = Result<Value>> + Send;

    /// Human-readable provider name, for the Settings screen.
    fn name(&self) -> &'static str;
}

/// Runtime-selected provider.
///
/// An enum rather than `Box<dyn LlmProvider>` because `complete_json` is an
/// async method: this keeps dispatch simple and avoids boxing every future.
pub enum Llm {
    Disabled,
    Ollama(ollama::Ollama),
    Anthropic(anthropic::Anthropic),
}

impl Llm {
    /// Build the provider described by `settings`.
    ///
    /// A misconfigured provider (missing key, missing URL) resolves to
    /// [`Llm::Disabled`] rather than an error: the whole app must keep working
    /// without an LLM, so a bad setting degrades instead of blocking.
    pub fn from_settings(settings: &Settings, http: reqwest::Client) -> Self {
        match settings.llm.provider {
            LlmProviderKind::Disabled => Llm::Disabled,
            LlmProviderKind::Ollama => Llm::Ollama(ollama::Ollama::new(
                http,
                settings.llm.ollama_url.clone(),
                settings.llm.ollama_model.clone(),
            )),
            LlmProviderKind::Anthropic => match settings.llm.anthropic_api_key.as_deref() {
                Some(key) if !key.is_empty() => Llm::Anthropic(anthropic::Anthropic::new(
                    http,
                    key.to_string(),
                    settings.llm.anthropic_model.clone(),
                )),
                _ => {
                    tracing::warn!("Anthropic provider selected but no API key is set; disabling");
                    Llm::Disabled
                }
            },
        }
    }

    pub fn is_enabled(&self) -> bool {
        !matches!(self, Llm::Disabled)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Llm::Disabled => "disabled",
            Llm::Ollama(p) => p.name(),
            Llm::Anthropic(p) => p.name(),
        }
    }

    pub async fn complete_json(&self, schema: &Value, system: &str, prompt: &str) -> Result<Value> {
        match self {
            Llm::Disabled => Err(CoreError::Config(
                "no LLM provider is configured".into(),
            )),
            Llm::Ollama(p) => p.complete_json(schema, system, prompt).await,
            Llm::Anthropic(p) => p.complete_json(schema, system, prompt).await,
        }
    }
}

/// Extract the first JSON object from a model response.
///
/// Even with schema-constrained decoding, local models sometimes wrap the object
/// in prose or a fenced code block. Recovering the object is cheaper than
/// failing the user's query over formatting.
pub fn extract_json(text: &str) -> Result<Value> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }

    // Strip a ```json … ``` fence if present.
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.rsplit_once("```").map(|(body, _)| body))
        .unwrap_or(trimmed);
    if let Ok(v) = serde_json::from_str::<Value>(unfenced.trim()) {
        return Ok(v);
    }

    // Last resort: the outermost brace-balanced span.
    let bytes = unfenced.as_bytes();
    let start = unfenced.find('{').ok_or_else(|| {
        CoreError::other(format!(
            "model response contained no JSON object: {}",
            truncate(unfenced, 200)
        ))
    })?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(serde_json::from_str(&unfenced[start..=i])?);
                }
            }
            _ => {}
        }
    }
    Err(CoreError::other(format!(
        "model response had unbalanced JSON: {}",
        truncate(unfenced, 200)
    )))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_json() {
        let v = extract_json(r#"{"genres":["soul"]}"#).unwrap();
        assert_eq!(v["genres"][0], "soul");
    }

    #[test]
    fn recovers_json_from_a_fenced_block() {
        let v = extract_json("```json\n{\"genres\":[\"samba\"]}\n```").unwrap();
        assert_eq!(v["genres"][0], "samba");

        let v2 = extract_json("```\n{\"a\":1}\n```").unwrap();
        assert_eq!(v2["a"], 1);
    }

    #[test]
    fn recovers_json_surrounded_by_prose() {
        // Small local models are prone to this.
        let v = extract_json("Sure! Here is the filter:\n{\"countries\":[\"BR\"]}\nHope that helps.")
            .unwrap();
        assert_eq!(v["countries"][0], "BR");
    }

    #[test]
    fn handles_braces_inside_strings() {
        let v = extract_json(r#"{"note":"a } brace","n":1}"#).unwrap();
        assert_eq!(v["note"], "a } brace");
        assert_eq!(v["n"], 1);
    }

    #[test]
    fn handles_nested_objects() {
        let v = extract_json("prefix {\"a\":{\"b\":{\"c\":2}}} suffix").unwrap();
        assert_eq!(v["a"]["b"]["c"], 2);
    }

    #[test]
    fn rejects_responses_with_no_json() {
        assert!(extract_json("I'm not sure what you mean.").is_err());
        assert!(extract_json("").is_err());
    }

    #[test]
    fn rejects_unbalanced_json() {
        assert!(extract_json(r#"{"genres":["soul""#).is_err());
    }

    #[tokio::test]
    async fn disabled_provider_reports_config_error_without_network() {
        let llm = Llm::Disabled;
        assert!(!llm.is_enabled());
        assert_eq!(llm.name(), "disabled");
        let err = llm
            .complete_json(&serde_json::json!({}), "sys", "hi")
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Config(_)), "{err}");
    }

    #[test]
    fn anthropic_without_a_key_degrades_to_disabled() {
        // A misconfigured provider must not break the app.
        let mut settings = Settings::default();
        settings.llm.provider = LlmProviderKind::Anthropic;
        settings.llm.anthropic_api_key = None;
        let llm = Llm::from_settings(&settings, reqwest::Client::new());
        assert!(!llm.is_enabled());
    }

    #[test]
    fn ollama_is_selected_when_configured() {
        let mut settings = Settings::default();
        settings.llm.provider = LlmProviderKind::Ollama;
        let llm = Llm::from_settings(&settings, reqwest::Client::new());
        assert!(llm.is_enabled());
        assert_eq!(llm.name(), "ollama");
    }
}
