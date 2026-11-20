//! What a conversation's agent launches with: platform, model, and effort.
//!
//! For a lifecycle conversation, the Lifecycle config's model and effort for
//! its phase (`lifecycle_config::resolve_agent`, from the nearest node that
//! sets each) win. Under them, the focus node's Agent capability (its own, or
//! the nearest ancestor's) decides whatever it sets; the rest follows the
//! settings for the kind of conversation — "Chat with agent" for talking with
//! the agent, "Default agent" for the work the lifecycle runs. Resolved again
//! for every turn, so a change to any of them applies to the next one.

use std::path::Path;
use tod_agent::AgentLaunchOptions;
use tod_agent::agent_launch::{coerce_effort, coerce_model};
use tod_store::conversation::{Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use tod_store::lifecycle_config;
use tod_store::review::Reviewer;
use tod_store::{AgentRole, TodSettings};

/// Which settings a conversation of `kind` follows where the node's Agent
/// capability leaves a value unset.
pub fn role_for(kind: ProtocolKind) -> AgentRole {
    match kind {
        ProtocolKind::Outline | ProtocolKind::Chat | ProtocolKind::VisualDesign => AgentRole::Chat,
        ProtocolKind::Implementation
        | ProtocolKind::Verification
        | ProtocolKind::Review
        | ProtocolKind::Fix
        | ProtocolKind::GateCheck
        | ProtocolKind::OnEntry
        | ProtocolKind::Phase
        | ProtocolKind::Evaluate
        | ProtocolKind::Incoming
        | ProtocolKind::Guidance
        | ProtocolKind::Pr => AgentRole::Default,
    }
}

/// The Lifecycle config phase key a conversation of `kind` about `focus`
/// works in, whose model and effort apply to it: the protocol's own phase
/// for the implementation, verification, review and fix protocols; the
/// node's lifecycle state for a phase agent, an evaluator, and the PR
/// agent (`pr`, `pr_draft`, `pr_review`). `None` for every other kind, and
/// for a node whose state has no phase. A review round's risk reviewer
/// (`reviewer`) works under `review_risk`, which falls back to `review`
/// (`lifecycle_config::fallback`).
pub fn phase_for(
    fleet: &FleetStore,
    focus: Focus,
    kind: ProtocolKind,
    reviewer: Option<Reviewer>,
) -> Option<String> {
    let state = || {
        let node = focus.node_id()?;
        fleet.get_node(&node.to_string()).ok()?.map(|n| n.lifecycle)
    };
    let phase = match kind {
        ProtocolKind::Implementation => "implement".to_string(),
        ProtocolKind::Verification => "verify".to_string(),
        ProtocolKind::Review => match reviewer {
            Some(Reviewer::Risk) => lifecycle_config::REVIEW_RISK.to_string(),
            _ => lifecycle_config::REVIEW.to_string(),
        },
        ProtocolKind::Fix => "fix".to_string(),
        ProtocolKind::Guidance => lifecycle_config::GUIDANCE.to_string(),
        ProtocolKind::Pr => match state()?.as_str() {
            "pr_draft" => "pr_draft".to_string(),
            "pr_review" => "pr_review".to_string(),
            _ => "pr".to_string(),
        },
        ProtocolKind::Phase => state()?,
        // The evidence evaluator in `verifying` has a key of its own, which
        // falls back to `verify`'s (`lifecycle_config::fallback`).
        ProtocolKind::Evaluate => match state()?.as_str() {
            "verifying" => lifecycle_config::VERIFY_EVALUATE.to_string(),
            other => other.to_string(),
        },
        ProtocolKind::Outline
        | ProtocolKind::Chat
        | ProtocolKind::VisualDesign
        | ProtocolKind::GateCheck
        | ProtocolKind::OnEntry
        | ProtocolKind::Incoming => return None,
    };
    lifecycle_config::is_phase(&phase).then_some(phase)
}

/// `fallback` with whatever the focus node's Agent capability sets over it,
/// and the Lifecycle config's model and effort for `phase` over that.
/// A failure to read them is logged and `fallback` used: for showing what a
/// launch would be. A launch itself uses [`try_for_focus`].
pub fn for_focus(
    fleet: &FleetStore,
    focus: Focus,
    phase: Option<&str>,
    fallback: AgentLaunchOptions,
) -> AgentLaunchOptions {
    try_for_focus(fleet, focus, phase, fallback.clone()).unwrap_or_else(|err| {
        tracing::warn!("{err:#}");
        fallback
    })
}

/// [`for_focus`], refusing when the node's Agent capability or Lifecycle
/// config cannot be read: launching with other settings than the node's
/// would be a silent change of model or platform.
pub fn try_for_focus(
    fleet: &FleetStore,
    focus: Focus,
    phase: Option<&str>,
    fallback: AgentLaunchOptions,
) -> anyhow::Result<AgentLaunchOptions> {
    let Some(node) = focus.node_id() else {
        return Ok(fallback);
    };
    let mut options = match fleet.resolve_agent_for_node(&node.to_string()) {
        Ok(Some(resolved)) => resolved.agent.launch_options(&fallback),
        Ok(None) => fallback,
        Err(err) => return Err(err.context(format!("reading node {node}'s Agent capability"))),
    };
    if let Some(phase) = phase {
        let configured = fleet
            .read(|conn| lifecycle_config::resolve_agent(conn, node, phase))
            .map_err(|err| err.context(format!("reading node {node}'s Lifecycle config for the {phase} phase")))?;
        // The guidance selector only matches descriptions against a change
        // summary: unless its key says otherwise, it runs on the small model
        // at low effort, whatever the node's Agent capability says.
        let (default_model, default_effort) = if phase == lifecycle_config::GUIDANCE {
            (Some(lifecycle_config::GUIDANCE_DEFAULT_MODEL), Some(lifecycle_config::GUIDANCE_DEFAULT_EFFORT))
        } else {
            (None, None)
        };
        if let Some(model) = configured.model.map(|(m, _)| m).or(default_model.map(str::to_string)) {
            options.model = coerce_model(options.platform, &model);
        }
        if let Some(effort) = configured.effort.map(|(e, _)| e).or(default_effort.map(str::to_string)) {
            options.effort = coerce_effort(options.platform, &effort);
        }
    }
    Ok(options)
}

/// The launch for a conversation of `kind` about `focus`, from `settings`.
pub fn resolve(
    fleet: &FleetStore,
    settings: &TodSettings,
    focus: Focus,
    kind: ProtocolKind,
) -> AgentLaunchOptions {
    resolve_reviewer(fleet, settings, focus, kind, None)
}

/// [`resolve`] for a review round's `reviewer` (the risk reviewer has a key
/// of its own).
pub fn resolve_reviewer(
    fleet: &FleetStore,
    settings: &TodSettings,
    focus: Focus,
    kind: ProtocolKind,
    reviewer: Option<Reviewer>,
) -> AgentLaunchOptions {
    let phase = phase_for(fleet, focus, kind, reviewer);
    for_focus(fleet, focus, phase.as_deref(), settings.launch_options_for(role_for(kind)))
}

/// [`resolve_reviewer`] with the settings read from `settings_path`; the
/// defaults (logged) when they cannot be read. A launch uses
/// [`try_resolve_from`].
pub fn resolve_from(
    fleet: &FleetStore,
    settings_path: &Path,
    focus: Focus,
    kind: ProtocolKind,
    reviewer: Option<Reviewer>,
) -> AgentLaunchOptions {
    let settings = TodSettings::load_from_path(settings_path).unwrap_or_else(|err| {
        tracing::warn!("reading settings for the agent's launch: {err:#}");
        TodSettings::default()
    });
    resolve_reviewer(fleet, &settings, focus, kind, reviewer)
}

/// [`resolve_from`] that fails when the settings or the node's Agent
/// capability cannot be read.
pub fn try_resolve_from(
    fleet: &FleetStore,
    settings_path: &Path,
    focus: Focus,
    kind: ProtocolKind,
    reviewer: Option<Reviewer>,
) -> anyhow::Result<AgentLaunchOptions> {
    let settings = TodSettings::load_from_path(settings_path)
        .map_err(|err| err.context("reading the settings for the agent's launch"))?;
    let phase = phase_for(fleet, focus, kind, reviewer);
    try_for_focus(fleet, focus, phase.as_deref(), settings.launch_options_for(role_for(kind)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_agent::AgentPlatform;
    use tod_store::fleet::FleetMutation;
    use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
    use uuid::Uuid;

    fn store_with_node() -> (std::path::PathBuf, FleetStore, Uuid) {
        let root = tod_test_util::scratch_dir(format!("tod-launch-{}", Uuid::new_v4()));
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

    #[test]
    fn the_lifecycle_configs_phase_model_and_effort_win_for_that_phase() {
        use tod_store::lifecycle_config::{Agents, PhaseAgent};
        let (root, store, node) = store_with_node();
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Agent, Capability::LifecycleConfig, Capability::Lifecycle],
            })
            .unwrap();
        store
            .enqueue(FleetMutation::UpsertNodeAgent {
                node_id: node.to_string(),
                platform: None,
                model: Some("sonnet".into()),
                effort: Some("low".into()),
            })
            .unwrap();
        let agents: Agents = [
            ("review".to_string(), PhaseAgent { model: Some("opus".into()), effort: None }),
            ("proposed".to_string(), PhaseAgent { model: None, effort: Some("max".into()) }),
            ("implement".to_string(), PhaseAgent { model: Some("not-a-model".into()), effort: None }),
        ]
        .into();
        store.enqueue_outline(OutlineMutation::SetNodeLifecycleAgents { node_id: node, agents }).unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let at = |kind| resolve(&store, &settings(), Focus::Node(node), kind);
        // The review phase's model, the Agent capability's effort.
        let review = at(ProtocolKind::Review);
        assert_eq!((review.model.as_str(), review.effort.as_str()), ("opus", "low"));
        // A phase with nothing set follows the Agent capability.
        let verify = at(ProtocolKind::Verification);
        assert_eq!((verify.model.as_str(), verify.effort.as_str()), ("sonnet", "low"));
        // A phase agent is in the node's state (`proposed`, from creation).
        let phase = at(ProtocolKind::Phase);
        assert_eq!((phase.model.as_str(), phase.effort.as_str()), ("sonnet", "max"));
        assert_eq!(at(ProtocolKind::Evaluate).effort, "max");
        // A model the platform does not know falls to its default, as the
        // Agent capability's would.
        assert_eq!(at(ProtocolKind::Implementation).model, "default");
        // Chat is not a phase.
        assert_eq!(at(ProtocolKind::Chat).model, "sonnet");
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The evaluator of verification's evidence runs under its own key,
    /// `verify_evaluate`, and takes `verify`'s settings for whatever its own
    /// leaves unset.
    #[test]
    fn the_verifying_evaluator_has_its_own_key_that_falls_back_to_verify() {
        use tod_store::lifecycle_config::{Agents, PhaseAgent, VERIFY_EVALUATE};
        let (root, store, node) = store_with_node();
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::LifecycleConfig, Capability::Lifecycle],
            })
            .unwrap();
        store.writer().flush().unwrap();
        let conn = tod_store::fleet::schema::open_writer_connection(store.writer().db_path()).unwrap();
        tod_store::outline::repos::NodeRepo::new(&conn).set_lifecycle(node, "verifying").unwrap();
        store.reload_if_stale().ok();
        assert_eq!(
            phase_for(&store, Focus::Node(node), ProtocolKind::Evaluate, None).as_deref(),
            Some(VERIFY_EVALUATE)
        );
        assert_eq!(
            phase_for(&store, Focus::Node(node), ProtocolKind::Verification, None).as_deref(),
            Some("verify")
        );
        let set = |agents: Agents| {
            store.enqueue_outline(OutlineMutation::SetNodeLifecycleAgents { node_id: node, agents }).unwrap();
            store.writer().flush().unwrap();
            store.reload_if_stale().ok();
        };
        let at = |kind| resolve(&store, &settings(), Focus::Node(node), kind);
        // Only `verify` set: the evaluator runs with it.
        set([("verify".to_string(), PhaseAgent { model: Some("sonnet".into()), effort: Some("high".into()) })].into());
        let evaluate = at(ProtocolKind::Evaluate);
        assert_eq!((evaluate.model.as_str(), evaluate.effort.as_str()), ("sonnet", "high"));
        // Its own model wins; the effort it leaves unset is still `verify`'s.
        set([
            ("verify".to_string(), PhaseAgent { model: Some("sonnet".into()), effort: Some("high".into()) }),
            (VERIFY_EVALUATE.to_string(), PhaseAgent { model: Some("opus".into()), effort: None }),
        ]
        .into());
        let evaluate = at(ProtocolKind::Evaluate);
        assert_eq!((evaluate.model.as_str(), evaluate.effort.as_str()), ("opus", "high"));
        let verify = at(ProtocolKind::Verification);
        assert_eq!(verify.model, "sonnet");
        // Skills fall back the same way.
        store
            .enqueue_outline(OutlineMutation::SetNodeLifecycleConfig {
                node_id: node,
                skills: [("verify".to_string(), vec!["drive-the-app".to_string()])].into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let skills = store
            .read(|conn| tod_store::lifecycle_config::resolve(conn, node, VERIFY_EVALUATE))
            .unwrap()
            .unwrap();
        assert_eq!(skills.skills, ["drive-the-app"]);
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A review round's risk reviewer runs under `review_risk`, taking
    /// `review`'s settings for whatever its own leaves unset; the main
    /// reviewer runs under `review`.
    #[test]
    fn the_risk_reviewer_has_its_own_key_that_falls_back_to_review() {
        use tod_store::lifecycle_config::{Agents, PhaseAgent, REVIEW_RISK};
        let (root, store, node) = store_with_node();
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::LifecycleConfig, Capability::Lifecycle],
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let focus = Focus::Node(node);
        assert_eq!(
            phase_for(&store, focus, ProtocolKind::Review, Some(Reviewer::Risk)).as_deref(),
            Some(REVIEW_RISK)
        );
        assert_eq!(phase_for(&store, focus, ProtocolKind::Review, Some(Reviewer::Main)).as_deref(), Some("review"));
        assert_eq!(phase_for(&store, focus, ProtocolKind::Review, None).as_deref(), Some("review"));
        let set = |agents: Agents| {
            store.enqueue_outline(OutlineMutation::SetNodeLifecycleAgents { node_id: node, agents }).unwrap();
            store.writer().flush().unwrap();
            store.reload_if_stale().ok();
        };
        let risk = || resolve_reviewer(&store, &settings(), focus, ProtocolKind::Review, Some(Reviewer::Risk));
        set([("review".to_string(), PhaseAgent { model: Some("sonnet".into()), effort: Some("high".into()) })].into());
        let launch = risk();
        assert_eq!((launch.model.as_str(), launch.effort.as_str()), ("sonnet", "high"));
        set([
            ("review".to_string(), PhaseAgent { model: Some("sonnet".into()), effort: Some("high".into()) }),
            (REVIEW_RISK.to_string(), PhaseAgent { model: Some("opus".into()), effort: None }),
        ]
        .into());
        let launch = risk();
        assert_eq!((launch.model.as_str(), launch.effort.as_str()), ("opus", "high"));
        assert_eq!(resolve(&store, &settings(), focus, ProtocolKind::Review).model, "sonnet");
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The guidance selector runs on haiku at low effort whatever the node's
    /// Agent capability says, unless its own lifecycle-config key sets them.
    #[test]
    fn the_guidance_selector_defaults_to_haiku_at_low_effort() {
        use tod_store::lifecycle_config::{Agents, GUIDANCE, PhaseAgent};
        let (root, store, node) = store_with_node();
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![Capability::Agent, Capability::LifecycleConfig, Capability::Lifecycle],
            })
            .unwrap();
        store
            .enqueue(FleetMutation::UpsertNodeAgent {
                node_id: node.to_string(),
                platform: None,
                model: Some("opus".into()),
                effort: Some("high".into()),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        assert_eq!(phase_for(&store, Focus::Node(node), ProtocolKind::Guidance, None).as_deref(), Some(GUIDANCE));
        let at = |kind| resolve(&store, &settings(), Focus::Node(node), kind);
        let guidance = at(ProtocolKind::Guidance);
        assert_eq!((guidance.model.as_str(), guidance.effort.as_str()), ("haiku", "low"));
        // Every other kind keeps the node's own.
        let implement = at(ProtocolKind::Implementation);
        assert_eq!((implement.model.as_str(), implement.effort.as_str()), ("opus", "high"));
        // The key's own model wins; the effort it leaves unset stays low.
        let agents: Agents =
            [(GUIDANCE.to_string(), PhaseAgent { model: Some("sonnet".into()), effort: None })].into();
        store.enqueue_outline(OutlineMutation::SetNodeLifecycleAgents { node_id: node, agents }).unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let guidance = at(ProtocolKind::Guidance);
        assert_eq!((guidance.model.as_str(), guidance.effort.as_str()), ("sonnet", "low"));
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
    }
}
