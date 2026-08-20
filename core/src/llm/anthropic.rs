//! Anthropic Messages API provider.
//!
//! Rust has no official Anthropic SDK, so this talks to `POST /v1/messages`
//! directly with `reqwest`. Structured output is requested via
//! `output_config.format` with a JSON schema — not tool use, and not the
//! deprecated top-level `output_format` parameter.

use super::LlmProvider;
use crate::error::{CoreError, Result};
use serde::Deserialize;
use serde_json::{json, Value};

const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Pinned per Anthropic's versioning header contract.
const API_VERSION: &str = "2023-06-01";

/// Generous enough that a filter object is never truncated. On current models
/// `max_tokens` bounds thinking *and* response text together, so a tight budget
/// would risk cutting off the JSON mid-object.
const MAX_TOKENS: u32 = 4096;

pub struct Anthropic {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl Anthropic {
    pub fn new(http: reqwest::Client, api_key: String, model: String) -> Self {
        Anthropic {
            http,
            api_key,
            model,
        }
    }
}

impl LlmProvider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn complete_json(&self, schema: &Value, system: &str, prompt: &str) -> Result<Value> {
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": [{ "role": "user", "content": prompt }],
            "output_config": {
                // Turning a short request into a filter is not a reasoning-heavy
                // task; low effort keeps latency and cost down. Thinking is left
                // at its default rather than disabled, which avoids the
                // disabled-thinking failure modes on current models.
                "effort": "low",
                "format": { "type": "json_schema", "schema": schema }
            }
        });

        let resp = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(CoreError::upstream(
                "anthropic",
                format!("HTTP {}: {}", status.as_u16(), truncate(&text, 400)),
            ));
        }

        let message: MessageResponse = serde_json::from_str(&text)?;
        parse_message(&message)
    }
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
}

/// Pull the JSON object out of a Messages API response.
///
/// Separated from the HTTP call so response handling is testable without
/// network access or an API key.
fn parse_message(message: &MessageResponse) -> Result<Value> {
    // A refusal arrives as a normal 200, so checking the status code is not
    // enough — the content array may be empty or hold only a partial answer.
    if message.stop_reason.as_deref() == Some("refusal") {
        return Err(CoreError::upstream(
            "anthropic",
            "the model declined this request",
        ));
    }
    if message.stop_reason.as_deref() == Some("max_tokens") {
        return Err(CoreError::upstream(
            "anthropic",
            "response hit the token limit before completing the JSON object",
        ));
    }

    let text = message
        .content
        .iter()
        // Skip `thinking` blocks, which carry no answer.
        .filter(|b| b.block_type == "text")
        .filter_map(|b| b.text.as_deref())
        .collect::<Vec<_>>()
        .join("");

    if text.trim().is_empty() {
        return Err(CoreError::upstream("anthropic", "response contained no text"));
    }
    super::extract_json(&text)
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

    fn response(json: &str) -> MessageResponse {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn extracts_the_filter_from_a_normal_response() {
        let msg = response(
            r#"{
              "id": "msg_1", "type": "message", "role": "assistant",
              "stop_reason": "end_turn",
              "content": [{"type": "text",
                           "text": "{\"genres\":[\"soul\"],\"countries\":[\"BR\"]}"}]
            }"#,
        );
        let v = parse_message(&msg).unwrap();
        assert_eq!(v["genres"][0], "soul");
        assert_eq!(v["countries"][0], "BR");
    }

    #[test]
    fn ignores_thinking_blocks() {
        let msg = response(
            r#"{
              "stop_reason": "end_turn",
              "content": [
                {"type": "thinking", "thinking": "considering the request"},
                {"type": "text", "text": "{\"genres\":[\"samba\"]}"}
              ]
            }"#,
        );
        assert_eq!(parse_message(&msg).unwrap()["genres"][0], "samba");
    }

    #[test]
    fn joins_split_text_blocks() {
        let msg = response(
            r#"{
              "stop_reason": "end_turn",
              "content": [
                {"type": "text", "text": "{\"genres\":"},
                {"type": "text", "text": "[\"funk\"]}"}
              ]
            }"#,
        );
        assert_eq!(parse_message(&msg).unwrap()["genres"][0], "funk");
    }

    #[test]
    fn treats_a_refusal_as_an_upstream_error_not_a_success() {
        // A refusal is HTTP 200 with an empty content array; code that indexed
        // content[0] would panic here.
        let msg = response(r#"{"stop_reason": "refusal", "content": []}"#);
        let err = parse_message(&msg).unwrap_err();
        assert!(matches!(err, CoreError::Upstream { .. }), "{err}");
        assert!(err.to_string().contains("declined"));
    }

    #[test]
    fn reports_truncation_rather_than_returning_broken_json() {
        let msg = response(
            r#"{"stop_reason": "max_tokens",
                "content": [{"type": "text", "text": "{\"genres\":[\"so"}]}"#,
        );
        let err = parse_message(&msg).unwrap_err();
        assert!(err.to_string().contains("token limit"), "{err}");
    }

    #[test]
    fn reports_an_empty_response() {
        let msg = response(r#"{"stop_reason": "end_turn", "content": []}"#);
        assert!(parse_message(&msg).is_err());
    }

    #[test]
    fn default_model_is_pinned_to_a_current_id() {
        // Guards against an ID with a date suffix, which the API rejects.
        let model = crate::config::LlmSettings::default().anthropic_model;
        assert_eq!(model, "claude-opus-5");
        assert!(!model.contains("2024") && !model.contains("2025"));
    }
}
