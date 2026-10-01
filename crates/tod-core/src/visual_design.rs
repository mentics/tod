//! The visual-design mockup operations the app and `tod-cli` share: staging a
//! mockup file under the data root, accepting a conversation's working draft
//! (recorded in its change set), and telling whether the draft differs from
//! the saved mockup. Design: `doc/ui/visual-design-browser.md` section 8.2.

use crate::conversation::context::visual_design_draft_path;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::outline::OutlineMutation;
use tod_store::outline::repos::ObligationRepo;
use uuid::Uuid;

/// Where an obligation's saved mockup lives: one file per obligation, so
/// saving again overwrites it.
pub fn saved_mockup_path(data_root: &Path, node_id: Uuid, obligation_id: Uuid) -> PathBuf {
    data_root
        .join("visual-design")
        .join(node_id.to_string())
        .join(format!("{obligation_id}.html"))
}

/// Check `html` is a self-contained mockup and write it as the obligation's
/// saved file. Returns the (canonicalized where possible) path to link.
pub fn stage_mockup(
    data_root: &Path,
    node_id: Uuid,
    obligation_id: Uuid,
    html: &str,
) -> Result<PathBuf> {
    if html.to_ascii_lowercase().contains("<script") {
        bail!(
            "visual design mockups must not contain <script> tags (self-contained HTML+CSS only)"
        );
    }
    let dest = saved_mockup_path(data_root, node_id, obligation_id);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::write(&dest, html).with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(tod_store::path_util::canonicalize_if_possible(&dest))
}

/// Whether the working draft differs from the saved mockup: false when there
/// is no draft, true when there is a draft and nothing saved (or the saved
/// file is unreadable).
pub fn draft_differs(draft: &Path, saved: Option<&str>) -> bool {
    let Ok(draft_bytes) = std::fs::read(draft) else {
        return false;
    };
    match saved.map(std::fs::read) {
        Some(Ok(saved_bytes)) => saved_bytes != draft_bytes,
        _ => true,
    }
}

/// Accept the conversation's working draft: copy it under the data root and
/// link it from the obligation, as the user's edit in that conversation (so it
/// is in the change set, can be reversed, and reaches the agent through the
/// next turn's delta). Blocking: run it off the UI thread. Returns the linked
/// path. Accepting again overwrites the file and relinks it.
pub fn accept_draft(
    fleet: &FleetStore,
    data_root: &Path,
    conversation_id: Uuid,
    obligation_id: Uuid,
) -> Result<PathBuf> {
    let draft = visual_design_draft_path(data_root, conversation_id);
    if !draft.is_file() {
        bail!(
            "there is no working draft to accept yet ({})",
            draft.display()
        );
    }
    let html = std::fs::read_to_string(&draft)
        .with_context(|| format!("failed to read {}", draft.display()))?;
    let obligation = fleet
        .read(|conn| ObligationRepo::new(conn).get(obligation_id))?
        .with_context(|| format!("obligation {obligation_id} not found"))?;
    let dest = stage_mockup(data_root, obligation.node_id, obligation_id, &html)?;
    fleet
        .interview(
            ACTOR_USER,
            InterviewCommand::ConversationEdit {
                conversation_id,
                mutation: OutlineMutation::UpdateObligationVisualDesign {
                    obligation_id,
                    path: Some(dest.display().to_string()),
                },
            },
        )
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::{Fixture, fixture};
    use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
    use tod_store::outline::KIND_REQUIREMENT;

    struct Fx {
        fx: Fixture,
        conversation: Uuid,
        obligation: Uuid,
    }

    fn setup() -> Fx {
        let fx = fixture();
        let obligation = Uuid::new_v4();
        fx.fleet
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(obligation),
                node_id: fx.node,
                kind: KIND_REQUIREMENT.into(),
                after_id: None,
                before: false,
                section: None,
                body: "A settings screen.".into(),
                phase: tod_store::interview::PHASE_DESIGN.into(),
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        let conversation = Uuid::new_v4();
        fx.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id: conversation,
                    focus: Focus::Obligation {
                        node: fx.node,
                        id: obligation,
                    },
                    protocol: ProtocolKind::VisualDesign,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        Fx {
            fx,
            conversation,
            obligation,
        }
    }

    fn write_draft(s: &Fx, html: &str) {
        let draft = visual_design_draft_path(&s.fx.root, s.conversation);
        std::fs::create_dir_all(draft.parent().unwrap()).unwrap();
        std::fs::write(draft, html).unwrap();
    }

    fn linked(s: &Fx) -> Option<String> {
        s.fx.fleet
            .read(|c| ObligationRepo::new(c).get(s.obligation))
            .unwrap()
            .unwrap()
            .visual_design_path
    }

    fn accept(s: &Fx) -> Result<PathBuf> {
        accept_draft(&s.fx.fleet, &s.fx.root, s.conversation, s.obligation)
    }

    fn delta(s: &Fx) -> String {
        s.fx.fleet
            .read(|c| {
                crate::conversation::context::delta(c, s.conversation, 0, &mut Default::default())
            })
            .unwrap()
    }

    #[test]
    fn accept_links_a_copy_and_clears_the_difference() {
        let s = setup();
        write_draft(&s, "<p>one</p>");
        let path = accept(&s).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "<p>one</p>");
        assert_eq!(linked(&s), Some(path.display().to_string()));
        let draft = visual_design_draft_path(&s.fx.root, s.conversation);
        assert!(!draft_differs(&draft, linked(&s).as_deref()));
        write_draft(&s, "<p>two</p>");
        assert!(draft_differs(&draft, linked(&s).as_deref()));
    }

    #[test]
    fn accept_twice_overwrites_the_one_file() {
        let s = setup();
        write_draft(&s, "<p>one</p>");
        let first = accept(&s).unwrap();
        write_draft(&s, "<p>two</p>");
        let second = accept(&s).unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "<p>two</p>");
        assert_eq!(linked(&s), Some(second.display().to_string()));
    }

    #[test]
    fn accept_is_in_the_change_set_reverses_and_reaches_the_agent() {
        let s = setup();
        write_draft(&s, "<p>one</p>");
        accept(&s).unwrap();
        assert!(delta(&s).contains("accepted mockup"), "{}", delta(&s));
        let actions = s
            .fx
            .fleet
            .read(|c| ConversationRepo::new(c).actions(s.conversation))
            .unwrap();
        assert_eq!(actions.len(), 1);
        s.fx.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::ReverseConversationActions {
                    conversation_id: s.conversation,
                    action_ids: vec![actions[0].id],
                    include_dependents: false,
                    force: false,
                },
            )
            .unwrap();
        assert_eq!(linked(&s), None);
    }

    #[test]
    fn accept_without_a_draft_is_an_error() {
        let s = setup();
        let err = accept(&s).unwrap_err().to_string();
        assert!(err.contains("no working draft"), "{err}");
        assert_eq!(linked(&s), None);
    }

    #[test]
    fn a_script_is_refused() {
        let s = setup();
        write_draft(&s, "<script>x</script>");
        assert!(accept(&s).is_err());
        assert_eq!(linked(&s), None);
    }
}
