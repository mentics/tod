//! Selectors over the accessibility tree: parsing and matching, with no GPUI in
//! it so it is plain unit-testable.
//!
//! ```text
//! #<id-path-suffix>      the node whose element-id path ends with these segments
//!                        (`#runner-menu`, `#task-panel/runner-menu`)
//! <role>:<label>         a node of that role whose label (or value) contains the
//!                        text, case-insensitively (`button:Resume`); an empty
//!                        label (`button:`) is every node of the role
//! @<n>                   one node by its raw tree id, for the frame it was listed in
//! <selector> >> <selector>   the right one, inside a node the left one matched
//! ```

use std::collections::HashMap;

/// One node of the tree as the socket lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// The AccessKit node id (stable while the element keeps its id path).
    pub id: u64,
    /// Index of the parent node in the list.
    pub parent: Option<usize>,
    /// The element-id path, compacted (`view-N` and focus-handle segments dropped).
    pub path: Option<String>,
    /// The AccessKit role's name (`Button`, `TextInput`, ...).
    pub role: String,
    pub label: String,
    pub value: String,
    /// `[x, y, width, height]` in logical window coordinates.
    pub bounds: Option<[f32; 4]>,
    /// The AccessKit actions the node supports (`Click`, `Focus`, ...).
    pub actions: Vec<String>,
    pub focused: bool,
    pub disabled: bool,
}

impl Node {
    pub fn center(&self) -> Option<(f32, f32)> {
        self.bounds
            .map(|[x, y, w, h]| (x + w / 2.0, y + h / 2.0))
    }

    pub fn supports(&self, action: &str) -> bool {
        self.actions.iter().any(|a| a.eq_ignore_ascii_case(action))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// `#a/b`: path segments, matched as a suffix on segment boundaries.
    Path(String),
    /// `role:label`, role normalised (lowercase, no `-`/`_`), label lowercased.
    RoleLabel { role: String, label: String },
    /// `@n`
    Raw(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    parts: Vec<Part>,
}

impl Selector {
    pub fn parse(text: &str) -> Result<Selector, String> {
        let source = text.trim();
        if source.is_empty() {
            return Err("empty selector".into());
        }
        let mut parts = Vec::new();
        for raw in source.split(">>") {
            parts.push(parse_part(raw.trim())?);
        }
        Ok(Selector { parts })
    }
}

fn parse_part(raw: &str) -> Result<Part, String> {
    if raw.is_empty() {
        return Err("empty selector part around `>>`".into());
    }
    if let Some(path) = raw.strip_prefix('#') {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Err("`#` needs an id path".into());
        }
        return Ok(Part::Path(path.to_string()));
    }
    if let Some(n) = raw.strip_prefix('@') {
        return n
            .parse::<u64>()
            .map(Part::Raw)
            .map_err(|_| format!("bad selector `{raw}`: `@` takes a node number"));
    }
    if let Some((role, label)) = raw.split_once(':') {
        let role = normalize_role(role);
        if !role.is_empty() && role.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(Part::RoleLabel {
                role,
                label: label.trim().to_lowercase(),
            });
        }
    }
    Err(format!(
        "bad selector `{raw}` (expected `#id-path`, `role:label` or `@n`)"
    ))
}

pub fn normalize_role(role: &str) -> String {
    role.chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect::<String>()
        .to_lowercase()
}

