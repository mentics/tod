//! Redis adapter: a fixed list of read-only commands over RESP.
//!
//! Every command sent is a [`Read`] variant's [`Read::name`]; the arguments an
//! agent supplies are keys, fields and numbers, sent as binary-safe bulk
//! strings, so they cannot become a command. The only other commands sent are
//! `AUTH` and `SELECT`, from the declaration. There is no way to name a
//! command, so a write is not refused, it is unrepresentable.

use super::{Action, Args, Service, action, only, require};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read as _, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// Keys one `scan` may return.
const SCAN_MAX: usize = 1000;
/// Elements of a list, sorted set or set one call may read.
const RANGE_MAX: usize = 500;

/// The commands this adapter can send, all read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Get,
    Type,
    Ttl,
    Pttl,
    Exists,
    Strlen,
    Hget,
    Hgetall,
    Hlen,
    Llen,
    Lrange,
    Scard,
    Sscan,
    Zcard,
    Zrange,
    Scan,
}

impl Read {
    const ALL: [Read; 16] = [
        Read::Get,
        Read::Type,
        Read::Ttl,
        Read::Pttl,
        Read::Exists,
        Read::Strlen,
        Read::Hget,
        Read::Hgetall,
        Read::Hlen,
        Read::Llen,
        Read::Lrange,
        Read::Scard,
        Read::Sscan,
        Read::Zcard,
        Read::Zrange,
        Read::Scan,
    ];

    fn name(self) -> &'static str {
        match self {
            Read::Get => "GET",
            Read::Type => "TYPE",
            Read::Ttl => "TTL",
            Read::Pttl => "PTTL",
            Read::Exists => "EXISTS",
            Read::Strlen => "STRLEN",
            Read::Hget => "HGET",
            Read::Hgetall => "HGETALL",
            Read::Hlen => "HLEN",
            Read::Llen => "LLEN",
            Read::Lrange => "LRANGE",
            Read::Scard => "SCARD",
            Read::Sscan => "SSCAN",
            Read::Zcard => "ZCARD",
            Read::Zrange => "ZRANGE",
            Read::Scan => "SCAN",
        }
    }

    /// The agent-facing action that sends this command.
    fn action(self) -> &'static str {
        match self {
            Read::Get => "get",
            Read::Type => "type",
            Read::Ttl => "ttl",
            Read::Pttl => "pttl",
            Read::Exists => "exists",
            Read::Strlen => "strlen",
            Read::Hget => "hget",
            Read::Hgetall => "hgetall",
            Read::Hlen => "hlen",
            Read::Llen => "llen",
            Read::Lrange => "lrange",
            Read::Scard => "scard",
            Read::Sscan => "smembers",
            Read::Zcard => "zcard",
            Read::Zrange => "zrange",
            Read::Scan => "scan",
        }
    }
}

pub(crate) fn actions() -> Vec<Action> {
    vec![
        action("get", &["key"], "a string value"),
        action("type", &["key"], "the type of a key (string, hash, list, set, zset, none)"),
        action("ttl", &["key"], "seconds to live (-1 none, -2 no such key)"),
        action("pttl", &["key"], "milliseconds to live"),
        action("exists", &["keys"], "how many of the comma-separated keys exist (up to 50)"),
        action("strlen", &["key"], "length of a string value"),
        action("hget", &["key", "field"], "one field of a hash"),
        action("hgetall", &["key"], "a whole hash (refused above 500 fields)"),
        action("hlen", &["key"], "number of fields of a hash"),
        action("llen", &["key"], "length of a list"),
        action("lrange", &["key", "start?", "stop?"], "list elements (default 0..99, at most 500)"),
        action("scard", &["key"], "size of a set"),
        action("smembers", &["key"], "members of a set (refused above 500)"),
        action("zcard", &["key"], "size of a sorted set"),
        action("zrange", &["key", "start?", "stop?"], "sorted-set members with scores (default 0..99, at most 500)"),
        action("scan", &["pattern", "max?"], "keys matching a glob pattern such as claim:*: walks the keyspace with SCAN, so it is slow on a big one; max 1-1000 (default 100)"),
    ]
}

