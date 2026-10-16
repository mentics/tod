//! The Pull requests block of an agent's context: the pull requests linked to
//! the node the work is on (its Ticket capability's links, where `tod-cli pr
//! open` adds the one it opens).
//!
//! Every surface gets it in every lifecycle state: a node sent back to an
//! earlier state after its pull request was opened is still about that pull
//! request, and what is wrong with it is usually the reason it was sent back.

use anyhow::Result;
use rusqlite::Connection;
use tod_store::fleet::FleetStore;
use tod_store::github::NodePrRepo;
use uuid::Uuid;

/// The block for `node`; empty when it has no linked pull requests.
pub fn render(conn: &Connection, node: Uuid) -> Result<String> {
    let links = NodePrRepo::new(conn).read(node)?;
    let urls: Vec<&str> = links
        .prs
        .iter()
        .map(|pr| pr.url.as_str())
        .chain(links.unrecognized.iter().map(String::as_str))
        .collect();
    Ok(render_links(&urls))
}

/// [`render`] for a caller with the store. A failed read is an error in the
/// log and a visible line in the context, never a silently missing block.
pub fn for_node(fleet: &FleetStore, node: Uuid) -> String {
    fleet.read(|conn| render(conn, node)).unwrap_or_else(|err| {
        tracing::error!("could not read the node's pull requests: {err:#}");
        format!(
            "\n## Pull requests\n\n(Could not read this node's linked pull requests: {err:#}. \
             Do not assume it has none.)\n"
        )
    })
}

pub fn render_links(urls: &[&str]) -> String {
    if urls.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Pull requests\n\n");
    out.push_str(if urls.len() == 1 {
        "This node has a linked pull request. It may be open, merged, closed, or overdue for \
         review; check GitHub for its current state before relying on it:\n\n"
    } else {
        "This node has linked pull requests. Each may be open, merged, closed, or overdue for \
         review; check GitHub for its current state before relying on it:\n\n"
    });
    for url in urls {
        out.push_str(&format!("- {url}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_links_render_nothing() {
        assert_eq!(render_links(&[]), "");
    }

    #[test]
    fn every_link_is_listed() {
        let text = render_links(&["https://github.com/o/r/pull/1", "https://github.com/o/s/pull/2"]);
        assert!(text.contains("## Pull requests"), "{text}");
        assert!(text.contains("- https://github.com/o/r/pull/1\n"), "{text}");
        assert!(text.contains("- https://github.com/o/s/pull/2\n"), "{text}");
    }
}
