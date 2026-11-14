//! Finding and acting on elements by identity: `tree`, `act`, `wait-for`.
//!
//! GPUI builds an AccessKit tree during prepaint from every element that has an
//! id and a role (`vendor/gpui-pre/README.md` says what tod added so that
//! covers every clickable, focusable and text-input element, named from its
//! text). The tree is only built while something asks for it, so the first
//! command here forces it on (`Window::set_a11y_forced_active`) and it is
//! switched off again after [`IDLE_OFF`] without one: a socket that is not
//! driving costs the UI nothing.
//!
//! `act` hands the request to the element's own AccessKit action listener
//! (`Window::dispatch_a11y_action`), the path a screen reader's request takes,
//! so it runs the element's real handler, not a pointer event at its centre.
//! Only an element with no handler for the action gets a synthetic click, and
//! the reply says so.

use crate::agent_socket::selector::{self, Node, Selector};
use gpui::accesskit::{Action, ActionData, ActionRequest, NodeId, TreeId};
use gpui::{App, Window};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// After this long with no `tree`/`act`/`wait-for`, the tree is switched off.
pub const IDLE_OFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeQuery {
    pub role: Option<String>,
    pub label: Option<String>,
    pub within: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActAction {
    Click,
    Focus,
    SetValue(String),
    ScrollIntoView,
}

/// `tree [--role R] [--label S] [--within SEL] [--json]`. A flag's value runs to
/// the next `--flag`, so none of them needs quoting.
pub fn parse_tree(rest: &str) -> Result<TreeQuery, String> {
    let mut query = TreeQuery::default();
    let mut tokens = rest.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        let mut value = || {
            let mut words = Vec::new();
            while let Some(next) = tokens.peek() {
                if next.starts_with("--") {
                    break;
                }
                words.push(tokens.next().unwrap_or_default());
            }
            if words.is_empty() {
                Err(format!("{token} needs a value"))
            } else {
                Ok(words.join(" "))
            }
        };
        match token {
            "--json" => query.json = true,
            "--role" => query.role = Some(value()?),
            "--label" => query.label = Some(value()?),
            "--within" => query.within = Some(value()?),
            other => return Err(format!("unknown tree option `{other}`")),
        }
    }
    if let Some(within) = &query.within {
        Selector::parse(within)?;
    }
    Ok(query)
}

/// `act <selector> [click|focus|set-value <text>|scroll-into-view]`; the action
/// is the end of the line, the selector everything before it.
pub fn parse_act(rest: &str) -> Result<(String, ActAction), String> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err("act requires a selector".into());
    }
    let (selector, action) = if let Some((sel, text)) = rest.split_once(" set-value ") {
        (sel, ActAction::SetValue(text.trim().to_string()))
    } else if let Some(sel) = rest.strip_suffix(" set-value") {
        (sel, ActAction::SetValue(String::new()))
    } else if let Some(sel) = rest.strip_suffix(" scroll-into-view") {
        (sel, ActAction::ScrollIntoView)
    } else if let Some(sel) = rest.strip_suffix(" focus") {
        (sel, ActAction::Focus)
    } else if let Some(sel) = rest.strip_suffix(" click") {
        (sel, ActAction::Click)
    } else {
        (rest, ActAction::Click)
    };
    Selector::parse(selector)?;
    Ok((selector.trim().to_string(), action))
}

/// `wait-for <selector> [--gone] [--timeout-ms N]`.
pub fn parse_wait_for(rest: &str) -> Result<(String, bool, u64), String> {
    let mut gone = false;
    let mut timeout_ms = 5000u64;
    let mut selector = Vec::new();
    let mut tokens = rest.split_whitespace();
    while let Some(token) = tokens.next() {
        match token {
            "--gone" => gone = true,
            "--timeout-ms" => {
                let value = tokens.next().ok_or("--timeout-ms needs a number")?;
                timeout_ms = value
                    .parse()
                    .map_err(|_| format!("invalid --timeout-ms `{value}`"))?;
            }
            other => selector.push(other),
        }
    }
    if selector.is_empty() {
        return Err("wait-for requires a selector".into());
    }
    let selector = selector.join(" ");
    Selector::parse(&selector)?;
    Ok((selector, gone, timeout_ms))
}

// ---------------------------------------------------------------------------
// The tree, as a list of `Node`s.
// ---------------------------------------------------------------------------

static FORCED: AtomicBool = AtomicBool::new(false);
static LAST_USE_MS: AtomicU64 = AtomicU64::new(0);
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn now_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Milliseconds since the last command that used the tree.
pub fn idle_for() -> Duration {
    Duration::from_millis(now_ms().saturating_sub(LAST_USE_MS.load(Ordering::Relaxed)))
}

