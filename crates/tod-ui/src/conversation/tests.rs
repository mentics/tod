//! ConversationView tests.

use super::change_set::{DisplayRow, Tab, display_rows, tab_counts};
use super::keyboard::*;
use super::*;
use crate::ui::agent_conversation::PanelStop;
use crate::views::rows::fixture::Fixture;
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use tod_agent::MockAgentProvider;
use tod_store::conversation::{NetOp, actor_for};
use tod_store::outline::{CreatePosition, OutlineMutation};

type Events = Rc<RefCell<Vec<ConversationViewEvent>>>;

fn open_view<'a>(
    fixture: &Fixture,
    focus: Focus,
    cx: &'a mut TestAppContext,
) -> (Entity<ConversationView>, Events, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        register_conversation_keyboard_bindings(cx);
    });
    let slot = Rc::new(RefCell::new(None));
    let events: Events = Rc::new(RefCell::new(Vec::new()));
    let store = fixture.store.clone();
    let (slot_in, events_in) = (slot.clone(), events.clone());
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(MockAgentProvider::new())));
        let view = cx.new(|cx| ConversationView::new(window, cx, agent, store));
        cx.subscribe(&view, move |_, _, event: &ConversationViewEvent, _| {
            events_in.borrow_mut().push(event.clone());
        })
        .detach();
        view.update(cx, |view, cx| view.open(focus, true, window, cx));
        *slot_in.borrow_mut() = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = slot.borrow_mut().take().unwrap();
    draw(cx);
    (view, events, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn create_conversation(fixture: &Fixture, focus: Focus) -> Uuid {
    let id = Uuid::new_v4();
    fixture
        .store
        .interview(
            ACTOR_USER,
            InterviewCommand::CreateConversation {
                id,
                protocol: tod_store::conversation::ProtocolKind::Outline,
                focus,
                platform: None,
                model: None,
                effort: None,
            },
        )
        .unwrap();
    id
}

/// Apply `mutation` as the conversation's agent.
fn agent_edit(fixture: &Fixture, conversation: Uuid, mutation: OutlineMutation) {
    fixture
        .store
        .interview(
            &actor_for(conversation),
            InterviewCommand::Outline {
                mutation,
                target: None,
            },
        )
        .unwrap();
}

fn flag(fixture: &Fixture, conversation: Uuid, id: Uuid) {
    fixture
        .store
        .interview(
            &actor_for(conversation),
            InterviewCommand::FlagConversationItem {
                conversation_id: conversation,
                entity: ItemEntity::Obligation,
                entity_id: id,
                reason: "Not sure".into(),
            },
        )
        .unwrap();
}

fn rename_step(fixture: &Fixture, step: usize, body: &str) -> OutlineMutation {
    OutlineMutation::UpdatePlanStepBody {
        step_id: fixture.steps[step],
        body: body.into(),
    }
}

fn reword(id: Uuid, body: &str) -> OutlineMutation {
    OutlineMutation::UpdateObligationBody {
        obligation_id: id,
        body: body.into(),
    }
}

fn changes(view: &Entity<ConversationView>, cx: &mut VisualTestContext) -> Vec<NetChange> {
    view.update(cx, |view, _| {
        view.reload();
        view.data.changes.clone()
    })
}

#[test]
fn focus_history_skips_repeats_and_pops_newest_first() {
    let node = Uuid::new_v4();
    let entry = |focus| HistoryEntry {
        focus,
        conversation: None,
    };
    let here = entry(Focus::Node(Uuid::new_v4()));
    let mut history = FocusHistory::default();
    history.push(entry(Focus::Project));
    history.push(entry(Focus::Node(node)));
    history.push(entry(Focus::Node(node)));
    assert_eq!(history.back(here), Some(entry(Focus::Node(node))));
    assert_eq!(
        history.back(entry(Focus::Node(node))),
        Some(entry(Focus::Project))
    );
    assert_eq!(history.back(entry(Focus::Project)), None);
}

#[test]
fn forward_retraces_the_trail_back_came_down_until_a_new_step() {
    let (a, b) = (Focus::Node(Uuid::new_v4()), Focus::Node(Uuid::new_v4()));
    let entry = |focus| HistoryEntry {
        focus,
        conversation: None,
    };
    let mut history = FocusHistory::default();
    history.push(entry(Focus::Project));
    history.push(entry(a));
    // Project <- a <- b, now standing on b.
    assert!(!history.can_go_forward());
    assert_eq!(history.back(entry(b)), Some(entry(a)));
    assert_eq!(history.back(entry(a)), Some(entry(Focus::Project)));
    assert!(history.can_go_forward());
    assert_eq!(history.forward(entry(Focus::Project)), Some(entry(a)));
    assert_eq!(history.forward(entry(a)), Some(entry(b)));
    assert!(!history.can_go_forward());

    // Stepping somewhere new abandons the forward trail.
    assert_eq!(history.back(entry(b)), Some(entry(a)));
    history.push(entry(a));
    assert!(!history.can_go_forward());
}

/// A child node of `parent` (or a top-level one), returning its id.
fn add_node(fixture: &Fixture, parent: Option<Uuid>, title: &str) -> Uuid {
    let node_id = Uuid::new_v4();
    let list_id = fixture.store.list_outline_lists().unwrap()[0].id;
    fixture
        .store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node_id),
            list_id,
            parent_id: parent,
            anchor_id: None,
            position: CreatePosition::Below,
            title: title.into(),
        })
        .unwrap();
    fixture.store.writer().flush().unwrap();
    node_id
}

