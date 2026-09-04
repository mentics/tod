use std::collections::HashSet;

use gpui::SharedString;
use gpui_flow::{EdgeType, FlowEdge, FlowNode, FlowPoint, FlowState, HandleDef, HandlePosition};

use crate::keyboard::{Command, Depth, Dir};
use crate::model::{NovEdge, NovGraph, NovNode};

const CARD_W: f32 = 176.0;
const CARD_H: f32 = 60.0;
const SLOT_STEP: f32 = 24.0;
const VIEW_PAD: f32 = 48.0;
const PAN_PX_PER_SEC: f32 = 720.0;
const ZOOM_PER_SEC: f32 = 1.35;

/// Paint hints for the node renderer (shared entity).
#[derive(Clone, Debug, Default)]
pub struct NavVisual {
    pub cursor: Option<String>,
    pub depth: Depth,
    pub connecting: bool,
    pub editing: Option<String>,
}

pub struct GraphController {
    pub depth: Depth,
    pub cursor: Option<String>,
    pub selected: HashSet<String>,
    pub shift_anchor: Option<String>,
    pub connecting: Option<Vec<String>>,
    pub editing: Option<String>,
    pub edit_buffer: String,
    pub pan: (f32, f32),
    pub zoom_held: f32,
    next_card: u32,
    next_edge: u32,
}

impl GraphController {
    pub fn new() -> Self {
        Self {
            depth: Depth::Graph,
            cursor: None,
            selected: HashSet::new(),
            shift_anchor: None,
            connecting: None,
            editing: None,
            edit_buffer: String::new(),
            pan: (0.0, 0.0),
            zoom_held: 0.0,
            next_card: 1,
            next_edge: 1,
        }
    }

    pub fn visual(&self) -> NavVisual {
        NavVisual {
            cursor: self.cursor.clone(),
            depth: self.depth,
            connecting: self.connecting.is_some(),
            editing: self.editing.clone(),
        }
    }

    pub fn hud_line(&self) -> String {
        if self.editing.is_some() {
            return format!(
                "Editing: {}  |  Enter/Esc leave (keeps text)",
                self.edit_buffer
            );
        }
        if let Some(sources) = &self.connecting {
            return format!(
                "Connect from {} → cursor, R/Enter commit, E/Esc cancel",
                sources.join(", ")
            );
        }
        let depth = match self.depth {
            Depth::Graph => "graph",
            Depth::Node => "node",
            Depth::Edit => "edit",
        };
        let cur = self.cursor.as_deref().unwrap_or("—");
        format!(
            "{depth}  cursor:{cur}  sel:{}  |  WASD move  E/R out/in  Shift+WASD pan  Shift+E/R zoom  G fit  Q edit  F new  C connect  X del",
            self.selected.len()
        )
    }

    pub fn seed_cursor(&mut self, state: &FlowState) {
        if self.cursor.is_some() {
            return;
        }
        let prefer = state
            .nodes
            .iter()
            .find(|n| n.id.as_ref() == "pr:auth-timing")
            .or_else(|| state.nodes.first());
        if let Some(node) = prefer {
            let id = node.id.to_string();
            self.cursor = Some(id.clone());
            self.selected.clear();
            self.selected.insert(id);
            self.shift_anchor = self.cursor.clone();
        }
    }

    pub fn sync_flow_selection(&self, state: &mut FlowState) {
        for node in &mut state.nodes {
            node.selected = self.selected.contains(node.id.as_ref());
        }
    }

    pub fn release_key(&mut self, key: &str) {
        match key {
            "w" | "up" => {
                if self.pan.1 < 0.0 {
                    self.pan.1 = 0.0;
                }
            }
            "s" | "down" => {
                if self.pan.1 > 0.0 {
                    self.pan.1 = 0.0;
                }
            }
            "a" | "left" => {
                if self.pan.0 < 0.0 {
                    self.pan.0 = 0.0;
                }
            }
            "d" | "right" => {
                if self.pan.0 > 0.0 {
                    self.pan.0 = 0.0;
                }
            }
            "r" => {
                if self.zoom_held > 0.0 {
                    self.zoom_held = 0.0;
                }
            }
            "e" => {
                if self.zoom_held < 0.0 {
                    self.zoom_held = 0.0;
                }
            }
            _ => {}
        }
    }

