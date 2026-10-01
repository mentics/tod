# Visual design in an external browser

Status: proposal, with the browser mechanics verified by a spike on Windows
(section 12). Replaces the embedded `gpui_wry::WebView` in
`crates/tod-ui/src/views/visual_design_panel.rs`, which is a dead end: it is
slated for deletion and is not extended. This is built for the
`VisualDesign` conversation protocol (section 8) and the old panel is deleted
when that replaces it. Nothing here is written twice.

## 1. Why change

The embedded webview works, but:

- It is a native child window over GPUI: GPUI popovers, menus and dialogs cannot
  draw over it, and focus and keys are contested (the panel already carries an
  arrow-key workaround).
- It differs per OS (WebView2, WKWebView, WebKitGTK) and is untested on Linux
  and macOS. It adds `gpui-wry` and `wry` and their build weight (the reported
  ~400 MB is likely mostly build artifacts; measure a release build before
  citing it).
- It has no devtools and no responsive-width testing, and it is not the browser
  the design will really be viewed in.

What it gives us, and what must be kept: a mockup beside the chat and reload
when the mockup changes. It has no page-to-app channel today (no IPC), so
feedback from the page is new work, in phase 2.

## 2. Goals and non-goals

Goals

1. Show an obligation's mockup in the user's real browser, reloading on change
   without losing scroll position where possible.
2. Place the window beside tod, so it reads as docked, and re-dock it on demand.
3. Drop `gpui-wry` and `wry`. Work the same on Windows, macOS and Linux.
4. Nothing on the UI thread blocks: discovery, launch, serving and window
   placement all run in the background.
5. Chrome only for now, with an error if it is missing, behind a trait so other browsers plug in (section 5).
6. Phase 2: the user can select parts of the page, or draw a rectangle, comment
   on the selection, and have it go to the design agent session.

Non-goals

- Live-tracking tod's window as it moves (no continuous sync). Re-docking is an
  explicit user action.
- Driving a browser through CDP / Playwright. An agent may do that itself for
  screenshots; it is not the viewer.
- Edge, Firefox, Safari, Brave in the first cut (the trait makes them additive).

## 3. Overview

```
 mockup file ──watch──▶ DesignServer (127.0.0.1:PORT) ◀──HTTP/SSE── Chrome window
                          │  injects bridge script                  (app mode,
                          ▼                                          own profile)
        phase 2: feedback ──▶ a turn in the design conversation
 VisualDesignPanel ──open / re-dock / close──▶ BrowserLauncher ──▶ WindowMover
```

Modules in `crates/tod-ui/src/visual_design/` (the server and launcher hold no
GPUI types, so they could move to `tod-core` if the CLI ever needs them):

- `server.rs`: `DesignServer`
- `browser.rs`: the `Browser` trait, `chrome.rs` its only implementation
- `launcher.rs`: `BrowserLauncher`, session and window state
- `placement.rs`: geometry maths
- `mover/`: `WindowMover` trait, `windows.rs`, `macos.rs`, `linux.rs`

## 4. DesignServer

One server per app run, bound to `127.0.0.1` on an OS-chosen port (port 0).
Never bind a non-loopback address.

Routes

| Route | Purpose |
|---|---|
| `GET /d/<token>/` | The mockup HTML with the bridge script injected before `</body>` (appended if there is none). The served `<title>` is made unique (`tod design <token prefix>`) so the window can be found by title. |
| `GET /d/<token>/<path>` | Sibling assets (css, images, js) from the mockup's directory. |
| `GET /d/<token>/__tod/events` | Server-sent events stream: `reload`, `navigate`, `closed`. |
| `POST /d/<token>/__tod/feedback` | Phase 2: the bridge posts a selection and comment. |
| `GET /__tod/bridge.js` | The bridge script. |

Security

- `<token>` is a random 128-bit value per design session, in the path. Without
  it the server returns 404, so another local page or user cannot read the
  mockup or post feedback. Also reject requests whose `Host` is not
  `127.0.0.1:PORT` (DNS rebinding) and `POST`s whose `Origin` is not the
  server's own.