/// `(tls, host, port)` from `redis://host[:port]` / `rediss://host[:port]`.
pub(crate) fn parse_url(url: &str) -> Result<(bool, String, u16)> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("rediss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("redis://") {
        (false, r)
    } else {
        bail!("url {url:?} is not redis:// or rediss://");
    };
    let rest = rest.trim_end_matches('/');
    ensure!(!rest.contains(['/', '@', ' ']), "url {url:?}: write only host[:port] (the credential is set apart, not in the URL)");
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().with_context(|| format!("url {url:?}: {p:?} is not a port"))?),
        None => (rest, 6379),
    };
    ensure!(!host.is_empty(), "url {url:?} names no host");
    Ok((tls, host.to_string(), port))
}

trait Stream: std::io::Read + Write + Send {}
impl<T: std::io::Read + Write + Send> Stream for T {}

struct Conn {
    reader: BufReader<Box<dyn Stream>>,
}

impl Conn {
    fn open(svc: &Service) -> Result<Conn> {
        let (tls, host, port) = parse_url(&svc.url)?;
        let target = format!("{host}:{port}");
        let addr = target
            .to_socket_addrs()
            .with_context(|| format!("look up {target}"))?
            .next()
            .with_context(|| format!("{target} has no address"))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(8))
            .with_context(|| format!("connect to {target} from this machine: is the VPN up, and is this machine allowed?"))?;
        tcp.set_read_timeout(Some(Duration::from_secs(20)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(20)))?;
        let stream: Box<dyn Stream> = if tls {
            let mut roots = rustls::RootCertStore::empty();
            roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
            let config = Arc::new(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth());
            let name = rustls::pki_types::ServerName::try_from(host.clone()).with_context(|| format!("{host:?} is not a valid TLS name"))?;
            let conn = rustls::ClientConnection::new(config, name).context("start TLS")?;
            Box::new(rustls::StreamOwned::new(conn, tcp))
        } else {
            Box::new(tcp)
        };
        Ok(Conn { reader: BufReader::new(stream) })
    }

    /// Sends one command and reads its reply.
    fn send(&mut self, parts: &[&[u8]]) -> Result<Resp> {
        let mut out = format!("*{}\r\n", parts.len()).into_bytes();
        for p in parts {
            out.extend(format!("${}\r\n", p.len()).as_bytes());
            out.extend(*p);
            out.extend(b"\r\n");
        }
        let stream = self.reader.get_mut();
        stream.write_all(&out)?;
        stream.flush()?;
        read_reply(&mut self.reader, 0)
    }

    /// Sends an allowlisted command.
    fn read(&mut self, cmd: Read, args: &[&[u8]]) -> Result<Resp> {
        let mut parts: Vec<&[u8]> = vec![cmd.name().as_bytes()];
        parts.extend(args);
        match self.send(&parts)? {
            Resp::Error(e) => bail!("redis answered an error to {}: {e}", cmd.name()),
            other => Ok(other),
        }
    }
}

#[derive(Debug, PartialEq)]
enum Resp {
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<Resp>>),
}

