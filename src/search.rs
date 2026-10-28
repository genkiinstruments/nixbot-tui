//! fzf-style search over the builds list.

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

/// A parsed query: words that must each match one field (`'exact`, `^prefix`, `!not` work as
/// in fzf).
pub struct Search {
    pattern: Pattern,
    matcher: Matcher,
}

impl Search {
    pub fn new(query: &str) -> Self {
        Self {
            pattern: Pattern::parse(query, CaseMatching::Smart, Normalization::Smart),
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Each word has to match within a single field: across fields joined together, a short
    /// word picks its letters out of unrelated ones.
    pub fn is_match(&mut self, fields: &[&str]) -> bool {
        let Self { pattern, matcher } = self;
        let mut buf = Vec::new();
        let mut matches = |atom: &nucleo_matcher::pattern::Atom, field: &str| {
            atom.score(Utf32Str::new(field, &mut buf), matcher)
                .is_some()
        };

        pattern.atoms.iter().all(|atom| {
            if atom.negative {
                fields.iter().all(|field| matches(atom, field))
            } else {
                fields.iter().any(|field| matches(atom, field))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: [&str; 4] = ["#120", "example/project", "feat/widget", "building"];

    #[test]
    fn words_match_fuzzily_within_one_field() {
        let mut search = Search::new("wdgt");
        assert!(search.is_match(&ROW));
        assert!(
            !Search::new("main").is_match(&ROW),
            "m-a-i-n is spread over fields"
        );
    }

    #[test]
    fn every_word_has_to_match_and_negations_match_nowhere() {
        assert!(Search::new("widget build").is_match(&ROW));
        assert!(!Search::new("widget failed").is_match(&ROW));
        assert!(Search::new("widget !failed").is_match(&ROW));
        assert!(!Search::new("!widget").is_match(&ROW));
    }

    #[test]
    fn quoted_words_match_exactly() {
        assert!(Search::new("'120").is_match(&ROW));
        assert!(!Search::new("'12o").is_match(&ROW));
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(Search::new("").is_match(&ROW));
        assert!(Search::new("  ").is_match(&[]));
    }
}