fn crumbs(view: &Entity<ConversationView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |view, _| {
        view.data.path.iter().map(|c| c.title.clone()).collect()
    })
}

#[gpui::test]
fn the_header_path_runs_from_the_root_to_the_focus(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let auth = add_node(&fixture, Some(fixture.node_id), "Auth");
    let login = add_node(&fixture, Some(auth), "Login");

    // A node's own title is the header title; every node above it is a crumb.
    let (view, _, cx) = open_view(&fixture, Focus::Node(login), cx);
    assert_eq!(crumbs(&view, cx), vec!["Web client", "Auth"]);
    view.read_with(cx, |view, _| assert_eq!(view.data.title, "Login"));

    // An obligation lives on a node, so its path ends with that node.
    view.update(cx, |view, cx| {
        let focus = Focus::Obligation {
            node: fixture.node_id,
            id: fixture.offline_obligation,
        };
        view.show(focus, None, true, cx);
    });
    assert_eq!(crumbs(&view, cx), vec!["Web client"]);
}

#[gpui::test]
fn the_drill_down_walks_into_the_tree_and_refocuses(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let auth = add_node(&fixture, Some(fixture.node_id), "Auth");
    let login = add_node(&fixture, Some(auth), "Login");
    let (view, _, cx) = open_view(&fixture, Focus::Project, cx);

    // From the project, the drill-down starts at the top-level nodes.
    view.update(cx, |view, cx| view.open_nav_menu(cx));
    let titles = |view: &ConversationView| -> Vec<String> {
        view.nav_rows().into_iter().map(|row| row.title).collect()
    };
    view.read_with(cx, |view, _| {
        assert_eq!(titles(view), vec!["Web client"]);
    });

    // Right expands, Down moves, and Enter focuses the node two levels down.
    view.update(cx, |view, cx| {
        assert!(view.nav_expand(cx));
        assert_eq!(titles(view), vec!["Web client", "Auth"]);
        assert!(view.nav_move(1, cx));
        assert!(view.nav_expand(cx));
        assert_eq!(titles(view), vec!["Web client", "Auth", "Login"]);
        assert!(view.nav_move(1, cx));
    });
    cx.update(|window, cx| {
        view.update(cx, |view, cx| assert!(view.nav_activate(window, cx)));
    });
    view.read_with(cx, |view, _| {
        assert_eq!(view.focus(), Focus::Node(login));
        assert!(view.nav.is_none());
    });
    assert_eq!(crumbs(&view, cx), vec!["Web client", "Auth"]);

    // The drill-down is a normal navigation: Back returns to the project,
    // Forward comes here again.
    cx.dispatch_action(ConversationBack);
    view.read_with(cx, |view, _| assert_eq!(view.focus(), Focus::Project));
    cx.dispatch_action(ConversationForward);
    view.read_with(cx, |view, _| {
        assert_eq!(view.focus(), Focus::Node(login));
        assert!(!view.history.can_go_forward());
    });
}

#[gpui::test]
fn back_walks_the_focus_history_then_leaves(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let (view, events, cx) = open_view(&fixture, node, cx);
    let step = Focus::PlanStep {
        node: fixture.node_id,
        id: fixture.steps[0],
    };
    view.update_in(cx, |view, window, cx| view.open(step, true, window, cx));
    // Reopening the same focus is not a step back.
    view.update_in(cx, |view, window, cx| view.open(step, true, window, cx));
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.focus()), step);

    cx.dispatch_action(ConversationBack);
    assert_eq!(view.read_with(cx, |v, _| v.focus()), node);
    assert!(events.borrow().is_empty());

    cx.dispatch_action(ConversationBack);
    assert!(matches!(
        events.borrow().as_slice(),
        [ConversationViewEvent::Leave]
    ));
}

#[gpui::test]
fn picker_lists_only_this_focus_and_opens_the_chosen_one(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let older = create_conversation(&fixture, node);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let newer = create_conversation(&fixture, node);
    create_conversation(&fixture, Focus::Project);

    let (view, _, cx) = open_view(&fixture, node, cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.conversation_id(), Some(newer));
        assert_eq!(view.data.conversations.len(), 2);
        assert_eq!(view.picker_label(), "2 of 2");
    });

    // Input -> Picker, open it, move to the older one, open that.
    cx.dispatch_action(ConversationUp);
    cx.dispatch_action(ConversationActivate);
    assert_eq!(view.read_with(cx, |v, _| v.picker), Some(0));
    cx.dispatch_action(ConversationDown);
    cx.dispatch_action(ConversationActivate);
    draw(cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.picker, None);
        assert_eq!(view.conversation_id(), Some(older));
        assert_eq!(view.picker_label(), "1 of 2");
    });

    // The highlight stays on the picker; it reopens on the current entry,
    // and the last entry starts a new conversation.
    assert_eq!(view.read_with(cx, |v, _| v.stop), Stop::Picker);
    cx.dispatch_action(ConversationActivate);
    assert_eq!(view.read_with(cx, |v, _| v.picker), Some(1));
    cx.dispatch_action(ConversationDown);
    cx.dispatch_action(ConversationActivate);
    view.read_with(cx, |view, _| {
        assert_eq!(view.conversation_id(), None);
        assert_eq!(view.picker_label(), "New, 2 earlier");
        assert!(view.input_editing);
    });
}

