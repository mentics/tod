//! Port forwards held open for a sandbox from a long-lived process (the
//! daemon), not for the length of an agent turn. Dropping the [`Held`] closes
//! them; the sandbox is free to sleep again. See
//! `doc/cloud-sandboxes/database-access.md`.

use crate::config::Forward;
use anyhow::{Result, anyhow, bail};
use std::time::Duration;

/// How long to wait for the relay to say it is listening on every forward.
const READY_WITHIN: Duration = Duration::from_secs(60);

/// A relay `/tunnel` carried to this machine on a thread of its own,
/// reconnecting if the connection drops, until dropped.
pub struct Held {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    pub forwards: Vec<Forward>,
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Opens `forwards` on the sandbox at `sandbox_url` and returns once the
/// relay is listening on all of them, or says why it will not.
pub fn hold(sandbox_url: &str, token: &str, forwards: Vec<Forward>) -> Result<Held> {
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();
    let (url, token, fwds) = (sandbox_url.to_string(), token.to_string(), forwards.clone());
    std::thread::Builder::new().name("tod-forward-hold".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("start a runtime: {e}")));
                return;
            }
        };
        runtime.block_on(async move {
            crate::relay::init_tls();
            let mut ready_tx = Some(ready_tx);
            loop {
                let (fwd_tx, fwd_rx) = tokio::sync::oneshot::channel();
                let run = crate::tunnel::run(&url, &token, 1, &fwds, None, Some(fwd_tx));
                tokio::pin!(run);
                let mut fwd_rx = Some(fwd_rx);
                let outcome = loop {
                    tokio::select! {
                        _ = &mut stop_rx => return,
                        result = &mut run => break result,
                        answer = async { fwd_rx.as_mut().unwrap().await }, if fwd_rx.is_some() => {
                            fwd_rx = None;
                            if let Some(tx) = ready_tx.take() {
                                let _ = tx.send(answer.unwrap_or_else(|_| Err("the tunnel ended".into())));
                            }
                        }
                    }
                };
                if let Some(tx) = ready_tx.take() {
                    // Ended before the relay answered.
                    let why = match outcome {
                        Ok(()) => "the relay closed the tunnel".to_string(),
                        Err(e) => format!("{e:#}"),
                    };
                    let _ = tx.send(Err(why));
                    return;
                }
                // Dropped after it was up: reconnect (the relay may have restarted).
                tokio::select! {
                    _ = &mut stop_rx => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        });
    })?;
    match ready_rx.recv_timeout(READY_WITHIN) {
        Ok(Ok(())) => Ok(Held { stop: Some(stop_tx), forwards }),
        Ok(Err(why)) => bail!("{why}"),
        Err(_) => Err(anyhow!("the sandbox's relay did not answer within {}s", READY_WITHIN.as_secs())),
    }
}
