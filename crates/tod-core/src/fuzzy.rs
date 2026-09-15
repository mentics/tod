//! Shared fuzzy text matching, used by the obligations list search, the
//! node tree search box, and `tod-cli node search`.

/// Case-insensitive, word-order-independent fuzzy match: `query` is split on
/// whitespace, and every word must independently match somewhere in `text`
/// (as a substring, or as an in-order subsequence for typo tolerance).
/// Returns a summed score (higher is a better match) or `None` when any word
/// doesn't match at all. Splitting by word means "login reusable" still
/// matches "Reusable Login Component" even though the words are reversed —
/// only each word's own characters need to stay in order.
pub fn fuzzy_score(text: &str, query: &str) -> Option<i32> {
    let text_l = text.to_lowercase();
    let query_l = query.trim().to_lowercase();
    if query_l.is_empty() {
        return Some(0);
    }
    let mut total = 0i32;
    for word in query_l.split_whitespace() {
        total += word_score(&text_l, word)?;
    }
    Some(total)
}

/// Score a single word (no whitespace) against the full text: an exact
/// substring scores highest, then tighter (less gappy) subsequence matches
/// beat looser ones. `None` when the word's characters don't appear in order
/// at all.
fn word_score(text_l: &str, word: &str) -> Option<i32> {
    if text_l.contains(word) {
        return Some(10_000 - text_l.len() as i32);
    }
    let mut word_chars = word.chars();
    let mut want = word_chars.next();
    let mut last_match: Option<i32> = None;
    let mut penalty = 0i32;
    for (pos, c) in text_l.chars().enumerate() {
        let Some(wc) = want else { break };
        if c == wc {
            if let Some(last) = last_match {
                penalty += pos as i32 - last - 1;
            }
            last_match = Some(pos as i32);
            want = word_chars.next();
        }
    }
    if want.is_some() {
        return None;
    }
    Some(-penalty)
}

/// Boolean form of [`fuzzy_score`] for callers that only need a filter.
pub fn fuzzy_matches(query: &str, text: &str) -> bool {
    fuzzy_score(text, query).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_substring_scores_highest() {
        assert!(fuzzy_score("Reusable Login Component", "login").unwrap() > 0);
    }

    #[test]
    fn subsequence_matches_with_typo_skip() {
        // "logn" (missing an 'i') still matches as a subsequence of "login".
        assert!(fuzzy_matches("logn", "Reusable Login Component"));
    }

    #[test]
    fn non_matching_query_returns_none() {
        assert!(!fuzzy_matches("xyz", "Reusable Login Component"));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy_score("anything", ""), Some(0));
    }

    #[test]
    fn tighter_match_scores_better_than_looser() {
        let tight = fuzzy_score("abXcd", "abcd").unwrap();
        let loose = fuzzy_score("aXbXcXd", "abcd").unwrap();
        assert!(tight > loose);
    }

    #[test]
    fn words_match_regardless_of_order() {
        assert!(fuzzy_matches("login reusable", "Reusable Login Component"));
    }

    #[test]
    fn every_word_must_match_something() {
        // "xyz" doesn't appear anywhere, so the whole query fails even
        // though "login" does match.
        assert!(!fuzzy_matches("login xyz", "Reusable Login Component"));
    }

    #[test]
    fn each_word_still_tolerates_typos() {
        // "logn" (missing 'i') and "reusabel" (transposed) both still
        // subsequence-match their targets, out of order.
        assert!(fuzzy_matches("reusabel logn", "Reusable Login Component"));
    }
}
