use anyhow::{Context, Result};
use petgraph::unionfind::UnionFind;
use petgraph::visit::{EdgeRef, IntoEdgeReferences, IntoNodeIdentifiers, NodeIndexable};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

use crate::model::NovGraph;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// Layered left-to-right flow — best for agent responses and dependency narratives.
    Flow,
    /// Disconnected-component overview — clusters separated spatially.
    Terrain,
    /// Preserve existing coordinates; re-route edges only.
    Interactive,
}

#[derive(Debug, Clone)]
pub struct LaidOutNode {
    pub id: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LaidOutEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub edge_type: String,
}

#[derive(Debug, Clone)]
pub struct LayoutResult {
    pub nodes: Vec<LaidOutNode>,
    pub edges: Vec<LaidOutEdge>,
}

const LAYOUT_PADDING: f64 = 48.0;
const COMPONENT_GAP: f64 = 96.0;

pub fn layout(graph: &NovGraph, projection: Projection) -> Result<LayoutResult> {
    let input = build_elk_json(graph, projection)?;
    let output = elkrs::create_elk()
        .layout_json(&input)
        .map_err(|err| anyhow::anyhow!("ELK layout failed: {err}"))?;
    let mut result = parse_elk_output(graph, &output)?;
    normalize_origin(&mut result, LAYOUT_PADDING);
    if projection != Projection::Interactive {
        shelf_secondary_components(&mut result, graph, "pr:auth-timing");
    }
    Ok(result)
}

fn build_elk_json(graph: &NovGraph, projection: Projection) -> Result<String> {
    let inner = graph.graph();
    let mut children = Vec::new();

    for idx in inner.node_indices() {
        let node = &inner[idx];
        children.push(node_elk_child(node));
    }

    let mut edges = Vec::new();
    for edge_ref in inner.edge_references() {
        let edge = edge_ref.weight();
        let source = &inner[edge_ref.source()].id;
        let target = &inner[edge_ref.target()].id;
        edges.push(json!({
            "id": edge.id,
            "sources": [source],
            "targets": [target],
            "layoutOptions": edge_layout_options(&edge.edge_type),
        }));
    }

    let root = json!({
        "id": "root",
        "layoutOptions": projection_options(projection),
        "children": children,
        "edges": edges,
    });

    Ok(root.to_string())
}

fn node_elk_child(node: &crate::model::NovNode) -> Value {
    json!({
        "id": node.id,
        "width": node.width,
        "height": node.height,
        "labels": [{ "text": node.label }],
    })
}

fn projection_options(projection: Projection) -> Value {
    match projection {
        Projection::Flow => json!({
            "org.eclipse.elk.algorithm": "org.eclipse.elk.layered",
            "org.eclipse.elk.direction": "RIGHT",
            "org.eclipse.elk.edgeRouting": "ORTHOGONAL",
            "org.eclipse.elk.spacing.nodeNode": "56",
            "org.eclipse.elk.layered.spacing.nodeNodeBetweenLayers": "120",
            "org.eclipse.elk.layered.spacing.edgeNodeBetweenLayers": "32",
            "org.eclipse.elk.layered.spacing.edgeEdgeBetweenLayers": "16",
            "org.eclipse.elk.layered.nodePlacement.strategy": "NETWORK_SIMPLEX",
            "org.eclipse.elk.layered.nodePlacement.bk.fixedAlignment": "BALANCED",
            "org.eclipse.elk.layered.crossingMinimization.strategy": "LAYER_SWEEP",
            "org.eclipse.elk.layered.cycleBreaking.strategy": "GREEDY_MODEL_ORDER",
            "org.eclipse.elk.layered.thoroughness": "7",
            "org.eclipse.elk.padding": "[top=32,left=32,bottom=32,right=32]",
        }),
        Projection::Terrain => json!({
            "org.eclipse.elk.algorithm": "org.eclipse.elk.disco",
            "org.eclipse.elk.spacing.componentComponent": "140",
            "org.eclipse.elk.spacing.nodeNode": "64",
            "org.eclipse.elk.padding": "[top=32,left=32,bottom=32,right=32]",
        }),
        Projection::Interactive => json!({
            "org.eclipse.elk.algorithm": "org.eclipse.elk.layered",
            "org.eclipse.elk.direction": "RIGHT",
            "org.eclipse.elk.edgeRouting": "ORTHOGONAL",
            "org.eclipse.elk.layered.nodePlacement.strategy": "INTERACTIVE",
            "org.eclipse.elk.spacing.nodeNode": "56",
            "org.eclipse.elk.layered.spacing.nodeNodeBetweenLayers": "120",
        }),
    }
}

fn edge_layout_options(edge_type: &str) -> Value {
    let priority = match edge_type {
        "blocks" => 10,
        "affects" => 8,
        "depends_on" => 6,
        "replaces" => 4,
        _ => 1,
    };
    json!({
        "org.eclipse.elk.priority": priority.to_string(),
    })
}

fn parse_elk_output(graph: &NovGraph, output: &Value) -> Result<LayoutResult> {
    let mut positions: Map<String, Value> = Map::new();
    collect_node_positions(output, &mut positions);

    let inner = graph.graph();
    let mut nodes = Vec::new();
    for idx in inner.node_indices() {
        let node = &inner[idx];
        let pos = positions
            .get(&node.id)
            .with_context(|| format!("missing layout for node {}", node.id))?;
        nodes.push(LaidOutNode {
            id: node.id.clone(),
            label: node.label.clone(),
            x: pos.get("x").and_then(Value::as_f64).unwrap_or(0.0),
            y: pos.get("y").and_then(Value::as_f64).unwrap_or(0.0),
            width: pos
                .get("width")
                .and_then(Value::as_f64)
                .unwrap_or(node.width),
            height: pos
                .get("height")
                .and_then(Value::as_f64)
                .unwrap_or(node.height),
            tags: node.tags.clone(),
        });
    }

    let mut edges = Vec::new();
    for edge_ref in inner.edge_references() {
        let edge = edge_ref.weight();
        edges.push(LaidOutEdge {
            id: edge.id.clone(),
            source: inner[edge_ref.source()].id.clone(),
            target: inner[edge_ref.target()].id.clone(),
            edge_type: edge.edge_type.clone(),
        });
    }

    Ok(LayoutResult { nodes, edges })
}