#[gpui::test]
fn tabs_count_flags_and_deletions(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let conversation = create_conversation(&fixture, node);
    agent_edit(
        &fixture,
        conversation,
        reword(fixture.offline_obligation, "Works offline for a day"),
    );
    flag(&fixture, conversation, fixture.offline_obligation);
    agent_edit(
        &fixture,
        conversation,
        OutlineMutation::DeleteObligation {
            obligation_id: fixture.design_obligation,
        },
    );
    agent_edit(&fixture, conversation, rename_step(&fixture, 0, "Build it"));

    let (view, _, cx) = open_view(&fixture, node, cx);
    let changes = changes(&view, cx);
    assert_eq!(tab_counts(&changes), [3, 1, 1]);

    let all = display_rows(&changes, Tab::All);
    assert_eq!(all[0], DisplayRow::Node(fixture.node_id));
    assert_eq!(
        all.iter()
            .filter(|r| matches!(r, DisplayRow::PlanLabel(_)))
            .count(),
        1
    );
    assert_eq!(
        all.iter()
            .filter(|r| matches!(r, DisplayRow::Change(_)))
            .count(),
        3
    );
    let deleted = display_rows(&changes, Tab::Deleted);
    assert_eq!(deleted.len(), 2);

    // 2 switches to Unsure; the cursor lands on the flagged item and F clears it.
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::ChangeSet, window, cx)
    });
    cx.dispatch_action(ConversationTabUnsure);
    draw(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.cursor),
        Some((ItemEntity::Obligation, fixture.offline_obligation))
    );
    cx.dispatch_action(ConversationClearFlag);
    let changes = self::changes(&view, cx);
    assert_eq!(tab_counts(&changes), [3, 0, 1]);
}

#[gpui::test]
fn the_input_is_a_tab_stop_only_while_editing(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, _, cx) = open_view(&fixture, Focus::Project, cx);
    let tab_stop = |view: &Entity<ConversationView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, cx| {
            let panel = view.transcript.read(cx);
            panel.input().read(cx).focus_handle(cx).tab_stop
        })
    };
    let panel_stop = |view: &Entity<ConversationView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, cx| view.transcript.read(cx).highlight())
    };
    assert!(!view.read_with(cx, |v, _| v.input_editing));
    assert!(!tab_stop(&view, cx));

    // The input is the default stop; Enter starts writing.
    cx.dispatch_action(ConversationActivate);
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.input_editing));
    assert!(tab_stop(&view, cx));

    cx.dispatch_action(ConversationEscape);
    draw(cx);
    assert!(!view.read_with(cx, |v, _| v.input_editing));
    assert!(!tab_stop(&view, cx));

    // With no turns, stops run Back, Forward, Picker, then the transcript's
    // input, and stop at the ends.
    cx.dispatch_action(ConversationUp);
    cx.dispatch_action(ConversationUp);
    cx.dispatch_action(ConversationUp);
    cx.dispatch_action(ConversationUp);
    assert_eq!(view.read_with(cx, |v, _| v.stop), Stop::Back);
    cx.dispatch_action(ConversationDown);
    cx.dispatch_action(ConversationDown);
    cx.dispatch_action(ConversationDown);
    assert_eq!(view.read_with(cx, |v, _| v.stop), Stop::Transcript);
    assert_eq!(panel_stop(&view, cx), PanelStop::Input);
}

fn append_turn(
    fixture: &Fixture,
    conversation: Uuid,
    role: tod_store::conversation::TurnRole,
    body: &str,
    parts: Vec<tod_agent::ReplyPart>,
) {
    fixture
        .store
        .interview(
            ACTOR_USER,
            InterviewCommand::AppendConversationTurn {
                conversation_id: conversation,
                role,
                body: body.into(),
                parts,
            },
        )
        .unwrap();
}

#[gpui::test]
fn a_reply_shows_its_answer_with_the_work_collapsed(cx: &mut TestAppContext) {
    use crate::ui::agent_conversation::ChunkId;
    use tod_agent::ReplyPart;
    use tod_store::conversation::TurnRole;

    let fixture = Fixture::new();
    let conversation = create_conversation(&fixture, Focus::Project);
    append_turn(
        &fixture,
        conversation,
        TurnRole::User,
        "Tidy it up",
        Vec::new(),
    );
    append_turn(
        &fixture,
        conversation,
        TurnRole::Agent,
        "Done.",
        vec![
            ReplyPart::Text {
                text: "Let me look at the outline.".into(),
            },
            ReplyPart::Thought {
                text: "Two nodes overlap.".into(),
            },
            ReplyPart::Tool {
                id: "t1".into(),
                title: "tod-cli node move".into(),
                status: "completed".into(),
            },
            ReplyPart::Text {
                text: "Done.".into(),
            },
        ],
    );
    let (view, _, cx) = open_view(&fixture, Focus::Project, cx);
    let chunk = |part| ChunkId { entry: 1, part };
    let expanded = |view: &Entity<ConversationView>, id, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.transcript.read(cx).is_expanded(id))
    };

    // The messages and the answer are open; narration, thinking, and the
    // tool call are one line each.
    assert!(expanded(
        &view,
        ChunkId {
            entry: 0,
            part: None
        },
        cx
    ));
    assert!(expanded(&view, chunk(None), cx));
    assert!(!expanded(&view, chunk(Some(0)), cx));
    assert!(!expanded(&view, chunk(Some(1)), cx));
    assert!(!expanded(&view, chunk(Some(2)), cx));
    assert!(expanded(&view, chunk(Some(3)), cx));

    // Up from the input walks the chunks, bottom first; Enter opens one.
    cx.dispatch_action(ConversationUp);
    cx.dispatch_action(ConversationUp);
    assert_eq!(
        view.read_with(cx, |v, cx| v.transcript.read(cx).highlight()),
        PanelStop::Chunk(chunk(Some(2)))
    );
    cx.dispatch_action(ConversationActivate);
    draw(cx);
    assert!(expanded(&view, chunk(Some(2)), cx));

    // Collapsing the reply hides its pieces from the keyboard too.
    for _ in 0..3 {
        cx.dispatch_action(ConversationUp);
    }
    assert_eq!(
        view.read_with(cx, |v, cx| v.transcript.read(cx).highlight()),
        PanelStop::Chunk(chunk(None))
    );
    cx.dispatch_action(ConversationActivate);
    draw(cx);
    assert!(!expanded(&view, chunk(None), cx));
    cx.dispatch_action(ConversationDown);
    assert_eq!(
        view.read_with(cx, |v, cx| v.transcript.read(cx).highlight()),
        PanelStop::Input
    );
    // Above the first message is the picker, then Forward and Back.
    cx.dispatch_action(ConversationUp);
    cx.dispatch_action(ConversationUp);
    cx.dispatch_action(ConversationUp);
    assert_eq!(view.read_with(cx, |v, _| v.stop), Stop::Picker);
    cx.dispatch_action(ConversationUp);
    assert_eq!(view.read_with(cx, |v, _| v.stop), Stop::Forward);
}