/// Switch the tree off (the idle timer calls this). Returns whether it was on.
pub fn release(window: &mut Window) -> bool {
    let was = FORCED.swap(false, Ordering::Relaxed);
    if was {
        window.set_a11y_forced_active(false);
    }
    was
}

/// Make sure the tree is on and describes the state right now: force a fresh
/// layout and paint, then read it.
pub fn fresh(window: &mut Window, cx: &mut App) -> (Vec<Node>, Vec<(f32, f32)>) {
    LAST_USE_MS.store(now_ms(), Ordering::Relaxed);
    if !FORCED.swap(true, Ordering::Relaxed) {
        window.set_a11y_forced_active(true);
    }
    window.refresh();
    let _ = window.draw(cx);
    read(window)
}

/// The last frame's tree in reading order (parents before children), plus the
/// viewport size as the one extra entry.
fn read(window: &Window) -> (Vec<Node>, Vec<(f32, f32)>) {
    let scale = window.scale_factor().max(0.01) as f64;
    let size = window.viewport_size();
    let viewport = vec![(f32::from(size.width), f32::from(size.height))];
    let snapshot = window.a11y_snapshot();
    let by_id: HashMap<NodeId, usize> = snapshot
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id, i))
        .collect();
    // The update lists children before parents; walk down from the root so the
    // listing reads in tree order.
    let mut order: Vec<(usize, Option<usize>)> = Vec::with_capacity(snapshot.len());
    let root = by_id.get(&NodeId(0)).copied();
    let mut stack: Vec<(usize, Option<usize>)> = root.map(|r| (r, None)).into_iter().collect();
    while let Some((snap_index, parent)) = stack.pop() {
        let position = order.len();
        order.push((snap_index, parent));
        for child in snapshot[snap_index].node.children().iter().rev() {
            if let Some(&child_index) = by_id.get(child) {
                stack.push((child_index, Some(position)));
            }
        }
    }
    let nodes = order
        .into_iter()
        .map(|(snap_index, parent)| {
            let s = &snapshot[snap_index];
            let mut actions = Vec::new();
            let mut next = 0u8;
            while let Some(action) = Action::n(next) {
                next += 1;
                if s.node.supports_action(action) {
                    actions.push(format!("{action:?}"));
                }
            }
            Node {
                id: s.id.0,
                parent,
                path: s
                    .path
                    .as_deref()
                    .map(selector::compact_path)
                    .filter(|p| !p.is_empty()),
                role: format!("{:?}", s.node.role()),
                label: s.node.label().unwrap_or("").to_string(),
                value: s.node.value().unwrap_or("").to_string(),
                bounds: s.node.bounds().map(|r| {
                    [
                        (r.x0 / scale) as f32,
                        (r.y0 / scale) as f32,
                        ((r.x1 - r.x0) / scale) as f32,
                        ((r.y1 - r.y0) / scale) as f32,
                    ]
                }),
                actions,
                focused: s.focused,
                disabled: s.node.is_disabled(),
            }
        })
        .collect();
    (nodes, viewport)
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn describe(nodes: &[Node], selectors: &[String], index: usize) -> String {
    let node = &nodes[index];
    format!(
        "{} {} \"{}\"",
        selectors[index],
        node.role.to_lowercase(),
        one_line(&node.label)
    )
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

/// `tree`: header `ok <n> nodes`, then one line per node (tab separated:
/// selector, role, label, value, `x,y,w,h`, actions, flags), or one JSON object
/// per line with `--json`.
pub fn run_tree(window: &mut Window, cx: &mut App, query: &TreeQuery) -> Result<String, String> {
    let (nodes, viewport) = fresh(window, cx);
    let (vw, vh) = viewport.first().copied().unwrap_or((0.0, 0.0));
    let selectors = selector::selectors_for(&nodes);
    let mut candidates: Vec<usize> = (0..nodes.len()).collect();
    if let Some(within) = &query.within {
        candidates = selector::within(&nodes, &Selector::parse(within)?)
            .ok_or_else(|| format!("no-match {within}"))?;
    }
    let role = query.role.as_deref().map(selector::normalize_role);
    let label = query.label.as_deref().map(str::to_lowercase);
    let lines: Vec<String> = candidates
        .into_iter()
        .filter(|&i| {
            let node = &nodes[i];
            role.as_ref()
                .is_none_or(|r| selector::normalize_role(&node.role) == *r)
                && label.as_ref().is_none_or(|l| {
                    node.label.to_lowercase().contains(l.as_str())
                        || node.value.to_lowercase().contains(l.as_str())
                })
        })
        .map(|i| {
            let node = &nodes[i];
            let offscreen = node
                .bounds
                .is_some_and(|[x, y, w, h]| x + w < 0.0 || y + h < 0.0 || x > vw || y > vh);
            if query.json {
                serde_json::json!({
                    "selector": selectors[i],
                    "role": node.role.to_lowercase(),
                    "label": node.label,
                    "value": node.value,
                    "bounds": node.bounds.map(|b| b.map(|v| v.round() as i64)),
                    "actions": node.actions,
                    "focused": node.focused,
                    "disabled": node.disabled,
                    "offscreen": offscreen,
                })
                .to_string()
            } else {
                let bounds = node
                    .bounds
                    .map(|[x, y, w, h]| {
                        format!("{},{},{},{}", x.round(), y.round(), w.round(), h.round())
                    })
                    .unwrap_or_default();
                let mut flags = Vec::new();
                if node.focused {
                    flags.push("focused");
                }
                if node.disabled {
                    flags.push("disabled");
                }
                if offscreen {
                    flags.push("offscreen");
                }
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    selectors[i],
                    node.role.to_lowercase(),
                    one_line(&node.label),
                    one_line(&node.value),
                    bounds,
                    node.actions.join(","),
                    flags.join(","),
                )
            }
        })
        .collect();
    let mut reply = format!("ok {} nodes", lines.len());
    for line in lines {
        reply.push('\n');
        reply.push_str(&line);
    }
    Ok(reply)
}

