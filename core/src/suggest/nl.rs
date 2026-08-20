//! Turning a free-text request into a [`PlaylistFilter`].
//!
//! There are two implementations of the same job, and they must agree:
//!
//! * this module — a deterministic parser handling the shapes that actually come
//!   up ("soul brasileira dos anos 70", "british rock from the 90s");
//! * [`crate::llm`] — an optional model that produces the same JSON.
//!
//! The deterministic parser is not a degraded fallback; it is what makes the app
//! fully usable with no model installed. Whichever produces the filter, the
//! result is validated against the canonical vocabulary before it can run, so an
//! invented genre is rejected rather than silently returning nothing.

use super::filter::{GenreMode, PlaylistFilter};
use crate::error::{CoreError, Result};
use crate::store::Store;

/// Demonyms and country names mapped to ISO 3166-1 alpha-2, in Portuguese and
/// English. Accents are stripped before lookup, so unaccented spellings work too.
const COUNTRY_WORDS: &[(&str, &str)] = &[
    // Brazil — the primary use case, so worth being generous.
    ("brasileira", "BR"), ("brasileiro", "BR"), ("brasileiras", "BR"), ("brasileiros", "BR"),
    ("brasil", "BR"), ("brazilian", "BR"), ("brazil", "BR"),
    ("inglesa", "GB"), ("ingles", "GB"), ("inglesas", "GB"), ("ingleses", "GB"),
    ("inglaterra", "GB"), ("english", "GB"), ("british", "GB"), ("britanica", "GB"),
    ("britanico", "GB"), ("uk", "GB"), ("britain", "GB"),
    ("americana", "US"), ("americano", "US"), ("american", "US"),
    ("estadunidense", "US"), ("eua", "US"), ("usa", "US"),
    ("francesa", "FR"), ("frances", "FR"), ("french", "FR"), ("franca", "FR"), ("france", "FR"),
    ("alema", "DE"), ("alemao", "DE"), ("german", "DE"), ("alemanha", "DE"), ("germany", "DE"),
    ("italiana", "IT"), ("italiano", "IT"), ("italian", "IT"), ("italia", "IT"), ("italy", "IT"),
    ("japonesa", "JP"), ("japones", "JP"), ("japanese", "JP"), ("japao", "JP"), ("japan", "JP"),
    ("jamaicana", "JM"), ("jamaicano", "JM"), ("jamaican", "JM"), ("jamaica", "JM"),
    ("argentina", "AR"), ("argentino", "AR"), ("argentinian", "AR"), ("argentine", "AR"),
    ("portuguesa", "PT"), ("portugues", "PT"), ("portuguese", "PT"), ("portugal", "PT"),
    ("espanhola", "ES"), ("espanhol", "ES"), ("spanish", "ES"), ("espanha", "ES"), ("spain", "ES"),
    ("mexicana", "MX"), ("mexicano", "MX"), ("mexican", "MX"), ("mexico", "MX"),
    ("cubana", "CU"), ("cubano", "CU"), ("cuban", "CU"), ("cuba", "CU"),
    ("nigeriana", "NG"), ("nigeriano", "NG"), ("nigerian", "NG"), ("nigeria", "NG"),
    ("canadense", "CA"), ("canadian", "CA"), ("canada", "CA"),
    ("australiana", "AU"), ("australiano", "AU"), ("australian", "AU"), ("australia", "AU"),
    ("sueca", "SE"), ("sueco", "SE"), ("swedish", "SE"), ("suecia", "SE"), ("sweden", "SE"),
    ("colombiana", "CO"), ("colombiano", "CO"), ("colombian", "CO"), ("colombia", "CO"),
];

