//! Connections to a sandbox's edge (Blaxel's CloudFront), ready ahead of time.
//!
//! Opening a relay connection is TCP, TLS, and then the WebSocket upgrade:
//! three round trips. Only the upgrade reaches Blaxel's gateway and the
//! sandbox; TCP and TLS end at CloudFront. So a process that will reconnect
//! soon (a parked terminal, Zed, an agent between turns) can hold a *spare*:
//! a TLS connection with no request on it, which neither wakes the sandbox nor
//! keeps it awake. The next connect sends its upgrade on the spare and saves
//! two round trips (about 35 ms measured, of which TLS is about 17).
//!
//! CloudFront closes an idle connection after about 30 s (open at 28 s, closed
//! at 30 s), so a spare is replaced before [`SPARE_MAX_AGE`] and never used
//! after it. It offers no 0-RTT (its session tickets allow no early
//! data), and no WebSocket over HTTP/2 (no `ENABLE_CONNECT_PROTOCOL`), so
//! HTTP/1.1 over a ready connection is the fewest round trips there are.

use anyhow::{Context, Result};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// A spare is replaced once this old, and not used after it.
const SPARE_MAX_AGE: Duration = Duration::from_secs(25);

/// Spares kept per process: an agent reattaching opens its `tod-cli` tunnel
/// and its own connection one after the other.
const SPARES: usize = 2;

/// One TLS configuration per process. Loading the OS's root certificates
/// costs a few milliseconds, and a shared configuration is what lets rustls
/// resume TLS sessions (less work for both sides, same round trips).
fn tls_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            crate::relay::init_tls();
            let mut roots = rustls::RootCertStore::empty();
            roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
            Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
        })
        .clone()
}

/// TCP and TLS to `host:port`, with nothing sent.
pub async fn open(host: &str, port: u16) -> Result<TlsStream<TcpStream>> {
    let tcp = TcpStream::connect((host, port)).await.with_context(|| format!("connect {host}:{port}"))?;
    tcp.set_nodelay(true)?;
    let name = ServerName::try_from(host.to_string()).with_context(|| format!("not a host name: {host}"))?;
    TlsConnector::from(tls_config()).connect(name, tcp).await.with_context(|| format!("TLS with {host}"))
}

struct Spare {
    host: String,
    port: u16,
    opened: Instant,
    /// The wall clock too: it keeps counting while the computer sleeps.
    opened_at: SystemTime,
    tls: TlsStream<TcpStream>,
}

impl Spare {
    fn new(host: String, port: u16, tls: TlsStream<TcpStream>) -> Self {
        Self { host, port, opened: Instant::now(), opened_at: SystemTime::now(), tls }
    }

    fn younger_than(&self, age: Duration) -> bool {
        self.opened.elapsed() < age && self.opened_at.elapsed().is_ok_and(|e| e < age)
    }

    /// Still open with nothing to read. Reading through TLS takes in the
    /// session tickets the edge sends after the handshake (which is what lets
    /// the next connection resume) and yields nothing; a closed connection
    /// reads as end of stream or an error.
    fn quiet(&mut self) -> bool {
        use tokio::io::AsyncRead;
        let mut buf = [0u8; 1];
        let mut buf = tokio::io::ReadBuf::new(&mut buf);
        let mut cx = std::task::Context::from_waker(futures_util::task::noop_waker_ref());
        std::pin::Pin::new(&mut self.tls).poll_read(&mut cx, &mut buf).is_pending()
    }

    fn for_host(&self, host: &str, port: u16) -> bool {
        self.host == host && self.port == port
    }
}

fn spares() -> &'static Mutex<Vec<Spare>> {
    static SPARES: OnceLock<Mutex<Vec<Spare>>> = OnceLock::new();
    SPARES.get_or_init(Default::default)
}

/// A spare for `host:port` young enough to trust, if there is one.
pub fn take(host: &str, port: u16) -> Option<TlsStream<TcpStream>> {
    let mut spares = spares().lock().unwrap();
    spares.retain_mut(|s| s.younger_than(SPARE_MAX_AGE) && s.quiet());
    let i = spares.iter().position(|s| s.for_host(host, port))?;
    Some(spares.swap_remove(i).tls)
}

/// Keeps [`SPARES`] spare connections to `url`'s host open, replacing any
/// that are getting old, in the background. Call it once a second or so
/// while a connection is parked; it returns at once. Needs a tokio runtime.
pub fn keep_spares(url: &str) {
    static OPENING: AtomicBool = AtomicBool::new(false);
    let Some((host, port)) = host_port(url) else { return };
    // Replaced a little before they are too old to use.
    let fresh = SPARE_MAX_AGE - Duration::from_secs(5);
    let missing = {
        let mut spares = spares().lock().unwrap();
        spares.retain_mut(|s| !s.for_host(&host, port) || (s.younger_than(fresh) && s.quiet()));
        SPARES.saturating_sub(spares.iter().filter(|s| s.for_host(&host, port)).count())
    };
    if missing == 0 || OPENING.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        for _ in 0..missing {
            match open(&host, port).await {
                Ok(tls) => spares().lock().unwrap().push(Spare::new(host.clone(), port, tls)),
                Err(_) => break,
            }
        }
        OPENING.store(false, Ordering::Release);
    });
}

/// Drops every spare, as when a process stops expecting to reconnect.
pub fn drop_spares() {
    spares().lock().unwrap().clear();
}

/// The host and port of a `wss://` URL; `None` for anything else.
pub fn host_port(url: &str) -> Option<(String, u16)> {
    let rest = url.strip_prefix("wss://")?;
    let authority = rest.split(['/', '?']).next()?;
    match authority.split_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), 443)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_host_and_port() {
        assert_eq!(host_port("wss://a.bl.run/port/2222/exec"), Some(("a.bl.run".into(), 443)));
        assert_eq!(host_port("wss://a.bl.run:8443/x"), Some(("a.bl.run".into(), 8443)));
        assert_eq!(host_port("wss://a.bl.run"), Some(("a.bl.run".into(), 443)));
        assert_eq!(host_port("ws://localhost:1/x"), None);
    }
}
