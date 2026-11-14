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

/// Whitespace-separated tokens of `text` with their byte ranges; a double-quoted
/// run (`"a b"`, with `\"` inside) is part of one token, quotes kept.
fn tokens(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut quoted, mut start) = (0, false, None);
    while i < bytes.len() {
        let b = bytes[i];
        if quoted && b == b'\\' {
            i += 2;
            continue;
        }
        if b == b'"' {
            quoted = !quoted;
            start.get_or_insert(i);
        } else if b.is_ascii_whitespace() && !quoted {
            if let Some(s) = start.take() {
                out.push((s, i));
            }
        } else {
            start.get_or_insert(i);
        }
        i += 1;
    }
    if let Some(s) = start {
        out.push((s, text.len()));
    }
    out
}

/// A flag value or action argument: the text, unquoted when it is one quoted string.
fn unquoted(text: &str) -> String {
    selector::unquote_whole(text.trim()).unwrap_or_else(|| text.trim().to_string())
}

/// `tree [--role R] [--label S] [--within SEL] [--json]`. A flag's value runs to
/// the next `--flag`, so none of them needs quoting (quote one to hold a `--`).
pub fn parse_tree(rest: &str) -> Result<TreeQuery, String> {
    let mut query = TreeQuery::default();
    let ranges = tokens(rest);
    let mut tokens = ranges.iter().map(|&(a, b)| &rest[a..b]).peekable();
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
            "--role" => query.role = Some(unquoted(&value()?)),
            "--label" => query.label = Some(unquoted(&value()?)),
            // A selector keeps its quotes: `Selector::parse` reads them.
            "--within" => query.within = Some(value()?),
            other => return Err(format!("unknown tree option `{other}`")),
        }
    }
    if let Some(within) = &query.within {
        Selector::parse(within)?;
    }
    Ok(query)
}

const ACTION_WORDS: [&str; 4] = ["click", "focus", "set-value", "scroll-into-view"];

