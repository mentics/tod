//! `tod-watchdog`: one pass of the watchdog, as the hourly Blaxel job runs it.
//! Everything comes from the environment (see `tod_sandbox::watchdog`): the
//! workspace, the token (a job secret), the orchestrator's URL, and optional
//! limits.

use std::time::SystemTime;
use tod_sandbox::watchdog::{self, BlaxelEnv, Policy};

fn main() {
    let code = match BlaxelEnv::from_env()
        .and_then(|env| watchdog::run_once(&env, &Policy::from_env(), SystemTime::now()))
    {
        Ok(out) => watchdog::report(&out),
        Err(err) => {
            eprintln!("tod-watchdog: {err:#}");
            1
        }
    };
    std::process::exit(code);
}