    pub fn clear_camera_held(&mut self) {
        self.pan = (0.0, 0.0);
        self.zoom_held = 0.0;
    }

    pub fn tick_camera(&mut self, state: &mut FlowState, width: f32, height: f32, dt: f32) {
        if self.pan != (0.0, 0.0) {
            let len = (self.pan.0 * self.pan.0 + self.pan.1 * self.pan.1)
                .sqrt()
                .max(1.0);
            let nx = self.pan.0 / len;
            let ny = self.pan.1 / len;
            state.viewport.x -= nx * PAN_PX_PER_SEC * dt;
            state.viewport.y -= ny * PAN_PX_PER_SEC * dt;
        }
        if self.zoom_held != 0.0 {
            let factor = (ZOOM_PER_SEC as f64).powf((self.zoom_held * dt) as f64) as f32;
            let old = state.viewport.zoom;
            let new = (old * factor).clamp(state.min_zoom, state.max_zoom);
            if old > 0.0 && (new - old).abs() > f32::EPSILON {
                let cx = width / 2.0;
                let cy = height / 2.0;
                state.viewport.x = cx - (cx - state.viewport.x) * (new / old);
                state.viewport.y = cy - (cy - state.viewport.y) * (new / old);
                state.viewport.zoom = new;
            }
        }
    }

    /// Returns true if the keystroke was consumed (including typing while editing).
    pub fn handle(
        &mut self,
        command: Command,
        is_held: bool,
        state: &mut FlowState,
        graph: &mut NovGraph,
        width: f32,
        height: f32,
    ) -> bool {
        if matches!(
            command,
            Command::Pan(_) | Command::ZoomIn | Command::ZoomOut
        ) {
            return self.handle_camera(command);
        }

        if self.editing.is_some() {
            return self.handle_edit_command(command, state, graph);
        }

        if is_held
            && matches!(
                command,
                Command::In
                    | Command::Out
                    | Command::Edit
                    | Command::Create
                    | Command::Delete
                    | Command::Connect
                    | Command::ToggleSelect
                    | Command::FitView
                    | Command::Undo
                    | Command::Redo
            )
        {
            return true;
        }

        match command {
            Command::Move(dir) => self.move_cursor(dir, false, state, width, height),
            Command::MoveExtend(dir) => self.move_cursor(dir, true, state, width, height),
            Command::In => self.go_in(state, graph),
            Command::Out => self.go_out(state),
            Command::Edit => self.start_edit(state),
            Command::Create => self.create_node(state, graph, width, height),
            Command::Delete => self.delete_selection(state, graph),
            Command::Connect => self.connect_key(),
            Command::ToggleSelect => self.toggle_select(state),
            Command::FitView => {
                state.fit_view(64.0, width, height);
                true
            }
            Command::Undo => {
                if state.undo() {
                    self.adopt_flow(state, graph);
                }
                true
            }
            Command::Redo => {
                if state.redo() {
                    self.adopt_flow(state, graph);
                }
                true
            }
            Command::ExitEdit => {
                if self.connecting.is_some() {
                    self.connecting = None;
                    true
                } else {
                    false
                }
            }
            Command::CommitEdit => false,
            Command::Pan(_) | Command::ZoomIn | Command::ZoomOut => unreachable!(),
        }
    }

    pub fn type_char(&mut self, ch: char) {
        if self.editing.is_some() {
            self.edit_buffer.push(ch);
        }
    }

    pub fn type_backspace(&mut self) {
        if self.editing.is_some() {
            self.edit_buffer.pop();
        }
    }