/// `act <selector> [click|focus|set-value <text>|scroll-into-view]`; the action
/// is the end of the line, the selector everything before it. When the line has
/// a double quote anywhere (`act "textinput:Item title" set-value Cov node`,
/// `act button:"Run now" click`) the selector ends at the first unquoted action
/// word instead, so a quoted label may contain one.
pub fn parse_act(rest: &str) -> Result<(String, ActAction), String> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err("act requires a selector".into());
    }
    if rest.contains('"') {
        let split = tokens(rest)
            .into_iter()
            .skip(1)
            .find(|&(a, b)| ACTION_WORDS.contains(&&rest[a..b]));
        let (selector, action) = match split {
            None => (rest, ActAction::Click),
            Some((a, b)) => {
                let after = rest[b..].trim();
                let action = match &rest[a..b] {
                    "set-value" => ActAction::SetValue(unquoted(after)),
                    word => {
                        if !after.is_empty() {
                            return Err(format!("unexpected text after `{word}`: {after}"));
                        }
                        match word {
                            "click" => ActAction::Click,
                            "focus" => ActAction::Focus,
                            _ => ActAction::ScrollIntoView,
                        }
                    }
                };
                (rest[..a].trim(), action)
            }
        };
        Selector::parse(selector)?;
        return Ok((selector.to_string(), action));
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

/// `wait-for <selector> [--gone] [--timeout-ms N]`; the selector may be quoted.
pub fn parse_wait_for(rest: &str) -> Result<(String, bool, u64), String> {
    let mut gone = false;
    let mut timeout_ms = 5000u64;
    let mut selector = Vec::new();
    let ranges = tokens(rest);
    let mut words = ranges.iter().map(|&(a, b)| &rest[a..b]);
    while let Some(token) = words.next() {
        match token {
            "--gone" => gone = true,
            "--timeout-ms" => {
                let value = words.next().ok_or("--timeout-ms needs a number")?;
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
    // A scroll wrapper around text (`#text-view-scroll-N`) has no name of its own:
    // the text is the `label` node beside it, so the wrapper is noise. Drop it and
    // hand its children to its parent.
    let dropped: Vec<bool> = order
        .iter()
        .map(|&(i, _)| is_empty_wrapper(&snapshot[i]))
        .collect();
    let mut keep_as: Vec<Option<usize>> = vec![None; order.len()];
    let mut kept = 0usize;
    for (pos, _) in order.iter().enumerate() {
        if !dropped[pos] {
            keep_as[pos] = Some(kept);
            kept += 1;
        }
    }
    let mut parent_of: Vec<Option<usize>> = order.iter().map(|&(_, p)| p).collect();
    for pos in 0..order.len() {
        let mut p = parent_of[pos];
        while let Some(q) = p {
            if dropped[q] {
                p = parent_of[q];
            } else {
                break;
            }
        }
        parent_of[pos] = p;
    }
    let order: Vec<(usize, Option<usize>)> = order
        .iter()
        .enumerate()
        .filter(|&(pos, _)| !dropped[pos])
        .map(|(pos, &(i, _))| (i, parent_of[pos].and_then(|q| keep_as[q])))
        .collect();
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
                label: label_of(s),
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

/// An empty-named, non-clickable group whose element id is a text view's scroll
/// wrapper: it only holds the text that is listed as its own `label` node.
fn is_empty_wrapper(s: &gpui::A11yNodeSnapshot) -> bool {
    s.node.role() == gpui::accesskit::Role::Group
        && s.node.label().is_none_or(str::is_empty)
        && s.node.value().is_none_or(str::is_empty)
        && !s.node.supports_action(Action::Click)
        && s.path
            .as_deref()
            .and_then(|p| p.rsplit('/').find(|seg| !seg.is_empty()))
            .is_some_and(|seg| seg.starts_with("text-view-scroll"))
}

/// The node's name. A clickable element that no text or `aria_label` names (a
/// widget from a library, such as a notification's close button) is named from
/// its element id (`close` -> "Close"), so it can still be targeted as
/// `role:label`; tod's own icon buttons are labelled where they are built.
fn label_of(s: &gpui::A11yNodeSnapshot) -> String {
    let label = s.node.label().unwrap_or("");
    if !label.trim().is_empty() {
        return label.to_string();
    }
    let clickable = s.node.supports_action(Action::Click)
        || matches!(
            s.node.role(),
            gpui::accesskit::Role::Button | gpui::accesskit::Role::DefaultButton
        );
    if !clickable {
        return label.to_string();
    }
    match s.path.as_deref() {
        Some(path) => name_from_path(path),
        None => String::new(),
    }
}

/// The name an unlabelled clickable gets from its id path, or "" when its id
/// is not word-like.
fn name_from_path(path: &str) -> String {
    let mut segs = path
        .rsplit('/')
        .filter(|seg| !seg.is_empty() && !seg.chars().all(|c| c.is_ascii_digit()));
    let Some(seg) = segs.next() else {
        return String::new();
    };
    // A split button's arrow (gpui-component's `DropdownButton`) is `popup`
    // inside the button's own id: name it after the nearest word-like id
    // above it (`unified-task-runner` -> "Unified task runner menu").
    let (seg, suffix) = if seg == "popup" {
        match segs.find_map(word_like) {
            Some(parent) => (parent, " menu"),
            None => (seg, ""),
        }
    } else {
        match word_like(seg) {
            Some(seg) => (seg, ""),
            None => return String::new(),
        }
    };
    let words = seg.replace(['-', '_'], " ");
    let mut chars = words.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str() + suffix,
        None => String::new(),
    }
}

/// Only a word-like id names a node (`close`, `nav-back`, `col-close-3`, with
/// a trailing index dropped); a generated one (`list-item-IndexPath(…)`, a
/// `~file.rs:LINE` marker) would read as noise.
fn word_like(seg: &str) -> Option<&str> {
    let seg = seg.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end_matches('-');
    (!seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphabetic() || c == '-' || c == '_'))
        .then_some(seg)
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
                .is_none_or(|r| selector::role_matches(&node.role, r))
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

/// `coverage`: `ok <clickable> clickable <exposed> exposed <explicit> explicit
/// <missing> missing`, then one line per clickable element with no node.
/// Average milliseconds per forced full draw, `n` draws.
fn time_draws(window: &mut Window, cx: &mut App, n: usize) -> f64 {
    let start = std::time::Instant::now();
    for _ in 0..n {
        window.refresh();
        let _ = window.draw(cx);
    }
    start.elapsed().as_secs_f64() * 1000.0 / n as f64
}

pub fn run_coverage(window: &mut Window, cx: &mut App, frames: usize) -> Result<String, String> {
    let timing = if frames > 0 {
        let was = release(window);
        let off = time_draws(window, cx, frames);
        window.set_a11y_forced_active(true);
        FORCED.store(true, Ordering::Relaxed);
        let on = time_draws(window, cx, frames);
        if !was {
            release(window);
        }
        Some(format!("frame-ms: tree off {off:.3}, tree on {on:.3} ({frames} draws each)"))
    } else {
        None
    };
    let _ = fresh(window, cx);
    let c = window.a11y_coverage();
    let mut reply = format!(
        "ok {} clickable {} exposed {} explicit {} missing",
        c.clickable,
        c.exposed,
        c.explicit,
        c.missing.len()
    );
    if let Some(timing) = timing {
        reply = reply.replacen("ok ", &format!("ok {timing}; "), 1);
    }
    for line in &c.missing {
        reply.push('\n');
        reply.push_str(line);
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
                // Focus it too, so the `key enter` / `text` that usually
                // follows goes to this field rather than wherever focus was.
                if window.dispatch_a11y_action(request(Action::Focus, None), cx) {
                    Ok(format!("ok set-value {name} (focused)"))
                } else {
                    Ok(format!("ok set-value {name} (not focusable: focus is unchanged)"))
                }
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
    #[test]
    fn only_word_like_ids_name_an_unlabelled_clickable() {
        assert_eq!(name_from_path("root/notification/close"), "Close");
        assert_eq!(name_from_path("unified-col-close-3"), "Unified col close");
        assert_eq!(name_from_path("panel/unified-task-runner/popup"), "Unified task runner menu");
        assert_eq!(
            name_from_path("panel/unified-task-runner/~button_group.rs:88/popup"),
            "Unified task runner menu"
        );
        assert_eq!(name_from_path("list/list-item-IndexPath(section: 0, row: 1, column: 0)"), "");
        assert_eq!(name_from_path("view/input-4294967389"), "Input");
        assert_eq!(name_from_path("menu/3"), "Menu");
    }

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

    #[test]
    fn quoted_selectors_in_act_tree_and_wait_for() {
        assert_eq!(
            parse_act(r#""textinput:Item title" set-value Cov node"#).unwrap(),
            (
                r#""textinput:Item title""#.into(),
                ActAction::SetValue("Cov node".into())
            )
        );
        assert_eq!(
            parse_act(r#"textinput:"Item title" set-value "quoted value""#).unwrap(),
            (
                r#"textinput:"Item title""#.into(),
                ActAction::SetValue("quoted value".into())
            )
        );
        // A quoted label may hold an action word.
        assert_eq!(
            parse_act(r#"button:"Click here to focus" focus"#).unwrap(),
            (r#"button:"Click here to focus""#.into(), ActAction::Focus)
        );
        assert_eq!(
            parse_act(r##""#a >> button:Go now""##).unwrap(),
            (r##""#a >> button:Go now""##.into(), ActAction::Click)
        );
        assert_eq!(
            parse_act(r#"button:"Run now" click"#).unwrap(),
            (r#"button:"Run now""#.into(), ActAction::Click)
        );
        assert!(parse_act(r#"button:"Run now" click extra"#).is_err());
        let q = parse_tree(r##"--label "two  words" --within "#a >> button:Go now" --role button"##).unwrap();
        assert_eq!(q.label.as_deref(), Some("two  words"));
        assert_eq!(q.within.as_deref(), Some(r##""#a >> button:Go now""##));
        assert_eq!(q.role.as_deref(), Some("button"));
        assert_eq!(
            parse_wait_for(r#"textinput:"Item  title" --gone --timeout-ms 9"#).unwrap(),
            (r#"textinput:"Item  title""#.into(), true, 9)
        );
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

    struct TextProbe;

    impl Render for TextProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("text-probe")
                .size_full()
                .flex()
                .flex_col()
                .child(div().id("heading").child("Needs your input").child("Asked 09:44"))
                .child(
                    // A clickable bar holding a clickable child: the bar is not
                    // named by the child's text.
                    div()
                        .id("bar")
                        .flex()
                        .on_click(|_, _, _| {})
                        .child("tod")
                        .child(div().id("talk").on_click(|_, _, _| {}).child("Talk about the selection")),
                )
                .child(
                    div()
                        .id("row")
                        .role(gpui::accesskit::Role::ListItem)
                        .child(div().id("row-title").on_click(|_, _, _| {}).child("Cov node"))
                        .child("proposed"),
                )
        }
    }

    #[gpui::test]
    fn text_is_readable_and_labels_exclude_interactive_descendants(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| TextProbe);
            Root::new(view, window, cx)
        });
        let tree = cx.update(|window, cx| run_tree(window, cx, &TreeQuery::default())).unwrap();
        // Plain text is a `label` node, findable by role:label.
        assert!(tree.contains("\tlabel\tNeeds your input\t"), "{tree}");
        assert!(tree.contains("\tlabel\tAsked 09:44\t"), "{tree}");
        assert_eq!(cx.update(|w, cx| count(w, cx, r#"label:"Asked 09""#)).unwrap(), 1);
        // Text a button names is not repeated as a node.
        assert!(!tree.contains("\tlabel\ttod\t"), "{tree}");
        assert!(!tree.contains("\tlabel\tTalk about the selection\t"), "{tree}");
        // The bar is named by its own text; `Talk about the selection` is one button.
        assert!(tree.contains("\tbutton\ttod\t"), "{tree}");
        assert_eq!(cx.update(|w, cx| count(w, cx, "button:Talk about the selection")).unwrap(), 1);
        // A list item is also named by the button inside it.
        assert!(tree.contains("\tlistitem\tCov node proposed\t"), "{tree}");
        assert_eq!(cx.update(|w, cx| count(w, cx, r#""listitem:Cov node""#)).unwrap(), 1);
        // A label found by text is placed in tree order and has bounds.
        let json = cx
            .update(|w, cx| run_tree(w, cx, &TreeQuery { label: Some("asked".into()), json: true, ..Default::default() }))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(json.lines().nth(1).unwrap()).unwrap();
        assert_eq!(value["role"], "label");
        assert!(value["bounds"].is_array());
    }

    /// Every clickable node of the last frame whose own name (before any id
    /// fallback) is empty.
    fn unnamed_clickables(window: &Window) -> Vec<String> {
        window
            .a11y_snapshot()
            .iter()
            .filter(|s| {
                s.node.supports_action(Action::Click)
                    && s.node.label().is_none_or(|l| l.trim().is_empty())
            })
            .map(|s| s.path.clone().unwrap_or_default())
            .collect()
    }

    struct IconProbe;

    impl Render for IconProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("icon-probe")
                .size_full()
                .flex()
                .child(
                    Button::new("named-icon")
                        .icon(gpui_component::IconName::Close)
                        .accessibility_label("Close the thing")
                        .on_click(|_, _, _| {}),
                )
                .child(
                    Button::new("bare-icon")
                        .icon(gpui_component::IconName::Copy)
                        .on_click(|_, _, _| {}),
                )
                .child(div().id("text-view-scroll-7").focusable().child("Some text"))
        }
    }

    #[gpui::test]
    fn a_labelled_icon_button_is_named_and_a_bare_one_is_found_and_still_targetable(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| IconProbe);
            Root::new(view, window, cx)
        });
        let tree = cx.update(|window, cx| run_tree(window, cx, &TreeQuery::default())).unwrap();
        assert_eq!(cx.update(|w, cx| count(w, cx, "button:Close the thing")).unwrap(), 1, "{tree}");
        // The raw snapshot shows the one nobody labelled: this is what the
        // source scan below prevents in tod's own views.
        let unnamed = cx.update(|window, _| unnamed_clickables(window));
        assert_eq!(unnamed.len(), 1, "{unnamed:?}");
        assert!(unnamed[0].contains("bare-icon"), "{unnamed:?}");
        // The listing still names it, from its id, so `role:label` reaches it.
        assert!(tree.contains("\tbutton\tBare icon\t"), "{tree}");
        // The scroll wrapper around text is not listed; the text is.
        assert!(!tree.contains("text-view-scroll"), "{tree}");
        assert!(tree.contains("\tlabel\tSome text\t"), "{tree}");
    }

    /// Every `Button::new(..)` in tod's own views that shows an icon must also be
    /// given a name (`.label(..)` or `.accessibility_label(..)`): an icon-only
    /// button has nothing a screen reader or the control socket can read. This
    /// scans the sources because the views cannot all be rendered in a test.
    #[test]
    fn every_icon_button_in_the_views_is_named() {
        fn skip_balanced(b: &[u8], mut i: usize) -> usize {
            let mut depth = 0i32;
            let mut in_str = false;
            while i < b.len() {
                let c = b[i];
                if in_str {
                    if c == b'\\' {
                        i += 1;
                    } else if c == b'"' {
                        in_str = false;
                    }
                } else if c == b'"' {
                    in_str = true;
                } else if c == b'(' || c == b'{' || c == b'[' {
                    depth += 1;
                } else if c == b')' || c == b'}' || c == b']' {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                i += 1;
            }
            b.len()
        }
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        let mut bad = Vec::new();
        for file in files {
            // This file's own probe has a deliberately bare icon button.
            if file.ends_with("agent_socket/a11y_tree.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            let b = text.as_bytes();
            let mut from = 0;
            while let Some(at) = text[from..].find("Button::new(") {
                let start = from + at;
                from = start + "Button::new(".len();
                let mut i = skip_balanced(b, start + "Button::new".len());
                let (mut icon, mut named) = (false, false);
                loop {
                    let mut j = i;
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j >= b.len() || b[j] != b'.' {
                        break;
                    }
                    let name_start = j + 1;
                    let mut k = name_start;
                    while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                        k += 1;
                    }
                    match &text[name_start..k] {
                        "icon" => icon = true,
                        "label" | "accessibility_label" => named = true,
                        _ => {}
                    }
                    i = if k < b.len() && b[k] == b'(' { skip_balanced(b, k) } else { k };
                }
                if icon && !named {
                    let line = text[..start].matches('\n').count() + 1;
                    bad.push(format!("{}:{line}", file.strip_prefix(&root).unwrap().display()));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "icon buttons with no `.label(..)` or `.accessibility_label(..)`:\n{}",
            bad.join("\n")
        );
    }
}