fn read_line(r: &mut impl BufRead) -> Result<String> {
    let mut line = Vec::new();
    let n = r.read_until(b'\n', &mut line).context("read the reply")?;
    ensure!(n > 0, "the server closed the connection");
    ensure!(line.ends_with(b"\r\n"), "malformed reply line");
    line.truncate(line.len() - 2);
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn read_reply(r: &mut impl BufRead, depth: u32) -> Result<Resp> {
    ensure!(depth < 8, "reply nested too deeply");
    let line = read_line(r)?;
    let (kind, rest) = line.split_at(line.chars().next().map_or(0, char::len_utf8));
    Ok(match kind {
        "+" => Resp::Simple(rest.to_string()),
        "-" => Resp::Error(rest.to_string()),
        ":" => Resp::Int(rest.parse().context("malformed integer reply")?),
        "$" => {
            let len: i64 = rest.parse().context("malformed bulk length")?;
            if len < 0 {
                Resp::Bulk(None)
            } else {
                ensure!(len <= 16 * 1024 * 1024, "a value of {len} bytes is too large to read here");
                let mut buf = vec![0u8; len as usize + 2];
                r.read_exact(&mut buf).context("read a value")?;
                buf.truncate(len as usize);
                Resp::Bulk(Some(buf))
            }
        }
        "*" => {
            let len: i64 = rest.parse().context("malformed array length")?;
            if len < 0 {
                Resp::Array(None)
            } else {
                ensure!(len <= 100_000, "an array of {len} elements is too large to read here");
                Resp::Array(Some((0..len).map(|_| read_reply(r, depth + 1)).collect::<Result<_>>()?))
            }
        }
        other => bail!("unexpected reply type {other:?}"),
    })
}

fn to_json(resp: Resp) -> Value {
    match resp {
        Resp::Simple(s) => json!(s),
        Resp::Error(e) => json!({"error": e}),
        Resp::Int(i) => json!(i),
        Resp::Bulk(None) | Resp::Array(None) => Value::Null,
        Resp::Bulk(Some(b)) => match String::from_utf8(b) {
            Ok(s) => json!(s),
            Err(e) => json!({"binary_base64_lossy": String::from_utf8_lossy(e.as_bytes()).into_owned(), "bytes": e.as_bytes().len()}),
        },
        Resp::Array(Some(items)) => Value::Array(items.into_iter().map(to_json).collect()),
    }
}

fn key(args: &Args) -> Result<&str> {
    let k = require(args, "key")?;
    ensure!(k.len() <= 512 && !k.contains('\0'), "key is longer than 512 bytes or holds a NUL");
    Ok(k)
}

fn int(args: &Args, name: &str, default: i64) -> Result<i64> {
    match args.get(name) {
        Some(v) => v.parse().map_err(|_| anyhow::anyhow!("{name} {v:?} is not a whole number")),
        None => Ok(default),
    }
}

/// Checks `start..=stop` asks for at most [`RANGE_MAX`] elements.
fn range(args: &Args) -> Result<(String, String)> {
    let (start, stop) = (int(args, "start", 0)?, int(args, "stop", 99)?);
    ensure!(start >= 0 && stop >= start && stop - start < RANGE_MAX as i64, "start..stop must be a range of at most {RANGE_MAX} elements, from 0 up");
    Ok((start.to_string(), stop.to_string()))
}

pub(crate) fn call(svc: &Service, token: Option<&str>, action: &str, args: &Args) -> Result<Value> {
    let cmd = *Read::ALL.iter().find(|c| c.action() == action).with_context(|| format!("no action {action:?}"))?;
    // Validate before connecting, so a bad argument never costs a connection.
    let plan = plan(cmd, args)?;
    let mut conn = Conn::open(svc)?;
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        let reply = match &svc.username {
            Some(user) => conn.send(&[b"AUTH", user.as_bytes(), token.as_bytes()])?,
            None => conn.send(&[b"AUTH", token.as_bytes()])?,
        };
        if let Resp::Error(e) = reply {
            bail!("the server refused the credential on this machine: {e}");
        }
    }
    if let Some(db) = svc.db {
        if let Resp::Error(e) = conn.send(&[b"SELECT", db.to_string().as_bytes()])? {
            bail!("SELECT {db}: {e}");
        }
    }
    run(&mut conn, cmd, plan)
}

/// An action's validated arguments.
enum Plan {
    Args(Vec<Vec<u8>>),
    Scan { pattern: String, max: usize },
    Members { key: String },
    Hgetall { key: String },
}

