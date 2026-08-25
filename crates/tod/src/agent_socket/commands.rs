use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum Command {
    Key { keystroke: String },
    Click { x: f32, y: f32 },
    Shot {
        path: PathBuf,
        crop: Option<(f32, f32, f32, f32)>,
    },
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
    fn parses_click_and_shot_crop() {
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
    }
}
