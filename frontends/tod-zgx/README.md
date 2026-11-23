# tod · ZGX

This is a standalone native frontend for the redesigned command palette. It
uses its own Cargo workspace and pins the ZGX revision used by the collection
and chat experiments. The GPUI app keeps its existing build.

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

Run the native smoke test by appending `-- --smoke-test` to the demo command.
For a seeded test data root, combine `--smoke-test` with the live command to
verify catalog search and navigation. The [design doc](../../doc/ui/command-palette.md)
defines the agreed interaction.
