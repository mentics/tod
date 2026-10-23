# scripts/

Two scripts for development:

| Script | Does |
|---|---|
| `scripts/build.sh` | Brings everything up to date, doing only what is stale. |
| `scripts/run.sh [tod args]` | Runs `build.sh`, then `tod`. `SKIP_BUILD=1` skips the build. |

`scripts/install.sh <dir>` installs a release build (`release.sh` / `release.ps1`
make releases). `dev.ps1`, `install.ps1` and `release.ps1` are the Windows
side; `dev.ps1` has not been moved to this build/run split yet.

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
   `<data root>/orchestrator-deployed.hash`, written when `build.sh` deployed
   it. The orchestrator is shared by the whole workspace, so a stale one is
   **reported, not replaced**; add `--deploy-orchestrator` to replace it.
   The data root is `--data-root`, else `TOD_DATA_ROOT`, else `data_root` from
   `install.toml` (read only; what `tod` itself uses), so normally you pass
   nothing. With none of them it is not checked and says so.
   It cannot see a deploy made from another machine.

Flags: `--data-root DIR`, `--deploy-orchestrator`, `--no-sandbox` (skip 2 and
3), and `-- <args>` passed to `tod-sandbox orchestrator` (e.g. `--move-data`).
`run.sh` passes the `--data-root` you give `tod` on to `build.sh`; without one both use the default.

Existing node sandboxes keep the binaries they were made with; `tod` uploads
the new ones when it makes or replaces a sandbox.

## Helpers (called by the above; rarely run directly)

- `build-sandbox-binaries.sh`: builds the Linux sandbox binaries.
  `--docker-test <cargo test args>` runs Linux-only tests in the same container.
- `build-linux-tod.sh`: the glibc Linux GUI `tod` for the test image.
- `lib/stamp.sh`: the source hash.
