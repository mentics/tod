use crate::interview::agent::AgentBackend;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Launch options for window size, optional agent control socket, sandbox root, agent backend.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub width: f32,
    pub height: f32,
    pub agent_socket: Option<SocketAddr>,
    /// When set, all `TodPaths` resolve under this root (isolated sandbox).
    pub data_root: Option<PathBuf>,
    pub agent_backend: AgentBackend,
    /// When true, open the window without stealing OS keyboard focus.
    pub no_focus: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            width: 1280.0,
            height: 768.0,
            agent_socket: None,
            data_root: None,
            agent_backend: AgentBackend::Cursor,
            no_focus: false,
        }
    }
}

impl LaunchOptions {
    /// Parse CLI flags from argv (after binary name).
    pub fn from_args(args: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut opts = Self::default();
        let mut iter = args.into_iter();
        // Skip binary name when present.
        let first = iter.next();
        let mut rest: Vec<String> = iter.collect();
        if let Some(f) = first {
            if f.starts_with('-') {
                rest.insert(0, f);
            }
        }

        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--width" => {
                    i += 1;
                    let v = rest
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("--width requires a value"))?;
                    opts.width = parse_positive_px(v, "--width")?;
                }
                "--height" => {
                    i += 1;
                    let v = rest
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("--height requires a value"))?;
                    opts.height = parse_positive_px(v, "--height")?;
                }
                "--agent-socket" => {
                    i += 1;
                    let v = rest
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("--agent-socket requires host:port"))?;
                    opts.agent_socket = Some(
                        v.parse::<SocketAddr>()
                            .map_err(|e| anyhow::anyhow!("invalid --agent-socket `{v}`: {e}"))?,
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
                        .ok_or_else(|| anyhow::anyhow!("--agent requires mock|cursor"))?;
                    opts.agent_backend = AgentBackend::parse(v)?;
                }
                "--no-focus" => {
                    opts.no_focus = true;
                }
                other if other.starts_with('-') => {
                    anyhow::bail!("unknown flag: {other}");
                }
                _ => {}
            }
            i += 1;
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
            "--agent-socket".into(),
            "127.0.0.1:9876".into(),
            "--data-root".into(),
            r"c:\sandbox".into(),
            "--agent".into(),
            "mock".into(),
            "--no-focus".into(),
        ])
        .unwrap();
        assert_eq!(opts.width, 1024.0);
        assert_eq!(opts.height, 600.0);
        assert!(opts.no_focus);
        assert_eq!(opts.agent_socket.unwrap().to_string(), "127.0.0.1:9876");
        assert_eq!(
            opts.data_root.as_deref(),
            Some(std::path::Path::new(r"c:\sandbox"))
        );
        assert_eq!(opts.agent_backend, AgentBackend::Mock);
    }

    #[test]
    fn defaults_without_flags() {
        let opts = LaunchOptions::from_args(["tod".into()]).unwrap();
        assert_eq!(opts.width, 1280.0);
        assert_eq!(opts.height, 768.0);
        assert!(opts.agent_socket.is_none());
        assert!(opts.data_root.is_none());
        assert_eq!(opts.agent_backend, AgentBackend::Cursor);
        assert!(!opts.no_focus);
    }
}
