//! `/tunnel`: connections made to a loopback port in the sandbox, carried to
//! the client, which connects each one to a port on its own machine. This is
//! how `tod-cli` in a sandbox reaches the tod app that holds the data: the
//! app's own `tod-cli` relay checks the token and runs the command.
//!
//! Frames, both ways: text `o<id>` (relay only) opens a stream, binary
//! `<id: u32 BE><bytes>` carries data, text `c<id>` says the sender has
//! nothing more to write on it. A connection that arrives while no client is
//! attached is closed at once.

use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::Message;

use crate::server::{Out, WsIn};

pub const DEFAULT_PORT: u16 = 2223;

/// Data for a stream's local connection; `None` shuts down its write side.
type StreamIn = UnboundedSender<Option<Vec<u8>>>;

#[derive(Default)]
pub struct Tunnel {
    /// Attached clients, newest last; new connections go to the newest.
    clients: Mutex<Vec<(u64, Out)>>,
    /// Open streams, by id, with the client carrying them. An entry goes when
    /// the client closes its side or detaches.
    streams: Mutex<HashMap<u32, (u64, StreamIn)>>,
    next: AtomicU32,
}

impl Tunnel {
    pub async fn listen(self: Arc<Self>, port: u16) {
        let listener = match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("tunnel: cannot listen on 127.0.0.1:{port}: {e}");
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let Some((conn, tx)) = self.clients.lock().unwrap().last().cloned() else {
                continue; // dropped: nobody to carry it to
            };
            let id = self.next.fetch_add(1, Ordering::SeqCst);
            let (itx, mut irx) = unbounded_channel::<Option<Vec<u8>>>();
            self.streams.lock().unwrap().insert(id, (conn, itx));
            if tx.send(Message::Text(format!("o{id}"))).is_err() {
                self.streams.lock().unwrap().remove(&id);
                continue;
            }
            let (mut rd, mut wr) = stream.into_split();
            tokio::spawn(async move {
                while let Some(Some(bytes)) = irx.recv().await {
                    if wr.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                let _ = wr.shutdown().await;
            });
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                loop {
                    match rd.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut frame = id.to_be_bytes().to_vec();
                            frame.extend_from_slice(&buf[..n]);
                            if tx.send(Message::Binary(frame)).is_err() {
                                break;
                            }
                        }
                    }
                }
                let _ = tx.send(Message::Text(format!("c{id}")));
            });
        }
    }

    pub async fn serve(self: Arc<Self>, conn: u64, tx: Out, mut incoming: WsIn) {
        self.clients.lock().unwrap().push((conn, tx));
        while let Some(msg) = incoming.next().await {
            match msg {
                Ok(Message::Binary(b)) if b.len() >= 4 => {
                    let id = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                    if let Some((_, s)) = self.streams.lock().unwrap().get(&id) {
                        let _ = s.send(Some(b[4..].to_vec()));
                    }
                }
                Ok(Message::Text(t)) if t.starts_with('c') => {
                    if let Ok(id) = t[1..].parse::<u32>() {
                        if let Some((_, s)) = self.streams.lock().unwrap().remove(&id) {
                            let _ = s.send(None);
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        self.clients.lock().unwrap().retain(|(c, _)| *c != conn);
        // Streams this client carried end with it.
        self.streams.lock().unwrap().retain(|_, (c, s)| {
            if *c == conn {
                let _ = s.send(None);
                false
            } else {
                true
            }
        });
    }
}
