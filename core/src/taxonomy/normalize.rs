//! Deterministic tag normalisation — the step that runs before any lookup.
//!
//! Everything here is a pure function of its input, which is what makes the
//! taxonomy reprocessable: `tag_signal` keeps the raw strings forever, so
//! improving a rule here re-derives every genre without touching the network.
//!
//! Two jobs:
//!
//!  * **Fold spelling variants together.** `Hip-Hop`, `hip hop` and `hiphop` are
//!    the same genre; so are `tropicália` and `tropicalia`. Case, accents,
//!    punctuation and separator style all have to stop being distinctions before
//!    a lookup can succeed, or the canonical vocabulary misses two thirds of the
//!    tags that actually name a genre it contains.
//!  * **Reject non-musical noise.** Last.fm's folksonomy is roughly half
//!    `favorites` / `seen live` / `beautiful` / `female vocalists` / `90s`. None
//!    of those is a genre, and none of them should reach the LLM queue either —
//!    they are recognisable by rule, so they are rejected by rule.
//!
//! The output form is the same form the canonical slugs in `genres.json` use:
//! lowercase ASCII words joined by `-`. That is deliberate — a normalised tag can
//! be compared to a canonical slug with `==`.

use unicode_normalization::UnicodeNormalization;

/// Multi-token rewrites that collapse separator and abbreviation variants onto
/// one spelling. Applied to the token sequence, so they also fix compounds:
/// `contemporary r&b` becomes `contemporary-rnb`, not `contemporary-r-b`.
///
/// `genres.json` is generated with these same rules applied, so canonical slugs
/// and normalised tags can never disagree about how to spell `drum and bass`.
const TOKEN_REWRITES: &[(&[&str], &[&str])] = &[
    // `r&b` loses its ampersand to punctuation stripping and arrives as ["r","b"].
    (&["r", "b"], &["rnb"]),
    (&["r", "n", "b"], &["rnb"]),
    (&["r", "and", "b"], &["rnb"]),
    (&["hiphop"], &["hip", "hop"]),
    (&["dnb"], &["drum", "and", "bass"]),
    (&["d", "n", "b"], &["drum", "and", "bass"]),
    (&["drum", "n", "bass"], &["drum", "and", "bass"]),
    (&["drum", "bass"], &["drum", "and", "bass"]),
];

/// Tags that are never a genre, in normalised form.
///
/// Kept sorted so lookup is a binary search; `non_genre_list_is_sorted` guards
/// that. Entries that *are* real genres in the MusicBrainz vocabulary
/// (`lounge`, `chillout`, `instrumental`, `easy-listening`) are deliberately
/// absent, however mood-like they read.
///
/// Bare nationalities (`brazilian`, `british`) are listed: they describe origin,
/// not genre, and origin is derived properly from MusicBrainz areas. Treating
/// them as genres would produce a "Brazilian" playlist that duplicates the
/// geography axis with worse data.
pub const NON_GENRE_TAGS: &[&str] = &[
    "5-stars",
    "album",
    "albums",
    "albums-i-own",
    "all-time-favorites",
    "all-time-favourites",
    "amazing",
    "american",
    "awesome",
    "awesome-songs",
    "background",
    "band",
    "bands",
    "beautiful",
    "best",
    "best-songs",
    "brasil",
    "brasileiro",
    "brazil",
    "brazilian",
    "brilliant",
    "british",
    "catchy",
    "cd",
    "checked",
    "classics",
    "composers",
    "cool",
    "cover",
    "covers",
    "drums",
    "energetic",
    "english",
    "epic",
    "favorite",
    "favorite-songs",
    "favorites",
    "favourite",
    "favourites",
    "female-fronted",
    "female-fronted-metal",
    "female-vocalist",
    "female-vocalists",
    "female-vocals",
    "fip",
    "french",
    "fun",
    "german",
    "good",
    "good-music",
    "great",
    "great-music",
    "guitar",
    "guitars",
    "happy",
    "heard-on-pandora",
    "i-own-it",
    "itunes",
    "japanese",
    "lastfm",
    "listen-later",
    "live",
    "love",
    "love-it",
    "loved",
    "lovely",
    "male-vocalist",
    "male-vocalists",
    "male-vocals",
    "masterpiece",
    "mp3",
    "music",
    "my-collection",
    "my-favorite-songs",
    "my-favorites",
    "my-favourites",
    "my-music",
    "old-school",
    "oldies",
    "owned",
    "perfect",
    "piano",
    "playlist",
    "playlists",
    "radio",
    "rating",
    "sad",
    "saxophone",
    "seen-live",
    "sexy",
    "singer",
    "singers",
    "song",
    "songs",
    "songwriter",
    "spotify",
    "study",
    "swedish",
    "to-listen",
    "uk",
    "usa",
    "vinyl",
    "vocal",
    "vocalist",
    "vocalists",
    "vocals",
    "want-to-buy",
    "want-to-see-live",
    "wishlist",
    "workout",
];

