# Visual design in an external browser: implementation plan

How to build `doc/ui/visual-design-browser.md` (read it first; section numbers
below refer to it). Each work item is sized for one agent, names what it depends
on, and names the files it owns so that items running at the same time do not
edit the same code.

## What the code has today

- **The old designer** is `views/visual_design_panel.rs`: a `gpui_wry::WebView`
  beside an `InteractiveAgentView` chat, opened by
  `DrawerRequest::OpenVisualDesign` (from the Obligations panel's Design
  affordance, handled in `app/window.rs` and `app/right_drawer.rs`). It, the chat
  view and `InteractiveAgentWindow` are slated for deletion. It is not extended.
- **`ProtocolKind::VisualDesign` exists as a stub**: `protocol_for` returns
  `ChatProtocol`, `header.rs` already labels the picker entry "New visual
  design", `side_pane.rs` shows a placeholder (`SideList::Empty`), and
  `launch::role_for` gives it `AgentRole::Default`.
- **Context**: the conversation driver builds openings through
  `opening_with(.., &RECIPE)` in `tod-core/src/conversation/context.rs`; it fills
  `Focus` and `Environment` but not `ancestor_context`. The old chat's
  `VISUAL_DESIGN_CHAT` recipe and `surface/visual-design.md` are for the old path.
- **Saving a mockup** is `tod-cli visual-design save`
  (`crates/tod-cli/src/visual_design.rs`) over
  `OutlineMutation::UpdateObligationVisualDesign`; files go under
  `TodInstallPaths::visual_design_dir`.
- **Dependencies already present**: `notify` 8 and `windows` 0.57
  (`cfg(windows)`, with `Win32_UI_WindowsAndMessaging`) in `tod-ui`;
  `ureq` 3 in `tod-core`/`tod-store`; `tokio-tungstenite` 0.24 in
  `tod-sandbox`/`tod-relay`/`tod-zed-shim`; `tod-orchestrator` has a std
  `TcpListener` HTTP server to follow.
- **Verified by spike** (`.local/agent/scratchpad/vd-spike/spike.py`, design
  section 12): Chrome `--app` with its own profile honours position and size
  (logical pixels); `EnumWindows` finds the window by title; `SetWindowPos`
  moves it (physical pixels); a second launch opens a duplicate window; CDP
  screenshots (region and full page) work.

## Ground rules for every item

- Follow `.claude/CLAUDE.md`: cargo commands with ~120 s timeouts, scoped to the
  crates touched; `cargo check --workspace --all-targets` before handing back an
  item that touches more than one crate. Code behind `cfg(target_os)` is only
  type-checked on its own OS, so say which OS an item was run on.
- Nothing on the GPUI main thread waits on Chrome, a socket, a file system watch,
  Docker or git. Long work goes to the background; results come back as events.
- No new crate where one already exists for the job (design section 10). A new
  direct dependency in `Cargo.toml` is a decision to flag in the hand-back.
- Dynamic text (errors, status, paths) is `selectable_text`.
- Every new user-facing button records a journey `UserAction` with a
  `Presented` snapshot (see `conversation/lifecycle.rs`).
- Keep `tod-agent` free of `tod-*` dependencies; browser, server, launcher and
  placement code holds no GPUI types, so it can sit in `tod-core` if `tod-ui`
  should not own it. Decide once in item **B1** and keep it.

## Work items

Phase 1 is everything needed to replace the webview: **B** (browser half) and
**C** (conversation half) can run in parallel; **W** (wiring) joins them. Phase 2
is **F** (feedback). The numbers are ids, not an order.

### B1. Placement maths and the module skeleton

Creates `visual_design/mod.rs`, `placement.rs` (design 6.3): `Rect`,
`dock_rect(tod, work_area, min_width)`, `to_native_units(rect, scale, platform)`,
with unit tests (right strip, left strip, tod fills the display, multi-display
centre, clamping and minimum size, 100/150/200 % scale). Also decides and records
(a comment in `mod.rs`) whether the module lives in `tod-ui` or `tod-core`; the
default is `tod-ui`, moving to `tod-core` only if a dependency forces it.
Depends on: nothing. Owns: `visual_design/mod.rs`, `placement.rs`.
Done when: tests pass, and the logical-vs-physical rule from the spike is
documented in the function docs.

### B2. DesignServer (reload only)

