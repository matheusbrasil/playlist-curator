//! The closed canonical genre vocabulary.
//!
//! # Why closed
//!
//! Upstream folksonomies name genres that do not exist and spell the ones that do
//! in a dozen ways. If any tag could become a genre, the app would offer the user
//! four hundred "genres", most of them with one track. So the vocabulary is fixed
//! at build time and every raw tag either maps into it or is recorded as
//! unresolved. `PlaylistFilter::validate` enforces the same closure at the other
//! end of the pipeline.
//!
//! # Where it comes from
//!
//! `genres.json` is the live MusicBrainz genre list
//! (`/ws/2/genre/all`, 2184 entries as fetched), slugged with the same rules
//! `normalize::normalize_tag` applies, so a normalised tag can be compared to a
//! canonical slug with `==`.
//!
//! # Why the hierarchy is ours and not MusicBrainz's
//!
//! MusicBrainz's genre list is **flat** — it has no parent links at all. Without
//! a hierarchy the suggestion engine emits forty playlists of three tracks each,
//! because `samba-rock`, `samba-jazz`, `samba-de-roda` and `pagode` are four
//! separate genres that never meet. So the `parent_slug` column here is
//! hand-authored on top of the flat list:
//!
//!  * explicit rollups for the cases that matter, especially Brazilian genres;
//!  * a modifier-head rule for the rest (`psychedelic rock` is a kind of rock —
//!    the last word names the family);
//!  * a family-prefix rule as a second pass (`samba de roda` → `samba`).
//!
//! The explicit rollups win, which is how the tradition beats the grammar:
//! `samba-rock` and `samba-jazz` are kinds of *samba*, not of rock or jazz, and
//! `funk-carioca` is a kind of *funk* even though nothing in the name says so.
//!
//! 793 of the 2183 slugs have a parent; the rest are roots, which is the right
//! answer for a list that is mostly regional traditions with no wider family in
//! the vocabulary.

use crate::error::Result;
use crate::model::CanonicalGenre;
use crate::store::Store;
use std::sync::OnceLock;

/// Embedded so the vocabulary needs no network, no migration and no data
/// directory — it is a property of the build.
const GENRES_JSON: &str = include_str!("genres.json");

/// The canonical vocabulary, parsed once per process.
pub fn canonical_vocabulary() -> &'static [CanonicalGenre] {
    static VOCAB: OnceLock<Vec<CanonicalGenre>> = OnceLock::new();
    VOCAB.get_or_init(|| {
        serde_json::from_str(GENRES_JSON).expect("embedded genres.json is malformed")
    })
}

