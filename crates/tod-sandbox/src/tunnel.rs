//! The client side of the relay's `/tunnel`: each connection made to the
//! relay's loopback port in the sandbox is connected to `127.0.0.1:<port>`
//! here. tod carries `tod-cli` this way: the sandbox's `tod-cli` script talks
//! to the tunnel, and this machine's end is the app's `tod-cli` relay.
//!
//! A tunnel is only open while something else holds the sandbox awake (an
//! agent turn, an attached terminal): an open socket keeps a sandbox from
//! sleeping, so it must not outlive the work that needs it.

use crate::relay;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Carries the sandbox's tunnel to `127.0.0.1:local_port` until the socket
/// closes. Drop the future to close it. `ready` hears once the relay has the
/// tunnel (it drops connections that come while none is open).
pub async fn run(
    sandbox_url: &str,
    token: &str,
    local_port: u16,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<()> {
    let ws = relay::connect(&relay::ws_url(sandbox_url, "/tunnel"), token).await?;
    let (mut sink, mut incoming) = ws.split();
    // The relay expects a first frame on every socket.
    sink.send(Message::text("{}")).await?;
    if let Some(ready) = ready {
        let _ = ready.send(());
    }
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    // Data for each stream's local socket; `None` shuts down its write side.
    let mut streams: HashMap<u32, mpsc::UnboundedSender<Option<Vec<u8>>>> = HashMap::new();
    while let Some(msg) = incoming.next().await {
        match msg? {
            Message::Text(t) if t.starts_with('o') => {
                let Ok(id) = t[1..].parse::<u32>() else { continue };
                let (tx, rx) = mpsc::unbounded_channel();
                streams.insert(id, tx);
                tokio::spawn(carry(id, local_port, rx, out_tx.clone()));
            }
            Message::Text(t) if t.starts_with('c') => {
                if let Some(tx) = t[1..].parse::<u32>().ok().and_then(|id| streams.remove(&id)) {
                    let _ = tx.send(None);
                }
            }
            Message::Binary(b) if b.len() >= 4 => {
                let id = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                if let Some(tx) = streams.get(&id) {
                    let _ = tx.send(Some(b[4..].to_vec()));
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    writer.abort();
    Ok(())
}

/// One stream: its local connection, both ways.
async fn carry(
    id: u32,
    port: u16,
    mut from_sandbox: mpsc::UnboundedReceiver<Option<Vec<u8>>>,
    out: mpsc::UnboundedSender<Message>,
) {
    let close = || Message::Text(format!("c{id}").into());
    let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await else {
        let _ = out.send(close());
        return;
    };
    let (mut rd, mut wr) = stream.into_split();
    tokio::spawn(async move {
        while let Some(Some(bytes)) = from_sandbox.recv().await {
            if wr.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    });
    let mut buf = vec![0u8; 65536];
    loop {
        match rd.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut frame = id.to_be_bytes().to_vec();
                frame.extend_from_slice(&buf[..n]);
                if out.send(Message::Binary(frame.into())).is_err() {
                    return;
                }
            }
        }
    }
    let _ = out.send(close());
}
