//! Shared fuzzy text matching, used by the obligations list search, the
//! node tree search box, and `tod-cli nodes search`.

/// Case-insensitive fuzzy match: `query`'s characters must appear as a
/// subsequence of `text`. Returns a score (higher is a better match) or
/// `None` when it doesn't match at all — an exact substring scores highest,
/// then tighter (less gappy) subsequence matches beat looser ones.
pub fn fuzzy_score(text: &str, query: &str) -> Option<i32> {
    let text_l = text.to_lowercase();
    let query_l = query.trim().to_lowercase();
    if query_l.is_empty() {
        return Some(0);
    }
    if text_l.contains(&query_l) {
        return Some(10_000 - text_l.len() as i32);
    }
    let mut query_chars = query_l.chars();
    let mut want = query_chars.next();
    let mut last_match: Option<i32> = None;
    let mut penalty = 0i32;
    for (pos, c) in text_l.chars().enumerate() {
        let Some(qc) = want else { break };
        if c == qc {
            if let Some(last) = last_match {
                penalty += pos as i32 - last - 1;
            }
            last_match = Some(pos as i32);
            want = query_chars.next();
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
}