/// Write the vocabulary into `genre_canonical`.
///
/// Idempotent: `upsert_canonical_genres` is an upsert keyed on the slug, so
/// calling this on every startup costs one transaction and changes nothing. It is
/// also how a shipped hierarchy correction reaches an existing database.
///
/// Returns the number of genres seeded.
pub fn seed_canonical_genres(store: &Store) -> Result<usize> {
    let vocab = canonical_vocabulary();
    store.upsert_canonical_genres(vocab)?;
    Ok(vocab.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taxonomy::normalize::normalize_tag;
    use std::collections::HashMap;

    #[test]
    fn vocabulary_parses_and_is_substantial() {
        let v = canonical_vocabulary();
        assert!(v.len() > 2000, "expected the full MusicBrainz list, got {}", v.len());
        assert!(v.iter().filter(|g| g.parent_slug.is_some()).count() > 500);
    }

    #[test]
    fn every_parent_exists_in_the_vocabulary() {
        // genre_canonical.parent_slug is a self-referencing foreign key; a
        // dangling parent would fail the seed at runtime instead of here.
        let v = canonical_vocabulary();
        let slugs: std::collections::HashSet<&str> = v.iter().map(|g| g.slug.as_str()).collect();
        for g in v {
            if let Some(parent) = &g.parent_slug {
                assert!(slugs.contains(parent.as_str()), "{} -> missing parent {parent}", g.slug);
                assert_ne!(parent, &g.slug, "{} is its own parent", g.slug);
            }
        }
    }

    #[test]
    fn slugs_are_unique_and_normalise_to_themselves() {
        // The invariant the whole resolver rests on: a canonical slug is already
        // in normalised form, so `normalize_tag(label) == Some(slug)` can be an
        // equality test rather than a fuzzy match.
        let v = canonical_vocabulary();
        let mut seen = HashMap::new();
        for g in v {
            assert!(seen.insert(&g.slug, ()).is_none(), "duplicate slug {}", g.slug);
            assert_eq!(
                normalize_tag(&g.slug).as_deref(),
                Some(g.slug.as_str()),
                "canonical slug {} does not survive normalisation",
                g.slug
            );
            assert_eq!(
                normalize_tag(&g.label).as_deref(),
                Some(g.slug.as_str()),
                "label {:?} does not normalise to its own slug",
                g.label
            );
        }
    }

    #[test]
    fn parent_chains_terminate() {
        let v = canonical_vocabulary();
        let parents: HashMap<&str, Option<&str>> = v
            .iter()
            .map(|g| (g.slug.as_str(), g.parent_slug.as_deref()))
            .collect();
        for g in v {
            let mut steps = 0;
            let mut cur = parents[g.slug.as_str()];
            while let Some(p) = cur {
                steps += 1;
                assert!(steps < 16, "parent chain from {} does not terminate", g.slug);
                cur = parents[p];
            }
        }
    }

    #[test]
    fn required_rollups_are_present() {
        // The rollups the app's usefulness depends on. Checked as reachability,
        // not as a direct parent, so inserting an intermediate level is allowed.
        let v = canonical_vocabulary();
        let parents: HashMap<&str, Option<&str>> = v
            .iter()
            .map(|g| (g.slug.as_str(), g.parent_slug.as_deref()))
            .collect();
        let reaches = |slug: &str, ancestor: &str| {
            let mut cur = parents.get(slug).copied().flatten();
            let mut hops = 0;
            while let Some(p) = cur {
                if p == ancestor {
                    return true;
                }
                hops += 1;
                assert!(hops < 16);
                cur = parents.get(p).copied().flatten();
            }
            false
        };

        for (child, ancestor) in [
            ("samba-rock", "samba"),
            ("samba-jazz", "samba"),
            ("samba-cancao", "samba"),
            ("pagode", "samba"),
            ("bossa-nova", "samba"),
            ("funk-carioca", "funk"),
            ("funk-rock", "funk"),
            ("northern-soul", "soul"),
            ("neo-soul", "soul"),
            ("southern-soul", "soul"),
            ("psychedelic-soul", "soul"),
            ("tropicalia", "mpb"),
            ("baiao", "forro"),
            ("hard-rock", "rock"),
            ("psychedelic-rock", "rock"),
            ("progressive-rock", "rock"),
            ("punk-rock", "rock"),
            ("britpop", "rock"),
            ("indie-rock", "rock"),
            ("alternative-rock", "rock"),
            ("boom-bap", "hip-hop"),
            ("trap", "hip-hop"),
            ("conscious-hip-hop", "hip-hop"),
            ("gangsta-rap", "hip-hop"),
            ("east-coast-hip-hop", "hip-hop"),
            ("west-coast-hip-hop", "hip-hop"),
            ("house", "electronic"),
            ("techno", "electronic"),
            ("drum-and-bass", "electronic"),
            ("jungle", "electronic"),
            ("ambient", "electronic"),
            ("bebop", "jazz"),
            ("delta-blues", "blues"),
            ("dub", "reggae"),
            ("death-metal", "metal"),
            ("bluegrass", "country"),
            ("opera", "classical"),
        ] {
            assert!(reaches(child, ancestor), "{child} must roll up to {ancestor}");
        }

        // Brazilian traditions that are roots in their own right.
        for root in ["samba", "mpb", "forro", "frevo", "axe", "maracatu", "choro", "sertanejo"] {
            assert!(parents.contains_key(root), "{root} missing from the vocabulary");
            assert_eq!(parents[root], None, "{root} should be a root genre");
        }
    }

    #[test]
    fn seeding_is_idempotent() {
        let store = Store::open_in_memory().unwrap();
        let first = seed_canonical_genres(&store).unwrap();
        let second = seed_canonical_genres(&store).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.all_canonical_genres().unwrap().len(), first);

        let samba_rock = store.canonical_genre("samba-rock").unwrap().unwrap();
        assert_eq!(samba_rock.parent_slug.as_deref(), Some("samba"));
    }
}
