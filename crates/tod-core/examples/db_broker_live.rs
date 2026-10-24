//! Runs the database broker (`tod_core::db_broker`) outside the app, for a
//! manual check of `doc/cloud-sandboxes/database-access.md` against a real
//! sandbox and an orchestrator it can reach:
//!
//! `db_broker_live --data-root DIR --databases FILE.toml --sandbox NAME
//!   --orchestrator-url URL --user USER`
//!
//! Every node is taken to run in `--sandbox`. The data root supplies only the
//! Blaxel account and its credentials; nothing is written to it.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tod_core::cloud_sync::HttpOrchestrator;
use tod_core::db_broker::{Service, spawn};
use tod_sandbox::config::Config;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut root, mut databases, mut sandbox, mut url, mut user) = (None, None, None, None, None);
    while let Some(flag) = args.next() {
        let value = args.next().with_context(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--data-root" => root = Some(PathBuf::from(value)),
            "--databases" => databases = Some(PathBuf::from(value)),
            "--sandbox" => sandbox = Some(value),
            "--orchestrator-url" => url = Some(value),
            "--user" => user = Some(value),
            other => anyhow::bail!("unknown flag {other}"),
        }
    }
    let root = root.context("--data-root")?;
    let databases = databases.context("--databases")?;
    let (sandbox, url, user) = (sandbox.context("--sandbox")?, url.context("--orchestrator-url")?, user.context("--user")?);
    let token = tod_store::fleet::sandbox::Sandboxes::load(&root)?.token()?;
    tod_sandbox::relay::init_tls();
    spawn(Service {
        root,
        databases: Box::new(move || Config::load(&databases)?.databases()),
        sandbox_of: Box::new(move |_| Some(sandbox.clone())),
        orchestrator: Box::new(move || Ok((HttpOrchestrator::new(url.clone(), Some(token.clone())), user.clone()))),
    });
    eprintln!("answering; Ctrl-C to stop");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
