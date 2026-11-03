//! The orchestrator's routes the supervisor uses (see `tod_orchestrator`):
//! the snapshot and the change feed for its node's slice of the user's
//! database, and
//! the node's transcripts. Requests go through the sandbox's proxy (from
//! `HTTPS_PROXY`), which adds the credentials; failures are retried with
//! backoff, as the `tod-cli` shim does.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;
use tod_store::fleet::cli_relay::{NODE_HEADER, USER_HEADER};
use tod_store::sync::{CLIENT_HEADER, Change};

/// The refusal for a 426: the orchestrator found this supervisor newer than
/// itself. Named, not retried: only a redeploy of the orchestrator fixes it.
pub fn older_orchestrator_message(body: &str) -> String {
    format!("the orchestrator is older than this supervisor: redeploy it (`scripts/deploy.sh`, or the app does so at launch): {}", body.trim())
}

#[derive(Clone)]
pub struct Orchestrator {
    /// E.g. `https://<orchestrator>.bl.run/port/8090`, no trailing `/`.
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
    /// Which seeding of the orchestrator's database the feed follows.
    #[serde(default)]
    pub seed_id: Option<String>,
    /// Which slice the feed is confined to (the node's ancestor chain); an
    /// orchestrator that does not cut slices sends none.
    #[serde(default)]
    pub slice_id: Option<String>,
}

/// The orchestrator's base URL from its `/cli` URL (`TOD_ORCHESTRATOR_CLI_URL`).
pub fn base_from_cli_url(cli_url: &str) -> String {
    let url = cli_url.trim_end_matches('/');
    url.strip_suffix("/cli").unwrap_or(url).to_string()
}

/// Through the sandbox's proxy, trusting its CA (`SSL_CERT_FILE`).
fn agent() -> ureq::Agent {
    tod_core::scheduler::sandbox_agent(Duration::from_secs(300))
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

    /// This supervisor as a sync client (`X-Tod-Client`): its pushes reach
    /// the app and never come back to it.
    pub fn client(&self) -> String {
        format!("supervisor-{}", self.node)
    }

    /// The identity and version-contract headers every request carries.
    fn stamp<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let mut req = req
            .header(USER_HEADER, &self.user)
            .header(NODE_HEADER, &self.node)
            .header(CLIENT_HEADER, &self.client());
        for (name, value) in tod_core::cloud_sync::my_contract().headers() {
            req = req.header(name, &value);
        }
        req
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
                Some(body) => self.stamp(self.agent.post(url)).send(body),
                None if method == "GET" => self.stamp(self.agent.get(url)).call(),
                None => self.stamp(self.agent.post(url)).send_empty(),
            };
            let retry = match result {
                Ok(mut resp) => {
                    let status = resp.status().as_u16();
                    let mut bytes = Vec::new();
                    resp.body_mut()
                        .as_reader()
                        .read_to_end(&mut bytes)
                        .with_context(|| format!("read {method} {url}"))?;
                    if status == 426 {
                        bail!("{}", older_orchestrator_message(&String::from_utf8_lossy(&bytes)));
                    }
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

    /// This node's slice of the user's database (`tod_store::sync::slice`),
    /// which records its own cursor and slice id.
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        self.ok("GET", "/snapshot?slice=1", None)
    }

    /// The changes in this node's slice after `after`.
    pub fn changes_after(&self, after: i64) -> Result<Feed> {
        let bytes = self.ok("GET", &format!("/changes?after={after}&slice=1"), None)?;
        serde_json::from_slice(&bytes).context("the orchestrator's change feed")
    }

    /// Sends this copy's own changes (tagged with [`Self::client`] there).
    pub fn push_changes(&self, changes: &[Change]) -> Result<()> {
        let body = serde_json::to_vec(changes)?;
        self.ok("POST", "/changes", Some(&body))?;
        Ok(())
    }

    /// Tells the app this supervisor has begun a wake, before it has a local
    /// copy of the database to record that in: sends the node's
    /// `cloud_activity` row (`what = "starting"`) as one change, directly. The
    /// seed that follows carries the row into the copy, so the first beat
    /// recorded there updates it. The row's foreign key is checked by the
    /// orchestrator, which holds the node because the app sent it.
    pub fn push_starting_beat(&self, node: uuid::Uuid, now_ms: i64) -> Result<()> {
        use tod_store::sync::{Row, SqlValue};
        let hex: String = tod_store::outline::uuid_blob::uuid_to_blob(node).iter().map(|b| format!("{b:02x}")).collect();
        let key: Row = [("node_id".to_string(), SqlValue::Blob(hex.clone()))].into();
        let mut after = key.clone();
        after.insert("started_at".into(), SqlValue::Integer(now_ms));
        after.insert("heartbeat_at".into(), SqlValue::Integer(now_ms));
        after.insert("what".into(), SqlValue::Text(tod_store::cloud_activity::STARTING_WHAT.into()));
        after.insert("stopped".into(), SqlValue::Null);
        // `before` is unknown (a row from the last wake may be there): the
        // orchestrator notes the difference and applies this row.
        self.push_changes(&[Change {
            seq: 0,
            node_id: Some(node),
            table: tod_store::cloud_activity::TABLE.to_string(),
            key,
            before: None,
            after: Some(after),
        }])
        .context("telling the app this supervisor has started")
    }

    /// Sends problems collected in this node's sandbox (`tod_problems`) to be
    /// shown in the app's log panel. Callers send these only along with a
    /// request they are making anyway: this must not be the reason the
    /// orchestrator is contacted.
    pub fn push_problems(&self, problems: &[tod_problems::Problem]) -> Result<()> {
        let body = serde_json::to_vec(&problems.iter().map(tod_problems::to_json).collect::<Vec<_>>())?;
        self.ok("POST", &format!("/nodes/{}/problems", self.node), Some(&body))?;
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// Serves one request with `status`/`body`; returns the base URL and the
    /// request's lowercased header block.
    fn stub(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                head.push_str(&line.to_lowercase());
            }
            write!(sock, "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
            head
        });
        (base, h)
    }

    #[test]
    fn requests_carry_the_version_contract() {
        let (base, h) = stub(200, "{}");
        Orchestrator::new(base, "u", "n").ok("GET", "/snapshot", None).unwrap();
        let head = h.join().unwrap();
        assert!(head.contains("x-tod-wire: "), "{head}");
        assert!(head.contains("x-tod-schema: "), "{head}");
    }

    #[test]
    fn a_426_is_a_named_error_not_a_retry() {
        let (base, h) = stub(426, "this orchestrator is older");
        let err = Orchestrator::new(base, "u", "n").ok("GET", "/snapshot", None).unwrap_err().to_string();
        h.join().unwrap();
        assert!(err.contains("orchestrator is older than this supervisor: redeploy it"), "{err}");
    }

    #[test]
    fn base_url_drops_the_cli_route() {
        assert_eq!(base_from_cli_url("https://o.bl.run/port/8090/cli"), "https://o.bl.run/port/8090");
        assert_eq!(base_from_cli_url("http://127.0.0.1:9/cli/"), "http://127.0.0.1:9");
        assert_eq!(base_from_cli_url("http://h:1"), "http://h:1");
    }
}
