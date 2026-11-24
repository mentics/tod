# tod · ZGX

This standalone native frontend brings together the redesigned command palette
and collection panels. It uses its own Cargo workspace and pins the ZGX
revision used by the chat experiment.

Run the demo from the repository root.

```sh
cargo run --release --locked --manifest-path frontends/tod-zgx/Cargo.toml
```

Press **Cmd/Ctrl+K**, then try `p rn`, `nmd`, or `keyboard shortcuts`.
**Option/Alt+↑/↓** jumps between sections. **Shift+Enter** opens actions.
**Escape** returns to the previous palette page or workspace focus.

The default demo uses an in-memory workspace. Mark two tasks with **Space**
and run **Pin tasks** or **Open agent conversation**. Right-click a
different task to invoke actions on that row while retaining the marks
and open detail. Command frequency is saved separately from app data.
Pass `-- --state <file>` to choose the frequency file.

## Collection panels

Open **Collection panels** from the window header or find **Design work** in the
command palette. The palette can open the collection, run **Edit collection**
for it, and open the requirements shown in the collection pane.

The two slots share the saved config and keep their own cursor, scroll position,
marks, and collapsed groups. In either slot, click the config name or press
`/` to open its chips. Try `Show it, co, ph` to set the visible columns. Comma
completes a unique field match, and Enter applies the chip or returns to an
unresolved field. Changes to the named config save immediately and appear in
both panels.

Open **Edit a collection config** (`REQ-COLLECTION-01`) to edit it, save a
revision, browse **History**, and follow its linked implementation task. The
Markdown supplies revision 1. The demo stores later revisions locally.

The default collection state uses the same file as the earlier prototype.
Pass `-- --collection-state <file>` for an isolated config and its adjacent
specification history.

## Existing TOD workspace

```sh
cargo run --release --locked --manifest-path frontends/tod-zgx/Cargo.toml --features live -- --data-root /path/to/tod/data
```

The palette's live catalog comes from TOD's existing store. Search for a saved
outline and press **Enter** to open it in the collection pane. The pane loads
the tree when you open the outline and shows its saved hierarchy, sibling
order, and initial collapsed branches. Use the arrow keys to browse and
expand or collapse branches, and **Enter** to inspect a row. The pane shows
item type, lifecycle phase, and tags from the store. Browse state stays in
the pane during the session. Reopen the outline from the palette to load
the current saved tree.

Live **Pause runner** and **Resume runner** call TOD's runner operator off
the UI thread and use the daemon for local runners. The live outline pane
is read-only for outline data. The demo exercises editable collection
configs, pinning, and multi-task chat.

## Lifecycle

Open **Lifecycle** from the window header. The diagram shows the Default
lifecycle, including review and fix work and the paths that send a task back.
Select a phase, gate, or return arrow to see its details beside the diagram.
In a live workspace, the tab uses the item open in Tasks or focused in a
saved outline. If the item has **Lifecycle config** enabled, the inspector
can save skills, model, and effort overrides for its agents.

Open **Workflows** to build a draft, load **Review and fix**, or copy the
operator-driven **Default phases**. Default phases maps the existing phase order
and shows rework and redesign as explicit routes. Most steps take operator
confirmation. Its review gate reads the task checkout. Its merge and release
gates use linked GitHub pull requests and their repository policy.
**Try automated Default** opens the candidate with native task actions and
phase mappings. In **Task workflow run**, select a mapped workflow to preview
adoption at the task's current phase. **Preview return to legacy runner**
shows what will be preserved before detaching.
Workflow execution targets local devcontainers. Tasks with a cloud sandbox
location continue to use their legacy runner.
The [parity matrix](../../doc/redesign/lifecycle-parity.md) tracks cutover evidence.
Select an arrow to edit its typed condition. **Data sources** binds declared
step outputs, current-task fields, linked GitHub pull requests, or typed
operator observations. Preview walks the graph on supplied values.

In a live data root, save a draft and publish a revision. Use **Agent actions**
to publish versioned prompts, skill instructions, context, and model settings.
An action node pins the revision you select. An agent action uses the task's
configured runner and working files. Add **Runtime context sources** to resolve
task data, current workflow records, inherited settings, or prior outputs at
launch. **View recorded context** on the run shows the saved launch input.

Open a task, enter **Lifecycle → Task workflow run**, and assign a published
revision. Record manual action results or start the configured task runner
when an agent action is ready. The run view highlights its current node,
records the observations and route decisions, and supports replay against
the current draft, metrics by revision, and lessons linked to specific steps,
rules, sources, prompts, skills, or context. Migration previews a
restart at the new revision's entry and preserves the old run history.

## Boundaries

- [tod-palette](../../crates/tod-palette/src/lib.rs) owns targets, ranking,
  grouping, result navigation, and transitions between palette pages.
- [workspace.rs](src/workspace.rs) supplies a catalog and applies demo
  effects. [live.rs](src/live.rs) connects the existing TOD APIs.
- [main.rs](src/main.rs) renders the model with ZGX and translates native
  input into model calls. The framework is absent from the model crate.
- [collection](src/collection/) owns the collection panel views, shared saved
  config, and per-slot navigation state.

Run the native smoke test by appending `-- --smoke-test` to the demo command.
For a seeded test data root, combine `--smoke-test` with the live command to
verify catalog search and navigation. The [design doc](../../doc/ui/command-palette.md)
defines the agreed interaction.

## ZGX inspection socket

```sh
ZGX_INSPECT_SOCKET=1 cargo run --release --locked --features agent-socket --manifest-path frontends/tod-zgx/Cargo.toml
```

The app prints the path of its user-local socket. It accepts one JSON request
per line. Send `{"cmd":"help"}` for the protocol. Use separate `--state` and
`--collection-state` files for a disposable test profile.