#[gpui::test]
fn ctrl_i_toggles_the_context_panel_while_writing(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, _, cx) = open_view(&fixture, Focus::Project, cx);
    cx.dispatch_action(ConversationActivate);
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.input_editing));
    // The panel focuses the input on a later frame; do it now, so the keys
    // below start from inside the text field.
    let input = view.read_with(cx, |v, cx| v.transcript.read(cx).input().clone());
    cx.update(|window, cx| {
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    });
    draw(cx);
    let input_focused = |view: &Entity<ConversationView>, cx: &mut VisualTestContext| {
        view.update_in(cx, |v, window, cx| {
            let panel = v.transcript.read(cx);
            panel.input().read(cx).focus_handle(cx).is_focused(window)
        })
    };
    assert!(input_focused(&view, cx));

    cx.simulate_keystrokes("ctrl-i");
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.context.open));
    // Still writing.
    assert!(view.read_with(cx, |v, _| v.input_editing));
    assert!(input_focused(&view, cx));

    cx.simulate_keystrokes("ctrl-i");
    draw(cx);
    assert!(!view.read_with(cx, |v, _| v.context.open));

    // And from the transcript's navigation mode.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!view.read_with(cx, |v, _| v.input_editing));
    cx.simulate_keystrokes("ctrl-i");
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.context.open));
}

#[gpui::test]
fn an_expanded_change_row_shows_all_of_its_text(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let conversation = create_conversation(&fixture, node);
    agent_edit(
        &fixture,
        conversation,
        reword(
            fixture.offline_obligation,
            "Works offline for a day,\nand syncs when it is back",
        ),
    );
    let (view, _, cx) = open_view(&fixture, node, cx);
    let key = (ItemEntity::Obligation, fixture.offline_obligation);
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::ChangeSet, window, cx);
        view.set_cursor(Some(key), cx);
    });
    draw(cx);

    cx.dispatch_action(ConversationActivate);
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.expanded.contains(&key)));

    // The row's disclosure does the same.
    let host = view.read_with(cx, |v, _| v.host.clone());
    cx.update(|_, cx| host.push(ChangeAction::Expand(key), cx));
    draw(cx);
    assert!(!view.read_with(cx, |v, _| v.expanded.contains(&key)));
}

#[gpui::test]
fn ctrl_j_on_a_change_talks_about_that_item(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let conversation = create_conversation(&fixture, Focus::Project);
    agent_edit(
        &fixture,
        conversation,
        reword(fixture.offline_obligation, "Works offline for a day"),
    );
    let (view, _, cx) = open_view(&fixture, Focus::Project, cx);
    view.update_in(cx, |view, window, cx| {
        view.open(node, true, window, cx);
        view.go_back(window, cx);
        view.focus_pane(Pane::ChangeSet, window, cx);
    });
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.focus()), Focus::Project);

    cx.dispatch_action(OpenAgentChat);
    let obligation = Focus::Obligation {
        node: fixture.node_id,
        id: fixture.offline_obligation,
    };
    view.read_with(cx, |view, _| {
        assert_eq!(view.focus(), obligation);
        assert_eq!(view.conversation_id(), None);
        assert_eq!(view.pane, Pane::Transcript);
    });

    // Back returns to the project conversation it came from.
    cx.dispatch_action(ConversationBack);
    view.read_with(cx, |view, _| {
        assert_eq!(view.focus(), Focus::Project);
        assert_eq!(view.conversation_id(), Some(conversation));
    });
}

#[gpui::test]
fn reversing_an_item_someone_changed_since_asks_first(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let conversation = create_conversation(&fixture, node);
    agent_edit(
        &fixture,
        conversation,
        reword(fixture.offline_obligation, "Agent wording"),
    );
    // The user changes it afterwards, outside the conversation.
    fixture
        .store
        .interview(
            ACTOR_USER,
            InterviewCommand::Outline {
                mutation: reword(fixture.offline_obligation, "User wording"),
                target: None,
            },
        )
        .unwrap();

    let (view, _, cx) = open_view(&fixture, node, cx);
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::ChangeSet, window, cx)
    });
    cx.dispatch_action(ConversationReverse);
    draw(cx);
    view.read_with(cx, |view, _| {
        let pending = view.confirm.as_ref().expect("asks before reversing");
        let ids: Vec<Uuid> = pending.conflicts.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![fixture.offline_obligation]);
    });

    // Escape keeps the change; Enter on the confirmation reverses it.
    cx.dispatch_action(ConversationEscape);
    assert!(view.read_with(cx, |v, _| v.confirm.is_none()));
    cx.dispatch_action(ConversationReverse);
    cx.dispatch_action(ConversationActivate);
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.confirm.is_none()));
    let changes = changes(&view, cx);
    assert!(
        changes
            .iter()
            .all(|c| c.id != fixture.offline_obligation || c.op == NetOp::Reversed),
        "{changes:?}"
    );
}