    fn handle_camera(&mut self, command: Command) -> bool {
        match command {
            Command::Pan(Dir::Left) => self.pan.0 = -1.0,
            Command::Pan(Dir::Right) => self.pan.0 = 1.0,
            Command::Pan(Dir::Up) => self.pan.1 = -1.0,
            Command::Pan(Dir::Down) => self.pan.1 = 1.0,
            Command::ZoomIn => self.zoom_held = 1.0,
            Command::ZoomOut => self.zoom_held = -1.0,
            _ => return false,
        }
        true
    }

    fn handle_edit_command(
        &mut self,
        command: Command,
        state: &mut FlowState,
        graph: &mut NovGraph,
    ) -> bool {
        match command {
            Command::ExitEdit | Command::CommitEdit | Command::In => {
                self.finish_edit(state, graph);
                true
            }
            _ => false,
        }
    }

    fn go_in(&mut self, state: &mut FlowState, graph: &mut NovGraph) -> bool {
        if let Some(sources) = self.connecting.clone() {
            return self.commit_connect(&sources, state, graph);
        }
        match self.depth {
            Depth::Graph => {
                if self.cursor.is_some() {
                    self.depth = Depth::Node;
                    true
                } else {
                    true
                }
            }
            Depth::Node => self.start_edit(state),
            Depth::Edit => true,
        }
    }

    fn go_out(&mut self, state: &mut FlowState) -> bool {
        if self.connecting.is_some() {
            self.connecting = None;
            return true;
        }
        match self.depth {
            Depth::Edit => true,
            Depth::Node => {
                self.depth = Depth::Graph;
                true
            }
            Depth::Graph => {
                self.selected.clear();
                self.sync_flow_selection(state);
                true
            }
        }
    }

    fn start_edit(&mut self, state: &FlowState) -> bool {
        let Some(id) = self.cursor.clone() else {
            return true;
        };
        let Some(node) = state.get_node(&nid(&id)) else {
            return true;
        };
        self.edit_buffer = node.label.to_string();
        self.editing = Some(id);
        self.depth = Depth::Edit;
        true
    }

    fn finish_edit(&mut self, state: &mut FlowState, graph: &mut NovGraph) {
        if let Some(id) = self.editing.take() {
            let label = self.edit_buffer.clone();
            state.push_undo();
            if let Some(node) = state.get_node_mut(&nid(&id)) {
                node.label = label.clone().into();
            }
            if let Some(idx) = graph.node_index_by_id(&id) {
                graph.update_label(idx, label);
            }
        }
        self.depth = Depth::Node;
        if self.cursor.is_none() {
            self.depth = Depth::Graph;
        }
        self.edit_buffer.clear();
    }

    fn move_cursor(
        &mut self,
        dir: Dir,
        extend: bool,
        state: &mut FlowState,
        width: f32,
        height: f32,
    ) -> bool {
        let Some(from) = self.cursor.clone() else {
            self.seed_cursor(state);
            self.sync_flow_selection(state);
            return true;
        };
        let Some(next) = nearest_in_dir(&state.nodes, &from, dir) else {
            return true;
        };
        self.cursor = Some(next.clone());
        if extend {
            let anchor = self.shift_anchor.clone().unwrap_or_else(|| from.clone());
            self.shift_anchor = Some(anchor.clone());
            self.selected = nodes_in_bbox(&state.nodes, &anchor, &next);
        } else {
            self.selected.clear();
            self.selected.insert(next.clone());
            self.shift_anchor = Some(next.clone());
        }
        self.sync_flow_selection(state);
        ensure_cursor_in_view(state, &next, width, height, VIEW_PAD);
        true
    }

    fn toggle_select(&mut self, state: &mut FlowState) -> bool {
        let Some(id) = self.cursor.clone() else {
            return true;
        };
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        self.sync_flow_selection(state);
        true
    }

