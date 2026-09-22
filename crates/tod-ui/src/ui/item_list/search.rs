//! The search every item list uses: space-separated terms ANDed together,
//! each matching some word in the row's text, typo-tolerantly.
//!
//! Lifted out of the obligations panel so a list does not get its own dialect
//! of matching. A caller supplies the row's searchable text; everything else
//! is here.

/// Whether a row whose searchable text is `fields` matches `query`.
///
/// Every term must match some word in some field (case-insensitive, fuzzy);
/// an empty query matches everything.
pub fn matches_query(query: &str, fields: &[&str]) -> bool {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();
    if terms.is_empty() {
        return true;
    }
    let lowered: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    let words: Vec<&str> = lowered.iter().flat_map(|f| f.split_whitespace()).collect();
    terms.iter().all(|term| fuzzy_term_matches(term, &words))
}

/// True if `term` fuzzily matches any word in `words` (typo-tolerant).
fn fuzzy_term_matches(term: &str, words: &[&str]) -> bool {
    if term.is_empty() {
        return true;
    }
    words.iter().any(|word| fuzzy_word_match(term, word))
}

/// A term matches a word if it's a substring, or within a small edit-distance
/// budget that grows with the term's length (so short terms require an exact
/// substring match, avoiding false positives).
fn fuzzy_word_match(term: &str, word: &str) -> bool {
    if word.contains(term) {
        return true;
    }
    let max_dist = match term.chars().count() {
        0..=3 => 0,
        4..=6 => 1,
        _ => 2,
    };
    if max_dist == 0 {
        return false;
    }
    bounded_edit_distance_substring(term, word, max_dist)
}

/// True if some contiguous run of words in `word` (a single word here, but
/// kept general) is within `max_dist` edits of `term` — checked by sliding a
/// window sized close to `term`'s length across `word` and taking the best
/// Levenshtein distance among windows, so a typo'd word of similar length
/// still matches without requiring the lengths to be identical.
fn bounded_edit_distance_substring(term: &str, word: &str, max_dist: usize) -> bool {
    let term_len = term.chars().count();
    let word_len = word.chars().count();
    if word_len == 0 {
        return false;
    }
    // Whole-word compare is enough for our use case (single tokens, not
    // phrases): try the full word plus a couple of length-adjusted windows.
    if levenshtein_within(term, word, max_dist) {
        return true;
    }
    if word_len <= term_len {
        return false;
    }
    let word_chars: Vec<char> = word.chars().collect();
    for start in 0..=(word_len - term_len) {
        let end = (start + term_len + max_dist).min(word_len);
        let window: String = word_chars[start..end].iter().collect();
        if levenshtein_within(term, &window, max_dist) {
            return true;
        }
    }
    false
}

/// Levenshtein distance between `a` and `b`, short-circuiting once it's
/// certain the distance exceeds `max_dist`.
fn levenshtein_within(a: &str, b: &str, max_dist: usize) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max_dist {
        return false;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(curr[j]);
        }
        if row_min > max_dist {
            return false;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()] <= max_dist
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_every_row() {
        assert!(matches_query("", &["anything"]));
        assert!(matches_query("   ", &[]));
    }

    #[test]
    fn every_term_must_match_some_word() {
        assert!(matches_query("works offline", &["Works offline", ""]));
        assert!(!matches_query("works online", &["Works offline"]));
    }

    #[test]
    fn terms_match_across_fields() {
        assert!(matches_query("offline sync", &["Works offline", "Sync"]));
    }

    #[test]
    fn longer_terms_tolerate_a_typo_and_short_ones_do_not() {
        assert!(matches_query("oflfine", &["works offline"]));
        assert!(!matches_query("ofl", &["works offline"]));
    }
}
