use petgraph::stable_graph::{NodeIndex, StableDiGraph};

#[derive(Debug, Clone)]
pub struct NovNode {
    pub id: String,
    pub label: String,
    pub tags: Vec<String>,
    pub width: f64,
    pub height: f64,
}

impl NovNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            tags: Vec::new(),
            width: 140.0,
            height: 56.0,
        }
    }

    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_size(mut self, width: f64, height: f64) -> Self {
        self.width = width;
        self.height = height;
        self
    }
}

#[derive(Debug, Clone)]
pub struct NovEdge {
    pub id: String,
    pub edge_type: String,
}

impl NovEdge {
    pub fn new(id: impl Into<String>, edge_type: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            edge_type: edge_type.into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NovGraph {
    inner: StableDiGraph<NovNode, NovEdge>,
}

impl NovGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn graph(&self) -> &StableDiGraph<NovNode, NovEdge> {
        &self.inner
    }

    pub fn graph_mut(&mut self) -> &mut StableDiGraph<NovNode, NovEdge> {
        &mut self.inner
    }

    pub fn add_node(&mut self, node: NovNode) -> NodeIndex {
        self.inner.add_node(node)
    }

    pub fn remove_node(&mut self, index: NodeIndex) -> Option<NovNode> {
        self.inner.remove_node(index)
    }

    pub fn add_edge(
        &mut self,
        source: NodeIndex,
        target: NodeIndex,
        edge: NovEdge,
    ) -> petgraph::stable_graph::EdgeIndex {
        self.inner.add_edge(source, target, edge)
    }

    pub fn remove_edge(
        &mut self,
        index: petgraph::stable_graph::EdgeIndex,
    ) -> Option<NovEdge> {
        self.inner.remove_edge(index)
    }

    pub fn update_label(&mut self, index: NodeIndex, label: impl Into<String>) -> bool {
        if let Some(node) = self.inner.node_weight_mut(index) {
            node.label = label.into();
            true
        } else {
            false
        }
    }

    pub fn node_index_by_id(&self, id: &str) -> Option<NodeIndex> {
        self.inner
            .node_indices()
            .find(|idx| self.inner[*idx].id == id)
    }
}