    fn create_node(
        &mut self,
        state: &mut FlowState,
        graph: &mut NovGraph,
        width: f32,
        height: f32,
    ) -> bool {
        state.push_undo();
        let desired = self.desired_create_point(state, width, height);
        let pos = nearest_free_slot(&state.nodes, desired.x, desired.y, CARD_W, CARD_H);
        let id = format!("card-{}", self.next_card);
        self.next_card += 1;
        let label = "new";
        graph.add_node(NovNode::new(&id, label).with_size(CARD_W as f64, CARD_H as f64));
        let node = FlowNode::new(id.clone(), pos.x, pos.y)
            .label(label)
            .node_type("nov-card")
            .size(CARD_W, CARD_H)
            .handles(vec![
                HandleDef::target(HandlePosition::Left),
                HandleDef::source(HandlePosition::Right),
            ]);
        state.nodes.push(node);
        state.rebuild_lookup();
        self.cursor = Some(id.clone());
        self.selected.clear();
        self.selected.insert(id.clone());
        self.shift_anchor = Some(id.clone());
        self.depth = Depth::Graph;
        self.connecting = None;
        self.sync_flow_selection(state);
        ensure_cursor_in_view(state, &id, width, height, VIEW_PAD);
        true
    }

    fn desired_create_point(&self, state: &FlowState, width: f32, height: f32) -> FlowPoint {
        if let Some(id) = &self.cursor {
            if let Some(node) = state.get_node(&nid(id)) {
                let (w, _h) = node_size(node);
                return FlowPoint::new(node.position.x + w + SLOT_STEP, node.position.y);
            }
        }
        let flow = state.viewport.screen_to_flow(width / 2.0, height / 2.0);
        FlowPoint::new(flow.x - CARD_W / 2.0, flow.y - CARD_H / 2.0)
    }

    fn delete_selection(&mut self, state: &mut FlowState, graph: &mut NovGraph) -> bool {
        let mut ids: Vec<String> = self.selected.iter().cloned().collect();
        if ids.is_empty() {
            if let Some(id) = &self.cursor {
                ids.push(id.clone());
            }
        }
        ids.retain(|id| {
            state
                .get_node(&nid(id))
                .map(|n| n.deletable)
                .unwrap_or(false)
        });
        if ids.is_empty() {
            return true;
        }
        state.push_undo();
        for id in &ids {
            if let Some(idx) = graph.node_index_by_id(id) {
                graph.remove_node(idx);
            }
        }
        state.edges.retain(|e| {
            !ids.iter()
                .any(|id| e.source.as_ref() == id || e.target.as_ref() == id)
        });
        state
            .nodes
            .retain(|n| !ids.iter().any(|id| n.id.as_ref() == id));
        state.rebuild_lookup();
        for id in &ids {
            self.selected.remove(id);
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|c| ids.iter().any(|id| id == c))
        {
            self.cursor = state.nodes.first().map(|n| n.id.to_string());
            if let Some(c) = &self.cursor {
                self.selected.insert(c.clone());
            }
        }
        self.depth = Depth::Graph;
        self.connecting = None;
        self.sync_flow_selection(state);
        true
    }

    fn connect_key(&mut self) -> bool {
        if self.connecting.is_some() {
            return true;
        }
        let mut sources: Vec<String> = self.selected.iter().cloned().collect();
        if sources.is_empty() {
            if let Some(id) = &self.cursor {
                sources.push(id.clone());
            }
        }
        if sources.is_empty() {
            return true;
        }
        sources.sort();
        self.connecting = Some(sources);
        true
    }

