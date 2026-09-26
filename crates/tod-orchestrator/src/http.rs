//! The least HTTP/1.1 the orchestrator needs: one request per connection,
//! bodies by `Content-Length` only, replies with `Connection: close`.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

/// Largest body accepted (a seed snapshot is the biggest).
pub const MAX_BODY: usize = 512 * 1024 * 1024;
const MAX_HEADERS: usize = 100;

pub struct Request {
    pub method: String,
    /// The path without the query string.
    pub path: String,
    pub query: String,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v)
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        let mut body = body.into();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        Self { status, content_type: "text/plain; charset=utf-8", body: body.into_bytes() }
    }

    pub fn bytes(body: Vec<u8>) -> Self {
        Self { status: 200, content_type: "application/octet-stream", body }
    }
}

pub fn read_request(stream: &TcpStream) -> Result<Request> {
    let mut reader = BufReader::new(stream);
    let line = read_line(&mut reader)?;
    let mut parts = line.split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        bail!("bad request line {line:?}");
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut headers = Vec::new();
    loop {
        let line = read_line(&mut reader)?;
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            bail!("too many headers");
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let mut request = Request {
        method: method.to_string(),
        path: path.to_string(),
        query: query.to_string(),
        headers,
        body: Vec::new(),
    };
    if request.header("transfer-encoding").is_some() {
        bail!("chunked bodies are not supported; send Content-Length");
    }
    let len: usize = match request.header("content-length") {
        Some(v) => v.parse().context("Content-Length")?,
        None => 0,
    };
    if len > MAX_BODY {
        bail!("body too large ({len} bytes)");
    }
    request.body = vec![0; len];
    reader.read_exact(&mut request.body).context("read body")?;
    Ok(request)
}

fn read_line(reader: &mut impl BufRead) -> Result<String> {
    let mut line = String::new();
    // Bounded: a header line longer than this is not one of ours.
    reader.by_ref().take(16 * 1024).read_line(&mut line)?;
    if !line.ends_with('\n') {
        bail!("truncated request");
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

pub fn write_response(mut stream: &TcpStream, response: &Response) -> Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        _ => "",
    };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()?;
    Ok(())
}
