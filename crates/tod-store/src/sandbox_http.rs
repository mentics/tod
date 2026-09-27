//! HTTP from inside an autonomous node's sandbox, through its proxy.
//!
//! Every outbound request there goes through Blaxel's proxy (`HTTPS_PROXY`,
//! `http://localhost:49152`), which terminates TLS with its own CA and adds
//! the user's credentials per destination (`tod_sandbox::node::proxy_rules`).
//! So a client there must use the proxy *and* trust that CA, which is in the
//! bundle `SSL_CERT_FILE` names. Design: `doc/cloud-sandboxes/autonomous-nodes.md`
//! (Credentials).

use std::sync::Arc;
use std::time::Duration;

/// An HTTP agent for calls through the sandbox's proxy (from `HTTPS_PROXY`),
/// trusting the CA bundle `SSL_CERT_FILE` names when it is set. Elsewhere,
/// the environment's proxy (if any) and the default roots. Status codes are
/// never errors: callers read them.
pub fn sandbox_agent(timeout: Duration) -> ureq::Agent {
    let pem = std::env::var_os("SSL_CERT_FILE").and_then(|path| std::fs::read(path).ok());
    agent_with(sandbox_proxy(), pem.as_deref(), timeout)
}

/// [`sandbox_agent`] with its proxy and CA bundle given.
pub fn agent_with(proxy: Option<ureq::Proxy>, ca_pem: Option<&[u8]>, timeout: Duration) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .proxy(proxy);
    let certs = ca_pem.map(certificates).unwrap_or_default();
    if !certs.is_empty() {
        config = config.tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::Specific(Arc::new(certs)))
                .build(),
        );
    }
    config.build().into()
}

/// The certificates in a PEM bundle (anything else in it is skipped).
pub fn certificates(pem: &[u8]) -> Vec<ureq::tls::Certificate<'static>> {
    ureq::tls::parse_pem(pem)
        .filter_map(|item| match item {
            Ok(ureq::tls::PemItem::Certificate(cert)) => Some(cert),
            _ => None,
        })
        .collect()
}

/// The proxy from the environment (`HTTPS_PROXY` and the rest, with
/// `NO_PROXY`), with a `localhost` host replaced by `127.0.0.1`: a node
/// sandbox's proxy is `http://localhost:49152`, and its image may have no
/// `/etc/hosts`, where musl (unlike glibc) then cannot resolve `localhost`.
fn sandbox_proxy() -> Option<ureq::Proxy> {
    let proxy = ureq::Proxy::try_from_env()?;
    if !proxy.host().eq_ignore_ascii_case("localhost") {
        return Some(proxy);
    }
    let mut builder = ureq::Proxy::builder(proxy.protocol()).host("127.0.0.1").port(proxy.port());
    if let Some(user) = proxy.username() {
        builder = builder.username(user);
    }
    if let Some(password) = proxy.password() {
        builder = builder.password(password);
    }
    let no_proxy = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")).unwrap_or_default();
    for entry in no_proxy.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        builder = builder.no_proxy(entry);
    }
    builder.build().ok().or(Some(proxy))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A PEM bundle of two certificates (their contents are never checked
    /// here) and a key, which is skipped.
    const BUNDLE: &str = "-----BEGIN CERTIFICATE-----\nMIIBAA==\n-----END CERTIFICATE-----\n\
        -----BEGIN PRIVATE KEY-----\nMIIBAA==\n-----END PRIVATE KEY-----\n\
        -----BEGIN CERTIFICATE-----\nMIIBAQ==\n-----END CERTIFICATE-----\n";

    #[test]
    fn a_bundle_gives_its_certificates_only() {
        assert_eq!(certificates(BUNDLE.as_bytes()).len(), 2);
        assert!(certificates(b"not pem").is_empty());
    }

    /// A one-shot HTTP proxy on loopback: answers `CONNECT` with 200, then
    /// reads the tunnelled request and answers it with `status` and `body`.
    /// Sends back every line it read, the `CONNECT` included.
    pub(crate) fn fake_proxy(status: u16, body: &'static str) -> (ureq::Proxy, mpsc::Receiver<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut seen = Vec::new();
            let head = |reader: &mut BufReader<_>, seen: &mut Vec<String>| {
                let mut first = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let line = line.trim_end().to_string();
                    if line.is_empty() {
                        break;
                    }
                    first.get_or_insert_with(|| line.clone());
                    seen.push(line);
                }
                first.unwrap_or_default()
            };
            let first = head(&mut reader, &mut seen);
            if first.starts_with("CONNECT ") {
                writer.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").unwrap();
                head(&mut reader, &mut seen);
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            writer.write_all(reply.as_bytes()).unwrap();
            let _ = tx.send(seen);
        });
        (ureq::Proxy::new(&format!("http://127.0.0.1:{port}")).unwrap(), rx)
    }

    #[test]
    fn requests_go_through_the_proxy_given() {
        let (proxy, seen) = fake_proxy(200, "{}");
        let agent = agent_with(Some(proxy), Some(BUNDLE.as_bytes()), Duration::from_secs(10));
        let resp = agent.get("http://api.example.test/ping").call().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let seen = seen.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(seen[0], "CONNECT api.example.test:80 HTTP/1.1", "{seen:?}");
        assert!(seen.iter().any(|l| l.starts_with("GET /ping")), "{seen:?}");
    }
}
