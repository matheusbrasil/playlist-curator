//! `Taxonomy` — the in-memory resolver from a raw upstream tag to a canonical
//! genre, plus the hierarchy walks the rest of the app needs.
//!
//! Built once per derivation run and then used as a pure function. That matters
//! for two reasons: aggregation calls `resolve` once per signal (tens of
//! thousands of times for a large playlist), and keeping it side-effect free is
//! what lets the genre pass be re-run offline over the stored raw signals.
//!
//! Resolution order, most authoritative first:
//!
//!  1. **Learned alias** (`genre_alias`) — a decision a human or the LLM already
//!     made. A row with a NULL slug is a decision too: "this is not a genre",
//!     which is why the queue never re-asks.
//!  2. **Exact canonical slug** — free, because normalisation emits the same form
//!     the slugs are written in.
//!  3. **Built-in synonym dictionary** — the spellings upstream actually uses that
//!     MusicBrainz does not have a genre for (`rap`, `funk / soul`).
//!  4. **Depluralisation** — one last cheap try before giving up.
//!
//! Anything left over is `None` and lands in `unresolved_tags`, which is the only
//! input the optional LLM pass ever gets.

use crate::error::Result;
use crate::model::CanonicalGenre;
use crate::store::Store;
use crate::taxonomy::genres::canonical_vocabulary;
use crate::taxonomy::normalize::normalize_tag;
use std::collections::{HashMap, HashSet};

/// Spellings upstream sources use that are not themselves MusicBrainz genres.
///
/// Keys are in normalised form; targets must be canonical slugs. Both halves are
/// asserted in the tests, so a typo here fails the build rather than silently
/// dropping a tag.
const SYNONYMS: &[(&str, &str)] = &[
    // Hip-hop: MusicBrainz calls the family "hip hop", the world calls it "rap".
    ("rap", "hip-hop"),
    ("rap-hip-hop", "hip-hop"),
    ("hip-hop-rap", "hip-hop"),
    ("gangster-rap", "gangsta-rap"),
    ("old-school-rap", "old-school-hip-hop"),
    ("underground-rap", "underground-hip-hop"),
    // Rock family shorthands.
    ("rock-n-roll", "rock-and-roll"),
    ("rock-roll", "rock-and-roll"),
    ("rnr", "rock-and-roll"),
    ("indie", "indie-rock"),
    ("alternative", "alternative-rock"),
    ("alt-rock", "alternative-rock"),
    ("prog", "progressive-rock"),
    ("prog-rock", "progressive-rock"),
    ("psych-rock", "psychedelic-rock"),
    ("psych", "psychedelic"),
    ("hardcore", "hardcore-punk"),
    // Discogs ships combined genres; keep the one the tag is really about.
    ("funk-soul", "funk"),
    ("soul-funk", "funk"),
    // Electronic. `electronica` and `edm` are canonical MB genres themselves so
    // they resolve by exact lookup; only misspellings / non-vocabulary entries
    // belong here.
    ("dance-music", "dance"),
    ("electronic-dance-music", "electronic"),
    ("afro-beat", "afrobeat"),
    // Brazil. The accent stripping in `normalize` already unifies the spellings;
    // these are the ones that need a *different* word, not a different spelling.
    ("musica-popular-brasileira", "mpb"),
    ("musica-brasileira", "mpb"),
    ("brazilian-music", "mpb"),
    ("brazilian-popular-music", "mpb"),
    ("bossa", "bossa-nova"),
    ("baile-funk", "funk-carioca"),
    ("brazilian-funk", "funk-carioca"),
    ("funk-brasileiro-carioca", "funk-carioca"),
    ("musica-sertaneja", "sertanejo"),
    ("forro-pe-de-serra", "forro"),
    ("samba-de-raiz", "samba"),
    ("axe-music", "axe"),
    // Afro-Brazilian is an umbrella the vocabulary has no entry for; samba is the
    // tradition it names in practice on the tags we see. A judgement call.
    ("afro-brazilian", "samba"),
    ("afro-brasileiro", "samba"),
    // World / Latin umbrellas.
    ("world", "world-fusion"),
    ("world-music", "world-fusion"),
    ("latin-music", "latin"),
    ("afro-cuban", "afro-cuban-jazz"),
    ("reggaton", "reggaeton"),
];