#[gpui::test]
fn reverse_all_undoes_every_change(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let conversation = create_conversation(&fixture, node);
    agent_edit(&fixture, conversation, rename_step(&fixture, 0, "One"));
    agent_edit(&fixture, conversation, rename_step(&fixture, 1, "Two"));
    let added = Uuid::new_v4();
    agent_edit(
        &fixture,
        conversation,
        OutlineMutation::CreateObligation {
            obligation_id: Some(added),
            node_id: fixture.node_id,
            kind: tod_store::outline::KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: "Added then reversed".into(),
            phase: tod_store::interview::PHASE_REQUIREMENTS.into(),
        },
    );
    let (view, _, cx) = open_view(&fixture, node, cx);
    assert_eq!(changes(&view, cx).len(), 3);
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::ChangeSet, window, cx)
    });
    cx.dispatch_action(ConversationReverseAll);
    draw(cx);
    view.read_with(cx, |view, _| {
        assert!(view.confirm.is_none(), "{:?}", view.error);
        assert!(view.error.is_none(), "{:?}", view.error);
    });
    let changes = changes(&view, cx);
    assert!(changes.iter().all(|c| c.op == NetOp::Reversed));
    // The reversed addition no longer exists, but still shows its text.
    let gone = changes.iter().find(|c| c.id == added).unwrap();
    assert!(gone.current.is_none());
    assert_eq!(
        gone.before.as_ref().map(|s| s.text()),
        Some("Added then reversed")
    );
}

mod context {
    use super::super::context_panel::{
        ContextTab, PanelTarget, removed_items, target_for_change, target_for_ref,
    };
    use super::*;
    use crate::ui::pane_nav::{PaneFocusLeft, PaneFocusRight};
    use tod_store::conversation::ContextTarget;
    use tod_store::outline::CreatePosition;

    fn item(node: Uuid, tab: ContextTab, id: Uuid) -> PanelTarget {
        PanelTarget {
            node,
            tab,
            item: Some(id),
        }
    }

    fn target(view: &Entity<ConversationView>, cx: &mut VisualTestContext) -> Option<PanelTarget> {
        view.read_with(cx, |v, _| v.context.target)
    }

    /// The obligations list's selection, as a conversation focus.
    fn obligations_focus(
        view: &Entity<ConversationView>,
        cx: &mut VisualTestContext,
    ) -> Option<Focus> {
        view.read_with(cx, |v, cx| {
            v.context.obligations.read(cx).conversation_focus()
        })
    }

    fn to_change_set(view: &Entity<ConversationView>, cx: &mut VisualTestContext) {
        view.update_in(cx, |view, window, cx| {
            view.focus_pane(Pane::ChangeSet, window, cx)
        });
        draw(cx);
    }

    fn select(view: &Entity<ConversationView>, key: ChangeKey, cx: &mut VisualTestContext) {
        view.update(cx, |view, cx| view.set_cursor(Some(key), cx));
        draw(cx);
    }