`visual_design/server.rs` plus `bridge.js` (design 4): std `TcpListener`,
hand-written HTTP/1.1 like `tod-orchestrator`, port 0 on `127.0.0.1`, random
128-bit token per session in the path, `Host` and `Origin` checks, `resolve_within`
(canonicalise, refuse `..` and escaping symlinks), the mockup with the bridge
injected (and a unique `<title>`: `tod design <token prefix>`), sibling assets,
an SSE stream with `reload` / `navigate` / `closed`, a `notify` watcher
(debounced about 150 ms) on the session's file directory, and a session API:
`open(path) -> Url`, `set_path`, `close`, `connected(session) -> bool`. The
bridge reloads on `reload`, saves and restores scroll across reloads, and follows
`navigate`. No feedback route yet (item F1 adds it).
Depends on: B1 (module). Owns: `server.rs`, `bridge.js`, its tests.
Done when: integration tests start the server on port 0, fetch the page and an
asset, edit the file and see `reload` on the stream, and reject a wrong token, a
wrong `Host`, `..` traversal and a symlink escape.

### B3. Browser trait and Chrome

`visual_design/browser.rs` (trait `Browser`, `Capabilities`, `registry()`) and
`chrome.rs` (design 5, 5.1): discovery on Windows (App Paths registry key, then
standard folders), macOS and Linux; `launch_args` producing `--app`,
`--user-data-dir`, `--no-first-run`, `--no-default-browser-check`, and
placement flags only when a placement is given; the `visual_design.browser`
setting override. Argument building is a pure function with tests; discovery takes
an injectable file-system/registry probe so it is testable. Missing Chrome is an
error value, not a fallback. Windows discovery needs a registry feature on the
existing `windows` crate (flag it); macOS and Linux are `cfg` code.
Depends on: B1. Owns: `browser.rs`, `chrome.rs`.
Done when: argument tests (profile always present, placement only on first
launch, URL quoted correctly), discovery tests with a fake probe, and the real
Windows discovery returns this machine's Chrome.

### B4. WindowMover and re-dock

`visual_design/mover/` (design 6.5): `WindowMover` trait (`find`, `move_to`,
`focus`), `MoverError { Unsupported, PermissionDenied, .. }`, a fake for tests, and
the **Windows** implementation (`EnumWindows` plus `GetWindowTextW` matched on the
title prefix, `SetWindowPos`, `SetForegroundWindow`) on the existing `windows`
0.57 (add only the missing feature flags). Splits into three sub-items that can
run in parallel once the trait lands: **B4a** Windows, **B4b** macOS
(`osascript`, Accessibility permission), **B4c** Linux (`wmctrl`/`xdotool` on X11,
`Unsupported` on Wayland or when the tool is missing).
Depends on: B1. Owns: `mover/`.
Done when: B4a is verified against a real Chrome window (the spike script is the
model: open, find by title, move, read the rect back). B4b and B4c are
type-checked on their OS, and the trait fake covers the callers.

### B5. Launcher (open or re-dock)

`visual_design/launcher.rs` (design 6.1): one window per profile. Decide **open or
re-dock** with the server's `connected` first and the mover's `find` second; a
window that exists is never launched again (the spike showed a duplicate window);
otherwise launch Chrome with the first-open placement. Owns the profile
directory (`<data root>/visual-design/browser-profile`), the session table, and
the state events the side pane listens to (`Opened`, `Closed`, `Redocked`,
`ChromeMissing`, `MoverUnsupported`, `PermissionDenied`). All of it runs off the UI
thread. Closed-window detection comes from the SSE connection dropping for a few
seconds, not process exit.
Depends on: B2, B3, B4 (trait and fake are enough). Owns: `launcher.rs`.
Done when: tests with a fake `Browser`, fake `WindowMover` and the real server
cover launch, re-dock, closed-then-reopen, Chrome missing, and mover unsupported;
and a manual run on Windows opens Chrome beside tod, then Re-dock returns it
after tod is moved.

### C1. `VisualDesign` protocol and recipe