fn plan(cmd: Read, args: &Args) -> Result<Plan> {
    let b = |s: &str| s.as_bytes().to_vec();
    Ok(match cmd {
        Read::Get | Read::Type | Read::Ttl | Read::Pttl | Read::Strlen | Read::Hlen | Read::Llen | Read::Scard | Read::Zcard => {
            only(args, &["key"])?;
            Plan::Args(vec![b(key(args)?)])
        }
        Read::Exists => {
            only(args, &["keys"])?;
            let keys: Vec<&str> = require(args, "keys")?.split(',').map(str::trim).filter(|k| !k.is_empty()).collect();
            ensure!(!keys.is_empty() && keys.len() <= 50, "keys: 1 to 50 comma-separated keys");
            ensure!(keys.iter().all(|k| k.len() <= 512), "a key is longer than 512 bytes");
            Plan::Args(keys.iter().map(|k| b(k)).collect())
        }
        Read::Hget => {
            only(args, &["key", "field"])?;
            Plan::Args(vec![b(key(args)?), b(require(args, "field")?)])
        }
        Read::Hgetall => {
            only(args, &["key"])?;
            Plan::Hgetall { key: key(args)?.to_string() }
        }
        Read::Lrange => {
            only(args, &["key", "start", "stop"])?;
            let (start, stop) = range(args)?;
            Plan::Args(vec![b(key(args)?), b(&start), b(&stop)])
        }
        Read::Zrange => {
            only(args, &["key", "start", "stop"])?;
            let (start, stop) = range(args)?;
            Plan::Args(vec![b(key(args)?), b(&start), b(&stop), b("WITHSCORES")])
        }
        Read::Sscan => {
            only(args, &["key"])?;
            Plan::Members { key: key(args)?.to_string() }
        }
        Read::Scan => {
            only(args, &["pattern", "max"])?;
            let pattern = require(args, "pattern")?;
            ensure!(pattern.len() <= 256 && !pattern.contains('\0'), "pattern is longer than 256 bytes");
            let max = int(args, "max", 100)?;
            ensure!((1..=SCAN_MAX as i64).contains(&max), "max {max} is outside 1-{SCAN_MAX}");
            Plan::Scan { pattern: pattern.to_string(), max: max as usize }
        }
    })
}

