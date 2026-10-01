//! `tod-agentd`: the resident daemon, one per data root (`doc/agentd.md`).
//! [`server::run`] is the `tod-agentd` binary; the protocol and the client are
//! in `tod-agentd-client`.

pub mod server;