In `tod-core` (design section 8): `VisualDesignProtocol` replacing the
`ChatProtocol` alias in `protocol_for`, with `surface()` = a new
`VISUAL_DESIGN_SURFACE`, `cwd` = the conversation's scratch directory
`<data root>/agent/visual-design/<conversation id>/`, `turn_env`/`delta` like
Chat, `opening` and `resume_snapshot` built with the new `VISUAL_DESIGN` recipe
(design 8.1), `role_for(VisualDesign) = AgentRole::Chat`. New
`DynamicBlock::VisualDesign` (the saved mockup path, the working draft path and
whether they differ). `opening_with`, `resume_snapshot_with` and the delta path
populate `ancestor_context` (from `node_context::render_inherited_context`) for any
recipe that lists `AncestorContext`. Rewrite `surface/visual-design.md` for the
protocol (design 8.3), add `cli/obligations` and `cli/changeset` to the recipe, and
register it in `ALL_RECIPES`. Extend the mock agent with whatever it needs to play
a design turn (it already handles `add obligation`). Update
`every_kind_resolves_to_its_own_protocol`.
Depends on: nothing. Owns: `tod-core/src/conversation/protocol.rs`,
`conversation/context.rs`, `context_recipes.rs`, `dynamic.rs`,
`session_name.rs`, `launch.rs`, `crates/tod/media/context/surface/visual-design.md`.
Done when: `context_recipes` tests pass (no CLI syntax outside `cli/`, every
fragment loads), a test asserts the recipe carries `cli/visual-design`,
`cli/obligations`, `cli/changeset` and no plan or lifecycle fragments, a test
shows the inherited constraints in an obligation-focused opening, and the old
`VISUAL_DESIGN_CHAT` still builds (it goes in W2).

### C2. Accept done by the app

A function in `tod-core` (design 8.2) that copies a conversation's working draft
under the data root and links it from the obligation with
`UpdateObligationVisualDesign`, recorded in the conversation's change set so it can
be reversed, plus the transcript note the agent sees on its next turn through the
delta. `tod-cli visual-design save` is changed to call the same function so there
is one implementation. A helper that says whether the draft differs from the saved
mockup (for the pane and the dynamic block).
Depends on: C1 (draft path). Owns: new `tod-core/src/visual_design.rs`,
`crates/tod-cli/src/visual_design.rs`, and the matching `doc_sync` expectations.
Done when: tests cover accept, accept twice, reverse, accepting with no draft (an
error), and `tod-cli` behaving as before; `tod_cli::doc_sync` passes.

### W1. Side pane, Design affordance and wiring

In `tod-ui` (design 8): replace the `SideList::Empty` placeholder with the designer
pane in `conversation/side_pane.rs`: mockup name, draft-differs indicator,
**Accept** (C2), **Open / Re-dock** and **Close window** (B5), window state and
notices. Make the Obligations row's Design affordance start or open the
obligation's most recent `VisualDesign` conversation in the conversation view
instead of `DrawerRequest::OpenVisualDesign`, and make "New visual design" appear
in the picker when the focus is a design-phase obligation. A store-change
subscription (no polling) tells the server the new mockup path; the pane never
blocks. The conversation appears in the workbench chat drawer's list for its
obligation with a visual-design label. Reads tod's window bounds and scale factor
on the UI thread and hands plain numbers to the launcher. Each button records a
`UserAction` with its `Presented` snapshot.
Depends on: B5, C1, C2. Owns: `conversation/side_pane.rs`, the pane's new file under
`conversation/`, `app/window.rs` and `views/obligations/` only for the affordance
change, the unified view's chat drawer list label.
Done when: `--agent mock --no-focus` driven through the agent socket reaches the
pane; Design on an obligation opens a conversation focused on it; the mock agent
writes the draft and the browser window reloads; Accept links the mockup and
satisfies the gate criterion; everything runs with the old panel still compiled but
unreferenced.

### W2. Delete the old path

One change: delete `views/visual_design_panel.rs`, `views/interactive_agent.rs`,
`app/interactive_agent_window.rs`, the `DrawerKind::VisualDesign` plumbing in
`app/right_drawer.rs` and `app/window.rs`, `VISUAL_DESIGN_CHAT` and its
old-path text, the `engagement()` accessor on `InteractiveAgentWindowControl`
(the registry itself already moved; this was merged separately), `gpui-wry`,
`lb-wry`, `examples/webview_spike.rs`, and any now-unused key bindings and
settings. Update the CLAUDE.md paragraph that describes the old path, and the
stale pointer to the deleted `doc/conversation/protocols.md`.
Depends on: W1, and the user's go-ahead (it removes a working feature).
Owns: all of the above. Done when: `cargo check --workspace --all-targets`, the
tod-ui tests and a release build with `--no-default-features` pass, and the
dependency list no longer holds `gpui-wry` or `wry`.

