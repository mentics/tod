use std::net::SocketAddr;

/// Launch options for window size and optional agent control socket.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub width: f32,
    pub height: f32,
    pub agent_socket: Option<SocketAddr>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            width: 1280.0,
            height: 768.0,
            agent_socket: None,
        }
    }
}

impl LaunchOptions {
    /// Parse `--width`, `--height`, and `--agent-socket` from argv (after binary name).
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
        ])
        .unwrap();
        assert_eq!(opts.width, 1024.0);
        assert_eq!(opts.height, 600.0);
        assert_eq!(
            opts.agent_socket.unwrap().to_string(),
            "127.0.0.1:9876"
        );
    }

    #[test]
    fn defaults_without_flags() {
        let opts = LaunchOptions::from_args(["tod".into()]).unwrap();
        assert_eq!(opts.width, 1280.0);
        assert_eq!(opts.height, 768.0);
        assert!(opts.agent_socket.is_none());
    }
}
