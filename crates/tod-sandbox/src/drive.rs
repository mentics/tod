//! Agent Drive: where a node's transcripts live (design: "Transcripts").
//!
//! One drive per workspace holds every node's transcripts under
//! `users/<user>/nodes/<node>/transcripts/<name>.jsonl`. Each node's sandbox
//! mounts only its own folder at [`MOUNT_PATH`], so the supervisor appends to
//! the files as ordinary files (the mount is POSIX; the drive's S3 endpoint
//! has no append). The app reads them from outside through the drive's S3
//! endpoint ([`DriveReader`]), which does not wake the sandbox, and can ask
//! for a byte range, so a transcript is followed from where it left off.
//!
//! Verified live (2026-09-30, workspace `testspace-358401`): a mount made on
//! a sandbox survives its standby; mounting a folder that does not exist
//! creates it; mounting what is already mounted succeeds; a line appended
//! through the mount is read over S3 at once, with the sandbox asleep; and
//! the JWT from the drive's `access-token` endpoint, sent as `Authorization:
//! Bearer`, is accepted by the S3 endpoint (including `Range`), so there is
//! no SigV4 signing and no API-key id to look up. The JWT lasts 30 minutes.

use crate::blaxel::Blaxel;
use anyhow::{Context, Result, bail};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The workspace's transcript drive.
pub const DRIVE_NAME: &str = "tod-transcripts";
/// Where a node's sandbox sees its transcripts folder. The supervisor is
/// told by [`TRANSCRIPTS_DIR_ENV`].
pub const MOUNT_PATH: &str = "/mnt/tod-transcripts";
/// Set in the supervisor's environment when the drive is mounted: the
/// directory it mirrors into instead of the orchestrator.
pub const TRANSCRIPTS_DIR_ENV: &str = "TOD_TRANSCRIPTS_DIR";

/// The folder of a node's transcripts on the drive, without a leading slash
/// (an S3 key prefix, with a trailing one).
pub fn node_prefix(user: &str, node: &str) -> String {
    format!("users/{user}/nodes/{node}/transcripts/")
}

/// Makes the drive and mounts the node's folder in its sandbox at `url`.
/// `Err` (the account has no Agent Drive, another region) means the node
/// keeps its transcripts with the orchestrator.
pub fn mount_for_node(bx: &Blaxel, url: &str, region: &str, user: &str, node: &str) -> Result<()> {
    bx.ensure_drive(DRIVE_NAME, region)?;
    let folder = format!("/{}", node_prefix(user, node).trim_end_matches('/'));
    bx.mount_drive(url, DRIVE_NAME, MOUNT_PATH, &folder)
}

/// One transcript on the drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// The file's name without the folder and without `.jsonl`.
    pub name: String,
    pub size: u64,
}

/// Reads a drive's S3 endpoint with a short-lived token it renews itself.
pub struct DriveReader<'a> {
    bx: &'a Blaxel,
    drive: String,
    /// `{endpoint}/{bucket}`.
    s3_url: String,
    agent: ureq::Agent,
    token: Mutex<Option<(String, Instant)>>,
}

impl<'a> DriveReader<'a> {
    /// A reader of the workspace's transcript drive; `None` when it does not exist.
    pub fn open(bx: &'a Blaxel) -> Result<Option<Self>> {
        Self::open_drive(bx, DRIVE_NAME)
    }

    pub fn open_drive(bx: &'a Blaxel, drive: &str) -> Result<Option<Self>> {
        let Some(info) = bx.get_drive(drive)? else { return Ok(None) };
        let s3_url = info.s3_url.with_context(|| format!("drive {drive} has no S3 endpoint"))?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Ok(Some(Self { bx, drive: drive.to_string(), s3_url, agent, token: Mutex::new(None) }))
    }

    fn bearer(&self) -> Result<String> {
        let mut held = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((token, until)) = held.as_ref()
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        let (token, lifetime) = self.bx.drive_access_token(&self.drive)?;
        // Renewed a minute early.
        let until = Instant::now() + Duration::from_secs(lifetime.saturating_sub(60).max(1));
        *held = Some((token.clone(), until));
        Ok(token)
    }

    fn get(&self, url: &str, range_from: Option<u64>) -> Result<(u16, ureq::Body)> {
        let mut req = self.agent.get(url).header("Authorization", &format!("Bearer {}", self.bearer()?));
        if let Some(from) = range_from {
            req = req.header("Range", &format!("bytes={from}-"));
        }
        let resp = req.call().with_context(|| format!("drive S3 {url}"))?;
        let status = resp.status().as_u16();
        Ok((status, resp.into_body()))
    }