    /// A second node, created by the user.
    fn add_node(fixture: &Fixture, title: &str) -> Uuid {
        let list_id = fixture.store.list_outline_lists().unwrap()[0].id;
        let id = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::Outline {
                    mutation: OutlineMutation::CreateNode {
                        node_id: Some(id),
                        list_id,
                        parent_id: None,
                        anchor_id: None,
                        position: CreatePosition::Below,
                        title: title.into(),
                    },
                    target: None,
                },
            )
            .unwrap();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::Outline {
                    mutation: OutlineMutation::EnableCapabilities {
                        node_id: id,
                        capabilities: vec![tod_store::outline::types::Capability::Spec],
                    },
                    target: None,
                },
            )
            .unwrap();
        id
    }

    /// Delete the design obligation and add its replacement in one turn.
    fn replace_design(fixture: &Fixture, conversation: Uuid) -> Uuid {
        agent_edit(
            fixture,
            conversation,
            OutlineMutation::DeleteObligation {
                obligation_id: fixture.design_obligation,
            },
        );
        let new = Uuid::new_v4();
        agent_edit(
            fixture,
            conversation,
            OutlineMutation::CreateObligation {
                obligation_id: Some(new),
                node_id: fixture.node_id,
                kind: tod_store::outline::KIND_REQUIREMENT.into(),
                after_id: None,
                before: false,
                section: None,
                body: "Sign-in is two screens".into(),
                phase: tod_store::interview::PHASE_DESIGN.into(),
            },
        );
        new
    }

    #[gpui::test]
    fn targets_come_from_the_change_or_the_store(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node = Focus::Node(fixture.node_id);
        let conversation = create_conversation(&fixture, node);
        agent_edit(&fixture, conversation, rename_step(&fixture, 0, "One"));
        agent_edit(
            &fixture,
            conversation,
            OutlineMutation::DeleteObligation {
                obligation_id: fixture.offline_obligation,
            },
        );
        let (view, _, cx) = open_view(&fixture, node, cx);
        let changes = changes(&view, cx);
        let n = fixture.node_id;
        let of = |id| changes.iter().find(|c| c.id == id).unwrap();
        assert_eq!(
            target_for_change(of(fixture.steps[0])),
            Some(item(n, ContextTab::Plan, fixture.steps[0]))
        );
        // A deleted item still targets the node it was on.
        assert_eq!(
            target_for_change(of(fixture.offline_obligation)),
            Some(item(n, ContextTab::Obligations, fixture.offline_obligation))
        );
        let (obligations, steps) = removed_items(&changes, n);
        assert_eq!(
            obligations.iter().map(|o| o.id).collect::<Vec<_>>(),
            vec![fixture.offline_obligation]
        );
        assert!(steps.is_empty());
        assert!(removed_items(&changes, Uuid::new_v4()).0.is_empty());

        // Refs to unchanged items and nodes resolve from the store.
        let unchanged = ContextTarget::Item {
            entity: ItemEntity::PlanStep,
            id: fixture.steps[1],
            label: "P-2".into(),
        };
        let node_ref = ContextTarget::Node {
            id: n,
            label: "Web client".into(),
        };
        let gone = ContextTarget::Item {
            entity: ItemEntity::Obligation,
            id: Uuid::new_v4(),
            label: "O-9".into(),
        };
        let resolved = fixture
            .store
            .read(|conn| {
                Ok((
                    target_for_ref(conn, &unchanged, &changes)?,
                    target_for_ref(conn, &node_ref, &changes)?,
                    target_for_ref(conn, &gone, &changes)?,
                ))
            })
            .unwrap();
        assert_eq!(
            resolved,
            (
                Some(item(n, ContextTab::Plan, fixture.steps[1])),
                Some(PanelTarget {
                    node: n,
                    tab: ContextTab::Obligations,
                    item: None
                }),
                None
            )
        );
    }

    #[gpui::test]
    fn the_panel_follows_the_highlight_and_keeps_it_across_close(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node = Focus::Node(fixture.node_id);
        let conversation = create_conversation(&fixture, node);
        agent_edit(
            &fixture,
            conversation,
            reword(fixture.offline_obligation, "Works offline for a day"),
        );
        agent_edit(&fixture, conversation, rename_step(&fixture, 1, "Sync"));
        let (view, _, cx) = open_view(&fixture, node, cx);
        to_change_set(&view, cx);
        let n = fixture.node_id;
        let offline = (ItemEntity::Obligation, fixture.offline_obligation);
        let step = (ItemEntity::PlanStep, fixture.steps[1]);
        select(&view, offline, cx);

        cx.simulate_keystrokes("ctrl-i");
        draw(cx);
        assert!(view.read_with(cx, |v, _| v.context.open));
        assert_eq!(
            target(&view, cx),
            Some(item(n, ContextTab::Obligations, fixture.offline_obligation))
        );
        assert_eq!(
            obligations_focus(&view, cx),
            Some(Focus::Obligation {
                node: n,
                id: fixture.offline_obligation
            })
        );

        // Moving the highlight moves the panel, tab included.
        cx.dispatch_action(ConversationDown);
        draw(cx);
        assert_eq!(view.read_with(cx, |v, _| v.cursor), Some(step));
        assert_eq!(
            target(&view, cx),
            Some(item(n, ContextTab::Plan, fixture.steps[1]))
        );
        view.read_with(cx, |v, cx| {
            assert_eq!(
                v.context.plan.read(cx).conversation_focus(),
                Some(Focus::PlanStep {
                    node: n,
                    id: fixture.steps[1]
                })
            );
        });

        // Entering the pane lands on the target, even after the list's
        // selection wandered.
        view.update_in(cx, |v, window, cx| {
            v.context.plan.update(cx, |list, cx| {
                list.highlight_item(fixture.steps[0], window, cx)
            });
            v.focus_pane(Pane::Context, window, cx);
        });
        draw(cx);
        view.read_with(cx, |v, cx| {
            assert_eq!(
                v.context.plan.read(cx).conversation_focus(),
                Some(Focus::PlanStep {
                    node: n,
                    id: fixture.steps[1]
                })
            );
        });
        view.update_in(cx, |v, window, cx| {
            v.focus_pane(Pane::ChangeSet, window, cx)
        });
        draw(cx);

        // Closed, it still tracks the highlight; reopened, it shows it.
        cx.simulate_keystrokes("ctrl-i");
        draw(cx);
        assert!(!view.read_with(cx, |v, _| v.context.open));
        cx.dispatch_action(ConversationUp);
        draw(cx);
        cx.dispatch_action(ConversationToggleContext);
        draw(cx);
        assert_eq!(
            target(&view, cx),
            Some(item(n, ContextTab::Obligations, fixture.offline_obligation))
        );
        assert_eq!(
            obligations_focus(&view, cx),
            Some(Focus::Obligation {
                node: n,
                id: fixture.offline_obligation
            })
        );
    }

    #[gpui::test]
    fn arrows_walk_links_then_panes(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node = Focus::Node(fixture.node_id);
        let conversation = create_conversation(&fixture, node);
        let new = replace_design(&fixture, conversation);
        let (view, _, cx) = open_view(&fixture, node, cx);
        let changes = changes(&view, cx);
        let deleted = changes
            .iter()
            .find(|c| c.id == fixture.design_obligation)
            .unwrap();
        assert_eq!(deleted.context.len(), 1, "{:?}", deleted.context);
        assert_eq!(deleted.context[0].phrase, "replaced by");
        to_change_set(&view, cx);
        select(
            &view,
            (ItemEntity::Obligation, fixture.design_obligation),
            cx,
        );

        // Right enters the links (not the next pane); Enter opens one.
        cx.simulate_keystrokes("right");
        view.read_with(cx, |v, _| {
            assert_eq!(v.link, Some(0));
            assert_eq!(v.pane, Pane::ChangeSet);
        });
        // Right at the last link stays put.
        cx.simulate_keystrokes("right");
        assert_eq!(view.read_with(cx, |v, _| v.link), Some(0));
        cx.simulate_keystrokes("enter");
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(v.context.open);
            assert_eq!(v.pane, Pane::ChangeSet);
            assert_eq!(
                v.context.target,
                Some(item(fixture.node_id, ContextTab::Obligations, new))
            );
        });
        assert_eq!(
            obligations_focus(&view, cx),
            Some(Focus::Obligation {
                node: fixture.node_id,
                id: new
            })
        );

        // Escape returns to the row.
        cx.simulate_keystrokes("escape");
        assert_eq!(view.read_with(cx, |v, _| v.link), None);

        // Left leaves the links, then moves to the transcript.
        cx.simulate_keystrokes("right left");
        assert_eq!(view.read_with(cx, |v, _| v.link), None);
        assert_eq!(view.read_with(cx, |v, _| v.pane), Pane::ChangeSet);
        cx.simulate_keystrokes("left");
        assert_eq!(view.read_with(cx, |v, _| v.pane), Pane::Transcript);

        // Right goes to the change set, then the (open) context pane.
        cx.simulate_keystrokes("right");
        assert_eq!(view.read_with(cx, |v, _| v.pane), Pane::ChangeSet);
        cx.dispatch_action(PaneFocusRight);
        draw(cx);
        view.update_in(cx, |v, window, cx| {
            assert_eq!(v.pane, Pane::Context);
            assert!(v.context_has_focus(window, cx));
        });
        // 2 switches the panel's tab; Escape goes back to the change set.
        cx.dispatch_action(ConversationTabUnsure);
        draw(cx);
        assert_eq!(
            view.read_with(cx, |v, _| v.context.target.map(|t| t.tab)),
            Some(ContextTab::Plan)
        );
        cx.dispatch_action(ConversationEscape);
        draw(cx);
        view.update_in(cx, |v, window, cx| {
            assert_eq!(v.pane, Pane::ChangeSet);
            assert!(!v.context_has_focus(window, cx));
        });
        cx.dispatch_action(PaneFocusRight);
        draw(cx);
        cx.dispatch_action(PaneFocusLeft);
        draw(cx);
        assert_eq!(view.read_with(cx, |v, _| v.pane), Pane::ChangeSet);

        // Closing the panel from inside it returns to the change set.
        cx.dispatch_action(PaneFocusRight);
        draw(cx);
        cx.dispatch_action(ConversationToggleContext);
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(!v.context.open);
            assert_eq!(v.pane, Pane::ChangeSet);
        });
        cx.dispatch_action(PaneFocusRight);
        assert_eq!(view.read_with(cx, |v, _| v.pane), Pane::ChangeSet);
    }

    #[gpui::test]
    fn a_node_link_opens_that_nodes_obligations(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let other = add_node(&fixture, "Server");
        let conversation = create_conversation(&fixture, Focus::Project);
        agent_edit(
            &fixture,
            conversation,
            OutlineMutation::MoveObligation {
                obligation_id: fixture.offline_obligation,
                target_node_id: other,
            },
        );
        let (view, _, cx) = open_view(&fixture, Focus::Project, cx);
        let changes = changes(&view, cx);
        let ix = changes
            .iter()
            .position(|c| c.id == fixture.offline_obligation)
            .unwrap();
        assert_eq!(changes[ix].context[0].phrase, "from");
        to_change_set(&view, cx);
        select(
            &view,
            (ItemEntity::Obligation, fixture.offline_obligation),
            cx,
        );
        // Highlighting the moved item targets its new node.
        assert_eq!(
            target(&view, cx),
            Some(item(
                other,
                ContextTab::Obligations,
                fixture.offline_obligation
            ))
        );

        // A click on the link pushes this row action.
        let host = view.read_with(cx, |v, _| v.host.clone());
        cx.update(|_, cx| host.push(ChangeAction::OpenLink { ix, link: 0 }, cx));
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(v.context.open);
            assert_eq!(v.link, Some(0));
            assert_eq!(
                v.context.target,
                Some(PanelTarget {
                    node: fixture.node_id,
                    tab: ContextTab::Obligations,
                    item: None
                })
            );
            assert_eq!(
                v.context.path.last().map(String::as_str),
                Some("Web client")
            );
        });
    }

    #[gpui::test]
    fn a_deleted_item_shows_struck_on_its_node(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node = Focus::Node(fixture.node_id);
        let conversation = create_conversation(&fixture, node);
        agent_edit(
            &fixture,
            conversation,
            OutlineMutation::DeleteObligation {
                obligation_id: fixture.design_obligation,
            },
        );
        agent_edit(
            &fixture,
            conversation,
            OutlineMutation::DeletePlanStep {
                step_id: fixture.steps[0],
            },
        );
        let (view, events, cx) = open_view(&fixture, node, cx);
        to_change_set(&view, cx);
        let key = (ItemEntity::Obligation, fixture.design_obligation);
        view.update_in(cx, |view, window, cx| {
            view.show_change_in_context(key, window, cx)
        });
        draw(cx);
        view.read_with(cx, |v, cx| {
            let obligations = v.context.obligations.read(cx);
            assert!(obligations.is_struck(fixture.design_obligation));
            assert!(!obligations.is_struck(fixture.offline_obligation));
            assert!(v.context.plan.read(cx).is_struck(fixture.steps[0]));
            // The struck row is highlighted, but is not something to talk about.
            assert_eq!(
                obligations.conversation_focus(),
                Some(Focus::Node(fixture.node_id))
            );
        });

        // G asks the shell to show the node in Tasks.
        cx.simulate_keystrokes("g");
        assert!(matches!(
            events.borrow().as_slice(),
            [ConversationViewEvent::GoToTasks { node_id, obligation_id: Some(id) }]
                if *node_id == fixture.node_id && *id == fixture.design_obligation
        ));
    }
}

