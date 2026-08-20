//! Wikidata SPARQL enrichment client.
//!
//! Used as a supplementary source for artist origin when MusicBrainz does not
//! carry geographic data. The Wikidata QID comes from MusicBrainz's url-rels
//! (type=="wikidata"), stored in `mb_artist.wikidata_qid`.
//!
//! Rate limit: no published limit; we use 2 req/s to be a good citizen on a
//! free public endpoint.

use crate::error::Result;
use serde_json::Value;

use super::fetch::Fetcher;
use super::ratelimit::Host;
use crate::model::Source;

const SPARQL_ENDPOINT: &str = "https://query.wikidata.org/sparql";

pub struct WikidataClient {
    pub fetcher: Fetcher,
}

impl WikidataClient {
    pub fn new(fetcher: Fetcher) -> Self {
        WikidataClient { fetcher }
    }

    /// Fetch the ISO 3166-1 alpha-2 country code for an entity identified by
    /// its Wikidata QID.
    ///
    /// Tries two Wikidata properties in one query:
    ///  - P495 (country of origin) — for recordings and groups.
    ///  - P740 (location of formation) → P17 (country) — for bands.
    ///
    /// Returns the first non-empty alpha-2 code found, or `None` if Wikidata has
    /// no geographic data for this entity.
    pub async fn country_of_origin(&self, wikidata_qid: &str) -> Result<Option<String>> {
        let query = format!(
            r#"SELECT ?countryCode WHERE {{
  {{
    wd:{qid} wdt:P495 ?country .
    ?country wdt:P297 ?countryCode .
  }} UNION {{
    wd:{qid} wdt:P740 ?location .
    ?location wdt:P17 ?country .
    ?country wdt:P297 ?countryCode .
  }}
}} LIMIT 1"#,
            qid = wikidata_qid
        );

        let url = format!(
            "{SPARQL_ENDPOINT}?format=json&query={}",
            url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
        );

        let body = match self
            .fetcher
            .get(Host::Wikidata, Source::Wikidata, &url)
            .await
        {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };

        let v: Value = serde_json::from_str(&body)?;
        let code = v["results"]["bindings"]
            .as_array()
            .and_then(|bindings| bindings.first())
            .and_then(|b| b["countryCode"]["value"].as_str())
            .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()))
            .map(|s| s.to_uppercase());

        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn parses_sparql_binding() {
        let v = json!({
            "results": {
                "bindings": [
                    {"countryCode": {"type": "literal", "value": "BR"}}
                ]
            }
        });
        let code = v["results"]["bindings"]
            .as_array()
            .and_then(|b| b.first())
            .and_then(|b| b["countryCode"]["value"].as_str())
            .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()))
            .map(|s| s.to_uppercase());
        assert_eq!(code, Some("BR".to_string()));
    }

    #[test]
    fn empty_bindings_returns_none() {
        let v = json!({"results": {"bindings": []}});
        let code = v["results"]["bindings"]
            .as_array()
            .and_then(|b| b.first())
            .and_then(|b| b["countryCode"]["value"].as_str())
            .map(|s| s.to_string());
        assert!(code.is_none());
    }

    #[test]
    fn rejects_non_alpha2_codes() {
        let v = json!({
            "results": {
                "bindings": [
                    {"countryCode": {"value": "BRA"}}  // 3-letter code, should be rejected
                ]
            }
        });
        let code = v["results"]["bindings"]
            .as_array()
            .and_then(|b| b.first())
            .and_then(|b| b["countryCode"]["value"].as_str())
            .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()))
            .map(|s| s.to_uppercase());
        assert!(code.is_none());
    }
}