    fn commit_connect(
        &mut self,
        sources: &[String],
        state: &mut FlowState,
        graph: &mut NovGraph,
    ) -> bool {
        let Some(target) = self.cursor.clone() else {
            return true;
        };
        state.push_undo();
        for src in sources {
            if src == &target {
                continue;
            }
            if state
                .edges
                .iter()
                .any(|e| e.source.as_ref() == src && e.target.as_ref() == target)
            {
                continue;
            }
            let eid = format!("e-{}-{}-{}", src, target, self.next_edge);
            self.next_edge += 1;
            if let (Some(s), Some(t)) =
                (graph.node_index_by_id(src), graph.node_index_by_id(&target))
            {
                graph.add_edge(s, t, NovEdge::new(&eid, "related"));
            }
            state.edges.push(
                FlowEdge::new(eid, src.clone(), target.clone())
                    .label("related")
                    .edge_type(EdgeType::SmoothStep {
                        border_radius: 10.0,
                        offset: 24.0,
                    })
                    .color(0x71717a)
                    .stroke_width(2.0),
            );
        }
        self.connecting = None;
        true
    }

    fn adopt_flow(&mut self, state: &FlowState, graph: &mut NovGraph) {
        *graph = nov_from_flow(state);
        self.selected = state
            .nodes
            .iter()
            .filter(|n| n.selected)
            .map(|n| n.id.to_string())
            .collect();
        if self
            .cursor
            .as_ref()
            .is_none_or(|c| state.get_node(&nid(c)).is_none())
        {
            self.cursor = self
                .selected
                .iter()
                .next()
                .cloned()
                .or_else(|| state.nodes.first().map(|n| n.id.to_string()));
        }
        self.depth = Depth::Graph;
        self.connecting = None;
        self.editing = None;
    }
}

fn nov_from_flow(state: &FlowState) -> NovGraph {
    let mut graph = NovGraph::new();
    for node in &state.nodes {
        let (w, h) = node_size(node);
        graph.add_node(
            NovNode::new(node.id.to_string(), node.label.to_string()).with_size(w as f64, h as f64),
        );
    }
    for edge in &state.edges {
        let Some(s) = graph.node_index_by_id(edge.source.as_ref()) else {
            continue;
        };
        let Some(t) = graph.node_index_by_id(edge.target.as_ref()) else {
            continue;
        };
        let et = edge
            .label
            .as_ref()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "related".into());
        graph.add_edge(s, t, NovEdge::new(edge.id.to_string(), et));
    }
    graph
}

pub fn nearest_in_dir(nodes: &[FlowNode], from_id: &str, dir: Dir) -> Option<String> {
    let from = nodes.iter().find(|n| n.id.as_ref() == from_id)?;
    let (fx, fy) = node_center(from);
    let mut best: Option<(f32, String)> = None;
    for node in nodes {
        if node.id.as_ref() == from_id || node.hidden {
            continue;
        }
        let (x, y) = node_center(node);
        let dx = x - fx;
        let dy = y - fy;
        let (primary, lateral) = match dir {
            Dir::Right => (dx, dy.abs()),
            Dir::Left => (-dx, dy.abs()),
            Dir::Down => (dy, dx.abs()),
            Dir::Up => (-dy, dx.abs()),
        };
        if primary < 8.0 {
            continue;
        }
        let score = primary + lateral * 2.0;
        if best.as_ref().is_none_or(|(s, _)| score < *s) {
            best = Some((score, node.id.to_string()));
        }
    }
    best.map(|(_, id)| id)
}

pub fn nodes_in_bbox(nodes: &[FlowNode], a_id: &str, b_id: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let Some(a) = nodes.iter().find(|n| n.id.as_ref() == a_id) else {
        return set;
    };
    let Some(b) = nodes.iter().find(|n| n.id.as_ref() == b_id) else {
        return set;
    };
    let (ax, ay) = node_center(a);
    let (bx, by) = node_center(b);
    let min_x = ax.min(bx);
    let max_x = ax.max(bx);
    let min_y = ay.min(by);
    let max_y = ay.max(by);
    for node in nodes {
        if node.hidden {
            continue;
        }
        let (x, y) = node_center(node);
        if x >= min_x - 1.0 && x <= max_x + 1.0 && y >= min_y - 1.0 && y <= max_y + 1.0 {
            set.insert(node.id.to_string());
        }
    }
    if set.is_empty() {
        set.insert(b_id.to_string());
    }
    set
}

