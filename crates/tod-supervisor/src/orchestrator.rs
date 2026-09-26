//! The orchestrator's routes the supervisor uses (see `tod_orchestrator`):
//! the snapshot and the change feed for its copy of the user's database, and
//! the node's transcripts. Requests go through the sandbox's proxy (from
//! `HTTPS_PROXY`), which adds the credentials; failures are retried with
//! backoff, as the `tod-cli` shim does.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;
use tod_store::fleet::cli_relay::{NODE_HEADER, USER_HEADER};
use tod_store::sync::Change;

#[derive(Clone)]
pub struct Orchestrator {
    /// E.g. `https://<orchestrator>.bl.run/port/8080`, no trailing `/`.
    base: String,
    user: String,
    node: String,
    agent: ureq::Agent,
}

/// `GET /users/<u>/changes?after=` reply.
#[derive(Debug, Deserialize)]
pub struct Feed {
    pub last_seq: i64,
    pub changes: Vec<Change>,
}

/// The orchestrator's base URL from its `/cli` URL (`TOD_ORCHESTRATOR_CLI_URL`).
pub fn base_from_cli_url(cli_url: &str) -> String {
    let url = cli_url.trim_end_matches('/');
    url.strip_suffix("/cli").unwrap_or(url).to_string()
}

fn agent() -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(300)));
    // The sandbox's proxy terminates TLS with its own CA, which is in the
    // system bundle `SSL_CERT_FILE` names.
    if let Some(certs) = std::env::var_os("SSL_CERT_FILE")
        .and_then(|path| std::fs::read(path).ok())
        .map(|pem| {
            ureq::tls::parse_pem(&pem)
                .filter_map(|item| match item {
                    Ok(ureq::tls::PemItem::Certificate(cert)) => Some(cert),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .filter(|certs| !certs.is_empty())
    {
        config = config.tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::Specific(std::sync::Arc::new(certs)))
                .build(),
        );
    }
    config.build().into()
}

impl Orchestrator {
    pub fn new(base: impl Into<String>, user: impl Into<String>, node: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            user: user.into(),
            node: node.into(),
            agent: agent(),
        }
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    fn url(&self, path: &str) -> String {
        format!("{}/users/{}{path}", self.base, self.user)
    }

    /// Status and body, retrying connection errors and 5xx/407/429.
    fn request(&self, method: &str, url: &str, body: Option<&[u8]>) -> Result<(u16, Vec<u8>)> {
        let mut delay = Duration::from_millis(250);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = match body {
                Some(body) => self
                    .agent
                    .post(url)
                    .header(USER_HEADER, &self.user)
                    .header(NODE_HEADER, &self.node)
                    .send(body),
                None if method == "GET" => self
                    .agent
                    .get(url)
                    .header(USER_HEADER, &self.user)
                    .header(NODE_HEADER, &self.node)
                    .call(),
                None => self
                    .agent
                    .post(url)
                    .header(USER_HEADER, &self.user)
                    .header(NODE_HEADER, &self.node)
                    .send_empty(),
            };
            let retry = match result {
                Ok(mut resp) => {
                    let status = resp.status().as_u16();
                    let mut bytes = Vec::new();
                    resp.body_mut()
                        .as_reader()
                        .read_to_end(&mut bytes)
                        .with_context(|| format!("read {method} {url}"))?;
                    if !(status >= 500 || status == 407 || status == 429) {
                        return Ok((status, bytes));
                    }
                    format!("{status}: {}", String::from_utf8_lossy(&bytes).trim())
                }
                Err(err) => format!("{err}"),
            };
            if attempt >= 6 {
                bail!("{method} {url}: {retry}");
            }
            tracing::warn!(%url, attempt, %retry, "orchestrator request failed; retrying");
            std::thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(8));
        }
    }

    fn ok(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<Vec<u8>> {
        let url = self.url(path);
        let (status, bytes) = self.request(method, &url, body)?;
        if status != 200 {
            bail!("{method} {url}: {status}: {}", String::from_utf8_lossy(&bytes).trim());
        }
        Ok(bytes)
    }

    /// The user's whole database.
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        self.ok("GET", "/snapshot", None)
    }

    pub fn changes_after(&self, after: i64) -> Result<Feed> {
        let bytes = self.ok("GET", &format!("/changes?after={after}"), None)?;
        serde_json::from_slice(&bytes).context("the orchestrator's change feed")
    }

    /// Sends this copy's own changes (logged there, so the app gets them).
    pub fn push_changes(&self, changes: &[Change]) -> Result<()> {
        let body = serde_json::to_vec(changes)?;
        self.ok("POST", "/changes?from=supervisor", Some(&body))?;
        Ok(())
    }

    fn transcripts_path(&self, name: Option<&str>) -> String {
        match name {
            Some(name) => format!("/nodes/{}/transcripts/{name}", self.node),
            None => format!("/nodes/{}/transcripts", self.node),
        }
    }

    pub fn list_transcripts(&self) -> Result<Vec<(String, u64)>> {
        #[derive(Deserialize)]
        struct Entry {
            name: String,
            size: u64,
        }
        #[derive(Deserialize)]
        struct List {
            transcripts: Vec<Entry>,
        }
        let bytes = self.ok("GET", &self.transcripts_path(None), None)?;
        let list: List = serde_json::from_slice(&bytes)?;
        Ok(list.transcripts.into_iter().map(|e| (e.name, e.size)).collect())
    }

    pub fn fetch_transcript(&self, name: &str) -> Result<Vec<u8>> {
        self.ok("GET", &self.transcripts_path(Some(name)), None)
    }

    /// Appends `bytes` at `offset`. `Err` of the copy's real size when it is
    /// not `offset` long.
    pub fn append_transcript(&self, name: &str, offset: u64, bytes: &[u8]) -> Result<std::result::Result<u64, u64>> {
        let url = self.url(&format!("{}?offset={offset}", self.transcripts_path(Some(name))));
        let (status, reply) = self.request("POST", &url, Some(bytes))?;
        let size = || -> Result<u64> {
            let v: serde_json::Value = serde_json::from_slice(&reply)?;
            v["size"].as_u64().context("size")
        };
        match status {
            200 => Ok(Ok(size()?)),
            409 => Ok(Err(size()?)),
            _ => bail!("POST {url}: {status}: {}", String::from_utf8_lossy(&reply).trim()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_drops_the_cli_route() {
        assert_eq!(base_from_cli_url("https://o.bl.run/port/8080/cli"), "https://o.bl.run/port/8080");
        assert_eq!(base_from_cli_url("http://127.0.0.1:9/cli/"), "http://127.0.0.1:9");
        assert_eq!(base_from_cli_url("http://h:1"), "http://h:1");
    }
}
