# vendor/

## gpui-pre (gpui-pre 0.3.5, patched)

A copy of the crates.io `gpui-pre` 0.3.5, substituted through
`[patch.crates-io]` in the workspace `Cargo.toml`. About 3 MB: `src/`,
`build.rs`, `resources/`, `LICENSE-APACHE`, and the crate's own `README.md`
(`include_str!`'d by the crate). Its `Cargo.toml` is upstream's with the
`[[example]]` and `[[test]]` tables removed (those files are not vendored) and
one new feature added.

### Why

The dev-only agent control socket (`crates/tod-ui/src/agent_socket/`) finds and
acts on UI elements by identity through GPUI's AccessKit tree
(`tree`, `act`, `wait-for`, `coverage`). Upstream gives no public way to do that
in-process:

- the tree is only built while a platform screen-reader adapter has marked it
  active (`A11y::is_active`), and nothing outside the crate can do that;
- `Window::handle_a11y_action` is `pub(crate)`, so nothing outside can dispatch
  an action through the tree;
- most clickable elements get no role or label, and elements with no id get no
  node at all.

### Everything is behind the `tod-a11y` cargo feature

`tod-ui`'s `agent-socket` feature turns on `gpui/tod-a11y`. Without it
(`--no-default-features`, the release build) every change is compiled out
(`const TOD_A11Y = cfg!(feature = "tod-a11y")`, or `#[cfg]`) and gpui behaves as
released. With it on but the socket not driving, the tree stays off: the added
code paths run only inside `if window.a11y.is_active()`.

### What changed

`src/window/a11y.rs`
- `forced_active` flag, honoured by `A11y::is_active`, and `set_forced_active`.
- Per-frame bookkeeping: `node_paths` (the element-id path of each node, for
  `#id` selectors), `coverage` (`A11yCoverage`: clickable, exposed, explicit,
  missing), `synthetic_node` (a stable node id and path for a clickable element
  with no element id: its ancestors' ids plus where it was built), `texts` (text drawn this frame, resolved at its end)
  and `note_text`,
  `A11yNodeSnapshot` and `last_tree()`.

`src/window/a11y/debug.rs`
- `last_tree_update()`: the most recent tree update, readable outside debug builds.

`src/window.rs`
- `Window::set_a11y_forced_active(bool)`, `a11y_snapshot()`, `a11y_coverage()`,
  and `dispatch_a11y_action(request, cx) -> bool` (public wrapper over
  `handle_a11y_action`).

`src/element.rs`
- `Element::a11y_click_info()` (default `None`), and the node-pushing code in
  `Drawable::prepaint`: counts clickables, makes a node for a clickable element
  with no id, records the node's path, and reports bounds clipped to the content
  mask (the part a scroll container leaves visible) so a click at the centre lands.

`src/elements/div.rs`
- `Div`'s default role: Button for a clickable element, Group for a focusable one.
- A clickable is a click or aux-click listener, or a mouse-down listener under a
  pointer cursor. Each such element registers a Click a11y action that runs its
  own listeners.

`src/elements/text.rs`
- Text prepaint calls `Window::a11y_note_text` (public, so an element that draws
  text some other way can report it too). At the end of the frame `A11yNodeBuilder::resolve_texts`
  gives each text to its nearest enclosing node that is named by its text (button,
  link, tab, list item, heading ...) as that node's label, text of a nested such
  node excluded (a list item, row or cell also takes the text of the buttons inside
  it); text with no such node around it becomes a `Label` node, with an id path
  ending in `~`, in drawing order among its siblings.

Search for `TOD PATCH` to find each change.

### Updating gpui

Re-copy the new crate here and re-apply the patches (the `TOD PATCH` markers
and `TOD_A11Y` guards show where). If upstream ever offers a public way to force
the tree active and dispatch actions, delete this directory and the
`[patch.crates-io]` entry.