/// Canonical vocabulary plus learned aliases, indexed for lookup.
#[derive(Debug, Clone, Default)]
pub struct Taxonomy {
    by_slug: HashMap<String, CanonicalGenre>,
    children: HashMap<String, Vec<String>>,
    /// From `genre_alias`. `None` records "deliberately not a genre".
    learned: HashMap<String, Option<String>>,
}

impl Taxonomy {
    /// Load the canonical vocabulary from the store.
    ///
    /// Seeds the store from the embedded vocabulary first if `genre_canonical` is
    /// empty, so a fresh database resolves tags without a separate setup step.
    pub fn load(store: &Store) -> Result<Taxonomy> {
        let mut genres = store.all_canonical_genres()?;
        if genres.is_empty() {
            crate::taxonomy::genres::seed_canonical_genres(store)?;
            genres = store.all_canonical_genres()?;
        }
        Ok(Taxonomy::from_genres(genres))
    }

    /// Build from an explicit vocabulary. Used by `load` and by tests that need a
    /// deliberately malformed hierarchy.
    pub fn from_genres(genres: Vec<CanonicalGenre>) -> Taxonomy {
        let by_slug: HashMap<String, CanonicalGenre> =
            genres.into_iter().map(|g| (g.slug.clone(), g)).collect();

        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        for g in by_slug.values() {
            if let Some(parent) = &g.parent_slug {
                // A parent outside the vocabulary would make the walks lie about
                // the hierarchy; drop the edge rather than invent a node.
                if by_slug.contains_key(parent) && parent != &g.slug {
                    children.entry(parent.clone()).or_default().push(g.slug.clone());
                }
            }
        }
        for kids in children.values_mut() {
            kids.sort();
        }

        Taxonomy { by_slug, children, learned: HashMap::new() }
    }

    /// The embedded vocabulary, no store involved. Handy for pure tests and for
    /// callers that only need the hierarchy.
    pub fn embedded() -> Taxonomy {
        Taxonomy::from_genres(canonical_vocabulary().to_vec())
    }

