//! What a conversation's agent launches with: platform, model, and effort.
//!
//! The focus node's Agent capability (its own, or the nearest ancestor's)
//! decides whatever it sets; the rest follows the settings for the kind of
//! conversation — "Chat with agent" for talking with the agent, "Default
//! agent" for the work the lifecycle runs. Resolved again for every turn, so
//! a change to either applies to the next one.

use std::path::Path;
use tod_agent::AgentLaunchOptions;
use tod_store::conversation::{Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use tod_store::{AgentRole, TodSettings};

/// Which settings a conversation of `kind` follows where the node's Agent
/// capability leaves a value unset.
pub fn role_for(kind: ProtocolKind) -> AgentRole {
    match kind {
        ProtocolKind::Outline | ProtocolKind::Chat => AgentRole::Chat,
        ProtocolKind::Implementation
        | ProtocolKind::Verification
        | ProtocolKind::Review
        | ProtocolKind::Fix
        | ProtocolKind::VisualDesign
        | ProtocolKind::GateCheck
        | ProtocolKind::OnEntry
        | ProtocolKind::Phase
        | ProtocolKind::Evaluate
        | ProtocolKind::Incoming
        | ProtocolKind::Pr => AgentRole::Default,
    }
}

/// `fallback` with whatever the focus node's Agent capability sets over it.
pub fn for_focus(fleet: &FleetStore, focus: Focus, fallback: AgentLaunchOptions) -> AgentLaunchOptions {
    let Some(node) = focus.node_id() else {
        return fallback;
    };
    match fleet.resolve_agent_for_node(&node.to_string()) {
        Ok(Some(resolved)) => resolved.agent.launch_options(&fallback),
        Ok(None) => fallback,
        Err(err) => {
            tracing::warn!("reading node {node}'s Agent capability: {err:#}");
            fallback
        }
    }
}

/// The launch for a conversation of `kind` about `focus`, from `settings`.
pub fn resolve(
    fleet: &FleetStore,
    settings: &TodSettings,
    focus: Focus,
    kind: ProtocolKind,
) -> AgentLaunchOptions {
    for_focus(fleet, focus, settings.launch_options_for(role_for(kind)))
}

/// [`resolve`] with the settings read from `settings_path`; the defaults when
/// they cannot be read.
pub fn resolve_from(
    fleet: &FleetStore,
    settings_path: &Path,
    focus: Focus,
    kind: ProtocolKind,
) -> AgentLaunchOptions {
    let settings = TodSettings::load_from_path(settings_path).unwrap_or_else(|err| {
        tracing::warn!("reading settings for the agent's launch: {err:#}");
        TodSettings::default()
    });
    resolve(fleet, &settings, focus, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_agent::AgentPlatform;
    use tod_store::fleet::FleetMutation;
    use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
    use uuid::Uuid;

    fn store_with_node() -> (std::path::PathBuf, FleetStore, Uuid) {
        let root = std::env::temp_dir().join(format!("tod-launch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        store.writer().flush().unwrap();
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Node".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        (root, store, node)
    }

    fn settings() -> TodSettings {
        let mut settings = TodSettings::default();
        for role in [AgentRole::Default, AgentRole::Chat, AgentRole::Interview] {
            settings.set_platform_for(role, AgentPlatform::Claude);
        }
        settings.set_model_for(AgentRole::Default, "opus");
        settings.set_model_for(AgentRole::Chat, "sonnet");
        settings.set_effort_for(AgentRole::Chat, "medium");
        settings.set_model_for(AgentRole::Interview, "haiku");
        settings
    }

    #[test]
    fn a_conversation_follows_its_kinds_settings_not_the_interviews() {
        let (root, store, node) = store_with_node();
        let settings = settings();
        let chat = resolve(&store, &settings, Focus::Node(node), ProtocolKind::Chat);
        assert_eq!((chat.model.as_str(), chat.effort.as_str()), ("sonnet", "medium"));
        let outline = resolve(&store, &settings, Focus::Project, ProtocolKind::Outline);
        assert_eq!(outline.model, "sonnet");
        let implement = resolve(&store, &settings, Focus::Node(node), ProtocolKind::Implementation);
        assert_eq!(implement.model, "opus");
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_nodes_agent_capability_sets_what_it_sets() {
        let (root, store, node) = store_with_node();
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Agent],
            })
            .unwrap();
        store
            .enqueue(FleetMutation::UpsertNodeAgent {
                node_id: node.to_string(),
                platform: None,
                model: Some("opus[1m]".into()),
                effort: None,
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let chat = resolve(&store, &settings(), Focus::Node(node), ProtocolKind::Chat);
        // Its model; the effort it leaves unset follows the settings.
        assert_eq!((chat.model.as_str(), chat.effort.as_str()), ("opus[1m]", "medium"));
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
    }
}