/// Parse `query` into a filter, scoped to `playlist_id`.
///
/// Returns [`CoreError::InvalidFilter`] when nothing recognisable is found, so the
/// UI can say what it did not understand instead of showing an empty playlist.
pub fn parse(store: &Store, query: &str, playlist_id: Option<&str>) -> Result<PlaylistFilter> {
    let normalized = fold(query);
    let words: Vec<&str> = normalized.split_whitespace().collect();

    let countries = find_countries(&words);
    let year_range = find_year_range(&normalized);
    let genres = find_genres(store, &normalized)?;

    if genres.is_empty() && countries.is_empty() && year_range.is_none() {
        return Err(CoreError::InvalidFilter(format!(
            "could not find a genre, country or period in “{query}”"
        )));
    }

    let filter = PlaylistFilter {
        genres,
        // Rollup is almost always what a person means: asking for samba should
        // include samba-rock.
        genre_mode: GenreMode::AnyWithChildren,
        countries,
        year_range,
        min_tracks: None,
        max_tracks: None,
        min_genre_score: None,
        source_playlist_id: playlist_id.map(str::to_string),
        exclude_needs_review: false,
    };
    filter.validate(store)?;
    Ok(filter)
}

/// Lowercase and strip diacritics, so `brasileiríssima` and `ingles` both match.
fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        // Keep digits and letters; turn everything else into a separator so
        // punctuation cannot glue words together.
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_combining_mark(c: char) -> bool {
    // Unicode combining diacritical marks, which NFD splits accents into.
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x20D0..=0x20FF)
}

fn find_countries(words: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for word in words {
        if let Some((_, code)) = COUNTRY_WORDS.iter().find(|(w, _)| w == word) {
            if !found.contains(&code.to_string()) {
                found.push(code.to_string());
            }
        }
    }
    found
}

/// Recognise a period: `anos 70`, `década de 1970`, `70s`, `1970s`, `1972-1979`,
/// `de 1970 a 1979`.
fn find_year_range(normalized: &str) -> Option<(i32, i32)> {
    let words: Vec<&str> = normalized.split_whitespace().collect();

    // An explicit span of two four-digit years anywhere in the text.
    let years: Vec<i32> = words
        .iter()
        .filter_map(|w| w.parse::<i32>().ok())
        .filter(|y| (1860..=2100).contains(y))
        .collect();
    if years.len() >= 2 {
        let (from, to) = (years[0].min(years[1]), years[0].max(years[1]));
        if from != to {
            return Some((from, to));
        }
    }

    // A decade, written as a bare number or with an `s` suffix.
    for (i, word) in words.iter().enumerate() {
        // `1970s` / `70s`
        if let Some(stem) = word.strip_suffix('s') {
            if let Some(decade) = decade_from_digits(stem) {
                return Some((decade, decade + 9));
            }
        }
        // `anos 70`, `decada de 70`, `década de 1970`
        let is_decade_word = matches!(*word, "anos" | "ano" | "decada" | "decade" | "década");
        if is_decade_word {
            for next in words.iter().skip(i + 1).take(3) {
                if let Some(decade) = decade_from_digits(next) {
                    return Some((decade, decade + 9));
                }
            }
        }
    }

    // A single bare four-digit year means that year alone.
    if years.len() == 1 {
        return Some((years[0], years[0]));
    }
    None
}

/// Interpret decade digits. Two-digit forms are ambiguous, so use the convention
/// people actually mean: `anos 70` is the 1970s, `anos 20` is the 2020s.
fn decade_from_digits(s: &str) -> Option<i32> {
    if !s.chars().all(|c| c.is_ascii_digit()) || s.is_empty() {
        return None;
    }
    match s.len() {
        2 => {
            let d: i32 = s.parse().ok()?;
            Some(if d >= 30 { 1900 + d } else { 2000 + d })
        }
        4 => {
            let y: i32 = s.parse().ok()?;
            (1860..=2100).contains(&y).then(|| y - y.rem_euclid(10))
        }
        _ => None,
    }
}

