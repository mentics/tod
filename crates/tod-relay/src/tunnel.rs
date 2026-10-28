//! `/tunnel`: connections made to a loopback port in the sandbox, carried to
//! the client, which connects each one to a port on its own machine. This is
//! how `tod-cli` in a sandbox reaches the tod app that holds the data: the
//! app's own `tod-cli` relay checks the token and runs the command.
//!
//! Frames, both ways: text `o<id>` (relay only) opens a stream, binary
//! `<id: u32 BE><bytes>` carries data, text `c<id>` says the sender has
//! nothing more to write on it. A connection that arrives while no client is
//! attached is closed at once.
//!
//! **Forwards.** A client may also ask the relay to listen on further loopback
//! ports (text `f<port>`; answered `l<port>`, or `e<port>:<reason>`). A
//! connection to one opens its stream as `o<id>:<port>`, so the client knows
//! which of its own targets to dial; the relay never names a target, only the
//! sandbox-side port. The default port's streams stay plain `o<id>`.

use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};
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
    /// Ports a client asked this relay to forward; each keeps listening.
    forwards: Mutex<HashSet<u16>>,
    /// The client that most recently asked for each forwarded port. A
    /// connection to the port goes to it, not to the newest client: a
    /// terminal or agent session that attaches later carries no such forward
    /// and would close the connection.
    owners: Mutex<HashMap<u16, u64>>,
}

impl Tunnel {
    pub async fn listen(self: Arc<Self>, port: u16) {
        let listener = match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => l,
            Err(e) => {
                log!("tunnel: cannot listen on 127.0.0.1:{port}: {e}");
                crate::problem(tod_problems::Level::Error, &format!("tunnel: cannot listen on 127.0.0.1:{port}: {e}"));
                return;
            }
        };
        self.accept_loop(listener, None).await
    }

    /// Starts listening on `port` for a client's forward (a no-op when it
    /// already is). Ports below 1024 are refused: a sandbox's own services
    /// (and the relay) live there.
    async fn start_forward(self: &Arc<Self>, port: u16) -> Result<(), String> {
        if port < 1024 {
            return Err("ports below 1024 cannot be forwarded".into());
        }
        if self.forwards.lock().unwrap().contains(&port) {
            return Ok(());
        }
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
        self.forwards.lock().unwrap().insert(port);
        tokio::spawn(self.clone().accept_loop(listener, Some(port)));
        Ok(())
    }

    /// The client to carry a connection: for a forwarded port, the one that
    /// asked for it (if still attached); otherwise the newest.
    fn client_for(&self, forward: Option<u16>) -> Option<(u64, Out)> {
        let clients = self.clients.lock().unwrap();
        if let Some(owner) = forward.and_then(|p| self.owners.lock().unwrap().get(&p).copied()) {
            if let Some(found) = clients.iter().find(|(c, _)| *c == owner) {
                return Some(found.clone());
            }
        }
        clients.last().cloned()
    }

    async fn accept_loop(self: Arc<Self>, listener: TcpListener, forward: Option<u16>) {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let Some((conn, tx)) = self.client_for(forward) else {
                continue; // dropped: nobody to carry it to
            };
            let id = self.next.fetch_add(1, Ordering::SeqCst);
            let (itx, mut irx) = unbounded_channel::<Option<Vec<u8>>>();
            self.streams.lock().unwrap().insert(id, (conn, itx));
            let open = match forward {
                Some(port) => format!("o{id}:{port}"),
                None => format!("o{id}"),
            };
            if tx.send(Message::Text(open)).is_err() {
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
        self.clients.lock().unwrap().push((conn, tx.clone()));
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
                Ok(Message::Text(t)) if t.starts_with('f') => {
                    let Ok(port) = t[1..].parse::<u16>() else { continue };
                    let result = self.start_forward(port).await;
                    if result.is_ok() {
                        self.owners.lock().unwrap().insert(port, conn);
                    }
                    let _ = tx.send(Message::Text(match result {
                        Ok(()) => format!("l{port}"),
                        Err(e) => format!("e{port}:{e}"),
                    }));
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        self.clients.lock().unwrap().retain(|(c, _)| *c != conn);
        self.owners.lock().unwrap().retain(|_, c| *c != conn);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forwarded_port_goes_to_the_client_that_asked_for_it() {
        let t = Tunnel::default();
        let (a, _ra) = unbounded_channel();
        let (b, _rb) = unbounded_channel();
        // Client 1 holds the forward; client 2 (a terminal) attaches later.
        t.clients.lock().unwrap().push((1, a));
        t.owners.lock().unwrap().insert(15433, 1);
        t.clients.lock().unwrap().push((2, b));
        assert_eq!(t.client_for(Some(15433)).unwrap().0, 1);
        // Plain connections still go to the newest client.
        assert_eq!(t.client_for(None).unwrap().0, 2);
        assert_eq!(t.client_for(Some(15999)).unwrap().0, 2);
        // The owner gone: the newest carries it, as before.
        t.clients.lock().unwrap().retain(|(c, _)| *c != 1);
        assert_eq!(t.client_for(Some(15433)).unwrap().0, 2);
    }
}
