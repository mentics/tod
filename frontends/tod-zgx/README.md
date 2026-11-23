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

The live adapter takes a startup snapshot of saved nodes and outlines through
TOD's existing store APIs. Enter opens the item in the ZGX list or detail
pane. Live **Pause runner** and **Resume runner** call TOD's runner operator
off the UI thread and use the daemon for local runners. Named dynamic
collections, pinning, and multi-task chat are exercised in the demo.
The live adapter uses the existing outline model and currently has no chat
host or collection editor.

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
