use gpui::*;
use gpui_flow::*;

use crate::keyboard::Depth;
use crate::layout::{LaidOutEdge, LaidOutNode, LayoutResult};
use crate::nav::NavVisual;

const ACCENT_BLUE: u32 = 0x3b82f6;
const ACCENT_AMBER: u32 = 0xf59e0b;
const ACCENT_ROSE: u32 = 0xf43f5e;
const ACCENT_MUTED: u32 = 0x71717a;
const CURSOR: u32 = 0xfafafa;
const INTERIOR: u32 = 0x22d3ee;
const EDITING: u32 = 0xe8a87c;

pub fn to_flow_nodes(laid_out: &[LaidOutNode]) -> Vec<FlowNode> {
    laid_out
        .iter()
        .map(|node| {
            FlowNode::new(&node.id, node.x as f32, node.y as f32)
                .label(node.label.clone())
                .node_type("nov-card")
                .size(node.width as f32, node.height as f32)
                .handles(vec![
                    HandleDef::target(HandlePosition::Left),
                    HandleDef::source(HandlePosition::Right),
                ])
        })
        .collect()
}

pub fn to_flow_edges(laid_out: &[LaidOutEdge]) -> Vec<FlowEdge> {
    laid_out
        .iter()
        .map(|edge| {
            FlowEdge::new(&edge.id, &edge.source, &edge.target)
                .label(edge.edge_type.clone())
                .edge_type(EdgeType::SmoothStep {
                    border_radius: 10.0,
                    offset: 24.0,
                })
                .color(edge_color(&edge.edge_type))
                .stroke_width(2.0)
        })
        .collect()
}

fn edge_color(edge_type: &str) -> u32 {
    match edge_type {
        "depends_on" => ACCENT_BLUE,
        "blocks" => ACCENT_ROSE,
        "affects" => ACCENT_AMBER,
        _ => ACCENT_MUTED,
    }
}

pub fn build_flow_graph(
    layout: &LayoutResult,
    visual: Entity<NavVisual>,
    cx: &mut App,
) -> (Entity<FlowState>, Entity<FlowGraph>) {
    let nodes = to_flow_nodes(&layout.nodes);
    let edges = to_flow_edges(&layout.edges);
    let state = cx.new(|_| {
        let mut state = FlowState::new(nodes, edges);
        state.min_zoom = 0.25;
        state.max_zoom = 4.0;
        state
    });
    let flow = cx.new(|cx| {
        FlowGraph::new(state.clone(), cx)
            .bg_color(0x09090b)
            .grid_color(0x18181b)
            .bg_pattern(BackgroundPattern::Cross)
            .no_node_chrome()
            .node_renderer("nov-card", {
                let visual = visual.clone();
                move |node, window, cx| render_nov_card(node, &visual, window, cx)
            })
    });
    (state, flow)
}

fn render_nov_card(
    node: &FlowNode,
    visual: &Entity<NavVisual>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    const TEXT: u32 = 0xfafafa;
    const TEXT_MUTED: u32 = 0xa1a1aa;
    const CARD: u32 = 0x0a0a0c;

    let vis = visual.read(cx);
    let id = node.id.to_string();
    let is_cursor = vis.cursor.as_deref() == Some(id.as_str());
    let is_editing = vis.editing.as_deref() == Some(id.as_str());
    let is_target = vis.connecting && is_cursor;
    let interior = vis.depth == Depth::Node && is_cursor;

    let border = if is_editing {
        EDITING
    } else if is_target {
        ACCENT_AMBER
    } else if interior {
        INTERIOR
    } else if is_cursor {
        CURSOR
    } else if node.selected {
        ACCENT_BLUE
    } else {
        0x27272a
    };

    let tag = node.id.to_string();
    let label = if node.label.is_empty() {
        node.id.to_string()
    } else {
        node.label.to_string()
    };

    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .px_3()
        .py_2()
        .bg(gpui::rgb(CARD))
        .border_1()
        .border_color(gpui::rgb(border))
        .rounded_md()
        .child(
            div()
                .text_xs()
                .text_color(gpui::rgb(TEXT_MUTED))
                .font_weight(FontWeight::MEDIUM)
                .child(tag),
        )
        .child(
            div()
                .text_sm()
                .text_color(gpui::rgb(TEXT))
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
        .into_any_element()
}
