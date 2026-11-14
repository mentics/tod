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
//!
//! Double quotes protect spaces, `>>` and action words: `role:"Item title"`,
//! `"textinput:Item title"`, `"#a >> button:Go now"`. Inside quotes a backslash
//! escapes a quote or a backslash. Unquoted forms work as before.

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
        // The whole selector in quotes: `"textinput:Item title"`.
        if let Some(inner) = unquote_whole(source) {
            return Selector::parse(&inner);
        }
        let mut parts = Vec::new();
        for raw in split_chain(source) {
            let raw = raw.trim();
            match unquote_whole(raw) {
                Some(inner) => parts.push(parse_part(inner.trim())?),
                None => parts.push(parse_part(raw)?),
            }
        }
        Ok(Selector { parts })
    }
}

/// `s` is exactly one double-quoted string: its contents, unescaped.
pub fn unquote_whole(s: &str) -> Option<String> {
    let rest = s.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, e @ ('"' | '\\'))) => out.push(e),
                Some((_, e)) => {
                    out.push('\\');
                    out.push(e);
                }
                None => return None,
            },
            '"' => {
                return if rest[i + 1..].is_empty() {
                    Some(out)
                } else {
                    None
                };
            }
            c => out.push(c),
        }
    }
    None
}

/// Quote `s` for use as a selector label when it needs it.
pub fn quote_if_needed(s: &str) -> String {
    if s.chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '>' | '\\'))
    {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// Split at `>>` outside double quotes.
fn split_chain(src: &str) -> Vec<&str> {
    let bytes = src.as_bytes();
    let mut parts = Vec::new();
    let (mut start, mut i, mut quoted) = (0, 0, false);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if quoted => i += 1,
            b'"' => quoted = !quoted,
            b'>' if !quoted && bytes.get(i + 1) == Some(&b'>') => {
                parts.push(&src[start..i]);
                i += 1;
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&src[start..]);
    parts
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
            let label = label.trim();
            let label = unquote_whole(label).unwrap_or_else(|| label.to_string());
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

/// A node's role is the selector's. `textinput` also names a multi-line one.
pub fn role_matches(node_role: &str, role: &str) -> bool {
    let node_role = normalize_role(node_role);
    node_role == role || (role == "textinput" && node_role == "multilinetextinput")
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
            role_matches(&node.role, role)
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

/// A selector part that names `node` by role and label if that is unique among
/// `nodes` (a long label is cut to a unique prefix), else `None`.
fn role_label_selector(nodes: &[Node], node: &Node) -> Option<String> {
    let label = one_line_label(&node.label);
    if label.is_empty() || node.role.is_empty() {
        return None;
    }
    let role = normalize_role(&node.role);
    let chars: Vec<char> = label.chars().collect();
    let mut lengths = vec![chars.len().min(48)];
    if chars.len() > 48 {
        lengths.push(chars.len());
    }
    for len in lengths {
        let shown: String = chars[..len].iter().collect::<String>().trim().to_string();
        let wanted = shown.to_lowercase();
        let count = nodes
            .iter()
            .filter(|n| {
                role_matches(&n.role, &role)
                    && (n.label.to_lowercase().contains(&wanted)
                        || n.value.to_lowercase().contains(&wanted))
            })
            .count();
        if count == 1 {
            return Some(format!("{}:{}", role, quote_if_needed(&shown)));
        }
    }
    None
}

fn one_line_label(label: &str) -> String {
    label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Ids made of a long run of digits change between runs (`input-4294967389`).
fn is_unstable(selector: &str) -> bool {
    // A bare index (`#3`, a menu item's position) moves when the items do.
    let bare = selector.trim_start_matches('#');
    if !bare.is_empty() && bare.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    let mut run = 0;
    for c in selector.chars() {
        if c.is_ascii_digit() {
            run += 1;
            if run >= 6 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// For each node, a selector that finds it: the shortest `#` suffix of its path
/// that no other node shares, else the full path, else `@id`. A node whose
/// selector would be a raw number or hold an unstable id (a long run of
/// digits) gets a unique `role:label` instead when it has a label; so does a text node.
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
            let by_path = list
                .iter()
                .find(|suffix| counts.get(*suffix) == Some(&1))
                .map(|suffix| format!("#{suffix}"))
                // Not unique even in full (a duplicated path): the raw id still is.
                .unwrap_or_else(|| format!("@{}", node.id));
            // A text node's path (`…/~#1`) says nothing: its text does.
            let anonymous = node.role == "Label" || by_path.contains("/~") || by_path.starts_with("#~");
            let spaced = by_path.contains(char::is_whitespace);
            if (by_path.starts_with('@') || is_unstable(&by_path) || anonymous || spaced)
                && let Some(by_label) = role_label_selector(nodes, node)
            {
                return by_label;
            }
            if spaced {
                return quote_if_needed(&by_path);
            }
            by_path
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
    fn quotes_protect_spaces_chains_and_escapes() {
        let label = |text: &str| match Selector::parse(text).unwrap().parts.as_slice() {
            [Part::RoleLabel { label, .. }] => label.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(label(r#"button:"Run >> now""#), "run >> now");
        assert_eq!(label(r#""button:Run now""#), "run now");
        assert_eq!(label(r#"button:"say \"hi\"""#), r#"say "hi""#);
        let chain = Selector::parse(r##""#a" >> button:"x >> y" >> "textinput:Item title""##).unwrap();
        assert_eq!(chain.parts.len(), 3);
        assert_eq!(
            Selector::parse(r##""#a >> button:Go""##).unwrap(),
            Selector::parse("#a >> button:Go").unwrap()
        );
        // Unbalanced quotes are not a quoted label.
        assert!(Selector::parse(r#""button:Run"#).is_err());
        assert_eq!(unquote_whole(r#""a" b"#), None);
        assert_eq!(quote_if_needed("plain"), "plain");
        assert_eq!(quote_if_needed("two words"), r#""two words""#);
        let round = quote_if_needed(r#"a "b" \ c"#);
        assert_eq!(unquote_whole(&round).as_deref(), Some(r#"a "b" \ c"#));
    }

    #[test]
    fn unstable_ids_get_a_role_label_selector() {
        let mut nodes = sample();
        nodes.push(node(7, Some(0), Some("input-4294967389"), "TextInput", "Item title"));
        nodes.push(node(8, Some(0), None, "Label", "Which database should the cache use?"));
        let listed = selectors_for(&nodes);
        assert_eq!(listed[6], r#"textinput:"Item title""#);
        assert_eq!(listed[7], r#"label:"Which database should the cache use?""#);
        for (i, text) in listed.iter().enumerate() {
            assert_eq!(find(&nodes, &Selector::parse(text).unwrap()), vec![i], "{text}");
        }
        // Not unique by label: the numbered path stays.
        nodes.push(node(9, Some(0), Some("input-4294967390"), "TextInput", "Item title"));
        let listed = selectors_for(&nodes);
        assert_eq!(listed[6], "#input-4294967389");
        // A bare index (a menu item's position) is listed by role:label.
        nodes.push(node(10, Some(0), Some("3"), "MenuItem", "Check now"));
        let listed = selectors_for(&nodes);
        assert_eq!(listed[9], r#"menuitem:"Check now""#);
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
        // The window has no path: its label names it.
        assert_eq!(listed[0], "window:tod");
        for (i, text) in listed.iter().enumerate() {
            let found = find(&nodes, &Selector::parse(text).unwrap());
            assert_eq!(found, vec![i], "{text}");
        }
    }
}
