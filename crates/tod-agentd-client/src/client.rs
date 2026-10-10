//! Finding the daemon, starting it when none runs, and asking it things.
//! Used by `tod` and `tod-cli` (`doc/agentd.md`, "Starting, finding, and
//! stopping it" and "Version check").

use crate::{BUILD_STAMP, BUILT_AT, Command, Info, Paths, Request, Response};
use anyhow::{Context, Result, anyhow, bail};
use fs2::FileExt;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Which build is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub stamp: String,
    pub built_at: u64,
}

impl Identity {
    /// The build this code is part of.
    pub fn this() -> Self {
        Self { stamp: BUILD_STAMP.to_string(), built_at: BUILT_AT }
    }
}

/// How a running daemon's build compares with the caller's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Same,
    /// Built from older source: drain it and start this build's.
    Older,
    /// Built from newer source (or the same moment but different source):
    /// leave it alone, since two builds would take turns restarting one
    /// daemon. The user needs to update.
    Newer,
}

pub fn compare(daemon: &Info, caller: &Identity) -> Comparison {
    if daemon.stamp == caller.stamp {
        Comparison::Same
    } else if daemon.built_at < caller.built_at {
        Comparison::Older
    } else {
        Comparison::Newer
    }
}

/// A connection to a running daemon.
pub struct Connection {
    pub info: Info,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Connection {
    /// Read one pushed line ([`crate::Event`]) from a subscribed connection,
    /// waiting as long as it takes. `None` when the connection ends.
    pub fn next_event(&mut self) -> Option<crate::Event> {
        let _ = self.writer.set_read_timeout(None);
        let mut line = String::new();
        loop {
            line.clear();
            match self.reader.read_line(&mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            if let Ok(event) = serde_json::from_str(&line) {
                return Some(event);
            }
        }
    }
}

/// The connection to the daemon failed, so whether the daemon got the request
/// is unknown. A request sent under an id can be sent again on a new
/// connection (`Connection::request_with_id`).
#[derive(Debug)]
pub struct ConnectionLost(pub String);

impl std::fmt::Display for ConnectionLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lost the connection to tod-agentd: {}", self.0)
    }
}

impl std::error::Error for ConnectionLost {}

impl Connection {
    /// Send `command` and read its reply.
    pub fn request(&mut self, command: Command) -> Result<Response> {
        self.request_with_id(None, command)
    }

    /// [`Self::request`] under `id`, so that sending it again after a lost
    /// connection is answered rather than applied twice.
    pub fn request_with_id(&mut self, id: Option<Uuid>, command: Command) -> Result<Response> {
        let request = Request { token: self.info.token.clone(), id, command };
        let mut text = serde_json::to_string(&request)?;
        text.push('\n');
        let lost = |err: std::io::Error| ConnectionLost(err.to_string());
        self.writer.write_all(text.as_bytes()).map_err(lost)?;
        self.writer.flush().map_err(lost)?;
        let mut line = String::new();
        if self.reader.read_line(&mut line).map_err(lost)? == 0 {
            return Err(ConnectionLost("the daemon closed the connection".into()).into());
        }
        let response: Response = serde_json::from_str(&line).context("the daemon's reply")?;
        if response.ok {
            Ok(response)
        } else {
            Err(anyhow!(response.error.unwrap_or_else(|| "the daemon refused".into())))
        }
    }
}

/// Connect to the daemon the info file names, if one answers. A file left by
/// a daemon that died is not an answer.
pub fn connect(paths: &Paths) -> Option<Connection> {
    let info = Info::read(paths)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], info.port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_millis(1000)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(120))).ok()?;
    let _ = stream.set_nodelay(true);
    let writer = stream.try_clone().ok()?;
    let mut connection = Connection { info: info.clone(), reader: BufReader::new(stream), writer };
    let hello = connection.request(Command::Hello).ok()?;
    // The file and the process must agree, or the file is stale and the port
    // belongs to something else.
    (hello.info.as_ref() == Some(&info)).then_some(connection)
}

/// What [`ensure_running`] found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ensured {
    pub info: Info,
    /// A daemon was started (none was running).
    pub started: bool,
    /// An older daemon was drained and replaced.
    pub restarted: bool,
}

/// A daemon built from newer source is running; this build must not replace it.
#[derive(Debug)]
pub struct DaemonNewer(pub Info);

impl std::fmt::Display for DaemonNewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the running tod-agentd (build {}) is newer than this program; update it",
            self.0.stamp
        )
    }
}

impl std::error::Error for DaemonNewer {}

/// The `tod-agentd` executable beside the running program. Only there: a
/// test binary (in `deps/`) must not find, and start, a daemon of its own.
pub fn locate_executable() -> Result<PathBuf> {
    let me = std::env::current_exe()?;
    let name = format!("tod-agentd{}", std::env::consts::EXE_SUFFIX);
    me.parent()
        .map(|dir| dir.join(&name))
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow!("{name} was not found beside {}", me.display()))
}

