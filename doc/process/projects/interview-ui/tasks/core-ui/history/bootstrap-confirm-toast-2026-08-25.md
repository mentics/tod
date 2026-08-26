# Bootstrap confirmation toast — 2026-08-25

**Context:** Opening an existing interview session (e.g. persistence — Initial / defining) could land in “Waiting for researcher scaffolding” forever when scaffolding was never bound (`config_path` null, no on-disk config). Auto-bootstrap on open was rejected; user requested self-healing with explicit consent.

**Decision:** Add project req **19 — Unbootstrapped session recovery** and core-ui design constructions for bootstrap confirmation toast + workspace self-healing.

**UX:**

- Toast (warning, non-autohide): *“{entity label} has not been set up yet. Do you want me to set it up?”* with **No** / **Yes**.
- **No:** dismiss; stay on session list; do not open workspace.
- **Yes:** reactivate if needed, start researcher bootstrap, open workspace.
- New compose kickoff (Shift+Enter) exempt — auto-bootstrap without toast.
- Workspace stuck on pending scaffolding with no bootstrap in flight → close to list and re-show toast.

**Implementation:**

- `crates/tod/src/ui/toast.rs` — `confirm_toast`
- `SessionsView` — open gate, `prompt_bootstrap_setup`, `accept_bootstrap_setup`
- `WorkspaceView` — `NeedsBootstrap` event when scaffolding pending and gate clear

**Traceability:** project `user.md` req 19; core-ui `design.md`, `plan.md` steps 16–17; tod `user.md` req 11 (confirmation toasts); `ux-design.md` States and feedback.