/// How many nodes the selector matches in a fresh frame (for `wait-for`).
pub fn count(window: &mut Window, cx: &mut App, selector_text: &str) -> Result<usize, String> {
    let selector = Selector::parse(selector_text)?;
    let (nodes, _) = fresh(window, cx);
    Ok(selector::find(&nodes, &selector).len())
}

pub fn run_act(
    window: &mut Window,
    cx: &mut App,
    selector_text: &str,
    action: &ActAction,
) -> Result<String, String> {
    let selector = Selector::parse(selector_text)?;
    let (nodes, viewport) = fresh(window, cx);
    let found = selector::find(&nodes, &selector);
    let selectors = selector::selectors_for(&nodes);
    let index = match found.as_slice() {
        [] => return Err(format!("no-match {selector_text}")),
        [one] => *one,
        many => {
            let listed = many
                .iter()
                .take(12)
                .map(|&i| describe(&nodes, &selectors, i))
                .collect::<Vec<_>>()
                .join("; ");
            let more = if many.len() > 12 { "; …" } else { "" };
            return Err(format!(
                "ambiguous {} matches: {listed}{more}",
                many.len()
            ));
        }
    };
    let node = &nodes[index];
    let target = NodeId(node.id);
    let request = |action: Action, data: Option<ActionData>| ActionRequest {
        action,
        target_tree: TreeId::ROOT,
        target_node: target,
        data,
    };
    let name = describe(&nodes, &selectors, index);
    match action {
        ActAction::Click => {
            if window.dispatch_a11y_action(request(Action::Click, None), cx) {
                Ok(format!("ok click {name} (its click handler)"))
            } else {
                // `dispatch_a11y_action` already clicked its centre when nothing
                // handled the action.
                Ok(format!(
                    "ok click {name} (no click handler: synthetic click at its centre)"
                ))
            }
        }
        ActAction::Focus => {
            if window.dispatch_a11y_action(request(Action::Focus, None), cx) {
                Ok(format!("ok focus {name} (its focus handle)"))
            } else {
                let (x, y) = node
                    .center()
                    .ok_or_else(|| format!("{name} has no bounds to click"))?;
                synthetic_click(window, cx, x, y);
                Ok(format!(
                    "ok focus {name} (not focusable: synthetic click at {x:.0},{y:.0})"
                ))
            }
        }
        ActAction::SetValue(text) => {
            let data = Some(ActionData::Value(text.clone().into_boxed_str()));
            if window.dispatch_a11y_action(request(Action::SetValue, data), cx) {
                Ok(format!("ok set-value {name}"))
            } else {
                Err(format!("{name} does not accept set-value (not a text input)"))
            }
        }
        ActAction::ScrollIntoView => scroll_into_view(window, cx, node.id, viewport[0], &name),
    }
}

fn synthetic_click(window: &mut Window, cx: &mut App, x: f32, y: f32) {
    use gpui::{Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, point, px};
    let position = point(px(x), px(y));
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        }),
        cx,
    );
}

