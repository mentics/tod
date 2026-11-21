//! Token estimation shared by conversation context assembly.

/// Rough token count for `text`.
pub fn estimate_tokens(text: &str) -> i64 {
    (text.len() / 4) as i64
}