/// Drop the segments that carry no meaning to a reader and change between runs.
pub fn compact_path(raw: &str) -> String {
    raw.split('/')
        .filter(|segment| {
            !(segment.is_empty()
                || segment.starts_with("view-")
                || *segment == "FocusHandle")
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn part_matches(node: &Node, part: &Part) -> bool {
    match part {
        Part::Path(suffix) => match &node.path {
            Some(path) => {
                path == suffix
                    || (path.len() > suffix.len()
                        && path.ends_with(suffix.as_str())
                        && path.as_bytes()[path.len() - suffix.len() - 1] == b'/')
            }
            None => false,
        },
        Part::RoleLabel { role, label } => {
            normalize_role(&node.role) == *role
                && (label.is_empty()
                    || node.label.to_lowercase().contains(label)
                    || node.value.to_lowercase().contains(label))
        }
        Part::Raw(id) => node.id == *id,
    }
}

/// `index` is inside one of `ancestors`: below it in the tree, or (when the
/// ancestor has an id path) with an id path that continues it. The second is
/// what lets `#task-panel >> button:Resume` work though the panel's own element
/// is not a node: every element under it carries its id in their path.
fn is_descendant_of(nodes: &[Node], index: usize, ancestors: &[usize]) -> bool {
    let mut at = index;
    while let Some(parent) = nodes[at].parent {
        if ancestors.contains(&parent) {
            return true;
        }
        at = parent;
    }
    match &nodes[index].path {
        Some(path) => ancestors.iter().any(|&a| {
            nodes[a].path.as_ref().is_some_and(|ancestor| {
                path.len() > ancestor.len()
                    && path.starts_with(ancestor.as_str())
                    && path.as_bytes()[ancestor.len()] == b'/'
            })
        }),
        None => false,
    }
}

/// The nodes plus, for every id-path prefix no node has, a stand-in (empty
/// role, no tree parent) so a selector can scope by an element that is not
/// itself exposed. Returns how many are real.
fn with_path_ancestors(nodes: &[Node]) -> (Vec<Node>, usize) {
    let real = nodes.len();
    let mut have: std::collections::HashSet<&str> =
        nodes.iter().filter_map(|n| n.path.as_deref()).collect();
    let mut extended = nodes.to_vec();
    for node in nodes {
        let Some(path) = &node.path else { continue };
        for (at, _) in path.match_indices('/') {
            let prefix = &path[..at];
            if have.insert(prefix) {
                extended.push(Node {
                    id: u64::MAX,
                    parent: None,
                    path: Some(prefix.to_string()),
                    role: String::new(),
                    label: String::new(),
                    value: String::new(),
                    bounds: None,
                    actions: Vec::new(),
                    focused: false,
                    disabled: false,
                });
            }
        }
    }
    (extended, real)
}

fn run_chain(extended: &[Node], selector: &Selector) -> Vec<usize> {
    let mut scope: Option<Vec<usize>> = None;
    for part in &selector.parts {
        let matched: Vec<usize> = (0..extended.len())
            .filter(|&i| part_matches(&extended[i], part))
            .filter(|&i| match &scope {
                Some(ancestors) => is_descendant_of(extended, i, ancestors),
                None => true,
            })
            .collect();
        scope = Some(matched);
    }
    scope.unwrap_or_default()
}

/// The indexes of the nodes a selector matches, in tree order.
pub fn find(nodes: &[Node], selector: &Selector) -> Vec<usize> {
    let (extended, real) = with_path_ancestors(nodes);
    run_chain(&extended, selector)
        .into_iter()
        .filter(|&i| i < real)
        .collect()
}

/// Every node inside what the selector names, which may be an element that is
/// not itself a node. `None` when it names nothing.
pub fn within(nodes: &[Node], selector: &Selector) -> Option<Vec<usize>> {
    let (extended, real) = with_path_ancestors(nodes);
    let roots = run_chain(&extended, selector);
    if roots.is_empty() {
        return None;
    }
    Some(
        (0..real)
            .filter(|&i| is_descendant_of(&extended, i, &roots))
            .collect(),
    )
}

/// For each node, a selector that finds it: the shortest `#` suffix of its path
/// that no other node shares, else the full path, else `@id`.
pub fn selectors_for(nodes: &[Node]) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let suffixes: Vec<Vec<String>> = nodes
        .iter()
        .map(|node| match &node.path {
            Some(path) => {
                let segments: Vec<&str> = path.split('/').collect();
                (0..segments.len())
                    .rev()
                    .map(|start| segments[start..].join("/"))
                    .collect()
            }
            None => Vec::new(),
        })
        .collect();
    for list in &suffixes {
        for suffix in list {
            *counts.entry(suffix.clone()).or_default() += 1;
        }
    }
    nodes
        .iter()
        .zip(&suffixes)
        .map(|(node, list)| {
            for suffix in list {
                if counts.get(suffix) == Some(&1) {
                    return format!("#{suffix}");
                }
            }
            match &node.path {
                // Not unique even in full (a duplicated path): the raw id still is.
                Some(_) | None => format!("@{}", node.id),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u64, parent: Option<usize>, path: Option<&str>, role: &str, label: &str) -> Node {
        Node {
            id,
            parent,
            path: path.map(str::to_string),
            role: role.into(),
            label: label.into(),
            value: String::new(),
            bounds: Some([10.0, 20.0, 100.0, 40.0]),
            actions: vec!["Click".into()],
            focused: false,
            disabled: false,
        }
    }

    fn sample() -> Vec<Node> {
        vec![
            node(0, None, None, "Window", "tod"),
            node(1, Some(0), Some("task-panel"), "Group", ""),
            node(2, Some(1), Some("task-panel/runner-menu"), "Button", "Resume"),
            node(3, Some(1), Some("task-panel/other"), "Button", "Pause"),
            node(4, Some(0), Some("tree/row-1"), "Button", "Resume later"),
            node(5, Some(0), Some("chat/input"), "TextInput", "Message"),
        ]
    }

    #[test]
    fn parses_the_three_forms_and_scoping() {
        assert_eq!(
            Selector::parse("#task-panel/runner-menu").unwrap().parts,
            vec![Part::Path("task-panel/runner-menu".into())]
        );
        assert_eq!(
            Selector::parse("Button:Resume").unwrap().parts,
            vec![Part::RoleLabel {
                role: "button".into(),
                label: "resume".into()
            }]
        );
        assert_eq!(Selector::parse("@12").unwrap().parts, vec![Part::Raw(12)]);
        let chain = Selector::parse("#task-panel >> button:Resume").unwrap();
        assert_eq!(chain.parts.len(), 2);
        // A label may hold spaces and colons.
        assert_eq!(
            Selector::parse("button:Run: now").unwrap().parts,
            vec![Part::RoleLabel {
                role: "button".into(),
                label: "run: now".into()
            }]
        );
        assert!(Selector::parse("").is_err());
        assert!(Selector::parse("resume").is_err());
        assert!(Selector::parse("#").is_err());
        assert!(Selector::parse("@x").is_err());
        assert!(Selector::parse("#a >> ").is_err());
    }

    #[test]
    fn role_names_ignore_case_and_separators() {
        let nodes = sample();
        let s = Selector::parse("text-input:message").unwrap();
        assert_eq!(find(&nodes, &s), vec![5]);
    }

    #[test]
    fn path_matches_on_segment_boundaries_only() {
        let nodes = sample();
        assert_eq!(find(&nodes, &Selector::parse("#runner-menu").unwrap()), vec![2]);
        assert_eq!(
            find(&nodes, &Selector::parse("#task-panel/runner-menu").unwrap()),
            vec![2]
        );
        // `menu` is the tail of a segment, not a segment.
        assert!(find(&nodes, &Selector::parse("#menu").unwrap()).is_empty());
        assert_eq!(find(&nodes, &Selector::parse("#task-panel").unwrap()), vec![1]);
    }

    #[test]
    fn label_is_a_case_insensitive_substring() {
        let nodes = sample();
        assert_eq!(find(&nodes, &Selector::parse("button:resume").unwrap()), vec![2, 4]);
        assert_eq!(find(&nodes, &Selector::parse("button:").unwrap()), vec![2, 3, 4]);
        assert!(find(&nodes, &Selector::parse("link:resume").unwrap()).is_empty());
    }

    #[test]
    fn scoping_keeps_only_descendants_of_the_left_match() {
        let nodes = sample();
        let s = Selector::parse("#task-panel >> button:Resume").unwrap();
        assert_eq!(find(&nodes, &s), vec![2]);
        let s = Selector::parse("#chat >> button:Resume").unwrap();
        assert!(find(&nodes, &s).is_empty());
        let s = Selector::parse("#task-panel >> #chat/input").unwrap();
        assert!(find(&nodes, &s).is_empty());
        // Three deep.
        let s = Selector::parse("window: >> #task-panel >> button:Pause").unwrap();
        assert_eq!(find(&nodes, &s), vec![3]);
    }

    #[test]
    fn a_selector_scopes_by_an_element_that_is_not_a_node() {
        let nodes = sample();
        // `chat` and `tree` are only path prefixes: no node has exactly that path.
        let s = Selector::parse("#chat >> textinput:message").unwrap();
        assert_eq!(find(&nodes, &s), vec![5]);
        let s = Selector::parse("#tree >> button:resume").unwrap();
        assert_eq!(find(&nodes, &s), vec![4]);
        // And a stand-in is never itself a result.
        assert!(find(&nodes, &Selector::parse("#chat").unwrap()).is_empty());
        let s = Selector::parse("#chat >> button:").unwrap();
        assert!(find(&nodes, &s).is_empty());
    }

    #[test]
    fn raw_ids_match() {
        let nodes = sample();
        assert_eq!(find(&nodes, &Selector::parse("@3").unwrap()), vec![3]);
    }

    #[test]
    fn compact_path_drops_view_and_focus_segments() {
        assert_eq!(compact_path("view-12/task-panel/FocusHandle/runner-menu"), "task-panel/runner-menu");
        assert_eq!(compact_path("a/b"), "a/b");
    }

    #[test]
    fn listed_selectors_are_the_shortest_unique_suffix_and_find_their_node() {
        let mut nodes = sample();
        nodes.push(node(6, Some(0), Some("other/runner-menu"), "Button", "X"));
        let listed = selectors_for(&nodes);
        assert_eq!(listed[1], "#task-panel");
        // `runner-menu` is now shared, so the parent segment is added.
        assert_eq!(listed[2], "#task-panel/runner-menu");
        assert_eq!(listed[6], "#other/runner-menu");
        // The window has no path.
        assert_eq!(listed[0], "@0");
        for (i, text) in listed.iter().enumerate() {
            let found = find(&nodes, &Selector::parse(text).unwrap());
            assert_eq!(found, vec![i], "{text}");
        }
    }
}