fn collect_node_positions(value: &Value, out: &mut Map<String, Value>) {
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        if id != "root" && value.get("x").is_some() && value.get("y").is_some() {
            out.insert(id.to_string(), value.clone());
        }
    }

    if let Some(children) = value.get("children").and_then(Value::as_array) {
        for child in children {
            collect_node_positions(child, out);
        }
    }
}

fn normalize_origin(result: &mut LayoutResult, padding: f64) {
    if result.nodes.is_empty() {
        return;
    }
    let min_x = result
        .nodes
        .iter()
        .map(|n| n.x)
        .fold(f64::INFINITY, f64::min);
    let min_y = result
        .nodes
        .iter()
        .map(|n| n.y)
        .fold(f64::INFINITY, f64::min);
    for node in &mut result.nodes {
        node.x = node.x - min_x + padding;
        node.y = node.y - min_y + padding;
    }
}

fn shelf_secondary_components(result: &mut LayoutResult, graph: &NovGraph, entry_id: &str) {
    let components = connected_components(graph);
    if components.len() <= 1 {
        return;
    }

    let main = components
        .iter()
        .find(|ids| ids.iter().any(|id| graph.graph()[*id].id == entry_id))
        .or_else(|| components.iter().max_by_key(|c| c.len()))
        .expect("non-empty components");

    let main_ids: HashSet<_> = main
        .iter()
        .map(|idx| graph.graph()[*idx].id.as_str())
        .collect();

    let positions: HashMap<&str, (f64, f64, f64, f64)> = result
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), (n.x, n.y, n.width, n.height)))
        .collect();

    let main_bbox = bbox_for_ids(&positions, &main_ids);

    let mut translations: Vec<(String, f64, f64)> = Vec::new();
    for component in &components {
        let ids: HashSet<_> = component
            .iter()
            .map(|idx| graph.graph()[*idx].id.as_str())
            .collect();
        if ids == main_ids {
            continue;
        }

        let sec_bbox = bbox_for_ids(&positions, &ids);
        let dy = main_bbox.max_y + COMPONENT_GAP - sec_bbox.min_y;
        let main_cx = (main_bbox.min_x + main_bbox.max_x) / 2.0;
        let sec_cx = (sec_bbox.min_x + sec_bbox.max_x) / 2.0;
        let dx = main_cx - sec_cx;

        for id in ids {
            translations.push((id.to_string(), dx, dy));
        }
    }

    for (id, dx, dy) in translations {
        if let Some(node) = result.nodes.iter_mut().find(|n| n.id == id) {
            node.x += dx;
            node.y += dy;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bbox {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

fn bbox_for_ids(nodes: &HashMap<&str, (f64, f64, f64, f64)>, ids: &HashSet<&str>) -> Bbox {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for id in ids {
        let Some(&(x, y, w, h)) = nodes.get(id) else {
            continue;
        };
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }

    Bbox {
        min_x,
        min_y,
        max_x,
        max_y,
    }
}

fn connected_components(graph: &NovGraph) -> Vec<Vec<petgraph::graph::NodeIndex>> {
    let inner = graph.graph();
    let n = inner.node_count();
    if n == 0 {
        return Vec::new();
    }

    let mut uf = UnionFind::new(n);
    for edge in inner.edge_references() {
        let a = inner.to_index(edge.source());
        let b = inner.to_index(edge.target());
        uf.union(a, b);
        uf.union(b, a);
    }

    let mut buckets: HashMap<usize, Vec<_>> = HashMap::new();
    for idx in inner.node_identifiers() {
        let i = inner.to_index(idx);
        buckets.entry(uf.find(i)).or_default().push(idx);
    }

    buckets.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::agent_response_sample;

    #[test]
    fn flow_layout_avoids_heavy_overlap() {
        let graph = agent_response_sample();
        let result = layout(&graph, Projection::Flow).expect("layout");

        let mut min_gap = f64::INFINITY;
        for i in 0..result.nodes.len() {
            for j in (i + 1)..result.nodes.len() {
                let a = &result.nodes[i];
                let b = &result.nodes[j];
                let dx = (a.x - b.x).abs();
                let dy = (a.y - b.y).abs();
                if dx < a.width && dy < a.height {
                    let gap = dx.max(dy);
                    min_gap = min_gap.min(gap);
                }
            }
        }

        assert!(
            min_gap > 8.0,
            "nodes overlap too heavily (min gap {min_gap:.1})"
        );
    }

    #[test]
    fn secondary_component_sits_below_main() {
        let graph = agent_response_sample();
        let result = layout(&graph, Projection::Flow).expect("layout");

        let pr = result
            .nodes
            .iter()
            .find(|n| n.id == "pr:auth-timing")
            .unwrap();
        let deps = result.nodes.iter().find(|n| n.id == "deps:graph").unwrap();

        assert!(
            deps.y > pr.y + 40.0,
            "unchanged cluster should sit below the main narrative (pr y={}, deps y={})",
            pr.y,
            deps.y
        );
    }
}