- Serve files only from the mockup's directory. Canonicalize and refuse `..`
  and symlinks leaving the directory (`resolve_within(root, rel)` is the one
  place this lives, with tests).
- Mockup JS is untrusted-ish (an agent wrote it). It runs in a dedicated
  browser profile (section 5) with no sign-ins.
- The server starts only while a design session is open and stops when the last
  one closes.

Reload

- A watcher (`notify` crate, debounced ~150 ms) watches the mockup directory.
  Any change sends `reload`; the bridge does `location.reload()` after saving
  `scrollX/scrollY` to `sessionStorage` and restoring them on load.
- A session's mockup path is the conversation's working draft until the user
  accepts (section 8.2), then the saved file; the server follows whichever it
  is, and the watcher covers its directory.
- The mockup *path* changes when the agent saves a new file
  (`tod-cli visual-design save` writes `visual_design_path`). The panel listens
  to the store change event (no polling), tells the server the new path for that
  session, and the server sends `navigate`, so the same window follows the
  obligation's current mockup.

Sessions

- A session is `(obligation id, mockup path, token)`. One browser window per
  session.

Server choice: a std `TcpListener` with a thread per connection, as in
`tod-orchestrator` (section 10); one user needs nothing heavier.

## 5. Browsers: a plug-in point

Everything browser-specific sits behind one trait, so adding a browser is one
new file and one registry line, with no change to the server, panel or mover.

```rust
pub trait Browser: Send + Sync {
    /// Stable id for settings and logs, e.g. "chrome".
    fn id(&self) -> &'static str;
    /// Where it is installed on this machine, or None. Cheap; cached by the caller.
    fn discover(&self) -> Option<PathBuf>;
    /// What the browser can do. Drives which UI is offered.
    fn capabilities(&self) -> Capabilities;
    /// The command line that opens `url` as a standalone window, using `profile`.
    /// `placement` is Some only on a first launch.
    fn launch_args(&self, url: &str, profile: &Path, placement: Option<BrowserRect>) -> Vec<OsString>;
}

pub struct Capabilities {
    pub app_mode: bool,          // a chrome-less standalone window
    pub launch_placement: bool,  // honours position/size flags
    pub separate_profile: bool,  // --user-data-dir or equivalent
}
```

`registry()` returns the browsers in preference order. Today it is
`[Chrome]`; the others add themselves later (Edge shares Chrome's flags, so it
is a few lines on top of a shared `ChromiumFamily` helper; Firefox and Safari
would report fewer capabilities and the launcher degrades accordingly).

The launcher never mentions "chrome" by name: it takes the first browser whose
`discover()` returns a path, and asks it for its arguments. Unit tests build
argument lists through the trait without spawning.

### 5.1 Chrome

Discovery (off the UI thread, cached for the process lifetime, re-run if the
setting changes):

| OS | Where to look |
|---|---|
| Windows | `HKLM/HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe`; then `%ProgramFiles%`, `%ProgramFiles(x86)%`, `%LocalAppData%` `\Google\Chrome\Application\chrome.exe`. |
| macOS | `/Applications/Google Chrome.app` and `~/Applications`, executing `Contents/MacOS/Google Chrome`. |
| Linux | `google-chrome`, `google-chrome-stable` on `PATH`. |

An optional `visual_design.browser` setting (a path) overrides discovery. If
Chrome is not found, the panel shows an error and nothing opens. There is no
fallback browser; other browsers are added later through the trait.

Arguments:

```
--app=http://127.0.0.1:PORT/d/<token>/
--user-data-dir=<data root>/visual-design/browser-profile
--window-position=X,Y --window-size=W,H        (first open only; see 6.4)
--no-first-run --no-default-browser-check
```

`--user-data-dir` is required: with the user's normal profile already running,
Chrome hands the URL to that process and ignores position and size flags. A
dedicated profile gives a separate browser process and keeps this window free
of the user's sign-ins, extensions and history. It lives under the data root
(`<data root>/visual-design/`), is created on first use, and is safe to delete.

## 6. Opening, placement and re-dock

### 6.1 One window per profile

Only tod uses this profile, so any window running under it is ours. Opening is
therefore:

1. If a window for this profile is already open, **re-dock it** (and navigate it
   to the session's URL) instead of launching. Command-line flags are ignored
   by an already-running browser, which is exactly why the placement is done by
   the mover.
2. Otherwise launch Chrome with the placement flags.

"Is it open?" is answered by the server (a live SSE connection for the session)
first, and by the mover's window search by title second (a window can exist
before its page has connected). A second `chrome --app=...` against a running profile exits at once and opens a
**second window** (spike), ignoring any placement flags, so a window that exists
must never be launched again: navigate it over SSE and re-dock it instead. For
the same reason process exit alone never means "closed".

### 6.2 Inputs

GPUI gives tod's window bounds and the display it is on (`Window::bounds()`,
`cx.displays()`, the display's usable bounds excluding taskbar and menu bar).
Read them on the UI thread (cheap) and pass plain numbers to the background
launcher.

### 6.3 The maths (pure, unit-tested in `placement.rs`)

`dock_rect(tod: Rect, work_area: Rect, min_width) -> Rect`

1. Free space to the right of tod: `work_area.right - tod.right`. If it is at
   least `min_width` (default 700 px), the browser takes that strip, full work
   area height, aligned to tod's top.
2. Else free space to the left, same rule.
3. Else (tod fills the display) place the browser over the right 45% of the work
   area and leave tod alone. Never move or resize tod without being asked; a
   "Tile side by side" action in the panel may offer that, as an explicit
   choice.
4. If tod spans several displays, use the display containing the window's
   center.

Everything is clamped to the work area, minimum size 480x360.