#[gpui::test]
fn an_item_focus_is_titled_by_its_text(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let focus = Focus::Obligation {
        node: fixture.node_id,
        id: fixture.offline_obligation,
    };
    let (view, _, cx) = open_view(&fixture, focus, cx);
    draw(cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.data.title, "Works offline");
    });
}

#[test]
fn display_title_is_one_line_and_bounded() {
    use super::header::display_title;
    use tod_core::dynamic::FocusSelection;
    let (node, id) = (Uuid::new_v4(), Uuid::new_v4());
    let item = |text: Option<&str>| FocusSelection {
        focus: Focus::PlanStep { node, id },
        path: Vec::new(),
        node: Some(node),
        title: "plan step 1234abcd (pending)".into(),
        slug: None,
        text: text.map(str::to_string),
        sections: Vec::new(),
    };
    assert_eq!(
        display_title(&item(Some("Build\n  the   form "))),
        "Build the form"
    );
    let long = display_title(&item(Some(&"word ".repeat(100))));
    assert!(long.ends_with('…') && long.chars().count() <= 201, "{long}");
    // A deleted item has no text; the ids label is all there is.
    assert_eq!(display_title(&item(None)), "plan step 1234abcd (pending)");
    let node_focus = FocusSelection {
        focus: Focus::Node(node),
        title: "Auth".into(),
        text: Some("ignored".into()),
        ..item(None)
    };
    assert_eq!(display_title(&node_focus), "Auth");
}

