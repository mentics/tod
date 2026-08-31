//! Human-readable node slug generation.

use crate::outline::uuid_blob::uuid_to_blob;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub const SLUG_MAX_LEN: usize = 40;

/// Lowercase slug fragment: alphanumeric plus `-` / `_`, no spaces.
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_sep = false;
    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
            prev_sep = false;
        } else if !prev_sep {
            out.push('-');
            prev_sep = true;
        }
    }
    out.trim_matches('-')
        .trim_matches('_')
        .trim_matches('-')
        .to_string()
}

/// Derive a slug base from title and optional linked ticket id.
pub fn derive_node_slug(title: &str, ticket_id: Option<&str>) -> String {
    let title_part = title_part_for_slug(title, ticket_id);
    let base = if let Some(ticket) = ticket_id.filter(|s| !s.is_empty()) {
        let ticket_slug = slugify(ticket);
        if title_part.is_empty() {
            ticket_slug
        } else {
            format!("{ticket_slug}-{title_part}")
        }
    } else if title_part.is_empty() {
        "untitled".into()
    } else {
        title_part
    };
    truncate_slug(&base)
}

fn title_part_for_slug(title: &str, ticket_id: Option<&str>) -> String {
    let mut remainder = title.trim();
    if let Some(ticket) = ticket_id.filter(|s| !s.is_empty()) {
        let prefixes = [
            format!("linear issue {ticket}"),
            format!("linear issue {}", ticket.to_ascii_lowercase()),
            ticket.to_string(),
            ticket.to_ascii_lowercase(),
        ];
        for prefix in prefixes {
            if remainder.len() >= prefix.len()
                && remainder[..prefix.len()].eq_ignore_ascii_case(prefix.as_str())
            {
                remainder = remainder[prefix.len()..].trim();
                remainder = remainder
                    .trim_start_matches('-')
                    .trim_start_matches(':')
                    .trim();
                break;
            }
        }
    }
    slugify(remainder)
}

pub fn truncate_slug(slug: &str) -> String {
    if slug.len() <= SLUG_MAX_LEN {
        return slug.to_string();
    }
    slug[..SLUG_MAX_LEN]
        .trim_end_matches('-')
        .trim_end_matches('_')
        .to_string()
}

fn truncate_for_suffix(base: &str, suffix: &str) -> String {
    let max_base = SLUG_MAX_LEN.saturating_sub(suffix.len());
    let trimmed = if base.len() <= max_base {
        base.to_string()
    } else {
        base[..max_base]
            .trim_end_matches('-')
            .trim_end_matches('_')
            .to_string()
    };
    truncate_slug(&format!("{trimmed}{suffix}"))
}

fn slug_taken(conn: &Connection, slug: &str, exclude_id: Option<Uuid>) -> Result<bool> {
    let existing: Option<Vec<u8>> = conn
        .query_row(
            "SELECT id FROM nodes WHERE lower(slug) = lower(?1)",
            params![slug],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match (existing, exclude_id) {
        (Some(blob), Some(exclude)) => blob != uuid_to_blob(exclude),
        (Some(_), None) => true,
        (None, _) => false,
    })
}

/// Pick a globally unique slug, appending `-2`, `-3`, … when needed.
pub fn allocate_unique_slug(
    conn: &Connection,
    base: &str,
    exclude_id: Option<Uuid>,
) -> Result<String> {
    let base = truncate_slug(base);
    if !slug_taken(conn, &base, exclude_id)? {
        return Ok(base);
    }
    for n in 2..=999 {
        let suffix = format!("-{n}");
        let candidate = truncate_for_suffix(&base, &suffix);
        if !slug_taken(conn, &candidate, exclude_id)? {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not allocate unique slug for {base}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn slugify_title() {
        assert_eq!(slugify("My Cool Feature"), "my-cool-feature");
        assert_eq!(slugify("  spaces  everywhere  "), "spaces-everywhere");
        assert_eq!(slugify("under_score"), "under_score");
    }

    #[test]
    fn derive_from_title_only() {
        assert_eq!(derive_node_slug("Fix login bug", None), "fix-login-bug");
        assert_eq!(derive_node_slug("", None), "untitled");
    }

    #[test]
    fn derive_with_ticket_id() {
        assert_eq!(
            derive_node_slug("Fix login bug", Some("TOD-142")),
            "tod-142-fix-login-bug"
        );
        assert_eq!(
            derive_node_slug("Linear issue TOD-142", Some("TOD-142")),
            "tod-142"
        );
        assert_eq!(derive_node_slug("TOD-142", Some("TOD-142")), "tod-142");
    }

    #[test]
    fn allocate_unique_slug_suffixes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (
                id BLOB PRIMARY KEY,
                slug TEXT NOT NULL UNIQUE
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (id, slug) VALUES (?1, ?2)",
            params![vec![1u8], "fix-bug"],
        )
        .unwrap();

        let id = Uuid::from_u128(2);
        assert_eq!(
            allocate_unique_slug(&conn, "fix-bug", Some(id)).unwrap(),
            "fix-bug-2"
        );
    }
}
