# scripts/

Two scripts for development:

| Script | Does |
|---|---|
| `scripts/build.sh` | Brings everything up to date, doing only what is stale. |
| `scripts/run.sh [tod args]` | Runs `build.sh`, then `tod`. `SKIP_BUILD=1` skips the build. |
| `scripts/deploy.sh` | Deploys whatever shared cloud piece is out of date (the orchestrator, the watchdog). No arguments. |

`scripts/install.sh <dir>` installs a release build (`release.sh` / `release.ps1`
make releases). `dev.ps1`, `install.ps1` and `release.ps1` are the Windows
side; `dev.ps1` has not been moved to this build/run split yet.

## What `deploy.sh` does

`scripts/deploy.sh` is the only place deployment happens. It takes no arguments.
It runs `build.sh` first, so the sandbox binaries are current, then deploys each
shared piece whose recorded hash differs from the current sandbox sources:
the orchestrator (`<data root>/orchestrator-deployed.hash`) and the watchdog job
(`<data root>/watchdog-deployed.hash`). A piece that is up to date is left
alone. It stops if there is no data root.

Nothing else needs a deploy. Node sandboxes get the binaries they are made with
(see the end of this file), and the app deploys the orchestrator itself on
launch when the workspace has none.

The orchestrator's extra options (e.g. `--move-data`) are no longer passed
through; run `tod-sandbox orchestrator <options>` directly when you need one.

## What `build.sh` checks

1. **Host binaries** (`tod`, `tod-agentd`, `tod-cli`, `tod-sandbox`,
   `tod-zed-shim`): `cargo build --profile dev-fast`. Cargo's own fingerprints
   make this under a second when nothing changed.
2. **Linux sandbox binaries** (`tod-relay`, `tod-supervisor`,
   `tod-orchestrator`, `tod-cli`, `tod-watchdog` in `target/sandbox/`): a
   content hash of `crates/`, `Cargo.lock`, `.cargo/`, `assets/sandbox/` and the
   build script (~0.1s) is compared with `target/sandbox/.source-hash`.
   Different, or a binary missing: `build-sandbox-binaries.sh` rebuilds (Docker
   or zigbuild; it fails loudly with neither). Any change under `crates/`
   counts, including UI-only ones, which cost a Docker rebuild.
3. **Cloud orchestrator**: the same hash is compared with
   `<data root>/orchestrator-deployed.hash`. A stale one is **reported only**,
   pointing to `deploy.sh`. The orchestrator is shared by the whole workspace.
   The data root is `--data-root`, else `TOD_DATA_ROOT`, else `data_root` from
   `install.toml` (read only; what `tod` itself uses), so normally you pass
   nothing. With none of them it is not checked and says so.
   It cannot see a deploy made from another machine.
4. **Watchdog job**: the same hash is compared with
   `<data root>/watchdog-deployed.hash`, reported the same way.

Flags: `--data-root DIR` and `--no-sandbox` (skip 2 to 4).
`run.sh` passes the `--data-root` you give `tod` on to `build.sh`; without one both use the default.

Existing node sandboxes keep the binaries they were made with; `tod` uploads
the new ones when it makes or replaces a sandbox.

## Helpers (called by the above; rarely run directly)

- `build-sandbox-binaries.sh`: builds the Linux sandbox binaries.
  `--docker-test <cargo test args>` runs Linux-only tests in the same container.
- `build-linux-tod.sh`: the glibc Linux GUI `tod` for the test image.
- `lib/stamp.sh`: the source hash.
