//! Prompt templates and JSON schemas for the three LLM use cases.
//!
//! Each use case has: a system prompt constant, a schema function, and a
//! builder for the user-facing prompt. Schemas are JSON Schema objects that
//! the provider passes to the model for constrained decoding.
//!
//! The LLM never decides facts — genre/origin/era always come from API data.
//! These prompts constrain the model to producing *filters* and *labels*, not
//! musical knowledge.

use serde_json::{json, Value};

// ── Use case 1: natural language → PlaylistFilter ────────────────────────────

pub const SYSTEM_NL_TO_FILTER: &str = r#"
You are a structured-output translator. The user describes a playlist in free
text (Portuguese or English). You output a JSON object that encodes their
request as a playlist filter, nothing else.

Rules:
- Only output the JSON object, no prose.
- Genre slugs must match the MusicBrainz vocabulary (e.g. "soul", "samba",
  "hip-hop", "bossa-nova"). Do not invent genres.
- Country codes are ISO 3166-1 alpha-2 (e.g. "BR", "US", "GB").
- year_range is [start_year, end_year] inclusive; round decades to the decade
  boundary (e.g. "70s" → [1970, 1979]).
- genre_mode: use "any_with_children" when the user wants sub-genres included
  (e.g. "anything samba"), "any" for exact genre match, "all" when multiple
  genres are required simultaneously.
- If the user does not constrain a field, omit it.
- min_tracks is optional; default 10 if the user does not mention it.
"#;

/// JSON Schema for the PlaylistFilter the model must produce.
pub fn nl_filter_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "genres": {
                "type": "array",
                "items": { "type": "string" },
                "description": "MusicBrainz genre slugs"
            },
            "genre_mode": {
                "type": "string",
                "enum": ["any", "any_with_children", "all"],
                "description": "How to combine multiple genres"
            },
            "countries": {
                "type": "array",
                "items": {
                    "type": "string",
                    "pattern": "^[A-Z]{2}$"
                },
                "description": "ISO 3166-1 alpha-2 codes"
            },
            "year_range": {
                "type": "array",
                "items": { "type": "integer" },
                "minItems": 2,
                "maxItems": 2,
                "description": "[start_year, end_year] inclusive"
            },
            "min_tracks": {
                "type": "integer",
                "minimum": 1
            }
        },
        "required": [],
        "additionalProperties": false
    })
}

/// Build the user prompt, injecting the available genres and countries from
/// the local cache so the model can only pick from what actually exists.
pub fn nl_to_filter_prompt(
    query: &str,
    available_genres: &[String],
    available_countries: &[String],
) -> String {
    let genres = available_genres.join(", ");
    let countries = available_countries.join(", ");
    format!(
        "User request: \"{query}\"\n\n\
         Available genres (choose only from this list): {genres}\n\n\
         Available country codes (choose only from this list): {countries}\n\n\
         Translate the user request into a JSON filter."
    )
}

// ── Use case 2: orphan tag normalisation ─────────────────────────────────────

pub const SYSTEM_NORMALISE_TAGS: &str = r#"
You are a music genre taxonomy mapper. Given a list of unknown raw tags and
the canonical genre vocabulary, map each tag to its best-matching canonical
slug or null if it is not a music genre.

Rules:
- Only output the JSON object, no prose.
- Each key is a raw tag; each value is a canonical slug or null.
- A tag maps to null if it is not a music genre (e.g. "favorites", "seen live",
  "beautiful", "female vocalists", "2000s", nationalities like "brazilian").
- Do not invent new genre slugs — only use slugs from the provided vocabulary.
- If a tag is a variant spelling of a canonical genre, map it.
- If genuinely uncertain, return null — the app will queue it for human review.
"#;

pub fn normalise_tags_schema(tags: &[String]) -> Value {
    let props: serde_json::Map<String, Value> = tags
        .iter()
        .map(|t| {
            (
                t.clone(),
                json!({
                    "oneOf": [
                        { "type": "string" },
                        { "type": "null" }
                    ]
                }),
            )
        })
        .collect();
    json!({
        "type": "object",
        "properties": props,
        "required": tags,
        "additionalProperties": false
    })
}

/// Build the normalisation prompt for a batch of unknown tags.
pub fn normalise_tags_prompt(tags: &[String], vocabulary: &[String]) -> String {
    let tag_list = tags.join(", ");
    let vocab = vocabulary.join(", ");
    format!(
        "Unknown tags to classify: [{tag_list}]\n\n\
         Canonical genre vocabulary (use only these as output values):\n{vocab}\n\n\
         For each unknown tag output its canonical slug or null."
    )
}

// ── Use case 3: playlist naming ───────────────────────────────────────────────

pub const SYSTEM_NAME_PLAYLIST: &str = r#"
You are a creative music playlist curator. Given a description of the filter
used to create a playlist, produce a human-readable name and a one-sentence
description for it.

Rules:
- Output only the JSON object, no prose.
- The name should be 2–6 words, evocative, no quotes, no parentheses.
- The description should be one sentence, factual, mentioning genre/country/era.
- Write in the same language as the locale field (default: "en").
- Do not repeat the word "playlist" in the name.
"#;

pub fn name_playlist_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Short evocative playlist name"
            },
            "description": {
                "type": "string",
                "description": "One-sentence factual description"
            }
        },
        "required": ["name", "description"],
        "additionalProperties": false
    })
}

/// Build the naming prompt from a human-readable filter summary.
///
/// `filter_summary` is built by the caller from the PlaylistFilter fields,
/// e.g. "Brazilian soul music from the 1970s (32 tracks)".
pub fn name_playlist_prompt(filter_summary: &str, locale: &str) -> String {
    format!(
        "Playlist content: {filter_summary}\nLocale: {locale}\n\n\
         Name and describe this playlist."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nl_filter_schema_is_valid_json_schema() {
        let s = nl_filter_schema();
        assert_eq!(s["type"], "object");
        assert!(s["properties"]["genres"].is_object());
        assert!(s["properties"]["year_range"]["minItems"].as_u64() == Some(2));
    }

    #[test]
    fn normalise_tags_schema_has_all_tags_as_required() {
        let tags = vec!["funk".into(), "seen-live".into()];
        let s = normalise_tags_schema(&tags);
        let required = s["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
        assert!(required.iter().any(|v| v == "funk"));
    }

    #[test]
    fn name_playlist_prompt_includes_summary() {
        let p = name_playlist_prompt("Brazilian soul from the 1970s", "pt");
        assert!(p.contains("Brazilian soul from the 1970s"));
        assert!(p.contains("pt"));
    }

    #[test]
    fn nl_to_filter_prompt_injects_lists() {
        let genres = vec!["soul".into(), "samba".into()];
        let countries = vec!["BR".into(), "US".into()];
        let p = nl_to_filter_prompt("soul brasileiro dos anos 70", &genres, &countries);
        assert!(p.contains("soul"));
        assert!(p.contains("BR"));
    }
}
