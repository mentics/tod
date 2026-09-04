use crate::model::{NovEdge, NovGraph, NovNode};

/// Sample agent-response graph inspired by the nov response profile example.
pub fn agent_response_sample() -> NovGraph {
    let mut graph = NovGraph::new();

    const CARD_W: f64 = 176.0;
    const CARD_H: f64 = 60.0;

    let auth = graph.add_node(
        NovNode::new("auth:refactored", "JWT replaces session cookies")
            .with_tags(["auth", "breaking"])
            .with_size(CARD_W, CARD_H),
    );
    let reconnect = graph.add_node(
        NovNode::new("reconnect:timing", "Handshake timing changed")
            .with_tags(["reconnect"])
            .with_size(CARD_W, CARD_H),
    );
    let tests = graph.add_node(
        NovNode::new("tests:2-fail", "reconnect timing assertion")
            .with_tags(["tests"])
            .with_size(CARD_W, CARD_H),
    );
    let deps = graph.add_node(
        NovNode::new("deps:graph", "Dependency graph unchanged")
            .with_tags(["deps", "unchanged"])
            .with_size(CARD_W, CARD_H),
    );
    let ui = graph.add_node(
        NovNode::new("ui:layer", "UI layer unchanged")
            .with_tags(["ui", "unchanged"])
            .with_size(CARD_W, CARD_H),
    );
    let refresh = graph.add_node(
        NovNode::new("token:refresh", "Token refresh now async")
            .with_tags(["auth"])
            .with_size(CARD_W, CARD_H),
    );
    let socket = graph.add_node(
        NovNode::new("socket:handshake", "Socket reconnect path")
            .with_tags(["socket", "reconnect"])
            .with_size(CARD_W, CARD_H),
    );
    let reconnect_rs = graph.add_node(
        NovNode::new("reconnect.rs", "2 failing tests in reconnect.rs")
            .with_tags(["tests", "file"])
            .with_size(CARD_W, CARD_H),
    );
    let session = graph.add_node(
        NovNode::new("session:cookie", "Session cookie removed")
            .with_tags(["auth", "removed"])
            .with_size(CARD_W, CARD_H),
    );
    let pr = graph.add_node(
        NovNode::new("pr:auth-timing", "PR breaks auth timing")
            .with_tags(["entry", "hot"])
            .with_size(CARD_W, CARD_H),
    );

    graph.add_edge(auth, reconnect, NovEdge::new("e-auth-reconnect", "affects"));
    graph.add_edge(auth, refresh, NovEdge::new("e-auth-refresh", "depends_on"));
    graph.add_edge(auth, session, NovEdge::new("e-auth-session", "replaces"));
    graph.add_edge(
        reconnect,
        socket,
        NovEdge::new("e-reconnect-socket", "affects"),
    );
    graph.add_edge(
        reconnect,
        tests,
        NovEdge::new("e-reconnect-tests", "blocks"),
    );
    graph.add_edge(tests, reconnect_rs, NovEdge::new("e-tests-file", "related"));
    graph.add_edge(pr, auth, NovEdge::new("e-pr-auth", "affects"));
    graph.add_edge(pr, tests, NovEdge::new("e-pr-tests", "related"));
    graph.add_edge(deps, ui, NovEdge::new("e-deps-ui", "related"));
    graph.add_edge(
        refresh,
        socket,
        NovEdge::new("e-refresh-socket", "depends_on"),
    );

    graph
}