#[gpui::test]
fn the_picker_offers_implementation_only_on_an_active_planned_node(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let (view, _, cx) = open_view(&fixture, node, cx);
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.data.new_kinds,
            vec![ProtocolKind::Outline, ProtocolKind::Chat]
        );
    });

    fixture
        .store
        .enqueue_outline(OutlineMutation::SetLifecycle {
            node_id: fixture.node_id,
            state: "active".into(),
        })
        .unwrap();
    fixture.store.writer().flush().unwrap();
    view.update_in(cx, |view, window, cx| view.open(node, false, window, cx));
    draw(cx);
    let implementation_ix = view.read_with(cx, |view, _| {
        assert_eq!(
            view.data.new_kinds,
            vec![
                ProtocolKind::Outline,
                ProtocolKind::Chat,
                ProtocolKind::Implementation
            ]
        );
        view.data.conversations.len() + 2
    });

    // Choosing it starts an unsaved implementation conversation, whose side
    // pane is the plan and whose input offers the starter message unsent.
    view.update_in(cx, |view, window, cx| {
        view.choose_picker_entry(implementation_ix, window, cx)
    });
    draw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.conversation_id(), None);
        assert_eq!(view.data.protocol, ProtocolKind::Implementation);
        assert_eq!(view.data.plan.len(), fixture.steps.len());
        assert!(view.data.turns.is_empty());
        let input = view.transcript.read(cx).input().read(cx).value();
        assert_eq!(input.as_ref(), "Implement the plan.");
    });

    // An outline conversation has no starter: its input stays empty.
    let outline_ix = view.read_with(cx, |view, _| view.data.conversations.len());
    view.update_in(cx, |view, window, cx| {
        view.choose_picker_entry(outline_ix, window, cx)
    });
    draw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.data.protocol, ProtocolKind::Outline);
        let input = view.transcript.read(cx).input().read(cx).value();
        assert_eq!(input.as_ref(), "");
    });
}

#[gpui::test]
fn up_and_down_move_through_the_implementation_pane(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = Focus::Node(fixture.node_id);
    let (view, _, cx) = open_view(&fixture, node, cx);
    view.update_in(cx, |view, window, cx| {
        view.open_with(node, ProtocolKind::Implementation, false, window, cx)
    });
    draw(cx);
    let steps = fixture.steps.len();
    assert!(steps >= 2, "the fixture has a plan to walk");
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::ChangeSet, window, cx)
    });

    cx.dispatch_action(ConversationDown);
    assert_eq!(view.read_with(cx, |v, _| v.side_cursor), Some(0));
    cx.dispatch_action(ConversationDown);
    assert_eq!(view.read_with(cx, |v, _| v.side_cursor), Some(1));
    for _ in 0..steps + 3 {
        cx.dispatch_action(ConversationDown);
    }
    assert_eq!(view.read_with(cx, |v, _| v.side_cursor), Some(steps - 1));
    cx.dispatch_action(ConversationUp);
    assert_eq!(view.read_with(cx, |v, _| v.side_cursor), Some(steps - 2));
    // The change set's own cursor is untouched.
    assert_eq!(view.read_with(cx, |v, _| v.cursor), None);
}