pub fn nearest_free_slot(nodes: &[FlowNode], x: f32, y: f32, w: f32, h: f32) -> FlowPoint {
    if !overlaps_any(nodes, x, y, w, h) {
        return FlowPoint::new(x, y);
    }
    for ring in 1..24 {
        let reach = ring as f32 * SLOT_STEP;
        for (dx, dy) in [
            (reach, 0.0),
            (-reach, 0.0),
            (0.0, reach),
            (0.0, -reach),
            (reach, reach),
            (reach, -reach),
            (-reach, reach),
            (-reach, -reach),
        ] {
            let nx = x + dx;
            let ny = y + dy;
            if !overlaps_any(nodes, nx, ny, w, h) {
                return FlowPoint::new(nx, ny);
            }
        }
    }
    FlowPoint::new(x + CARD_W + SLOT_STEP, y)
}

fn overlaps_any(nodes: &[FlowNode], x: f32, y: f32, w: f32, h: f32) -> bool {
    nodes.iter().any(|n| {
        if n.hidden {
            return false;
        }
        let (nw, nh) = node_size(n);
        rects_overlap(x, y, w, h, n.position.x, n.position.y, nw, nh)
    })
}

fn rects_overlap(ax: f32, ay: f32, aw: f32, ah: f32, bx: f32, by: f32, bw: f32, bh: f32) -> bool {
    ax < bx + bw + SLOT_STEP
        && ax + aw + SLOT_STEP > bx
        && ay < by + bh + SLOT_STEP
        && ay + ah + SLOT_STEP > by
}

fn node_size(node: &FlowNode) -> (f32, f32) {
    (
        node.measured_width.map(|p| p.as_f32()).unwrap_or(CARD_W),
        node.measured_height.map(|p| p.as_f32()).unwrap_or(CARD_H),
    )
}

fn node_center(node: &FlowNode) -> (f32, f32) {
    let (w, h) = node_size(node);
    (node.position.x + w / 2.0, node.position.y + h / 2.0)
}

fn nid(id: &str) -> SharedString {
    SharedString::from(id)
}

pub fn ensure_cursor_in_view(state: &mut FlowState, id: &str, width: f32, height: f32, pad: f32) {
    let Some(node) = state.nodes.iter().find(|n| n.id.as_ref() == id) else {
        return;
    };
    let (w, h) = node_size(node);
    let (sx, sy) = state.viewport.flow_to_screen(node.position);
    let sw = w * state.viewport.zoom;
    let sh = h * state.viewport.zoom;
    if sx < pad {
        state.viewport.x += pad - sx;
    }
    if sy < pad {
        state.viewport.y += pad - sy;
    }
    if sx + sw > width - pad {
        state.viewport.x -= (sx + sw) - (width - pad);
    }
    if sy + sh > height - pad {
        state.viewport.y -= (sy + sh) - (height - pad);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, x: f32, y: f32) -> FlowNode {
        FlowNode::new(id, x, y).size(20.0, 20.0)
    }

    #[test]
    fn spatial_right_picks_east_neighbor() {
        let nodes = vec![
            node("a", 0.0, 0.0),
            node("b", 80.0, 4.0),
            node("c", 80.0, 200.0),
        ];
        assert_eq!(
            nearest_in_dir(&nodes, "a", Dir::Right).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn bbox_covers_nodes_between_anchor_and_cursor() {
        let nodes = vec![
            node("a", 0.0, 0.0),
            node("b", 40.0, 40.0),
            node("c", 80.0, 80.0),
            node("d", 400.0, 0.0),
        ];
        let set = nodes_in_bbox(&nodes, "a", "c");
        assert!(set.contains("a") && set.contains("b") && set.contains("c"));
        assert!(!set.contains("d"));
    }
}
