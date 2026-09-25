//! Cloud sandboxes for tod (Blaxel today).
//!
//! - [`blaxel`]: the provider's control plane and each sandbox's own API.
//! - [`relay`]: the client for `tod-relay`, the one server tod runs in a sandbox.
//! - [`provision`]: making a sandbox ready, idempotently.
//! - [`terminal`]: interactive terminals that let the sandbox sleep when idle.
//! - [`agent`]: an agent in a sandbox bridged to local stdio, detaching when idle.
//! - [`tunnel`]: `tod-cli` in a sandbox reaching the app on this machine.
//! - [`config`]: `sandboxes.toml`.
//!
//! Design: `doc/cloud-sandboxes/blaxel-remote.md`.

pub mod agent;
pub mod blaxel;
pub mod config;
pub mod provision;
pub mod relay;
pub mod terminal;
pub mod tunnel;