/// Make sure a daemon of this build serves `data_root`: use the one running,
/// replace an older one, or start one. `executable` is the `tod-agentd` to
/// copy and run.
pub fn ensure_running(data_root: &Path, executable: &Path) -> Result<Ensured> {
    let me = Identity::this();
    // A `tod-agentd` left over from an older build would start, be found
    // older, and be replaced by the next caller's, over and over.
    if !matches!(Info::read(&Paths::new(data_root)), Some(i) if i.stamp == me.stamp) {
        let built = std::process::Command::new(executable)
            .arg("--build-stamp")
            .output()
            .with_context(|| format!("run {}", executable.display()))?;
        let built = String::from_utf8_lossy(&built.stdout).trim().to_string();
        if built != me.stamp {
            bail!(
                "{} is from a different build than this program ({built}, not {});                  build tod-agentd too (cargo build -p tod-agentd)",
                executable.display(),
                me.stamp
            );
        }
    }
    ensure_running_as(data_root, executable, &me)
}

pub fn ensure_running_as(data_root: &Path, executable: &Path, me: &Identity) -> Result<Ensured> {
    let paths = Paths::new(data_root);
    std::fs::create_dir_all(paths.dir())?;
    // Two launches at once must not both start one.
    let start_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.start_lock())?;
    start_lock.lock_exclusive().context("waiting for another start")?;

    let mut restarted = false;
    if let Some(mut running) = connect(&paths) {
        match compare(&running.info, me) {
            Comparison::Same => {
                return Ok(Ensured { info: running.info, started: false, restarted: false });
            }
            Comparison::Newer => return Err(DaemonNewer(running.info).into()),
            Comparison::Older => {
                running.request(Command::Quit)?;
                let pid = running.info.pid;
                drop(running);
                wait_gone(&paths, pid)?;
                restarted = true;
            }
        }
    }
    let info = spawn(&paths, executable, me)?;
    Ok(Ensured { info, started: !restarted, restarted })
}

/// Ask the running daemon, if any, to drain and exit, and wait until it has.
pub fn quit(data_root: &Path) -> Result<bool> {
    let paths = Paths::new(data_root);
    let Some(mut running) = connect(&paths) else {
        return Ok(false);
    };
    running.request(Command::Quit)?;
    let pid = running.info.pid;
    drop(running);
    wait_gone(&paths, pid)?;
    Ok(true)
}

/// Wait for the daemon to give up its lock (it has exited).
fn wait_gone(paths: &Paths, pid: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let free = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(paths.daemon_lock())
            .map(|f| f.try_lock_exclusive().is_ok())
            .unwrap_or(false);
        if free {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the old daemon (pid {pid}) did not exit within 30s")
}

fn spawn(paths: &Paths, executable: &Path, me: &Identity) -> Result<Info> {
    // Run from a copy: a running program cannot be replaced on Windows, which
    // would break rebuilding and updating.
    let copy = paths.executable(&me.stamp);
    if !copy.is_file() {
        let tmp = copy.with_extension("partial");
        let _ = std::fs::remove_file(&tmp);
        // A hard link where the platform allows one: macOS checks the
        // signature of every new executable on its first run (~0.7s, and
        // serialized across processes), but not of a second name for one
        // already checked. Builds and installs replace the file rather than
        // write into it, so the link keeps the build it was made from.
        let linked = cfg!(unix) && std::fs::hard_link(executable, &tmp).is_ok();
        if !linked {
            std::fs::copy(executable, &tmp).with_context(|| format!("copy {}", executable.display()))?;
        }
        std::fs::rename(&tmp, &copy)?;
    }
    let data_root = paths.dir().parent().ok_or_else(|| anyhow!("no data root"))?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.log())?;

    let launch = |detach: Detach| -> std::io::Result<std::process::Child> {
        let mut command = std::process::Command::new(&copy);
        command
            .arg("--data-root")
            .arg(data_root)
            .env("TOD_PROGRAM_DIR", executable.parent().unwrap_or(Path::new(".")))
            .env("TOD_AGENTD_TEST_STAMP", &me.stamp)
            .env("TOD_AGENTD_TEST_BUILT_AT", me.built_at.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        detach.apply(&mut command);
        command.spawn()
    };
    // A launcher inside a job object that kills its children on exit needs
    // breakaway; if that is refused, start without it and say so.
    let mut child = match launch(Detach::Breakaway) {
        Ok(child) => child,
        Err(err) => {
            eprintln!("tod-agentd: starting with breakaway failed ({err}); starting without");
            launch(Detach::Plain).context("start tod-agentd")?
        }
    };

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Some(connection) = connect(paths) {
            return Ok(connection.info);
        }
        if let Some(status) = child.try_wait()? {
            // It may have lost a race to another daemon that is now serving.
            if let Some(connection) = connect(paths) {
                return Ok(connection.info);
            }
            bail!("tod-agentd exited at start ({status}); see {}", paths.log().display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("tod-agentd did not answer within 60s; see {}", paths.log().display())
}

#[derive(Clone, Copy)]
enum Detach {
    Breakaway,
    Plain,
}

impl Detach {
    #[cfg(windows)]
    fn apply(self, command: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let mut flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        if matches!(self, Detach::Breakaway) {
            flags |= CREATE_BREAKAWAY_FROM_JOB;
        }
        command.creation_flags(flags);
    }

    #[cfg(unix)]
    fn apply(self, command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        // Its own session and process group: closing the terminal or the
        // launcher does not signal it.
        command.process_group(0);
        let _ = self;
    }
}
