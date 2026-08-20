//! Ollama provider — a local model over Ollama's OpenAI-compatible surface.
//!
//! This is the private, zero-cost path: nothing about the user's listening
//! habits leaves the machine. Qwen3 8B or Gemma 3 4B are both adequate, because
//! the only jobs asked of the model are constrained ones.

use super::LlmProvider;
use crate::error::{CoreError, Result};
use serde::Deserialize;
use serde_json::{json, Value};

pub struct Ollama {
    http: reqwest::Client,
    /// Base URL, e.g. `http://127.0.0.1:11434`.
    base_url: String,
    model: String,
}

impl Ollama {
    pub fn new(http: reqwest::Client, base_url: String, model: String) -> Self {
        Ollama {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/v1/chat/completions", self.base_url)
    }

    /// Whether the daemon is reachable and the configured model is present.
    /// Used by the Settings screen so a misconfiguration is visible before the
    /// user relies on it.
    pub async fn probe(&self) -> Result<bool> {
        let resp = self
            .http
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        let body: Value = resp.json().await?;
        let installed = body["models"]
            .as_array()
            .map(|models| {
                models.iter().any(|m| {
                    m["name"]
                        .as_str()
                        // Ollama reports `qwen3:8b`; a configured bare `qwen3`
                        // should still count as present.
                        .is_some_and(|n| n == self.model || n.starts_with(&format!("{}:", self.model)))
                })
            })
            .unwrap_or(false);
        Ok(installed)
    }
}

impl LlmProvider for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    async fn complete_json(&self, schema: &Value, system: &str, prompt: &str) -> Result<Value> {
        let body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": prompt }
            ],
            // Constrained decoding against the schema. Small models ignore a
            // plain "reply with JSON" instruction often enough that this matters.
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "result", "strict": true, "schema": schema }
            },
            // Deterministic: the same query should yield the same filter.
            "temperature": 0,
            "stream": false
        });

        let resp = self.http.post(self.chat_url()).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;

        if !status.is_success() {
            // A connection refused surfaces earlier as a transport error; this
            // is the daemon answering with a complaint, most often an unpulled
            // model.
            return Err(CoreError::upstream(
                "ollama",
                format!("HTTP {}: {}", status.as_u16(), truncate(&text, 400)),
            ));
        }

        let parsed: ChatResponse = serde_json::from_str(&text)?;
        parse_chat(&parsed)
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<ChatMessage>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
}

/// Pull the JSON object out of a chat-completions response. Separated from the
/// HTTP call so it is testable without a running daemon.
fn parse_chat(resp: &ChatResponse) -> Result<Value> {
    let content = resp
        .choices
        .first()
        .and_then(|c| c.message.as_ref())
        .and_then(|m| m.content.as_deref())
        .ok_or_else(|| CoreError::upstream("ollama", "response contained no message content"))?;
    super::extract_json(content)
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
    fn normalises_a_trailing_slash_in_the_base_url() {
        let o = Ollama::new(
            reqwest::Client::new(),
            "http://127.0.0.1:11434/".into(),
            "qwen3:8b".into(),
        );
        assert_eq!(o.chat_url(), "http://127.0.0.1:11434/v1/chat/completions");
    }

    #[test]
    fn extracts_the_filter_from_a_chat_response() {
        let resp: ChatResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"role":"assistant",
                "content":"{\"genres\":[\"soul\"],\"year_range\":[1970,1979]}"}}]}"#,
        )
        .unwrap();
        let v = parse_chat(&resp).unwrap();
        assert_eq!(v["genres"][0], "soul");
        assert_eq!(v["year_range"][0], 1970);
    }

    #[test]
    fn recovers_from_a_chatty_local_model() {
        // Small models often wrap the object despite constrained decoding.
        let resp: ChatResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"Here you go:\n```json\n{\"countries\":[\"BR\"]}\n```"}}]}"#,
        )
        .unwrap();
        assert_eq!(parse_chat(&resp).unwrap()["countries"][0], "BR");
    }

    #[test]
    fn reports_a_response_with_no_choices() {
        let resp: ChatResponse = serde_json::from_str(r#"{"choices":[]}"#).unwrap();
        let err = parse_chat(&resp).unwrap_err();
        assert!(matches!(err, CoreError::Upstream { .. }), "{err}");
    }

    #[test]
    fn reports_a_choice_with_no_content() {
        let resp: ChatResponse =
            serde_json::from_str(r#"{"choices":[{"message":{"role":"assistant"}}]}"#).unwrap();
        assert!(parse_chat(&resp).is_err());
    }
}