fn run(conn: &mut Conn, cmd: Read, plan: Plan) -> Result<Value> {
    match plan {
        Plan::Args(args) => {
            let refs: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
            let value = to_json(conn.read(cmd, &refs)?);
            // ZRANGE WITHSCORES comes back flat: pair it up.
            if cmd == Read::Zrange {
                if let Value::Array(flat) = &value {
                    let pairs: Vec<Value> = flat.chunks(2).map(|p| json!({"member": p[0], "score": p.get(1)})).collect();
                    return Ok(json!(pairs));
                }
            }
            Ok(value)
        }
        Plan::Hgetall { key } => {
            let len = to_json(conn.read(Read::Hlen, &[key.as_bytes()])?).as_i64().unwrap_or(0);
            ensure!(len <= RANGE_MAX as i64, "hash {key} has {len} fields, more than the {RANGE_MAX} one call reads: use hget for the field you need");
            let Value::Array(flat) = to_json(conn.read(Read::Hgetall, &[key.as_bytes()])?) else { return Ok(Value::Null) };
            let mut map = serde_json::Map::new();
            for pair in flat.chunks(2) {
                map.insert(pair[0].as_str().unwrap_or("?").to_string(), pair.get(1).cloned().unwrap_or(Value::Null));
            }
            Ok(Value::Object(map))
        }
        Plan::Members { key } => {
            let len = to_json(conn.read(Read::Scard, &[key.as_bytes()])?).as_i64().unwrap_or(0);
            ensure!(len <= RANGE_MAX as i64, "set {key} has {len} members, more than the {RANGE_MAX} one call reads");
            let mut members = Vec::new();
            let mut cursor = "0".to_string();
            loop {
                let Value::Array(reply) = to_json(conn.read(Read::Sscan, &[key.as_bytes(), cursor.as_bytes(), b"COUNT", b"500"])?) else { bail!("unexpected SSCAN reply") };
                cursor = reply.first().and_then(Value::as_str).unwrap_or("0").to_string();
                if let Some(Value::Array(batch)) = reply.get(1) {
                    members.extend(batch.iter().cloned());
                }
                if cursor == "0" || members.len() > RANGE_MAX * 2 {
                    break;
                }
            }
            Ok(Value::Array(members))
        }
        Plan::Scan { pattern, max } => {
            let mut keys: Vec<Value> = Vec::new();
            let mut cursor = "0".to_string();
            let mut complete = false;
            for _ in 0..500 {
                let Value::Array(reply) = to_json(conn.read(Read::Scan, &[cursor.as_bytes(), b"MATCH", pattern.as_bytes(), b"COUNT", b"1000"])?) else { bail!("unexpected SCAN reply") };
                cursor = reply.first().and_then(Value::as_str).unwrap_or("0").to_string();
                if let Some(Value::Array(batch)) = reply.get(1) {
                    keys.extend(batch.iter().cloned());
                }
                if cursor == "0" {
                    complete = true;
                    break;
                }
                if keys.len() >= max {
                    break;
                }
            }
            keys.sort_by_key(|k| k.to_string());
            keys.dedup();
            let found = keys.len();
            keys.truncate(max);
            Ok(json!({"keys": keys, "complete": complete && found <= max, "note": if complete && found <= max { "every matching key" } else { "the first matches only: narrow the pattern or raise max" }}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Kind;
    use crate::service::tests::svc;
    use std::net::TcpListener;

    fn args(kv: &[(&str, &str)]) -> Args {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A fake Redis on loopback: records every command, answers from `replies`
    /// in order (one per command, AUTH and SELECT included).
    fn fake(replies: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let mut seen = Vec::new();
            for reply in replies {
                let Ok(Resp::Array(Some(parts))) = read_reply(&mut reader, 0) else { break };
                seen.push(parts.into_iter().map(|p| match p { Resp::Bulk(Some(b)) => String::from_utf8_lossy(&b).into_owned(), _ => "?".into() }).collect());
                sock.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (url, handle)
    }

    #[test]
    fn every_command_that_can_be_sent_is_a_read() {
        // The writers and admin commands Redis has that matter here; none may be in the list.
        let writers = [
            "SET", "SETEX", "SETNX", "GETSET", "GETDEL", "GETEX", "DEL", "UNLINK", "EXPIRE", "PEXPIRE", "PERSIST", "RENAME", "INCR", "DECR", "INCRBY", "APPEND",
            "HSET", "HDEL", "HINCRBY", "LPUSH", "RPUSH", "LPOP", "RPOP", "LSET", "LREM", "SADD", "SREM", "SPOP", "ZADD", "ZREM", "ZINCRBY", "FLUSHALL", "FLUSHDB",
            "EVAL", "EVALSHA", "FCALL", "SCRIPT", "CONFIG", "SHUTDOWN", "DEBUG", "MIGRATE", "RESTORE", "MULTI", "EXEC", "PUBLISH", "SUBSCRIBE", "CLIENT", "SLAVEOF",
            "REPLICAOF", "ACL", "MODULE", "KEYS", "SORT", "OBJECT", "MONITOR", "COPY", "MOVE", "XADD", "XDEL", "XTRIM", "XREADGROUP", "XACK",
        ];
        for c in Read::ALL {
            assert!(!writers.contains(&c.name()), "{} is not read-only", c.name());
        }
        // Every command has exactly one action, and every action maps to one.
        let acts: Vec<&str> = actions().iter().map(|a| a.name.clone()).map(|n| &*Box::leak(n.into_boxed_str())).collect();
        assert_eq!(acts.len(), Read::ALL.len());
        for c in Read::ALL {
            assert!(acts.contains(&c.action()), "{}", c.action());
        }
    }

    #[test]
    fn urls_are_host_and_port_only() {
        assert_eq!(parse_url("rediss://r.example:6380").unwrap(), (true, "r.example".into(), 6380));
        assert_eq!(parse_url("redis://r.example").unwrap(), (false, "r.example".into(), 6379));
        assert!(parse_url("redis://user:pw@r.example").is_err());
        assert!(parse_url("http://r.example").is_err());
    }

    #[test]
    fn get_sends_auth_then_exactly_one_get() {
        let (url, handle) = fake(vec!["+OK\r\n", "$5\r\nvalue\r\n"]);
        let s = svc(Kind::Redis, &url);
        let out = s.call(Some("pw"), "get", &args(&[("key", "claim:42")])).unwrap();
        assert_eq!(out, json!("value"));
        assert_eq!(handle.join().unwrap(), vec![vec!["AUTH", "pw"], vec!["GET", "claim:42"]]);
    }

    #[test]
    fn a_key_that_looks_like_a_command_is_only_a_key() {
        let (url, handle) = fake(vec![":0\r\n"]);
        let out = svc(Kind::Redis, &url).call(None, "exists", &args(&[("keys", "FLUSHALL, x y")])).unwrap();
        assert_eq!(out, json!(0));
        assert_eq!(handle.join().unwrap(), vec![vec!["EXISTS", "FLUSHALL", "x y"]]);
    }

    #[test]
    fn bad_arguments_fail_before_connecting() {
        let s = svc(Kind::Redis, "redis://127.0.0.1:1");
        for (action, kv) in [
            ("get", vec![]),
            ("get", vec![("key", "k"), ("extra", "x")]),
            ("lrange", vec![("key", "k"), ("stop", "100000")]),
            ("scan", vec![("pattern", "*"), ("max", "0")]),
            ("hget", vec![("key", "k")]),
        ] {
            let err = s.call(None, action, &args(&kv)).unwrap_err().to_string();
            assert!(!err.contains("connect"), "{action}: validated first: {err}");
        }
    }

    #[test]
    fn scan_collects_matches_and_says_when_it_is_complete() {
        let (url, handle) = fake(vec!["*2\r\n$1\r\n0\r\n*2\r\n$7\r\nclaim:2\r\n$7\r\nclaim:1\r\n"]);
        let out = svc(Kind::Redis, &url).call(None, "scan", &args(&[("pattern", "claim:*")])).unwrap();
        assert_eq!(out["keys"], json!(["claim:1", "claim:2"]));
        assert_eq!(out["complete"], true);
        assert_eq!(handle.join().unwrap()[0][..3], ["SCAN", "0", "MATCH"]);
    }

    #[test]
    fn a_server_error_and_a_refused_credential_are_named() {
        let (url, handle) = fake(vec!["-WRONGPASS invalid username-password pair\r\n"]);
        let err = svc(Kind::Redis, &url).call(Some("bad"), "get", &args(&[("key", "k")])).unwrap_err().to_string();
        handle.join().unwrap();
        assert!(err.contains("redis get on s") && err.contains("refused the credential") && err.contains("WRONGPASS"), "{err}");
        assert!(!err.contains("bad"), "{err}");
        let (url, handle) = fake(vec!["-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"]);
        let err = svc(Kind::Redis, &url).call(None, "get", &args(&[("key", "k")])).unwrap_err().to_string();
        handle.join().unwrap();
        assert!(err.contains("WRONGTYPE"), "{err}");
    }

    #[test]
    fn an_unreachable_server_says_where_it_tried() {
        let err = svc(Kind::Redis, "redis://127.0.0.1:1").call(None, "get", &args(&[("key", "k")])).unwrap_err().to_string();
        assert!(err.contains("redis get on s") && err.contains("127.0.0.1:1"), "{err}");
    }
}
