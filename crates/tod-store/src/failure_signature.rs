//! A short, stable fingerprint of an error text, for "is this the same
//! failure?" across nodes and across time: a request an agent files about a
//! broken tool (`decisions ask --error`), and the cross-node incidents that
//! match on it (`doc/requests/design.md` 4.1, 4.3, 4.5).
//!
//! It builds on [`crate::github::failure_signature`]'s normal form (ANSI
//! escapes, timestamps, UUIDs, hashes, temp directories, run ids and the line
//! and column after a file name removed; line order and repeats ignored) and
//! additionally drops bare numbers (a port, a pid, a count, a duration in
//! its own word), since an error that differs only in those is the same
//! error to a person.

/// The signature of `error`, or empty when nothing is left of it after
/// normalizing (an empty signature never matches anything).
pub fn of_text(error: &str) -> String {
    if error.len() > 8 * 1024 * 1024 {
        return String::new();
    }
    let lines: Vec<String> = crate::github::normalized_failure_lines(error)
        .into_iter()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().filter(|w| !is_bare_number(w)).collect();
            (!words.is_empty()).then(|| words.join(" "))
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    hash_hex(&lines.join("\n"))
}

fn is_bare_number(word: &str) -> bool {
    let core = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    !core.is_empty() && core.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// FNV-1a, 16 hex digits.
pub(crate) fn hash_hex(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in text.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volatile_parts_do_not_change_the_signature() {
        let a = of_text("\u{1b}[31mError\u{1b}[0m: connect ECONNREFUSED 10.0.0.5:6379 at 2026-11-20T10:00:00Z (pid 4411)");
        let b = of_text("Error: connect ECONNREFUSED 10.0.0.5:6379 at 2026-11-21T08:30:12Z (pid 98)");
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_error_is_a_different_signature_and_nothing_is_empty() {
        assert_ne!(of_text("permission denied for table orders"), of_text("permission denied for table users"));
        assert_eq!(of_text("   \n"), "");
        assert_eq!(of_text("2026-11-20T10:00:00Z 4411"), "");
    }
}