/// Find canonical genres named in the text.
///
/// Matches against slugs and labels from the vocabulary, longest first, so
/// "samba rock" wins over "samba" and "rock". Only the vocabulary can produce a
/// genre — the parser never invents one.
fn find_genres(store: &Store, normalized: &str) -> Result<Vec<String>> {
    let mut candidates: Vec<(String, String)> = Vec::new();
    for genre in store.all_canonical_genres()? {
        candidates.push((fold(&genre.slug), genre.slug.clone()));
        let folded_label = fold(&genre.label);
        if folded_label != fold(&genre.slug) {
            candidates.push((folded_label, genre.slug.clone()));
        }
    }
    // Longest surface form first so multi-word genres are not shadowed.
    candidates.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let mut found: Vec<String> = Vec::new();
    // Blank out each match so an inner word cannot match again: after "samba
    // rock" is consumed, plain "rock" must not also be reported.
    let mut haystack = format!(" {normalized} ");
    for (surface, slug) in candidates {
        if surface.is_empty() || found.contains(&slug) {
            continue;
        }
        // Hyphenated slugs appear as spaced words once folded.
        let needle = format!(" {} ", surface.replace('-', " "));
        if let Some(pos) = haystack.find(&needle) {
            found.push(slug);
            let blanked = " ".repeat(needle.len() - 2);
            haystack.replace_range(pos + 1..pos + needle.len() - 1, &blanked);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CanonicalGenre;

    fn store() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.upsert_canonical_genres(&[
            CanonicalGenre { slug: "soul".into(), label: "Soul".into(), parent_slug: None },
            CanonicalGenre { slug: "funk-soul".into(), label: "Funk Soul".into(), parent_slug: Some("soul".into()) },
            CanonicalGenre { slug: "samba".into(), label: "Samba".into(), parent_slug: None },
            CanonicalGenre { slug: "samba-rock".into(), label: "Samba Rock".into(), parent_slug: Some("samba".into()) },
            CanonicalGenre { slug: "rock".into(), label: "Rock".into(), parent_slug: None },
            CanonicalGenre { slug: "hip-hop".into(), label: "Hip-Hop".into(), parent_slug: None },
            CanonicalGenre { slug: "tropicalia".into(), label: "Tropicália".into(), parent_slug: None },
            CanonicalGenre { slug: "mpb".into(), label: "MPB".into(), parent_slug: None },
        ])
        .unwrap();
        s
    }

    #[test]
    fn parses_the_headline_portuguese_request() {
        // The exact phrasing from the project brief.
        let s = store();
        let f = parse(&s, "Soul brasileira da década de 70", Some("p1")).unwrap();
        assert_eq!(f.genres, vec!["soul".to_string()]);
        assert_eq!(f.countries, vec!["BR".to_string()]);
        assert_eq!(f.year_range, Some((1970, 1979)));
        assert_eq!(f.genre_mode, GenreMode::AnyWithChildren);
        assert_eq!(f.source_playlist_id.as_deref(), Some("p1"));
    }

    #[test]
    fn parses_the_english_equivalent_identically() {
        let s = store();
        let pt = parse(&s, "soul brasileira dos anos 70", None).unwrap();
        let en = parse(&s, "Brazilian soul from the 70s", None).unwrap();
        assert_eq!(pt.genres, en.genres);
        assert_eq!(pt.countries, en.countries);
        assert_eq!(pt.year_range, en.year_range);
    }

    #[test]
    fn handles_accents_and_casing() {
        let s = store();
        let f = parse(&s, "TROPICÁLIA BRASILEIRA", None).unwrap();
        assert_eq!(f.genres, vec!["tropicalia".to_string()]);
        assert_eq!(f.countries, vec!["BR".to_string()]);

        // The unaccented spelling must work too.
        let f2 = parse(&s, "tropicalia brasileira", None).unwrap();
        assert_eq!(f2.genres, f.genres);
    }

    #[test]
    fn prefers_the_longest_genre_name() {
        let s = store();
        // "samba rock" must not be read as samba plus rock.
        let f = parse(&s, "samba rock dos anos 70", None).unwrap();
        assert_eq!(f.genres, vec!["samba-rock".to_string()]);
    }

    #[test]
    fn recognises_hyphenated_and_spaced_spellings() {
        let s = store();
        for query in ["hip-hop americano", "hip hop americano"] {
            let f = parse(&s, query, None).unwrap();
            assert_eq!(f.genres, vec!["hip-hop".to_string()], "query: {query}");
            assert_eq!(f.countries, vec!["US".to_string()]);
        }
    }

    #[test]
    fn recognises_decade_spellings() {
        let s = store();
        for (query, expected) in [
            ("rock dos anos 90", (1990, 1999)),
            ("rock 90s", (1990, 1999)),
            ("rock 1990s", (1990, 1999)),
            ("rock da decada de 1970", (1970, 1979)),
            ("rock década de 80", (1980, 1989)),
        ] {
            let f = parse(&s, query, None).unwrap();
            assert_eq!(f.year_range, Some(expected), "query: {query}");
        }
    }

    #[test]
    fn two_digit_decades_use_the_convention_people_mean() {
        // "anos 70" is the 1970s; "anos 20" is the 2020s, not the 1920s.
        assert_eq!(decade_from_digits("70"), Some(1970));
        assert_eq!(decade_from_digits("90"), Some(1990));
        assert_eq!(decade_from_digits("30"), Some(1930));
        assert_eq!(decade_from_digits("20"), Some(2020));
        assert_eq!(decade_from_digits("00"), Some(2000));
        assert_eq!(decade_from_digits("10"), Some(2010));
        // Four-digit forms are unambiguous.
        assert_eq!(decade_from_digits("1975"), Some(1970));
        assert_eq!(decade_from_digits("abc"), None);
        assert_eq!(decade_from_digits(""), None);
    }

    #[test]
    fn parses_an_explicit_year_span() {
        let s = store();
        let f = parse(&s, "samba de 1968 a 1974", None).unwrap();
        assert_eq!(f.year_range, Some((1968, 1974)));

        let single = parse(&s, "rock de 1972", None).unwrap();
        assert_eq!(single.year_range, Some((1972, 1972)));
    }

    #[test]
    fn genre_alone_is_a_valid_request() {
        let s = store();
        let f = parse(&s, "mpb", None).unwrap();
        assert_eq!(f.genres, vec!["mpb".to_string()]);
        assert!(f.countries.is_empty());
        assert!(f.year_range.is_none());
    }

    #[test]
    fn country_alone_is_a_valid_request() {
        let s = store();
        let f = parse(&s, "música brasileira", None).unwrap();
        assert_eq!(f.countries, vec!["BR".to_string()]);
        assert!(f.genres.is_empty());
    }

    #[test]
    fn reports_what_it_could_not_understand() {
        let s = store();
        let err = parse(&s, "something happy for the gym", None).unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidFilter(ref m) if m.contains("something happy")),
            "{err}"
        );
    }

    #[test]
    fn never_produces_a_genre_outside_the_vocabulary() {
        // "vaporwave" is not in this store's vocabulary, so it must not appear —
        // and with a country present the query still parses usefully.
        let s = store();
        let f = parse(&s, "vaporwave brasileira", None).unwrap();
        assert!(f.genres.is_empty());
        assert_eq!(f.countries, vec!["BR".to_string()]);
        // Whatever comes out is always executable.
        f.validate(&s).unwrap();
    }

    #[test]
    fn multiple_countries_are_collected_without_duplicates() {
        let s = store();
        let f = parse(&s, "rock ingles e americano e britanico", None).unwrap();
        assert_eq!(f.countries, vec!["GB".to_string(), "US".to_string()]);
    }

    #[test]
    fn every_parse_result_is_immediately_executable() {
        let s = store();
        for query in [
            "soul brasileira dos anos 70",
            "samba rock",
            "hip-hop americano atual",
            "rock ingles dos anos 90",
            "tropicalia",
        ] {
            let f = parse(&s, query, Some("p1")).unwrap();
            f.validate(&s).expect(query);
            // And it must actually run against the schema.
            crate::suggest::execute(&s, &f).expect(query);
        }
    }
}
