use crate::interview::agent::AgentBackend;
use crate::logging::LogLevel;
use std::path::PathBuf;

#[cfg(feature = "agent-socket")]
use std::net::SocketAddr;

/// Launch options for window size, optional agent control socket, sandbox root, agent backend.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub width: f32,
    pub height: f32,
    pub width_from_cli: bool,
    pub height_from_cli: bool,
    #[cfg(feature = "agent-socket")]
    pub agent_socket: Option<SocketAddr>,
    /// When set, all `TodPaths` resolve under this root (isolated sandbox).
    pub data_root: Option<PathBuf>,
    pub agent_backend: AgentBackend,
    /// True when `--agent` was passed on the CLI (overrides settings).
    pub agent_backend_from_cli: bool,
    /// When true, open the window without stealing OS keyboard focus.
    pub no_focus: bool,
    /// CLI `--log-level` override for this process run.
    pub log_level: Option<LogLevel>,
    /// When true, run `doc/process` bootstrap import before opening the UI.
    pub import_process: bool,
    /// When true, discover bundled process docs, load manifest, print paths, and exit.
    pub verify_process_bundle: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            width: 1280.0,
            height: 768.0,
            width_from_cli: false,
            height_from_cli: false,
            #[cfg(feature = "agent-socket")]
            agent_socket: None,
            data_root: None,
            agent_backend: AgentBackend::Cursor,
            agent_backend_from_cli: false,
            no_focus: false,
            log_level: None,
            import_process: false,
            verify_process_bundle: false,
        }
    }
}

impl LaunchOptions {
    /// Parse CLI flags from argv (after binary name).
    pub fn from_args(args: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut opts = Self::default();
        let mut iter = args.into_iter();
        let first = iter.next();
        let mut rest: Vec<String> = iter.collect();
        if let Some(f) = first {
            if f.starts_with('-') {
                rest.insert(0, f);
            }
        }

        let mut i = 0;
        #[cfg(feature = "agent-socket")]
        let mut agent_socket_port: Option<u16> = None;
        while i < rest.len() {
            match rest[i].as_str() {
                "--width" => {
                    i += 1;
                    let v = rest
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("--width requires a value"))?;
                    opts.width = parse_positive_px(v, "--width")?;
                    opts.width_from_cli = true;
                }
                "--height" => {
                    i += 1;
                    let v = rest
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("--height requires a value"))?;
                    opts.height = parse_positive_px(v, "--height")?;
                    opts.height_from_cli = true;
                }
                #[cfg(feature = "agent-socket")]
                "--agent-socket" => {
                    i += 1;
                    let v = rest
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("--agent-socket requires host:port"))?;
                    if agent_socket_port.is_some() {
                        anyhow::bail!(
                            "--agent-socket and --agent-socket-port are mutually exclusive"
                        );
                    }
                    opts.agent_socket = Some(
                        v.parse::<SocketAddr>()
                            .map_err(|e| anyhow::anyhow!("invalid --agent-socket `{v}`: {e}"))?,
                    );
                }
                #[cfg(feature = "agent-socket")]
                "--agent-socket-port" => {
                    i += 1;
                    let v = rest.get(i).ok_or_else(|| {
                        anyhow::anyhow!("--agent-socket-port requires a port number")
                    })?;
                    if opts.agent_socket.is_some() {
                        anyhow::bail!(
                            "--agent-socket and --agent-socket-port are mutually exclusive"
                        );
                    }
                    let port: u16 = v.parse().map_err(|_| {
                        anyhow::anyhow!("invalid --agent-socket-port `{v}` (must be 1–65535)")
                    })?;
                    if port == 0 {
                        anyhow::bail!("invalid --agent-socket-port `{v}` (must be 1–65535)");
                    }
                    agent_socket_port = Some(port);
                }
                #[cfg(not(feature = "agent-socket"))]
                "--agent-socket" => {
                    anyhow::bail!(
                        "this build was compiled without the agent-socket feature; \
                         rebuild with --features agent-socket for UI automation"
                    );
                }
                #[cfg(not(feature = "agent-socket"))]
                "--agent-socket-port" => {
                    anyhow::bail!(
                        "this build was compiled without the agent-socket feature; \
                         rebuild with --features agent-socket for UI automation"
                    );
                }
                "--data-root" => {
                    i += 1;
                    let v = rest
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("--data-root requires a path"))?;
                    opts.data_root = Some(PathBuf::from(v));
                }
                "--agent" => {
                    i += 1;
                    let v = rest
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("--agent requires mock|cursor|claude"))?;
                    opts.agent_backend = AgentBackend::parse(v)?;
                    opts.agent_backend_from_cli = true;
                }
                "--log-level" => {
                    i += 1;
                    let v = rest.get(i).ok_or_else(|| {
                        anyhow::anyhow!("--log-level requires error|info|debug|trace")
                    })?;
                    opts.log_level = Some(
                        v.parse::<LogLevel>()
                            .map_err(|e| anyhow::anyhow!("invalid --log-level: {e}"))?,
                    );
                }
                "--no-focus" => {
                    opts.no_focus = true;
                }
                "--import-process" => {
                    opts.import_process = true;
                }
                "--verify-process-bundle" => {
                    opts.verify_process_bundle = true;
                }
                other if other.starts_with('-') => {
                    anyhow::bail!("unknown flag: {other}");
                }
                _ => {}
            }
            i += 1;
        }

        #[cfg(feature = "agent-socket")]
        if let Some(port) = agent_socket_port {
            opts.agent_socket = Some(SocketAddr::from(([127, 0, 0, 1], port)));
        }

        Ok(opts)
    }
}