### F1. Page selection and the feedback route (phase 2)

Bridge selection logic in one dependency-free JS module (design 7.1, 7.3): hover
outline, click select, widen and narrow the scope with `[` / `]` and overlay
buttons, breadcrumb, Shift to extend, Esc to clear, `Alt+P` pick mode, then the
rubber-band rectangle that selects the outermost elements completely inside it,
with live preview; comment box submitted with Ctrl+Enter. The overlay lives in a
shadow root. The server gains `POST .../__tod/feedback` (token, origin and size
checks) and hands a validated feedback struct to a callback. Pick and settle the
test approach for the JS (Node runner for that one file, or a headless-Chrome run
through the CDP client from F3).
Depends on: B2. Owns: the bridge's selection module, the server's feedback route.
Done when: logic tests cover containment, outermost-only, scope widening and
selector generation; a manual run on a real mockup selects a card by click and a
group by rectangle.

### F2. Deliver feedback to the conversation (phase 2)

Render the feedback struct as a user turn (comment first, then each selection's
selector, text snippet and size) and send it through the driver to the
obligation's most recent `VisualDesign` conversation, off the UI thread, so it
shows in the transcript. Pasted-image attachments are the path for screenshots
(F3).
Depends on: F1, C1, W1. Owns: the delivery glue in `tod-ui`/`tod-core`.
Done when: with the mock agent, a posted feedback appears as a turn and the agent
replies; rotation does not lose the target conversation.

### F3. CDP screenshots (phase 2)

`visual_design/cdp.rs` (design 7.4): `Browser::debug_args`
(`--remote-debugging-port=0`), read `DevToolsActivePort`, `GET /json` with `ureq`,
pick the page by our title (never the first page: the spike showed extra
targets), a `tokio-tungstenite` 0.24 client on a short-lived runtime in a
background thread (add tokio's `net` and `io-util` features to `tod-ui`, flag
it), `Page.captureScreenshot` with `clip` (region) or `captureBeyondViewport`
(full page), downscale to a cap, attach through the existing pasted-image path.
The bridge hides its overlay and waits a frame before capture. Add the
full-page button. If the agent does not support images, send the turn without it
and a note.
Depends on: B3, B5, F2. Owns: `cdp.rs`, the debug flag in `chrome.rs`, the overlay
button. Done when: a region capture of the spike page's 200x120 box returns exactly
that box, a full-page capture returns the full page, and the turn carries the PNG.

## Order and parallelism

```
B1 ─┬─ B2 ──┬─ B5 ─┐
    ├─ B3 ──┤      ├─ W1 ── W2
    └─ B4 ──┘      │
C1 ── C2 ───────────┘
B2 ── F1 ── F2 ── F3  (phase 2; F2 needs C1 and W1, F3 needs B3 and B5)
```

- **First wave, in parallel:** B1 and C1 (no dependencies). Then B2, B3, B4a-c
  and C2 in parallel (B2/B3/B4 need only B1's skeleton, C2 needs C1). Then B5.
- **File ownership keeps them apart:** B* items all live under `visual_design/`
  (one file each), C* items are `tod-core` and the media fragment, W1 is the
  only item that edits `app/window.rs` and `side_pane.rs`.
- **Shared files to coordinate:** `Cargo.toml` for `tod-ui` (B3 registry feature,
  B4 window features, F3 tokio features) is touched by several items. Each adds
  only its own line and flags it in the hand-back; conflicts there are
  mechanical.
- **Manual and OS checks** the plan cannot automate: B4b (macOS), B4c (Linux,
  X11 and Wayland), the units at 150 % on macOS, and every "real Chrome" check
  on a machine with Chrome.

## What each item hands back

The files changed, the commands run and their results (with the OS), any new
dependency or feature flag, anything left unverified, and for W1/F-items the
agent-socket script used to drive it. Items do not merge themselves; the
integrator merges in the order above.

## Not in this plan

Browsers other than Chrome (the trait makes each one a new file and a registry
line), a fallback when Chrome is missing (an error), continuous window tracking
(Re-dock is explicit), and the feedback store (feedback is just a conversation
turn).
