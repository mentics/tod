use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptsCommand {
    Open,
    Close,
    Focus,
    Status,
}

#[derive(Debug, Clone)]
pub enum AgentPlatformSocketCommand {
    Get,
    Cycle,
    Set(String),
}

#[derive(Debug, Clone)]
pub enum Command {
    Key {
        keystroke: String,
    },
    /// Insert text into the focused GPUI input handler (after a prior focus click + sync).
    Text {
        text: String,
    },
    Click {
        x: f32,
        y: f32,
    },
    Shot {
        path: PathBuf,
        crop: Option<(f32, f32, f32, f32)>,
    },
    /// Wait one UI frame (layout/paint) before the next observation.
    Sync,
    Transcripts(TranscriptsCommand),
    AgentPlatform(AgentPlatformSocketCommand),
}

/// Parse one protocol line into a command.
pub fn parse_line(line: &str) -> Result<Command, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty command".into());
    }

    let mut parts = line.split_whitespace();
    let verb = parts.next().unwrap_or("");
    match verb {
        "key" => {
            let rest = line["key".len()..].trim();
            if rest.is_empty() {
                return Err("key requires a keystroke".into());
            }
            Ok(Command::Key {
                keystroke: rest.to_string(),
            })
        }
        "text" => {
            let rest = line["text".len()..].trim();
            if rest.is_empty() {
                return Err("text requires a string".into());
            }
            Ok(Command::Text {
                text: rest.to_string(),
            })
        }
        "click" => {
            let x = parts
                .next()
                .ok_or_else(|| "click requires x y".to_string())?;
            let y = parts
                .next()
                .ok_or_else(|| "click requires x y".to_string())?;
            if parts.next().is_some() {
                return Err("click takes exactly two numbers".into());
            }
            Ok(Command::Click {
                x: parse_coord(x, "x")?,
                y: parse_coord(y, "y")?,
            })
        }
        "shot" => {
            let path = parts
                .next()
                .ok_or_else(|| "shot requires a path".to_string())?;
            let crop = match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (None, None, None, None) => None,
                (Some(x0), Some(y0), Some(x1), Some(y1)) => {
                    if parts.next().is_some() {
                        return Err("shot crop is path [x0 y0 x1 y1]".into());
                    }
                    Some((
                        parse_coord(x0, "x0")?,
                        parse_coord(y0, "y0")?,
                        parse_coord(x1, "x1")?,
                        parse_coord(y1, "y1")?,
                    ))
                }
                _ => return Err("shot crop is path [x0 y0 x1 y1]".into()),
            };
            Ok(Command::Shot {
                path: PathBuf::from(path),
                crop,
            })
        }
        "sync" => {
            if parts.next().is_some() {
                return Err("sync takes no arguments".into());
            }
            Ok(Command::Sync)
        }
        "transcripts" => {
            let action = parts
                .next()
                .ok_or_else(|| "transcripts requires open|close|focus|status".to_string())?;
            if parts.next().is_some() {
                return Err("transcripts takes exactly one argument".into());
            }
            Ok(Command::Transcripts(match action {
                "open" => TranscriptsCommand::Open,
                "close" => TranscriptsCommand::Close,
                "focus" => TranscriptsCommand::Focus,
                "status" => TranscriptsCommand::Status,
                other => return Err(format!("unknown transcripts action `{other}`")),
            }))
        }
        "agent-platform" => {
            let action = parts
                .next()
                .ok_or_else(|| "agent-platform requires get|cycle|set".to_string())?;
            match action {
                "get" => {
                    if parts.next().is_some() {
                        return Err("agent-platform get takes no arguments".into());
                    }
                    Ok(Command::AgentPlatform(AgentPlatformSocketCommand::Get))
                }
                "cycle" => {
                    if parts.next().is_some() {
                        return Err("agent-platform cycle takes no arguments".into());
                    }
                    Ok(Command::AgentPlatform(AgentPlatformSocketCommand::Cycle))
                }
                "set" => {
                    let value = parts
                        .next()
                        .ok_or_else(|| "agent-platform set requires cursor|claude".to_string())?;
                    if parts.next().is_some() {
                        return Err("agent-platform set takes exactly one value".into());
                    }
                    Ok(Command::AgentPlatform(AgentPlatformSocketCommand::Set(
                        value.to_string(),
                    )))
                }
                other => Err(format!(
                    "unknown agent-platform action `{other}` (expected get|cycle|set)"
                )),
            }
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

fn parse_coord(raw: &str, name: &str) -> Result<f32, String> {
    raw.parse::<f32>()
        .map_err(|_| format!("invalid {name}: `{raw}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_with_modifiers() {
        let c = parse_line("key ctrl-enter").unwrap();
        match c {
            Command::Key { keystroke } => assert_eq!(keystroke, "ctrl-enter"),
            _ => panic!(),
        }
    }

    #[test]
    fn parses_click_shot_and_sync() {
        let c = parse_line("click 10 20.5").unwrap();
        match c {
            Command::Click { x, y } => {
                assert_eq!(x, 10.0);
                assert_eq!(y, 20.5);
            }
            _ => panic!(),
        }
        let c = parse_line("shot out.png 0 80 640 400").unwrap();
        match c {
            Command::Shot { path, crop } => {
                assert_eq!(path, PathBuf::from("out.png"));
                assert_eq!(crop, Some((0.0, 80.0, 640.0, 400.0)));
            }
            _ => panic!(),
        }
        assert!(matches!(parse_line("sync").unwrap(), Command::Sync));
    }

    #[test]
    fn parses_text_command() {
        let c = parse_line("text hello world").unwrap();
        match c {
            Command::Text { text } => assert_eq!(text, "hello world"),
            _ => panic!(),
        }
    }

    #[test]
    fn parses_transcripts_commands() {
        assert!(matches!(
            parse_line("transcripts open").unwrap(),
            Command::Transcripts(TranscriptsCommand::Open)
        ));
        assert!(matches!(
            parse_line("transcripts close").unwrap(),
            Command::Transcripts(TranscriptsCommand::Close)
        ));
        assert!(matches!(
            parse_line("transcripts status").unwrap(),
            Command::Transcripts(TranscriptsCommand::Status)
        ));
        assert!(parse_line("transcripts nope").is_err());
    }

    #[test]
    fn parses_agent_platform_commands() {
        assert!(matches!(
            parse_line("agent-platform get").unwrap(),
            Command::AgentPlatform(AgentPlatformSocketCommand::Get)
        ));
        assert!(matches!(
            parse_line("agent-platform cycle").unwrap(),
            Command::AgentPlatform(AgentPlatformSocketCommand::Cycle)
        ));
        match parse_line("agent-platform set claude").unwrap() {
            Command::AgentPlatform(AgentPlatformSocketCommand::Set(value)) => {
                assert_eq!(value, "claude");
            }
            _ => panic!("expected set"),
        }
    }

    #[test]
    fn rejects_removed_domain_helpers() {
        assert!(parse_line("shell launch task-1 cfg-1").is_err());
        assert!(parse_line("zed launch task-1 cfg-1").is_err());
        assert!(parse_line("terminal-agent launch cfg-1").is_err());
    }
}