    /// Pull the learned `genre_alias` rows for a specific set of raw tags.
    ///
    /// Deliberately not part of `load`: the repo exposes alias lookup one tag at a
    /// time, and the only aliases a derivation run can possibly need are the tags
    /// standing in front of it. Fetching exactly those keeps `resolve` a pure
    /// in-memory function without reading the whole table.
    ///
    /// Returns how many of the requested tags had a recorded decision.
    pub fn learn_aliases<'a>(
        &mut self,
        store: &Store,
        raw_tags: impl IntoIterator<Item = &'a str>,
    ) -> Result<usize> {
        let mut found = 0;
        for raw in raw_tags {
            if self.learned.contains_key(raw) {
                continue;
            }
            if let Some(mapped) = store.genre_alias(raw)? {
                found += 1;
                // Index under the raw key *and* the normalised one, so an alias
                // recorded for "Hip-Hop" also answers for "hip hop".
                if let Some(normalized) = normalize_tag(raw) {
                    self.learned.insert(normalized, mapped.clone());
                }
                self.learned.insert(raw.to_string(), mapped);
            }
        }
        Ok(found)
    }

    /// Record a learned alias in memory only. For tests and for an LLM pass that
    /// wants to use its answers before committing them.
    pub fn set_learned_alias(&mut self, raw_tag: &str, canonical_slug: Option<&str>) {
        let mapped = canonical_slug.map(String::from);
        if let Some(normalized) = normalize_tag(raw_tag) {
            self.learned.insert(normalized, mapped.clone());
        }
        self.learned.insert(raw_tag.to_string(), mapped);
    }

    /// Resolve a raw upstream tag to a canonical slug, or `None` if it is not a
    /// genre this vocabulary knows.
    pub fn resolve(&self, raw_tag: &str) -> Option<String> {
        // A learned decision on the exact raw string wins even over normalisation:
        // it is the only place a human can overrule these rules.
        if let Some(mapped) = self.learned.get(raw_tag) {
            return mapped.clone().filter(|s| self.by_slug.contains_key(s));
        }

        let normalized = normalize_tag(raw_tag)?;

        if let Some(mapped) = self.learned.get(&normalized) {
            return mapped.clone().filter(|s| self.by_slug.contains_key(s));
        }
        if self.by_slug.contains_key(&normalized) {
            return Some(normalized);
        }
        if let Some(slug) = self.synonym(&normalized) {
            return Some(slug);
        }

        // "sambas", "ballads": a plural of something we do know.
        for singular in [normalized.strip_suffix("es"), normalized.strip_suffix('s')]
            .into_iter()
            .flatten()
        {
            if self.by_slug.contains_key(singular) {
                return Some(singular.to_string());
            }
            if let Some(slug) = self.synonym(singular) {
                return Some(slug);
            }
        }

        None
    }

    fn synonym(&self, normalized: &str) -> Option<String> {
        SYNONYMS
            .iter()
            .find(|(from, _)| *from == normalized)
            .map(|(_, to)| (*to).to_string())
            .filter(|slug| self.by_slug.contains_key(slug))
    }

    pub fn contains(&self, slug: &str) -> bool {
        self.by_slug.contains_key(slug)
    }

    pub fn genre(&self, slug: &str) -> Option<&CanonicalGenre> {
        self.by_slug.get(slug)
    }

    pub fn len(&self) -> usize {
        self.by_slug.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_slug.is_empty()
    }

    /// `slug` followed by its parents up to the root.
    ///
    /// Empty if `slug` is not in the vocabulary. Cycle-guarded: a malformed
    /// `genres.json` (or a hand-edited database) must not hang the app, so a
    /// repeated node ends the walk.
    pub fn ancestors(&self, slug: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = self.by_slug.get(slug);
        while let Some(g) = cur {
            if !seen.insert(g.slug.clone()) {
                break;
            }
            out.push(g.slug.clone());
            cur = g.parent_slug.as_deref().and_then(|p| self.by_slug.get(p));
        }
        out
    }

    /// `slug` followed by every genre transitively beneath it.
    ///
    /// This is what `GenreMode::AnyWithChildren` expands: asking for `samba` has
    /// to collect `samba-rock`, `pagode` and `bossa-nova` too, or the app produces
    /// forty playlists of three tracks each.
    pub fn descendants(&self, slug: &str) -> Vec<String> {
        if !self.by_slug.contains_key(slug) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![slug.to_string()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            out.push(current.clone());
            if let Some(kids) = self.children.get(&current) {
                stack.extend(kids.iter().cloned());
            }
        }
        out
    }

    pub fn children(&self, slug: &str) -> &[String] {
        self.children.get(slug).map(Vec::as_slice).unwrap_or_default()
    }

    /// Raw tags in `tag_signal` that no rule here can place.
    ///
    /// This is the queue the (optional) LLM pass consumes. Tags that normalisation
    /// rejects outright are excluded — asking a model whether `seen live` is a
    /// genre wastes a call on a question already answered by rule.
    pub fn unresolved_tags(&self, store: &Store) -> Result<Vec<String>> {
        Ok(store
            .unresolved_raw_tags()?
            .into_iter()
            .filter(|raw| normalize_tag(raw).is_some() && self.resolve(raw).is_none())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CanonicalGenre;
    use crate::taxonomy::genres::seed_canonical_genres;

    fn seeded_store() -> Store {
        let s = Store::open_in_memory().unwrap();
        seed_canonical_genres(&s).unwrap();
        s
    }

    fn genre(slug: &str, parent: Option<&str>) -> CanonicalGenre {
        CanonicalGenre {
            slug: slug.into(),
            label: slug.into(),
            parent_slug: parent.map(String::from),
        }
    }

    #[test]
    fn every_synonym_is_well_formed() {
        let tax = Taxonomy::embedded();
        for (from, to) in SYNONYMS {
            assert_eq!(
                normalize_tag(from).as_deref(),
                Some(*from),
                "synonym key {from:?} is not in normalised form"
            );
            assert!(tax.contains(to), "synonym {from} -> {to}, which is not canonical");
            assert!(
                !tax.contains(from),
                "synonym {from} is already a canonical slug, so the entry is dead"
            );
        }
    }

    #[test]
    fn load_seeds_an_empty_store() {
        let store = Store::open_in_memory().unwrap();
        let tax = Taxonomy::load(&store).unwrap();
        assert!(tax.len() > 2000);
        assert!(store.canonical_genre("samba").unwrap().is_some());
    }

    #[test]
    fn resolves_exact_canonical_slugs_through_spelling_variants() {
        let tax = Taxonomy::embedded();
        for variant in ["Hip-Hop", "hip hop", "hiphop", "HIP HOP"] {
            assert_eq!(tax.resolve(variant).as_deref(), Some("hip-hop"), "{variant}");
        }
        for variant in ["Tropicália", "tropicalia", "TROPICALIA"] {
            assert_eq!(tax.resolve(variant).as_deref(), Some("tropicalia"), "{variant}");
        }
        assert_eq!(tax.resolve("Forró").as_deref(), Some("forro"));
        assert_eq!(tax.resolve("Bossa Nova").as_deref(), Some("bossa-nova"));
        assert_eq!(tax.resolve("R&B").as_deref(), Some("rnb"));
        assert_eq!(tax.resolve("drum n bass").as_deref(), Some("drum-and-bass"));
    }

    #[test]
    fn resolves_through_the_built_in_synonym_dictionary() {
        let tax = Taxonomy::embedded();
        assert_eq!(tax.resolve("rap").as_deref(), Some("hip-hop"));
        // "electronica" is itself a canonical MB genre — it resolves by exact match.
        assert_eq!(tax.resolve("Electronica").as_deref(), Some("electronica"));
        assert_eq!(tax.resolve("Funk / Soul").as_deref(), Some("funk"));
        assert_eq!(tax.resolve("baile funk").as_deref(), Some("funk-carioca"));
        assert_eq!(tax.resolve("Música Popular Brasileira").as_deref(), Some("mpb"));
        assert_eq!(tax.resolve("rock 'n' roll").as_deref(), Some("rock-and-roll"));
        assert_eq!(tax.resolve("Nu Jazz").as_deref(), Some("nu-jazz"));
    }

    #[test]
    fn rejects_noise_without_consulting_the_vocabulary() {
        let tax = Taxonomy::embedded();
        for junk in ["seen live", "favorites", "beautiful", "my music", "female vocalists", "90s"] {
            assert_eq!(tax.resolve(junk), None, "{junk}");
        }
    }

    #[test]
    fn a_learned_alias_beats_the_built_in_rules() {
        let mut tax = Taxonomy::embedded();
        // Without the alias this resolves by exact match.
        assert_eq!(tax.resolve("samba").as_deref(), Some("samba"));
        tax.set_learned_alias("samba", Some("samba-rock"));
        assert_eq!(tax.resolve("samba").as_deref(), Some("samba-rock"));

        // A NULL alias is a decision: "not a genre".
        tax.set_learned_alias("chillout", None);
        assert_eq!(tax.resolve("chillout"), None);
    }

    #[test]
    fn learned_aliases_load_from_the_store() {
        let store = seeded_store();
        store.upsert_genre_alias("Black Rio", Some("funk"), "user").unwrap();
        let mut tax = Taxonomy::load(&store).unwrap();
        assert_eq!(tax.resolve("Black Rio"), None, "not learned yet");

        tax.learn_aliases(&store, ["Black Rio", "never-seen"]).unwrap();
        assert_eq!(tax.resolve("Black Rio").as_deref(), Some("funk"));
        // The normalised form answers too.
        assert_eq!(tax.resolve("black rio").as_deref(), Some("funk"));
    }

    #[test]
    fn an_alias_pointing_outside_the_vocabulary_is_ignored() {
        let mut tax = Taxonomy::embedded();
        tax.set_learned_alias("brazilian cosmic soul", Some("brazilian-cosmic-soul"));
        assert_eq!(tax.resolve("brazilian cosmic soul"), None);
    }

    #[test]
    fn ancestors_walk_to_the_root_and_descendants_walk_down() {
        let tax = Taxonomy::embedded();
        assert_eq!(tax.ancestors("samba-rock"), vec!["samba-rock".to_string(), "samba".into()]);
        assert_eq!(
            tax.ancestors("ragga-jungle"),
            vec![
                "ragga-jungle".to_string(),
                "jungle".into(),
                "drum-and-bass".into(),
                "electronic".into()
            ]
        );
        assert_eq!(tax.ancestors("samba"), vec!["samba".to_string()]);
        assert!(tax.ancestors("not-a-genre").is_empty());

        let samba = tax.descendants("samba");
        assert!(samba.contains(&"samba".to_string()));
        for child in ["samba-rock", "samba-jazz", "pagode", "bossa-nova"] {
            assert!(samba.contains(&child.to_string()), "descendants(samba) missing {child}");
        }
        // Transitive: pagode-romantico sits under samba via pagode's siblings.
        assert!(tax.descendants("electronic").contains(&"ragga-jungle".to_string()));
        assert!(tax.descendants("not-a-genre").is_empty());
    }

    #[test]
    fn a_cyclic_parent_chain_terminates_instead_of_hanging() {
        // A hand-edited database or a bad genres.json must not wedge the app.
        let tax = Taxonomy::from_genres(vec![
            genre("a", Some("b")),
            genre("b", Some("c")),
            genre("c", Some("a")),
            genre("leaf", Some("a")),
        ]);
        let anc = tax.ancestors("a");
        assert_eq!(anc, vec!["a".to_string(), "b".into(), "c".into()]);
        assert_eq!(tax.ancestors("leaf").len(), 4);

        let desc = tax.descendants("a");
        assert_eq!(desc.len(), 4, "each node visited once: {desc:?}");
    }

    #[test]
    fn a_self_parenting_genre_does_not_loop() {
        let tax = Taxonomy::from_genres(vec![genre("ouroboros", Some("ouroboros"))]);
        assert_eq!(tax.ancestors("ouroboros"), vec!["ouroboros".to_string()]);
        assert_eq!(tax.descendants("ouroboros"), vec!["ouroboros".to_string()]);
    }

    #[test]
    fn unresolved_queue_excludes_noise_and_resolvable_tags() {
        use crate::model::{EntityType, Source, TagSignal, TagKind};
        let store = seeded_store();
        store.upsert_mb_artist(&crate::model::MbArtist {
            mbid: "mb1".into(),
            ..Default::default()
        })
        .unwrap();
        for tag in ["seen live", "Samba-Rock", "rap", "brazilian cosmic soul", "90s"] {
            store
                .insert_tag_signal(&TagSignal {
                    entity_type: EntityType::MbArtist,
                    entity_id: "mb1".into(),
                    source: Source::Lastfm,
                    raw_tag: tag.into(),
                    weight: 0.5,
                    kind: Some(TagKind::Tag),
                    fetched_at: String::new(),
                })
                .unwrap();
        }

        let tax = Taxonomy::load(&store).unwrap();
        // Only the tag that is plausibly a genre but unknown reaches the queue.
        assert_eq!(tax.unresolved_tags(&store).unwrap(), vec!["brazilian cosmic soul".to_string()]);
    }
}
