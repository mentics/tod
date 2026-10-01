//! Local HTTP server that serves a mockup and its live-reload stream.
//! Design: `doc/ui/visual-design-browser.md` section 4. One server per app
//! run on `127.0.0.1` (port 0); a session is a mockup path plus a random token
//! that sits in the URL path. No feedback route yet (item F1).

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BRIDGE_JS: &str = include_str!("bridge.js");
const DEBOUNCE: Duration = Duration::from_millis(150);
const KEEPALIVE: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Reload,
    Navigate,
    Closed,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::Reload => "reload",
            Event::Navigate => "navigate",
            Event::Closed => "closed",
        }
    }
}

struct Session {
    path: PathBuf,
    subs: Vec<Sender<Event>>,
    live: Arc<AtomicUsize>,
    watcher: Option<RecommendedWatcher>,
}

type Sessions = Arc<Mutex<HashMap<String, Session>>>;

pub struct DesignServer {
    port: u16,
    sessions: Sessions,
    stop: Arc<AtomicBool>,
}

impl DesignServer {
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let sessions: Sessions = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (sessions.clone(), stop.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if st.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(conn) = conn {
                    let s = s.clone();
                    std::thread::spawn(move || {
                        let _ = handle(conn, port, &s);
                    });
                }
            }
        });
        Ok(Self { port, sessions, stop })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Opens a session on `path` and returns the URL the browser should load.
    pub fn open(&self, path: &Path) -> String {
        let token = new_token();
        let watcher = watch(&self.sessions, &token, path);
        self.sessions.lock().unwrap().insert(
            token.clone(),
            Session { path: path.to_path_buf(), subs: Vec::new(), live: Arc::default(), watcher },
        );
        format!("http://127.0.0.1:{}/d/{}/", self.port, token)
    }

    /// Points a session at another mockup; connected windows navigate.
    pub fn set_path(&self, url_or_token: &str, path: &Path) {
        let token = token_of(url_or_token);
        let watcher = watch(&self.sessions, &token, path);
        let mut map = self.sessions.lock().unwrap();
        if let Some(s) = map.get_mut(&token) {
            s.path = path.to_path_buf();
            s.watcher = watcher;
            broadcast(s, Event::Navigate);
        }
    }

    pub fn close(&self, url_or_token: &str) {
        if let Some(mut s) = self.sessions.lock().unwrap().remove(&token_of(url_or_token)) {
            broadcast(&mut s, Event::Closed);
        }
    }

    /// Whether a browser window currently holds the session's event stream.
    pub fn connected(&self, url_or_token: &str) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(&token_of(url_or_token))
            .is_some_and(|s| s.live.load(Ordering::SeqCst) > 0)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

impl Drop for DesignServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
        for s in self.sessions.lock().unwrap().values_mut() {
            broadcast(s, Event::Closed);
        }
    }
}

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Accepts a full session URL or a bare token.
fn token_of(s: &str) -> String {
    match s.split("/d/").nth(1) {
        Some(rest) => rest.split('/').next().unwrap_or("").to_string(),
        None => s.to_string(),
    }
}

fn broadcast(s: &mut Session, ev: Event) {
    s.subs.retain(|tx| tx.send(ev).is_ok());
}

/// Watches the directory of `path`; any change (debounced) sends `reload`.
fn watch(sessions: &Sessions, token: &str, path: &Path) -> Option<RecommendedWatcher> {
    let dir = path.parent()?.to_path_buf();
    let (tx, rx) = mpsc::channel::<()>();
    let mut w = notify::recommended_watcher(move |r: notify::Result<notify::Event>| {
        if r.is_ok() {
            let _ = tx.send(());
        }
    })
    .ok()?;
    w.watch(&dir, RecursiveMode::Recursive).ok()?;
    let (sessions, token) = (sessions.clone(), token.to_string());
    std::thread::spawn(move || {
        // Ends when the watcher (which owns the sender) is dropped.
        while rx.recv().is_ok() {
            while rx.recv_timeout(DEBOUNCE).is_ok() {}
            match sessions.lock().unwrap().get_mut(&token) {
                Some(s) => broadcast(s, Event::Reload),
                None => break,
            }
        }
    });
    Some(w)
}

/// The one place that decides whether `rel` may be served from `root`:
/// refuses `..`, absolute parts, and symlinks that leave the directory.
pub fn resolve_within(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let full = root.join(rel_path).canonicalize().ok()?;
    full.starts_with(&root).then_some(full)
}

