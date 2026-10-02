//! Which published release carries a merged pull request.
//!
//! A node in `merged` waits for its change to be released
//! (`crate::autopilot`, "Waiting on GitHub"). The next release is not assumed
//! to include it: a cherry-pick or a release from another branch breaks that.
//! Instead the release notes are searched, since the project's notes list the
//! pull requests' titles (GitHub's generated notes also link each pull
//! request by number). A release counts only if it was published after the
//! pull request was merged. Pure: the caller reads GitHub.

use chrono::{DateTime, FixedOffset};
use tod_store::github::{NodePr, PrMerge, Release};

/// The newest release in `releases` (newest first, as GitHub lists them)
/// whose notes name `pr`, by its title or by a link or `#number` reference,
/// and that was published after `pr` was merged. `None` while no release does.
pub fn release_carrying<'a>(pr: &NodePr, merge: &PrMerge, releases: &'a [Release]) -> Option<&'a Release> {
    let merged_at = merge.merged_at.as_deref().and_then(parse_time);
    releases
        .iter()
        .filter(|release| match (merged_at, release.published_at.as_deref().and_then(parse_time)) {
            (Some(merged), Some(published)) => published >= merged,
            // A time GitHub did not give: do not rule the release out.
            _ => true,
        })
        .find(|release| notes_name(&release.body, pr, &merge.title))
}

fn parse_time(text: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(text).ok()
}

/// Whether `notes` name the pull request: its title, compared without regard
/// to case and spacing, or its URL or `#number`.
fn notes_name(notes: &str, pr: &NodePr, title: &str) -> bool {
    let title = squash(title);
    if !title.is_empty() && squash(notes).contains(&title) {
        return true;
    }
    let link = format!("{}/{}/pull/{}", pr.owner, pr.repo, pr.pr_number).to_lowercase();
    let lower = notes.to_lowercase();
    refers_to(&lower, &link, false) || refers_to(&lower, &format!("#{}", pr.pr_number), true)
}

/// Lower case, with every run of whitespace one space.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Whether `text` holds `needle` as a whole reference: not followed by
/// another digit (`#12` is not `#123`), and, for a `#number`, not part of a
/// longer word or link (`issues/#12`, `abc#12`).
fn refers_to(text: &str, needle: &str, bare: bool) -> bool {
    text.match_indices(needle).any(|(at, _)| {
        let after = text[at + needle.len()..].chars().next();
        let before = text[..at].chars().next_back();
        !after.is_some_and(|c| c.is_ascii_digit())
            && (!bare || !before.is_some_and(|c| c.is_alphanumeric() || c == '/' || c == '#'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr() -> NodePr {
        NodePr::new("acme", "app", 12)
    }

    fn merge(at: Option<&str>) -> PrMerge {
        PrMerge { title: "Add the widget".into(), merged: true, merged_at: at.map(str::to_string) }
    }

    fn release(tag: &str, published: &str, body: &str) -> Release {
        Release {
            tag: tag.into(),
            name: None,
            body: body.into(),
            url: format!("https://github.com/acme/app/releases/tag/{tag}"),
            published_at: Some(published.into()),
        }
    }

    #[test]
    fn finds_the_release_whose_notes_have_the_title() {
        let releases = [
            release("v2", "2026-03-02T00:00:00Z", "* Something else by @bob"),
            release("v1", "2026-03-01T12:00:00Z", "* add  the WIDGET by @ann in #12"),
        ];
        let found = release_carrying(&pr(), &merge(Some("2026-03-01T00:00:00Z")), &releases).unwrap();
        assert_eq!(found.tag, "v1");
    }

    #[test]
    fn the_next_release_is_not_assumed_to_include_it() {
        let releases = [release("v2", "2026-03-02T00:00:00Z", "* Unrelated change by @bob in #99")];
        assert!(release_carrying(&pr(), &merge(Some("2026-03-01T00:00:00Z")), &releases).is_none());
    }

    #[test]
    fn a_release_before_the_merge_does_not_count() {
        // The same title in an older release: a different change.
        let releases = [release("v0", "2026-02-01T00:00:00Z", "* Add the widget")];
        assert!(release_carrying(&pr(), &merge(Some("2026-03-01T00:00:00Z")), &releases).is_none());
        // Without a merge time there is nothing to compare with.
        assert!(release_carrying(&pr(), &merge(None), &releases).is_some());
    }

    #[test]
    fn the_pull_request_may_be_named_by_link_or_number() {
        let by_link = release("a", "2026-03-02T00:00:00Z", "renamed in https://github.com/acme/app/pull/12 .");
        let by_number = release("b", "2026-03-02T00:00:00Z", "* tweak (#12)");
        let m = merge(None);
        assert!(release_carrying(&pr(), &m, &[by_link]).is_some());
        assert!(release_carrying(&pr(), &m, &[by_number]).is_some());
    }

    #[test]
    fn another_pull_request_with_a_longer_number_is_not_this_one() {
        let m = merge(None);
        for body in ["* x (#123)", "see https://github.com/acme/app/pull/123", "acme/app#12", "issues/#12"] {
            assert!(release_carrying(&pr(), &m, &[release("a", "2026-03-02T00:00:00Z", body)]).is_none(), "{body}");
        }
    }

    #[test]
    fn an_empty_title_matches_nothing_by_itself() {
        let m = PrMerge { title: "  ".into(), merged: true, merged_at: None };
        assert!(release_carrying(&pr(), &m, &[release("a", "2026-03-02T00:00:00Z", "anything")]).is_none());
    }
}