fn parse_positive_px(raw: &str, flag: &str) -> anyhow::Result<f32> {
    let v: f32 = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("{flag} must be a number, got `{raw}`"))?;
    if !(v.is_finite() && v > 0.0) {
        anyhow::bail!("{flag} must be a positive finite number, got `{raw}`");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags() {
        let opts = LaunchOptions::from_args([
            "tod".into(),
            "--width".into(),
            "1024".into(),
            "--height".into(),
            "600".into(),
            "--data-root".into(),
            r"c:\sandbox".into(),
            "--agent".into(),
            "mock".into(),
            "--log-level".into(),
            "debug".into(),
            "--no-focus".into(),
        ])
        .unwrap();
        assert_eq!(opts.width, 1024.0);
        assert_eq!(opts.height, 600.0);
        assert!(opts.width_from_cli);
        assert!(opts.height_from_cli);
        assert!(opts.no_focus);
        assert_eq!(
            opts.data_root.as_deref(),
            Some(std::path::Path::new(r"c:\sandbox"))
        );
        assert_eq!(opts.agent_backend, AgentBackend::Mock);
        assert!(opts.agent_backend_from_cli);
        assert_eq!(opts.log_level, Some(LogLevel::Debug));
    }

    #[cfg(feature = "agent-socket")]
    #[test]
    fn parses_agent_socket_flag() {
        let opts = LaunchOptions::from_args([
            "tod".into(),
            "--agent-socket".into(),
            "127.0.0.1:9876".into(),
        ])
        .unwrap();
        assert_eq!(opts.agent_socket.unwrap().to_string(), "127.0.0.1:9876");
    }

    #[test]
    fn defaults_without_flags() {
        let opts = LaunchOptions::from_args(["tod".into()]).unwrap();
        assert_eq!(opts.width, 1280.0);
        assert_eq!(opts.height, 768.0);
        assert!(!opts.width_from_cli);
        assert!(!opts.height_from_cli);
        #[cfg(feature = "agent-socket")]
        assert!(opts.agent_socket.is_none());
        assert!(opts.data_root.is_none());
        assert_eq!(opts.agent_backend, AgentBackend::Cursor);
        assert!(!opts.agent_backend_from_cli);
        assert!(!opts.no_focus);
        assert!(opts.log_level.is_none());
    }

    #[cfg(feature = "agent-socket")]
    #[test]
    fn parses_agent_socket_port_flag() {
        let opts =
            LaunchOptions::from_args(["tod".into(), "--agent-socket-port".into(), "9877".into()])
                .unwrap();
        assert_eq!(opts.agent_socket.unwrap().to_string(), "127.0.0.1:9877");
    }

    #[cfg(feature = "agent-socket")]
    #[test]
    fn rejects_conflicting_agent_socket_flags() {
        let err = LaunchOptions::from_args([
            "tod".into(),
            "--agent-socket".into(),
            "127.0.0.1:9876".into(),
            "--agent-socket-port".into(),
            "9877".into(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[cfg(not(feature = "agent-socket"))]
    #[test]
    fn rejects_agent_socket_without_feature() {
        let err = LaunchOptions::from_args([
            "tod".into(),
            "--agent-socket".into(),
            "127.0.0.1:9876".into(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("agent-socket"));
    }

    #[test]
    fn rejects_unknown_log_level() {
        let err = LaunchOptions::from_args(["tod".into(), "--log-level".into(), "warn".into()])
            .unwrap_err();
        assert!(err.to_string().contains("log-level"));
    }
}