/// Case-, accent- and punctuation-insensitive key for a free-text string.
///
/// Used for genre tags *and* for MusicBrainz area names, which is why it does no
/// genre-specific rewriting and never rejects anything: `derive::origin` needs
/// `São Paulo` and `Sao Paulo` to hash the same, and knows nothing about genres.
///
/// Characters outside ASCII that survive accent stripping (Cyrillic, CJK) are
/// dropped, so a wholly non-Latin tag folds to the empty string. That is the
/// honest answer: nothing here can match a vocabulary that is ASCII slugs.
pub fn fold_key(raw: &str) -> String {
    ascii_tokens(raw).join("-")
}

fn ascii_tokens(raw: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    // NFD splits `á` into `a` + combining acute; dropping the marks is what makes
    // `tropicália` and `tropicalia` the same tag.
    for ch in raw.nfd().filter(|c| !unicode_normalization::char::is_combining_mark(*c)) {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Normalise a raw upstream tag, or reject it.
///
/// `None` means "this is not a genre and never will be" — either it folded to
/// nothing or it matched a noise rule. Callers should record that decision (an
/// alias row with a NULL slug) rather than re-asking the same question.
pub fn normalize_tag(raw: &str) -> Option<String> {
    let tokens = rewrite_tokens(ascii_tokens(raw));
    if tokens.is_empty() {
        return None;
    }
    let normalized = tokens.join("-");
    (!is_non_genre(&normalized)).then_some(normalized)
}

fn rewrite_tokens(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    'outer: while i < tokens.len() {
        for (pattern, replacement) in TOKEN_REWRITES {
            if tokens[i..].len() >= pattern.len()
                && tokens[i..i + pattern.len()]
                    .iter()
                    .zip(pattern.iter())
                    .all(|(t, p)| t == p)
            {
                out.extend(replacement.iter().map(|s| s.to_string()));
                i += pattern.len();
                continue 'outer;
            }
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out
}

/// Whether an already-normalised tag is recognisable noise.
pub fn is_non_genre(normalized: &str) -> bool {
    NON_GENRE_TAGS.binary_search(&normalized).is_ok()
        || is_single_letter(normalized)
        || is_bare_number(normalized)
        || is_decade_tag(normalized)
}

fn is_single_letter(s: &str) -> bool {
    let mut chars = s.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphabetic())
}

fn is_bare_number(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// `90s`, `1990s`, `00s`, `2010s`, `80's` (which folds to `80-s`). These are era
/// tags, and era is derived from release dates — with the reissue trap handled,
/// which a Last.fm tag cannot be.
fn is_decade_tag(s: &str) -> bool {
    let core = s
        .strip_suffix("-s")
        .or_else(|| s.strip_suffix('s'))
        .unwrap_or(s);
    matches!(core.len(), 2 | 4) && core.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_genre_list_is_sorted() {
        // `is_non_genre` binary-searches it.
        let mut sorted = NON_GENRE_TAGS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), NON_GENRE_TAGS, "NON_GENRE_TAGS must be sorted and unique");
    }

    #[test]
    fn hip_hop_spelling_variants_collapse_to_one_slug() {
        for variant in ["hip hop", "Hip-Hop", "hiphop", "HIP  HOP", "hip_hop", "Hip Hop!"] {
            assert_eq!(normalize_tag(variant).as_deref(), Some("hip-hop"), "{variant}");
        }
    }

    #[test]
    fn rnb_spelling_variants_collapse_to_one_slug() {
        for variant in ["r&b", "R&B", "rnb", "R n B", "r and b", "R-N-B"] {
            assert_eq!(normalize_tag(variant).as_deref(), Some("rnb"), "{variant}");
        }
        // The rewrite is positional, so compounds are fixed too.
        assert_eq!(normalize_tag("Contemporary R&B").as_deref(), Some("contemporary-rnb"));
    }

    #[test]
    fn drum_and_bass_spelling_variants_collapse_to_one_slug() {
        for variant in ["drum and bass", "drum n bass", "Drum & Bass", "DnB", "d n b", "drum'n'bass"]
        {
            assert_eq!(normalize_tag(variant).as_deref(), Some("drum-and-bass"), "{variant}");
        }
    }

    #[test]
    fn accents_are_stripped_so_brazilian_tags_unify() {
        assert_eq!(normalize_tag("Tropicália").as_deref(), Some("tropicalia"));
        assert_eq!(normalize_tag("forró").as_deref(), Some("forro"));
        assert_eq!(normalize_tag("MPB brasileiró"), normalize_tag("mpb brasileiro"));
        assert_eq!(normalize_tag("Música Popular Brasileira").as_deref(),
                   Some("musica-popular-brasileira"));
    }

    #[test]
    fn punctuation_and_whitespace_stop_being_distinctions() {
        assert_eq!(normalize_tag("  Bossa   Nova  ").as_deref(), Some("bossa-nova"));
        assert_eq!(normalize_tag("rock'n'roll").as_deref(), Some("rock-n-roll"));
        assert_eq!(normalize_tag("jazz/funk").as_deref(), Some("jazz-funk"));
        assert_eq!(normalize_tag("Samba-Rock.").as_deref(), Some("samba-rock"));
    }

    #[test]
    fn seen_live_and_friends_are_rejected() {
        for junk in [
            "seen live", "Seen Live", "favorites", "favourites", "beautiful", "awesome",
            "my music", "Spotify", "albums i own", "male vocalists", "female vocalists",
            "love", "want to see live",
        ] {
            assert_eq!(normalize_tag(junk), None, "{junk} must not become a genre");
        }
    }

    #[test]
    fn decade_and_numeric_tags_are_rejected() {
        // Era is derived from release dates, where the reissue trap is handled.
        for junk in ["10s", "90s", "00s", "2010s", "1990s", "80's", "1972", "7"] {
            assert_eq!(normalize_tag(junk), None, "{junk} must not become a genre");
        }
        // But a genre that merely starts with a digit survives.
        assert_eq!(normalize_tag("2-step").as_deref(), Some("2-step"));
        assert_eq!(normalize_tag("8-bit").as_deref(), Some("8-bit"));
    }

    #[test]
    fn single_letters_and_empty_input_are_rejected() {
        for junk in ["a", "X", "", "   ", "!!!", "日本語"] {
            assert_eq!(normalize_tag(junk), None, "{junk:?} must not become a genre");
        }
    }

    #[test]
    fn fold_key_keeps_place_names_comparable_without_rejecting_them() {
        assert_eq!(fold_key("São Paulo"), "sao-paulo");
        assert_eq!(fold_key("Rio de Janeiro"), "rio-de-janeiro");
        // fold_key has no opinion about noise; that is normalize_tag's job.
        assert_eq!(fold_key("seen live"), "seen-live");
        assert_eq!(fold_key("90s"), "90s");
    }
}
