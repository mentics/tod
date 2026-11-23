pub fn normalize(text: &str) -> String {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Exact matches have space above every fuzzy match. Frequency adds at most
/// six points and so cannot turn a weak match into an exact one.
pub fn quality(query: &str, value: &str) -> u32 {
    let value = normalize(value);
    if value.is_empty() {
        return 0;
    }
    if value == query {
        return 110;
    }
    if value.starts_with(query) {
        return 95;
    }

    let words: Vec<_> = value.split_whitespace().collect();
    if value
        .match_indices(query)
        .any(|(index, _)| value[..index].ends_with(' '))
    {
        return 87;
    }
    if value.contains(query) {
        return 80;
    }
    let initials: String = words
        .iter()
        .filter_map(|word| word.chars().next())
        .collect();
    if initials == query {
        return 77;
    }

    let tokens: Vec<_> = query.split_whitespace().collect();
    if tokens.len() > 1 {
        let mut remaining = words.as_slice();
        let mut points = 73u32;
        for token in tokens.iter().copied() {
            let Some((index, penalty)) = remaining.iter().enumerate().find_map(|(index, word)| {
                let penalty = if word.starts_with(token) {
                    0
                } else if token.chars().count() == 1 {
                    return None;
                } else if let Some(gaps) = ordered_gaps(token, word).filter(|gaps| *gaps <= 3) {
                    2 + gaps
                } else if token.chars().count() >= 4 && one_edit(token, word) {
                    8
                } else {
                    return None;
                };
                Some((index, penalty))
            }) else {
                return 0;
            };
            points = points.saturating_sub(penalty + index.min(10) as u32);
            remaining = &remaining[index + 1..];
        }
        return points.max(35);
    }
    if let Some(gaps) = ordered_gaps(query, &value) {
        return 68u32.saturating_sub(gaps.min(29));
    }
    if query.chars().count() >= 4 && words.iter().any(|word| one_edit(query, word)) {
        return 55;
    }
    0
}

fn ordered_gaps(query: &str, value: &str) -> Option<u32> {
    let mut characters = value.chars();
    let mut gaps = 0u32;
    for letter in query.chars() {
        gaps = gaps.saturating_add(characters.position(|candidate| candidate == letter)? as u32);
    }
    Some(gaps)
}

/// One insertion, deletion, substitution, or adjacent transposition.
fn one_edit(left: &str, right: &str) -> bool {
    let left: Vec<_> = left.chars().collect();
    let right: Vec<_> = right.chars().collect();
    let prefix = left.iter().zip(&right).take_while(|(a, b)| a == b).count();
    let (left, right) = (&left[prefix..], &right[prefix..]);
    match (left, right) {
        ([a, b, rest @ ..], [x, y, tail @ ..]) if a == y && b == x && rest == tail => true,
        ([_, rest @ ..], [_, tail @ ..]) if rest == tail => true,
        ([_, rest @ ..], right) if rest == right => true,
        (left, [_, tail @ ..]) => left == tail,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_partial_words_initials_and_ordered_letters() {
        for (query, label) in [
            ("p rn", "Pause runner"),
            ("nmd", "Needs my decision"),
            ("paus runer", "Pause runner"),
            ("psrn", "Pause runner"),
            ("syn ret", "Fix sync retries"),
            ("retri", "Retries"),
        ] {
            assert!(quality(query, label) > 0, "{query} should match {label}");
        }
        assert_eq!(quality("p rn", "Open agent conversation"), 0);
        assert!(
            quality("keyboard shortcuts", "Keyboard shortcuts")
                > quality("keyboard shortcuts", "Show keyboard shortcuts")
        );
    }

    #[test]
    fn matches_unicode_and_small_edits() {
        assert_eq!(normalize("  RÉSUMÉ—Runner "), "résumé runner");
        assert!(quality("résumé", "Résumé runner") > 0);
        assert!(quality("shorctuts", "Keyboard shortcuts") > 0);
    }
}
