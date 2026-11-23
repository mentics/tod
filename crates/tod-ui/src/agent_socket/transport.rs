//! Transport and reply framing for the dev-only desktop control socket.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Bind the control socket. Call before opening the main window so port conflicts fail fast.
pub fn bind(addr: SocketAddr) -> anyhow::Result<TcpListener> {
    TcpListener::bind(addr).map_err(|err| {
        anyhow::anyhow!("agent-socket: bind {addr} failed: {err} (choose another port)")
    })
}

pub(super) struct Reply {
    pub result: Result<String, String>,
    pub note: Option<String>,
}

impl Reply {
    fn into_frame(self) -> String {
        match self.result {
            Ok(line) => match self.note {
                // Put the note on the first line so tree replies keep their announced line count.
                Some(note) => match line.split_once('\n') {
                    Some((first, rest)) => format!("{first} # note: {note}\n{rest}\n"),
                    None => format!("{line} # note: {note}\n"),
                },
                None => format!("{line}\n"),
            },
            Err(error) => format!("err {error}\n"),
        }
    }
}

/// Serve complete lines. The handler applies the active desktop application's rules and actions.
pub(super) fn listen(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    handler: impl Fn(&str) -> Reply + Send + Sync + 'static,
) {
    if let Err(error) = listener.set_nonblocking(true) {
        eprintln!("agent-socket: set listener nonblocking: {error}");
        return;
    }
    let handler = Arc::new(handler);
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let handler = handler.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("tod-agent-client".into())
                    .spawn(move || {
                        if let Err(error) = serve_client(stream, handler.as_ref()) {
                            eprintln!("agent-socket: client error: {error}");
                        }
                    })
                {
                    eprintln!("agent-socket: spawn client thread: {error}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                eprintln!("agent-socket: accept error: {error}");
                break;
            }
        }
    }
}

fn serve_client(mut stream: TcpStream, handler: &impl Fn(&str) -> Reply) -> std::io::Result<()> {
    // Accepted sockets can inherit nonblocking mode, notably on Windows.
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        let frame = handler(line.trim_end_matches(['\r', '\n'])).into_frame();
        stream.write_all(frame.as_bytes())?;
        stream.flush()?;
        line.clear();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_fails_when_port_in_use() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let err = bind(addr).unwrap_err();
        assert!(err.to_string().contains("bind"));
        assert!(err.to_string().contains("choose another port"));
    }

    #[test]
    fn reply_notes_keep_multiline_results_framed() {
        let reply = Reply {
            result: Ok("ok 2 nodes\nnode one\nnode two".into()),
            note: Some("resized".into()),
        };
        assert_eq!(
            reply.into_frame(),
            "ok 2 nodes # note: resized\nnode one\nnode two\n"
        );
    }
}