struct Request {
    method: String,
    target: String,
    host: Option<String>,
    origin: Option<String>,
}

fn read_request(conn: &mut TcpStream) -> Option<Request> {
    conn.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > 16 * 1024 {
            return None;
        }
        let n = conn.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let (mut host, mut origin) = (None, None);
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "host" => host = Some(v.trim().to_string()),
                "origin" => origin = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    Some(Request { method, target, host, origin })
}

fn respond(conn: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        conn,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    conn.write_all(body)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle(mut conn: TcpStream, port: u16, sessions: &Sessions) -> std::io::Result<()> {
    let Some(req) = read_request(&mut conn) else {
        return respond(&mut conn, "400 Bad Request", "text/plain", b"bad request");
    };
    if req.host.as_deref() != Some(&format!("127.0.0.1:{port}")) {
        return respond(&mut conn, "403 Forbidden", "text/plain", b"bad host");
    }
    if let Some(o) = &req.origin {
        if o != &format!("http://127.0.0.1:{port}") {
            return respond(&mut conn, "403 Forbidden", "text/plain", b"bad origin");
        }
    }
    if req.method != "GET" {
        return respond(&mut conn, "405 Method Not Allowed", "text/plain", b"GET only");
    }
    let path = req.target.split(['?', '#']).next().unwrap_or("");
    if path == "/__tod/bridge.js" {
        return respond(&mut conn, "200 OK", "text/javascript", BRIDGE_JS.as_bytes());
    }
    let Some(rest) = path.strip_prefix("/d/") else {
        return respond(&mut conn, "404 Not Found", "text/plain", b"not found");
    };
    let (token, rel) = rest.split_once('/').unwrap_or((rest, ""));
    let rel = percent_decode(rel);
    let mockup = sessions.lock().unwrap().get(token).map(|s| s.path.clone());
    let Some(mockup) = mockup else {
        return respond(&mut conn, "404 Not Found", "text/plain", b"not found");
    };
    if rel == "__tod/events" {
        return stream_events(conn, sessions, token);
    }
    let file = if rel.is_empty() {
        mockup.canonicalize().ok()
    } else {
        mockup.parent().and_then(|root| resolve_within(root, &rel))
    };
    let bytes = file.as_ref().filter(|f| f.is_file()).and_then(|f| std::fs::read(f).ok());
    let (Some(file), Some(bytes)) = (file, bytes) else {
        return respond(&mut conn, "404 Not Found", "text/plain", b"not found");
    };
    if rel.is_empty() {
        let html = inject(&String::from_utf8_lossy(&bytes), token);
        respond(&mut conn, "200 OK", "text/html; charset=utf-8", html.as_bytes())
    } else {
        respond(&mut conn, "200 OK", content_type(&file), &bytes)
    }
}

fn stream_events(mut conn: TcpStream, sessions: &Sessions, token: &str) -> std::io::Result<()> {
    let (tx, rx) = mpsc::channel::<Event>();
    let live = {
        let mut map = sessions.lock().unwrap();
        let Some(s) = map.get_mut(token) else {
            return respond(&mut conn, "404 Not Found", "text/plain", b"not found");
        };
        s.subs.push(tx);
        s.live.fetch_add(1, Ordering::SeqCst);
        s.live.clone()
    };
    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _guard = Guard(live);
    conn.set_read_timeout(None)?;
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n: connected\n\n"
    )?;
    conn.flush()?;
    loop {
        match rx.recv_timeout(KEEPALIVE) {
            Ok(ev) => {
                write!(conn, "event: {}\ndata: {}\n\n", ev.name(), ev.name())?;
                conn.flush()?;
                if ev == Event::Closed {
                    return Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                write!(conn, ": keepalive\n\n")?;
                conn.flush()?;
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css",
        Some("js" | "mjs") => "text/javascript",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    }
}

/// Adds the bridge before `</body>` (or at the end) and makes the title unique.
fn inject(html: &str, token: &str) -> String {
    let title = format!("tod design {}", &token[..token.len().min(8)]);
    let script = format!("<script src=\"/__tod/bridge.js\" data-base=\"/d/{token}/\"></script>");
    let mut out = html.to_string();
    match out.to_ascii_lowercase().rfind("</body>") {
        Some(i) => out.insert_str(i, &script),
        None => out.push_str(&script),
    }
    let lower = out.to_ascii_lowercase();
    if let (Some(a), Some(b)) = (lower.find("<title"), lower.find("</title>")) {
        if let Some(gt) = lower[a..].find('>') {
            if a + gt < b {
                out.replace_range(a + gt + 1..b, &title);
                return out;
            }
        }
    }
    let tag = format!("<title>{title}</title>");
    match lower.find("</head>") {
        Some(i) => out.insert_str(i, &tag),
        None => out.insert_str(0, &tag),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tod-design-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn get(port: u16, target: &str, host: Option<&str>) -> (String, String) {
        let mut c = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let h = host.map(str::to_string).unwrap_or(format!("127.0.0.1:{port}"));
        write!(c, "GET {target} HTTP/1.1\r\nHost: {h}\r\n\r\n").unwrap();
        let mut s = String::new();
        let _ = c.read_to_string(&mut s);
        let (head, body) = s.split_once("\r\n\r\n").unwrap_or((&s, ""));
        (head.lines().next().unwrap_or("").to_string(), body.to_string())
    }

    fn path_of(url: &str) -> String {
        format!("/d/{}/", token_of(url))
    }

    #[test]
    fn serves_page_asset_and_rejects_bad_requests() {
        let dir = tmp();
        std::fs::write(dir.join("m.html"), "<html><head><title>x</title></head><body>hi</body></html>").unwrap();
        std::fs::write(dir.join("a.css"), "b{}").unwrap();
        let srv = DesignServer::start().unwrap();
        let url = srv.open(&dir.join("m.html"));
        let p = path_of(&url);
        let (st, body) = get(srv.port(), &p, None);
        assert!(st.contains("200"), "{st}");
        assert!(body.contains("bridge.js") && body.contains("<title>tod design "));
        let (st, body) = get(srv.port(), &format!("{p}a.css"), None);
        assert!(st.contains("200") && body == "b{}");
        let (st, _) = get(srv.port(), &format!("/d/{}/", "0".repeat(64)), None);
        assert!(st.contains("404"));
        let (st, _) = get(srv.port(), &p, Some("evil.example"));
        assert!(st.contains("403"));
        let (st, _) = get(srv.port(), &format!("{p}../secret"), None);
        assert!(st.contains("404"));
        let (st, _) = get(srv.port(), &format!("{p}%2e%2e/secret"), None);
        assert!(st.contains("404"));
        let (st, body) = get(srv.port(), "/__tod/bridge.js", None);
        assert!(st.contains("200") && body.contains("EventSource"));
    }

    #[test]
    fn edit_sends_reload_and_set_path_navigates() {
        let dir = tmp();
        let f = dir.join("m.html");
        std::fs::write(&f, "<body>1</body>").unwrap();
        let srv = DesignServer::start().unwrap();
        let url = srv.open(&f);
        let mut c = TcpStream::connect((Ipv4Addr::LOCALHOST, srv.port())).unwrap();
        c.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        write!(c, "GET {}__tod/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", path_of(&url), srv.port()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !srv.connected(&url) {
            assert!(Instant::now() < deadline, "never connected");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(&f, "<body>2</body>").unwrap();
        let mut got = String::new();
        let mut buf = [0u8; 512];
        while !got.contains("event: reload") {
            assert!(Instant::now() < deadline, "no reload: {got}");
            if let Ok(n) = c.read(&mut buf) {
                got.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        srv.set_path(&url, &dir.join("other.html"));
        while !got.contains("event: navigate") {
            assert!(Instant::now() < deadline, "no navigate: {got}");
            if let Ok(n) = c.read(&mut buf) {
                got.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        srv.close(&url);
        assert_eq!(srv.session_count(), 0);
    }

    #[test]
    fn resolve_within_refuses_escapes() {
        let dir = tmp();
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ok.txt"), "x").unwrap();
        std::fs::write(dir.join("secret.txt"), "s").unwrap();
        assert!(resolve_within(&root, "ok.txt").is_some());
        assert!(resolve_within(&root, "../secret.txt").is_none());
        assert!(resolve_within(&root, "/etc/passwd").is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("secret.txt"), root.join("link")).unwrap();
            assert!(resolve_within(&root, "link").is_none());
        }
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(dir.join("secret.txt"), root.join("link")).is_ok() {
            assert!(resolve_within(&root, "link").is_none());
        }
    }
}