    /// The node's transcripts.
    pub fn list(&self, user: &str, node: &str) -> Result<Vec<Object>> {
        let prefix = node_prefix(user, node);
        let mut out = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut url = format!("{}?list-type=2&prefix={}", self.s3_url, encode_query(&prefix));
            if let Some(token) = &continuation {
                url.push_str(&format!("&continuation-token={}", encode_query(token)));
            }
            let (status, mut body) = self.get(&url, None)?;
            let xml = body.read_to_string().unwrap_or_default();
            if status != 200 {
                bail!("list drive transcripts: {status}: {}", xml.chars().take(300).collect::<String>());
            }
            let (objects, next) = parse_listing(&xml, &prefix);
            out.extend(objects);
            match next {
                Some(token) => continuation = Some(token),
                None => return Ok(out),
            }
        }
    }

    /// `name` from byte `from` on; `None` when there is no such transcript.
    pub fn read(&self, user: &str, node: &str, name: &str, from: u64) -> Result<Option<Vec<u8>>> {
        use std::io::Read;
        let url = format!("{}/{}{name}.jsonl", self.s3_url, node_prefix(user, node));
        let (status, mut body) = self.get(&url, (from > 0).then_some(from))?;
        match status {
            404 => Ok(None),
            // The copy is not as long as `from`: nothing new.
            416 => Ok(Some(Vec::new())),
            200 | 206 => {
                let mut bytes = Vec::new();
                body.as_reader().read_to_end(&mut bytes)?;
                // A server that ignored the range sent it all.
                if status == 200 && from > 0 {
                    bytes.drain(..(from as usize).min(bytes.len()));
                }
                Ok(Some(bytes))
            }
            other => bail!("read drive transcript {name}: {other}"),
        }
    }
}

/// Percent-encodes a query value (everything but unreserved characters).
fn encode_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn tag<'x>(xml: &'x str, name: &str) -> Option<&'x str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(&xml[start..end])
}

/// The `.jsonl` objects under `prefix` in a ListObjectsV2 reply, and the
/// token for the next page when it was cut off.
fn parse_listing(xml: &str, prefix: &str) -> (Vec<Object>, Option<String>) {
    let mut objects = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<Contents>") {
        let after = &rest[i..];
        let end = after.find("</Contents>").unwrap_or(after.len());
        let entry = &after[..end];
        if let (Some(key), Some(size)) = (tag(entry, "Key"), tag(entry, "Size"))
            && let Some(name) = key.strip_prefix(prefix).and_then(|k| k.strip_suffix(".jsonl"))
            && !name.is_empty()
            && !name.contains('/')
            && let Ok(size) = size.parse()
        {
            objects.push(Object { name: name.to_string(), size });
        }
        rest = &after[end..];
    }
    let next = (tag(xml, "IsTruncated") == Some("true"))
        .then(|| tag(xml, "NextContinuationToken").map(str::to_string))
        .flatten();
    (objects, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_has_its_own_folder() {
        assert_eq!(node_prefix("u1", "n1"), "users/u1/nodes/n1/transcripts/");
    }

    #[test]
    fn a_listing_gives_the_transcripts_in_the_folder() {
        let xml = "<ListBucketResult><Name>b</Name><IsTruncated>false</IsTruncated>\
            <Contents><Key>users/u/nodes/n/transcripts/-workspace-repo__abc.jsonl</Key><Size>24</Size></Contents>\
            <Contents><Key>users/u/nodes/n/transcripts/sub/deep.jsonl</Key><Size>1</Size></Contents>\
            <Contents><Key>users/u/nodes/n/transcripts/notes.txt</Key><Size>2</Size></Contents>\
            <Contents><Key>users/u/nodes/other/transcripts/x.jsonl</Key><Size>3</Size></Contents>\
            </ListBucketResult>";
        let (objects, next) = parse_listing(xml, "users/u/nodes/n/transcripts/");
        assert_eq!(objects, vec![Object { name: "-workspace-repo__abc".into(), size: 24 }]);
        assert_eq!(next, None);
    }

    #[test]
    fn a_cut_off_listing_says_where_to_go_on() {
        let xml = "<IsTruncated>true</IsTruncated><NextContinuationToken>a+b/c=</NextContinuationToken>";
        assert_eq!(parse_listing(xml, "p/").1.as_deref(), Some("a+b/c="));
        assert_eq!(encode_query("a+b/c="), "a%2Bb%2Fc%3D");
    }
}