Units (verified on Windows at 150%): Chrome's `--window-position` and
`--window-size` are **logical** pixels (900x700 asked, a 1350x1050 physical window
came out), while `SetWindowPos` takes **physical** pixels. Compute `dock_rect` in
logical pixels (GPUI's bounds and the display's work area, divided by the
window's scale factor); pass it unchanged to Chrome's flags, and convert once with
`to_native_units(rect, scale_factor, platform)` for the mover. Tested at 100%,
150% and 200%. macOS (retina) and Linux still need a hand check; CI cannot see
them. The process doing the moving must be DPI-aware (tod is; a plain script
is not and sees virtualised numbers).

### 6.4 Re-dock (a first-class button)

The panel has **Open / Re-dock** and **Close window**. Re-dock recomputes `dock_rect` from tod's current bounds and moves the
window with the OS mover. It is also what "Open" does when a window is already
there (6.1). It is always explicit; there is no continuous tracking.

First launch passes the flags, so placement works even where no mover exists.
Re-dock needs a mover.

### 6.5 WindowMover

```rust
pub trait WindowMover: Send + Sync {
    /// Find our window by its unique title; None if not open.
    fn find(&self, title_prefix: &str) -> Option<WindowHandle>;
    fn move_to(&self, window: &WindowHandle, rect: NativeRect) -> Result<(), MoverError>;
    fn focus(&self, window: &WindowHandle) -> Result<(), MoverError>;
}
```

`MoverError` includes `Unsupported` and `PermissionDenied`; the panel shows the
reason and hides Re-dock when it is `Unsupported`.

| OS | Size | How |
|---|---|---|
| Windows | ~60-80 lines | `EnumWindows` + `GetWindowTextW` to match the title prefix, then `SetWindowPos` (and `SetForegroundWindow` for focus). Uses the `windows` crate. Built first. |
| macOS | ~20-30 lines | `osascript` ("tell System Events ... set position/size of window whose name starts with ..."). Needs the user to grant Accessibility permission once; if denied the error says where to grant it. |
| Linux | ~15-20 lines | X11 only: shell out to `wmctrl -r <title> -e ...` (or `xdotool`) when present. Wayland and a missing tool report `Unsupported`. |

Each is gated with `#[cfg(target_os = ...)]`, so CI on the other two OSes only
type-checks it (CLAUDE.md's cross-platform CI note); the shared maths is
what the tests cover.

## 7. Phase 2: feedback from the page

Not in the first cut. The current webview has no page-to-app channel, so this
is new capability. The destination is the obligation's **visual-design conversation** (section 8):
the whole design process is agent-driven, so feedback is simply the next user
turn in the most recent `VisualDesign` conversation for that obligation (the
same one that carries on after a session rotation). There is no separate store, no new table (so no
`journey_changes` trigger) and no `tod-cli ... feedback` verb. If a comment
justifies a requirement, the agent records an obligation with the normal CLI.

### 7.1 What the user can do

1. **Click to select.** Hover outlines the element under the pointer; a click
   selects it. Selection scope is adjustable: after the first click, the user
   can widen to the parent, or narrow to a child (keys `[` and `]`, and small
   buttons in the overlay), so one component, a container, a section or a whole
   panel can be chosen. The overlay shows the breadcrumb (`main > .card >
   button.primary`) so the scope is clear.
2. **Draw a rectangle.** Drag on the page to draw a selection box. Everything
   **completely inside** the rectangle is selected. Selection is the
   outermost elements fully contained, not every descendant: a card entirely
   inside the box counts as one, its children are implied. Partially covered
   elements are not selected, and the box shows live outlines of what it would
   take so the user sees the effect before releasing.
3. **Add to or adjust a selection** with Shift (extend) and Esc (clear).
4. **Comment.** A small input anchored to the selection takes free text and is
   submitted with Ctrl+Enter. Cancel with Esc.

A pick mode toggle (`Alt+P`, plus a small floating toggle) keeps ordinary
interaction with the mockup (links, buttons) available when it is off.

### 7.2 What is sent

`POST /feedback`, validated (token, origin, size cap):

```json
{
  "comment": "Make these the same height",
  "selections": [
    { "selector": "main > .cards > .card:nth-of-type(2)",
      "tag": "div", "classes": ["card"],
      "text": "first 200 chars of text content",
      "rect": { "x": 0, "y": 0, "w": 0, "h": 0 },
      "scope": "element|container|region",
      "outerHtml": "truncated to a cap" }
  ],
  "box": { "x": 0, "y": 0, "w": 0, "h": 0 },
  "viewport": { "w": 0, "h": 0, "scrollX": 0, "scrollY": 0 }
}
```

`box` is present only for a rectangle selection. Selectors prefer stable
attributes (`id`, `data-*`) and fall back to a short structural path. The
server renders this into a plain-text turn for the design conversation (the
comment first, then each selection with its selector, text snippet and size) and
sends it through the same path as a message the user typed, so it shows in the
transcript and the agent can act on it.

Screenshots are in 7.4. The user can also capture the whole page without a
selection.

### 7.3 Implementation notes

- The overlay (outlines, rubber-band box, comment input) is drawn by the bridge
  inside a shadow root on a top-level element, so the mockup's CSS cannot
  affect it and it does not affect the mockup's layout.
- Hit-testing for the rectangle uses `getBoundingClientRect()` containment on
  the visible elements, skipping `html`, `body` and the overlay.
- Pure selection logic (containment, outermost-only, selector generation) lives
  in one small dependency-free JS module, kept separate from the overlay's
  drawing code so it can be tested on its own. The test approach (a Node test
  runner for that one file, or a headless-Chrome page driven through the CDP
  client this design already needs) is chosen when phase 2 is scheduled.

### 7.4 Screenshots

Two kinds, one mechanism: **region** (the dragged box, or the bounds of the
selected elements) and **full page**. The agent receives the PNG with the turn so
it can see a broken layout as the user sees it.

Capture uses the Chrome DevTools Protocol, not an in-page DOM re-render, which
misdraws fonts, cross-origin images and modern CSS (the cases a broken layout
tends to involve). `Page.captureScreenshot` returns Chrome's own pixels; a
region is its `clip` parameter and the full page is `captureBeyondViewport`.

- Launch adds `--remote-debugging-port=0` (via `Browser::debug_args`). Chrome
  writes the chosen port to `DevToolsActivePort` in our profile directory. The
  port binds loopback only. Any local process could drive that browser, which
  is acceptable: the profile is dedicated and has no sign-ins.
- `visual_design/cdp.rs` (about 150-250 lines, plus a websocket crate such as
  `tungstenite`): read `DevToolsActivePort`, list targets over HTTP, pick the
  page whose title carries our token prefix, open its websocket, send one
  command, decode the base64 PNG. It runs off the UI thread.
- Flow: the bridge hides its overlay, waits a frame, and posts the feedback with
  the box in CSS pixels plus scroll offset; the server captures with that clip
  and attaches the PNG to the design turn through the existing pasted-image
  path (`ui::pasted_image`, `conversation_turns.attachments`), so it appears in
  the transcript.
- A full-page capture is a toolbar button in the overlay (and the same call with
  no clip). It needs no selection.
- Size: downscale to a cap (long edge about 1600 px) before attaching.
- An agent whose `promptCapabilities.image` is unset cannot take the image: the
  turn is sent without it, with a note saying a screenshot was left out.
- `Capabilities` gains `screenshot: bool`, so a browser without CDP simply does
  not offer the buttons. Chromium-family browsers added later share it.

## 8. Where it lives: the `VisualDesign` conversation protocol

`ProtocolKind::VisualDesign` already exists in `tod_core::conversation` as a
stub (it resolves to `ChatProtocol`, nothing launches it). The old panel hosts its
own chat through `InteractiveAgentView`; that path is dead and is not extended.
This design is the rebuild's browser half; the conversation half is a separate
piece of work, specified here only where they meet. (The earlier protocols spec,
`doc/conversation/protocols.md`, was deleted in commit `1e13f79`, "cleanup doc
dir"; `git show 1e13f79^:doc/conversation/protocols.md` recovers it. The old
panel, and its header comment, have since been deleted.)

**Associated with the obligation, like any chat.** A visual-design conversation
is a row in `conversations` with `focus = Obligation { node, id }` and
`protocol = visual_design`: it belongs to that one obligation, not to its node. So it
shows up wherever the obligation's conversations are listed: the conversation
view's picker (Ctrl+N, Ctrl+J opens the obligation's most recent), and the
workbench's chat drawer for that obligation, mixed in with any plain chats about
it and labelled as a visual design. It uses the same driver, session, transcript,
context budget and rotation as every other conversation; nothing about it is
special-cased in the list. The Design affordance on a design-phase obligation's
row starts a new one or opens the most recent. When its session rotates, the
newest session carries on and is the one page feedback goes to.

### 8.1 Context: its own recipe

`ProtocolKind::VisualDesign` stops borrowing `ChatProtocol` and gets its own
protocol, `VisualDesignProtocol`, with its own `ContextRecipe`. The old chat
already has one (`VISUAL_DESIGN_CHAT`, for `InteractiveAgentView`); the
conversation protocol needs a sibling, `VISUAL_DESIGN`, because the conversation
driver supplies a `Focus` block and records changes, and the old one assumes
neither. When the old chat is deleted, `VISUAL_DESIGN_CHAT` and its
`surface/visual-design.md` text go with it.

```rust
pub const VISUAL_DESIGN: ContextRecipe = ContextRecipe {
    name: "visual design",
    situational: false,
    layers: &[
        "stance/interactive-chat",
        "domain/outline",
        "domain/obligations",
        "cli/intro",
        "cli/visual-design",
        "cli/obligations",
        "cli/changeset",
        "surface/visual-design",
    ],
    blocks: &[
        DynamicBlock::DataRoot,
        DynamicBlock::Focus,
        DynamicBlock::AncestorContext,
        DynamicBlock::VisualDesign,
    ],
};
```

Why each piece:

- **`stance/interactive-chat`** is the one stance: the human is at the prompt.
- **`domain/outline`, `domain/obligations`**: the concepts it handles. No plan or
  lifecycle: a mockup is not a plan, and the agent must not drive the node's
  state.
- **`cli/visual-design`** is the agent's mockup command. `save` stays for an
  explicit request; accepting is the app's own button (8.2).
  **`cli/obligations`**: page feedback may justify a requirement or constraint, and
  the agent records it as an obligation (this is how feedback turns into
  requirements). **`cli/changeset`**: those writes are attributed to the
  conversation (`TOD_INTERVIEW_ACTOR=conversation:<id>`, as for a chat), so they land in
  its change set and can be reversed. No `cli/node`, `cli/content`, `cli/plan`,
  `cli/secrets` or `cli/environment`: not this job, and `cli/intro` already says
  how to find the rest with `tod-cli help`.
- **`DynamicBlock::Focus`** gives the obligation's id, its full text, the path
  above it and the node it lives on. That is "what this mockup must cover".
- **`DynamicBlock::AncestorContext`** gives the **inherited constraints**
  (design obligations and constraints on ancestors, summarized as other surfaces
  already receive them). The conversation driver's opening does not fill
  `ancestor_context` today (only `agent_context` does), so `opening_with`,
  `resume_snapshot_with` and the delta path populate it for any recipe that lists
  the block, from `node_context::render_inherited_context` for the focus node.
  Without this the designer would not know the constraints it is designing
  within.
- **`DynamicBlock::VisualDesign`** (new, small): the mockup state: the saved
  mockup's path if one is linked, and the working draft's path (8.2). Rendered by
  `tod_core::dynamic`, knowing nothing about the surface.

### 8.2 Draft versus accepted

The old panel showed a mockup only after the human accepted one in chat, so each
iteration lived inline in the reply. With a live browser window that is wrong:
the user should see each revision as it is made.

- Each conversation has a **working draft** file the agent edits in place, at a
  path the dynamic block gives it (under the conversation's scratch directory,
  `<data root>/agent/visual-design/<conversation id>/mockup.html`, which is also
  its `cwd`, so drafts never land in the user's repository). The server watches
  that file (section 4); every save reloads the browser.
- **Accepting is done by the app, not the agent.** The side pane's **Accept**
  button performs the same operation `tod-cli visual-design save` wraps (copy the
  draft under the data root and link it from the obligation with
  `OutlineMutation::UpdateObligationVisualDesign`, which is what the
  `design-planning.visual-packages-accepted-or-waived` gate reads), directly, in the
  background, with no agent turn. The rule throughout: whatever the app can do
  itself it does itself, and the agent is engaged only for what only an agent can
  do (revising the mockup, judging feedback). The conversation records the accept
  as a note in the transcript so the agent knows on its next turn (through the
  delta, like the user's own edits), and the agent does not save on its own. The
  save is recorded in the conversation's change set like other edits so it can be
  reversed.
- After an accept the session's URL serves the saved file; further edits go back
  to the draft, and the pane shows that the draft differs from what is linked.

### 8.3 What the surface fragment says

`surface/visual-design.md` is rewritten for this protocol (the old text is for the
retired chat). It says: the job is one design-phase obligation's mockup (the id and
text are in the focus block); the mockup is a self-contained HTML+CSS file
(inline `<style>`, no `<script>`, no external resources, flexbox over absolute
positioning, `data-component` / `data-role` annotations where the visual alone does
not carry intent: all kept from the old fragment); the user is watching it in a
browser window beside the app, so edit the draft file rather than pasting HTML
into the reply; keep replies short, say what you changed and why, never dump the
markup; messages that begin with a **page selection** carry selector, text, size
and an optional screenshot, and the comment is about exactly that selection;
record a requirement or constraint as an obligation only when the user's
comment justifies one, not for every remark; the app saves an accepted mockup,
so the agent does not save unless the user explicitly asks; do not advance the
lifecycle or run gate checks. It states its exceptions to the
stance as exceptions. `context_recipes` tests already forbid `tod-cli` syntax
here (it lives in `cli/visual-design`), and `doc_sync` pins `cli/visual-design`
to the binary's usage.

### 8.4 Protocol details

- `surface()` returns a new `VISUAL_DESIGN_SURFACE` ("visual design") for session
  names, so its transcripts sort apart from chats.
- `cwd`: the conversation's scratch directory (8.2), not the focus node's Files
  directory. The agent can still read the repository where it needs to (to match
  the app's real UI, `doc/ui-style-guide.yaml` among it), but it does not write
  there.
- `turn_env`, `delta`, `resume_snapshot`: as for `ChatProtocol` (actor,
  change-set delta, rotation snapshot built with `VISUAL_DESIGN`), so the
  recipe is the only difference.
- `role_for(VisualDesign)` becomes `AgentRole::Chat` (it is interactive, like
  Chat), not `Default`.
- `opening` and `resume_snapshot` use `VISUAL_DESIGN`; a test asserts that every
  recipe-registered surface, including this one, loads (the existing
  `context_recipes` tests) and that this one carries `cli/changeset` and
  `cli/visual-design` and no plan or lifecycle fragments.
- Page feedback turns are sent through the driver's normal `send` with the
  rendered selection text and an image attachment, exactly as a pasted image.

**The side pane is the designer.** The conversation view hosts; the designer is
the side pane (`tod_ui::conversation::side_pane`), replacing the old panel's
left half. It is a compact strip, not a viewer: the mockup name, whether the
draft differs from the saved mockup, **Accept** (8.2), **Open /
Re-dock**, **Close window**, whether a window is open, and any notice (Chrome not
found, Re-dock unsupported or needs permission). Rules for it:

- Opening and everything else runs in the background and reports back through an
  event; the UI thread never waits on Chrome, the server or a mover.
- Store changes drive navigation (the `visual_design_path` update), no polling.
- Buttons are nav stops; no text input is added (keyboard-focus convention).
- Error text uses `selectable_text`.
- Open, Re-dock and Close each record a `UserAction` with a `Presented`
  snapshot (journeys, per CLAUDE.md).
- The server, launcher, placement and movers have no GPUI and no panel
  dependencies, so the side pane (and later the workbench) use them as is.

The old panel stays as it is, unused by the new path, until the protocol is
complete; then it, `InteractiveAgentView` and `InteractiveAgentWindow` are
deleted together. `gpui-wry`, `wry` and the spike example go in that same
change.

## 9. Lifecycle and cleanup

- Window closed by the user: the SSE drop is noticed (no reconnect within a few
  seconds); the panel shows "Window closed"; Open brings it back.
- Panel closed or session switched: the session is dropped and the window gets a
  final `closed` event, which the bridge shows as "Session ended". The user's
  window is not auto-closed.
- App quit: stop the server and watcher. Terminate a Chrome process only if tod
  holds its handle (never by name). A window left running under our profile is
  found again by title at the next launch.
- The server is not the agent control socket and not behind the `agent-socket`
  feature; it is loopback-only and token-protected.

## 10. Dependencies

Nothing new where the repo already has a crate for the job; one choice per
concern across the workspace.

| Need | Use | Precedent |
|---|---|---|
| HTTP server for the mockup, assets, SSE, feedback | `std::net::TcpListener` and hand-written HTTP/1.1, no crate | `tod-orchestrator` ("a small std one, no new dependencies"). SSE is just a long-lived response, so no websocket is needed on this side. |
| Websocket client for CDP | `tokio-tungstenite` 0.24 | already used by `tod-sandbox`, `tod-relay`, `tod-zed-shim`. Add it to `tod-ui` at the same version, with tokio's `net` and `io-util` features, run on a short-lived runtime in a background thread. Do not add plain `tungstenite` or `async-tungstenite`. |
| `GET /json` against the debug port | `ureq` 3 | already in `tod-core`, `tod-store`, `tod-sandbox`. Add the same version to `tod-ui`. |
| File watching | `notify` 8 | already in `tod-ui` and `tod-core`. |
| Win32 mover (`EnumWindows`, `GetWindowTextW`, `SetWindowPos`) | `windows` 0.57 | already a `cfg(windows)` dependency of `tod-ui` with `Win32_UI_WindowsAndMessaging`; at most one more feature flag. |
| PNG downscale | `image` 0.25, `base64` | both already in the lockfile; check the feature set. |

No `tiny_http`, `axum`, `hyper` or `reqwest` is added. If `tod-ui` should not
take these directly (layering), the server, CDP client and launcher move to
`tod-core`, which already has `notify` and `ureq`; they hold no GPUI types, so
either works. Removed: `gpui-wry`, `lb-wry`, and the `webview_spike` example.

## 11. Settings

| Setting | Meaning |
|---|---|
| `visual_design.browser` | Optional path overriding Chrome discovery. |
| `visual_design.dock` | `auto` (default) or `off` (never place; just open). |

No saved window rect: placement is computed on every open and re-dock.

## 12. Spike results (Windows 11, Chrome, 150% display)

Run from `.local/agent/scratchpad/vd-spike/spike.py`: a local page, `chrome
--app` with a dedicated `--user-data-dir`, `--remote-debugging-port=0`.

- Window placement flags are honoured with a dedicated profile (asked for
  1500,100 900x700 logical, got 2250,150 1350x1050 physical).
- The app window's title is the page's `<title>`; `EnumWindows` finds it by
  prefix. `SetWindowPos` re-docks it (verified).
- `DevToolsActivePort` is written into the profile with the chosen port;
  `/json` lists the page target; `Page.captureScreenshot` over the websocket
  works. A clip of the 200x120 CSS-pixel box came back 300x180 (device pixel
  ratio 1.5), exactly the box; a full-page capture came back at the full page
  size. Pass `scale` to normalise the output size.
- A second launch against the running profile exits immediately and opens an
  extra window with the flags ignored (hence section 6.1's rule).
- The fresh profile still starts bundled component pages (a "Google Hangouts"
  background page target appears in `/json`), so target selection must be by our
  page's title or URL, never "the first page".
- Not yet verified: macOS and Linux behaviour of any of this.

## 13. Testing

- Unit: `dock_rect` (right, left, full-screen, multi-display), unit conversion at
  several scales, `resolve_within` traversal and symlink refusal, token and
  Host/Origin checks, bridge injection (with and without `</body>`), Chrome
  `launch_args` through the `Browser` trait (profile always present, position
  flags only when a placement is given), registry order.
- Integration: start the server, fetch the page and an asset over HTTP, change
  the file and assert the SSE `reload`; phase 2 posts good and bad tokens.
- Mover: a fake `WindowMover` drives the open-or-re-dock decision (window
  present means re-dock, absent means launch).
- Manual, per OS (CI cannot cover): real Chrome placement beside tod at 100%
  and a 150%+ display, re-dock after tod is moved, closing and reopening, Chrome
  missing, macOS Accessibility denied, Wayland.
- UI: drive the panel with `--agent mock --no-focus` and the agent control
  socket, checking panel state (the browser is outside the socket's reach).

## 14. Rollout

1. `DesignServer` with reload, the bridge (reload only), and tests.
2. `Browser` trait and Chrome; launcher with first-open placement; `dock_rect`.
3. The `VisualDesign` protocol's side pane (section 8) drives it: recipe and
   context, the Design affordance, the pane's buttons. The old panel is left
   untouched until this is complete; then it, `InteractiveAgentView`,
   `InteractiveAgentWindow`, `gpui-wry`, `wry` and the spike example are deleted
   in one change so no code path keeps both.
4. `WindowMover`: Windows, then macOS, then Linux; Re-dock wired.
5. Phase 2: click selection with scope adjustment, then rectangle selection,
   comment, delivery to the design conversation, then the optional screenshot.

## 15. Open questions

- Edge is always on Windows 10/11 and Chrome is not; with Chrome only, a machine
  without it just sees the error. Edge is a small addition through the trait if
  that becomes painful.
- The `InteractiveAgentWindowControl::engagement` registry the action panel
  reads must move before the old panel's deletion (noted in the deleted
  protocols spec); not part of this design but it gates the cleanup.
- Resolved: feedback goes to the most recent `VisualDesign` conversation for the
  obligation, which is also the one that carries on after a session rotation.

### 6.6 Lifecycle

- The window is never opened proactively: Open (or starting a design) brings it up.
- Leaving the visual-design conversation view (the shell switches view, or the conversation is not a design) **hides** the window (`WindowMover::hide`); returning **shows** it (`show`). Hiding is quick and the page keeps running; the window is not closed and relaunched. `find` still finds a hidden window.
- **Close window** really closes it: `WindowMover::close` (WM_CLOSE on Windows), then the browser process is killed if it lingers, and the session ends.
- When tod quits, `launcher::shutdown_all` (window-closed and app-quit hooks) closes every design window and browser process.