/// GPUI has no "scroll this element into view", so wheel the scroll container
/// under the node's nearest on-screen point until the node is inside the window.
fn scroll_into_view(
    window: &mut Window,
    cx: &mut App,
    id: u64,
    (vw, vh): (f32, f32),
    name: &str,
) -> Result<String, String> {
    use gpui::{Modifiers, MouseMoveEvent, PlatformInput, ScrollDelta, ScrollWheelEvent, TouchPhase, point, px};
    let mut last: Option<[f32; 4]> = None;
    for _ in 0..40 {
        let (nodes, _) = fresh(window, cx);
        let node = nodes
            .iter()
            .find(|n| n.id == id)
            .ok_or_else(|| format!("{name} disappeared while scrolling"))?;
        let [x, y, w, h] = node.bounds.ok_or_else(|| format!("{name} has no bounds"))?;
        let (cx_, cy) = (x + w / 2.0, y + h / 2.0);
        let inside = cy >= 0.0 && cy <= vh && cx_ >= 0.0 && cx_ <= vw;
        if inside {
            return Ok(format!("ok scroll-into-view {name} (visible at {cx_:.0},{cy:.0})"));
        }
        if last == Some([x, y, w, h]) {
            return Err(format!("{name} is off screen and nothing scrolled it"));
        }
        last = Some([x, y, w, h]);
        // Scroll by what is missing, at most a page, from a point the node's
        // container spans.
        let dy = if cy > vh { (cy - vh * 0.6).min(vh) } else { (cy - vh * 0.4).max(-vh) };
        let dx = if cx_ > vw { (cx_ - vw * 0.6).min(vw) } else if cx_ < 0.0 { (cx_ - vw * 0.4).max(-vw) } else { 0.0 };
        let position = point(px(cx_.clamp(1.0, vw - 1.0)), px(cy.clamp(1.0, vh - 1.0)));
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(-dx), px(-dy))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            }),
            cx,
        );
    }
    Err(format!("{name} is still off screen after scrolling"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tree_options() {
        let q = parse_tree("").unwrap();
        assert_eq!(q, TreeQuery::default());
        let q = parse_tree("--role button --label Run now --within #task-panel >> group: --json").unwrap();
        assert_eq!(q.role.as_deref(), Some("button"));
        assert_eq!(q.label.as_deref(), Some("Run now"));
        assert_eq!(q.within.as_deref(), Some("#task-panel >> group:"));
        assert!(q.json);
        assert!(parse_tree("--role").is_err());
        assert!(parse_tree("--bogus x").is_err());
        assert!(parse_tree("--within nonsense").is_err());
    }

    #[test]
    fn parses_act_actions_off_the_end() {
        assert_eq!(
            parse_act("button:Resume").unwrap(),
            ("button:Resume".into(), ActAction::Click)
        );
        assert_eq!(
            parse_act("button:Resume now click").unwrap(),
            ("button:Resume now".into(), ActAction::Click)
        );
        assert_eq!(
            parse_act("#chat/input focus").unwrap(),
            ("#chat/input".into(), ActAction::Focus)
        );
        assert_eq!(
            parse_act("textinput:Message set-value hello  big world").unwrap(),
            (
                "textinput:Message".into(),
                ActAction::SetValue("hello  big world".into())
            )
        );
        assert_eq!(
            parse_act("#row-9 scroll-into-view").unwrap(),
            ("#row-9".into(), ActAction::ScrollIntoView)
        );
        assert_eq!(
            parse_act("#a >> button:Go").unwrap(),
            ("#a >> button:Go".into(), ActAction::Click)
        );
        assert!(parse_act("").is_err());
        assert!(parse_act("nonsense").is_err());
    }

    #[test]
    fn parses_wait_for() {
        assert_eq!(
            parse_wait_for("button:Resume now").unwrap(),
            ("button:Resume now".into(), false, 5000)
        );
        assert_eq!(
            parse_wait_for("#runner-menu --gone --timeout-ms 250").unwrap(),
            ("#runner-menu".into(), true, 250)
        );
        assert!(parse_wait_for("--gone").is_err());
        assert!(parse_wait_for("#a --timeout-ms x").is_err());
    }

    // ---- GPUI: a rendered view, found and clicked by selector ----

    use gpui::{
        AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
        StatefulInteractiveElement as _, Styled as _, TestAppContext, Window, div, px,
    };
    use gpui_component::Root;
    use gpui_component::button::{Button, ButtonVariants as _};
    use std::cell::Cell;
    use std::rc::Rc;

    struct Probe {
        resumed: Rc<Cell<u32>>,
        paused: Rc<Cell<u32>>,
        plain: Rc<Cell<u32>>,
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let (resumed, paused, plain) = (self.resumed.clone(), self.paused.clone(), self.plain.clone());
            div()
                .id("probe")
                .size_full()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    Button::new("resume")
                        .label("Resume")
                        .primary()
                        .on_click(move |_, _, _| resumed.set(resumed.get() + 1)),
                )
                .child(
                    // A bare clickable div: no role, no label of its own.
                    div()
                        .id("pause")
                        .w(px(120.))
                        .h(px(24.))
                        .child("Pause run")
                        .on_click(move |_, _, _| paused.set(paused.get() + 1)),
                )
                .child(
                    div()
                        .id("plain-row")
                        .w(px(120.))
                        .h(px(24.))
                        .child("Resume later")
                        .on_click(move |_, _, _| plain.set(plain.get() + 1)),
                )
        }
    }

    fn probe(cx: &mut TestAppContext) -> (Rc<Cell<u32>>, Rc<Cell<u32>>, Rc<Cell<u32>>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let (resumed, paused, plain) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let (r, pa, pl) = (resumed.clone(), paused.clone(), plain.clone());
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|_| Probe { resumed: r, paused: pa, plain: pl });
            Root::new(view, window, cx)
        });
        (resumed, paused, plain, cx)
    }

    #[gpui::test]
    fn tree_lists_buttons_with_labels_and_act_clicks_by_label_and_id(cx: &mut TestAppContext) {
        let (resumed, paused, plain, cx) = probe(cx);
        let tree = cx
            .update(|window, cx| run_tree(window, cx, &TreeQuery { role: Some("button".into()), ..Default::default() }))
            .unwrap();
        // The component button and both bare clickable divs are buttons, each
        // named from what it shows.
        assert!(tree.starts_with("ok 3 nodes"), "{tree}");
        assert!(tree.contains("\tbutton\tResume\t"), "{tree}");
        assert!(tree.contains("\tbutton\tPause run\t"), "{tree}");
        assert!(tree.contains("\tbutton\tResume later\t"), "{tree}");

        // By role and label: "Pause" is unique.
        let reply = cx
            .update(|window, cx| run_act(window, cx, "button:Pause", &ActAction::Click))
            .unwrap();
        assert!(reply.contains("its click handler"), "{reply}");
        assert_eq!(paused.get(), 1);

        // By id path suffix.
        let reply = cx
            .update(|window, cx| run_act(window, cx, "#plain-row", &ActAction::Click))
            .unwrap();
        assert!(reply.starts_with("ok click"), "{reply}");
        assert_eq!(plain.get(), 1);
        let reply = cx
            .update(|window, cx| run_act(window, cx, "#probe >> #resume", &ActAction::Click))
            .unwrap();
        assert!(reply.starts_with("ok click"), "{reply}");
        assert_eq!(resumed.get(), 1);

        // "resume" names the component button and the "Resume later" row.
        let err = cx
            .update(|window, cx| run_act(window, cx, "button:resume", &ActAction::Click))
            .unwrap_err();
        assert!(err.starts_with("ambiguous 2 matches:"), "{err}");
        assert!(err.contains("Resume later"), "{err}");
        assert_eq!((resumed.get(), plain.get()), (1, 1), "an ambiguous act clicks nothing");

        let err = cx
            .update(|window, cx| run_act(window, cx, "button:nothing here", &ActAction::Click))
            .unwrap_err();
        assert_eq!(err, "no-match button:nothing here");
    }

    #[gpui::test]
    fn count_follows_the_selector_and_within_scopes_the_tree(cx: &mut TestAppContext) {
        let (_, _, _, cx) = probe(cx);
        assert_eq!(cx.update(|window, cx| count(window, cx, "button:Pause")).unwrap(), 1);
        assert_eq!(cx.update(|window, cx| count(window, cx, "button:Gone")).unwrap(), 0);
        let scoped = cx
            .update(|window, cx| {
                run_tree(window, cx, &TreeQuery { within: Some("#probe".into()), role: Some("button".into()), ..Default::default() })
            })
            .unwrap();
        assert!(scoped.starts_with("ok 3 nodes"), "{scoped}");
        let json = cx
            .update(|window, cx| run_tree(window, cx, &TreeQuery { label: Some("pause".into()), json: true, ..Default::default() }))
            .unwrap();
        let line = json.lines().nth(1).unwrap();
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["role"], "button");
        assert_eq!(value["label"], "Pause run");
        assert!(value["actions"].as_array().unwrap().iter().any(|a| a == "Click"));
    }
}
