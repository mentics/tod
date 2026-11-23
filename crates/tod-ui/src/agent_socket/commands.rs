//! Desktop automation commands and their wire syntax.

use super::selector::{self, Selector};
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
    /// Insert text into the focused input handler (after a prior focus click + sync).
    Text {
        text: String,
    },
    Click {
        x: f32,
        y: f32,
        /// The right button, for a row's context menu (`rclick`).
        right: bool,
    },
    /// A mouse wheel / trackpad scroll at a point. Deltas are pixels in the
    /// user's sense: positive `dy` scrolls down (content moves up), positive
    /// `dx` scrolls right (content moves left).
    Scroll {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    /// Press on one point, move to another, release: what reorders a list row.
    Drag {
        from: (f32, f32),
        to: (f32, f32),
    },
    /// One step of a drag held open, for looking at the UI mid-gesture.
    DragStep {
        step: DragStep,
        x: f32,
        y: f32,
    },
    Shot {
        path: PathBuf,
        crop: Option<(f32, f32, f32, f32)>,
    },
    /// Like `Shot`, but forces a fresh draw and renders the window offscreen,
    /// so it is current even while the window is covered or minimized.
    Shot2 {
        path: PathBuf,
        crop: Option<(f32, f32, f32, f32)>,
    },
    /// Wait one UI frame (layout/paint) before the next observation.
    Sync,
    /// Close every window, as the user would: the app then quits itself.
    Quit,
    /// Ask the window to close, as its close button does (the close guard).
    Close,
    Transcripts(TranscriptsCommand),
    AgentPlatform(AgentPlatformSocketCommand),
    /// Reply `ok driven|user|idle` (`agent_socket::mode`).
    Mode,
    /// Reply `ok W H`: the window's content size in logical pixels.
    Size,
    /// Set the window's content size; part of the agent's saved view.
    Resize {
        w: f32,
        h: f32,
    },
    /// The accessibility tree, one line per node (see `a11y_tree`).
    Tree(TreeQuery),
    /// Act on the one element a selector names, through its own handler.
    /// An input command, like `Click`.
    Act {
        selector: String,
        action: ActAction,
    },
    /// How many clickable elements the current frame has and how many the tree
    /// exposes; the ones it does not.
    /// `coverage [frames N]`: clickable-versus-exposed counts; with `frames`, also times N draws with the tree off and on.
    Coverage { frames: usize },
    /// Dev-only test seam: play an event of the *user* (see `TestUser`).
    TestUser(TestUser),
    /// Block until a selector matches (or, with `gone`, stops matching).
    WaitFor {
        selector: String,
        gone: bool,
        timeout_ms: u64,
    },
}

/// An event of the user's, delivered the way real input arrives (no
/// `synthetic` guard), so the capture listeners, the keystroke interceptor and
/// the bounds observer see it as theirs. Test-only: no real OS input is made.
#[derive(Debug, Clone, PartialEq)]
pub enum TestUser {
    Move { x: f32, y: f32 },
    Click { x: f32, y: f32 },
    Key(String),
    /// The window's content size changes and the app did not do it.
    Resize { w: f32, h: f32 },
    /// The pointer leaves the window.
    Leave,
    /// The window loses / regains key focus (what the activation observer reports).
    Blur,
    Focus,
    /// Reply what the view holds now, whoever has the app; changes nothing.
    Peek,
}

fn parse_test_user(rest: &str) -> Result<TestUser, String> {
    let usage = "usage: test-user move X Y | click X Y | key K | resize W H | leave | blur | focus | peek";
    let mut p = rest.split_whitespace();
    let verb = p.next().ok_or_else(|| usage.to_string())?;
    let mut num = |name: &str| -> Result<f32, String> {
        p.next()
            .ok_or_else(|| usage.to_string())
            .and_then(|v| parse_coord(v, name))
    };
    let out = match verb {
        "move" => TestUser::Move { x: num("X")?, y: num("Y")? },
        "click" => TestUser::Click { x: num("X")?, y: num("Y")? },
        "resize" => {
            let (w, h) = (num("W")?, num("H")?);
            if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0) {
                return Err("resize needs a positive W and H".into());
            }
            TestUser::Resize { w, h }
        }
        "key" => {
            let key = rest.trim()["key".len()..].trim();
            if key.is_empty() {
                return Err(usage.to_string());
            }
            TestUser::Key(key.to_string())
        }
        "leave" => TestUser::Leave,
        "blur" => TestUser::Blur,
        "focus" => TestUser::Focus,
        "peek" => TestUser::Peek,
        _ => return Err(usage.to_string()),
    };
    Ok(out)
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
        "click" | "rclick" => {
            let x = parts.next().ok_or_else(|| format!("{verb} requires x y"))?;
            let y = parts.next().ok_or_else(|| format!("{verb} requires x y"))?;
            if parts.next().is_some() {
                return Err(format!("{verb} takes exactly two numbers"));
            }
            Ok(Command::Click {
                x: parse_coord(x, "x")?,
                y: parse_coord(y, "y")?,
                right: verb == "rclick",
            })
        }
        "scroll" => {
            let mut num = |name: &str| {
                parts
                    .next()
                    .ok_or_else(|| "scroll requires x y dx dy".to_string())
                    .and_then(|value| {
                        value
                            .parse::<f32>()
                            .ok()
                            .filter(|v| v.is_finite())
                            .ok_or_else(|| format!("invalid {name}: {value}"))
                    })
            };
            let (x, y, dx, dy) = (num("x")?, num("y")?, num("dx")?, num("dy")?);
            if parts.next().is_some() {
                return Err("scroll takes exactly four numbers".into());
            }
            Ok(Command::Scroll { x, y, dx, dy })
        }
        "drag" => {
            let mut coord = |name: &str| {
                parts
                    .next()
                    .ok_or_else(|| "drag requires x1 y1 x2 y2".to_string())
                    .and_then(|value| parse_coord(value, name))
            };
            let from = (coord("x1")?, coord("y1")?);
            let to = (coord("x2")?, coord("y2")?);
            if parts.next().is_some() {
                return Err("drag takes exactly four numbers".into());
            }
            Ok(Command::Drag { from, to })
        }
        "press" | "moveto" | "release" => {
            let x = parts.next().ok_or_else(|| format!("{verb} requires x y"))?;
            let y = parts.next().ok_or_else(|| format!("{verb} requires x y"))?;
            if parts.next().is_some() {
                return Err(format!("{verb} takes exactly two numbers"));
            }
            let step = match verb {
                "press" => DragStep::Press,
                "moveto" => DragStep::Move,
                _ => DragStep::Release,
            };
            Ok(Command::DragStep {
                step,
                x: parse_coord(x, "x")?,
                y: parse_coord(y, "y")?,
            })
        }
        "shot" | "shot2" => {
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
            let path = PathBuf::from(path);
            Ok(if verb == "shot2" {
                Command::Shot2 { path, crop }
            } else {
                Command::Shot { path, crop }
            })
        }
        "quit" => {
            if parts.next().is_some() {
                return Err("quit takes no arguments".into());
            }
            Ok(Command::Quit)
        }
        "close" => {
            if parts.next().is_some() {
                return Err("close takes no arguments".into());
            }
            Ok(Command::Close)
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
                        .ok_or_else(|| "agent-platform set requires cursor|claude|openai|google".to_string())?;
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
        "mode" => {
            if parts.next().is_some() {
                return Err("mode takes no arguments".into());
            }
            Ok(Command::Mode)
        }
        "size" => {
            if parts.next().is_some() {
                return Err("size takes no arguments".into());
            }
            Ok(Command::Size)
        }
        "resize" => {
            let w = parts.next().ok_or_else(|| "resize requires W H".to_string())?;
            let h = parts.next().ok_or_else(|| "resize requires W H".to_string())?;
            if parts.next().is_some() {
                return Err("resize takes exactly two numbers".into());
            }
            let (w, h) = (parse_coord(w, "W")?, parse_coord(h, "H")?);
            if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0) {
                return Err("resize needs a positive W and H".into());
            }
            Ok(Command::Resize { w, h })
        }
        "test-user" => Ok(Command::TestUser(parse_test_user(line["test-user".len()..].trim())?)),
        "coverage" => {
            let frames = match (parts.next(), parts.next(), parts.next()) {
                (None, _, _) => 0,
                (Some("frames"), Some(n), None) => n
                    .parse::<usize>()
                    .map_err(|_| format!("`{n}` is not a frame count"))?,
                _ => return Err("usage: coverage [frames N]".into()),
            };
            Ok(Command::Coverage { frames })
        }
        "tree" => Ok(Command::Tree(parse_tree(line["tree".len()..].trim())?)),
        "act" => {
            let (selector, action) = parse_act(line["act".len()..].trim())?;
            Ok(Command::Act { selector, action })
        }
        "wait-for" => {
            let (selector, gone, timeout_ms) =
                parse_wait_for(line["wait-for".len()..].trim())?;
            Ok(Command::WaitFor { selector, gone, timeout_ms })
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

/// One step of a drag held open: press, move, or release.
///
/// `drag` does the whole gesture in one message and is what a test wants. This
/// is for looking at the UI *during* a drag — the landing line a list draws
/// under the pointer only exists while the button is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragStep {
    Press,
    Move,
    Release,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeQuery {
    pub role: Option<String>,
    pub label: Option<String>,
    pub within: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActAction {
    Click,
    Focus,
    SetValue(String),
    ScrollIntoView,
}

/// Whitespace-separated tokens of `text` with their byte ranges; a double-quoted
/// run (`"a b"`, with `\"` inside) is part of one token, quotes kept.
fn tokens(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut quoted, mut start) = (0, false, None);
    while i < bytes.len() {
        let b = bytes[i];
        if quoted && b == b'\\' {
            i += 2;
            continue;
        }
        if b == b'"' {
            quoted = !quoted;
            start.get_or_insert(i);
        } else if b.is_ascii_whitespace() && !quoted {
            if let Some(s) = start.take() {
                out.push((s, i));
            }
        } else {
            start.get_or_insert(i);
        }
        i += 1;
    }
    if let Some(s) = start {
        out.push((s, text.len()));
    }
    out
}

/// A flag value or action argument: the text, unquoted when it is one quoted string.
fn unquoted(text: &str) -> String {
    selector::unquote_whole(text.trim()).unwrap_or_else(|| text.trim().to_string())
}

/// `tree [--role R] [--label S] [--within SEL] [--json]`. A flag's value runs to
/// the next `--flag`, so none of them needs quoting (quote one to hold a `--`).
pub fn parse_tree(rest: &str) -> Result<TreeQuery, String> {
    let mut query = TreeQuery::default();
    let ranges = tokens(rest);
    let mut tokens = ranges.iter().map(|&(a, b)| &rest[a..b]).peekable();
    while let Some(token) = tokens.next() {
        let mut value = || {
            let mut words = Vec::new();
            while let Some(next) = tokens.peek() {
                if next.starts_with("--") {
                    break;
                }
                words.push(tokens.next().unwrap_or_default());
            }
            if words.is_empty() {
                Err(format!("{token} needs a value"))
            } else {
                Ok(words.join(" "))
            }
        };
        match token {
            "--json" => query.json = true,
            "--role" => query.role = Some(unquoted(&value()?)),
            "--label" => query.label = Some(unquoted(&value()?)),
            // A selector keeps its quotes: `Selector::parse` reads them.
            "--within" => query.within = Some(value()?),
            other => return Err(format!("unknown tree option `{other}`")),
        }
    }
    if let Some(within) = &query.within {
        Selector::parse(within)?;
    }
    Ok(query)
}

const ACTION_WORDS: [&str; 4] = ["click", "focus", "set-value", "scroll-into-view"];

/// `act <selector> [click|focus|set-value <text>|scroll-into-view]`; the action
/// is the end of the line, the selector everything before it. When the line has
/// a double quote anywhere (`act "textinput:Item title" set-value Cov node`,
/// `act button:"Run now" click`) the selector ends at the first unquoted action
/// word instead, so a quoted label may contain one.
pub fn parse_act(rest: &str) -> Result<(String, ActAction), String> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err("act requires a selector".into());
    }
    if rest.contains('"') {
        let split = tokens(rest)
            .into_iter()
            .skip(1)
            .find(|&(a, b)| ACTION_WORDS.contains(&&rest[a..b]));
        let (selector, action) = match split {
            None => (rest, ActAction::Click),
            Some((a, b)) => {
                let after = rest[b..].trim();
                let action = match &rest[a..b] {
                    "set-value" => ActAction::SetValue(unquoted(after)),
                    word => {
                        if !after.is_empty() {
                            return Err(format!("unexpected text after `{word}`: {after}"));
                        }
                        match word {
                            "click" => ActAction::Click,
                            "focus" => ActAction::Focus,
                            _ => ActAction::ScrollIntoView,
                        }
                    }
                };
                (rest[..a].trim(), action)
            }
        };
        Selector::parse(selector)?;
        return Ok((selector.to_string(), action));
    }
    let (selector, action) = if let Some((sel, text)) = rest.split_once(" set-value ") {
        (sel, ActAction::SetValue(text.trim().to_string()))
    } else if let Some(sel) = rest.strip_suffix(" set-value") {
        (sel, ActAction::SetValue(String::new()))
    } else if let Some(sel) = rest.strip_suffix(" scroll-into-view") {
        (sel, ActAction::ScrollIntoView)
    } else if let Some(sel) = rest.strip_suffix(" focus") {
        (sel, ActAction::Focus)
    } else if let Some(sel) = rest.strip_suffix(" click") {
        (sel, ActAction::Click)
    } else {
        (rest, ActAction::Click)
    };
    Selector::parse(selector)?;
    Ok((selector.trim().to_string(), action))
}

/// `wait-for <selector> [--gone] [--timeout-ms N]`; the selector may be quoted.
pub fn parse_wait_for(rest: &str) -> Result<(String, bool, u64), String> {
    let mut gone = false;
    let mut timeout_ms = 5000u64;
    let mut selector = Vec::new();
    let ranges = tokens(rest);
    let mut words = ranges.iter().map(|&(a, b)| &rest[a..b]);
    while let Some(token) = words.next() {
        match token {
            "--gone" => gone = true,
            "--timeout-ms" => {
                let value = words.next().ok_or("--timeout-ms needs a number")?;
                timeout_ms = value
                    .parse()
                    .map_err(|_| format!("invalid --timeout-ms `{value}`"))?;
            }
            other => selector.push(other),
        }
    }
    if selector.is_empty() {
        return Err("wait-for requires a selector".into());
    }
    let selector = selector.join(" ");
    Selector::parse(&selector)?;
    Ok((selector, gone, timeout_ms))
}

fn parse_coord(raw: &str, name: &str) -> Result<f32, String> {
    raw.parse::<f32>()
        .map_err(|_| format!("invalid {name}: `{raw}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tree_options() {
        let q = parse_tree("").unwrap();
        assert_eq!(q, TreeQuery::default());
        let q = parse_tree("--role button --label Run now --within #task-panel >> group: --json").unwrap();
        assert_eq!(q.role.as_deref(), Some("button"));
        assert_eq!(q.label.as_deref(), Some("Run now"));
        assert_eq!(q.within.as_deref(), Some("#task-panel >> group:"));
        assert!(q.json);
        assert!(parse_tree("--role").is_err());
        assert!(parse_tree("--bogus x").is_err());
        assert!(parse_tree("--within nonsense").is_err());
    }

    #[test]
    fn parses_act_actions_off_the_end() {
        assert_eq!(
            parse_act("button:Resume").unwrap(),
            ("button:Resume".into(), ActAction::Click)
        );
        assert_eq!(
            parse_act("button:Resume now click").unwrap(),
            ("button:Resume now".into(), ActAction::Click)
        );
        assert_eq!(
            parse_act("#chat/input focus").unwrap(),
            ("#chat/input".into(), ActAction::Focus)
        );
        assert_eq!(
            parse_act("textinput:Message set-value hello  big world").unwrap(),
            (
                "textinput:Message".into(),
                ActAction::SetValue("hello  big world".into())
            )
        );
        assert_eq!(
            parse_act("#row-9 scroll-into-view").unwrap(),
            ("#row-9".into(), ActAction::ScrollIntoView)
        );
        assert_eq!(
            parse_act("#a >> button:Go").unwrap(),
            ("#a >> button:Go".into(), ActAction::Click)
        );
        assert!(parse_act("").is_err());
        assert!(parse_act("nonsense").is_err());
    }

    #[test]
    fn parses_wait_for() {
        assert_eq!(
            parse_wait_for("button:Resume now").unwrap(),
            ("button:Resume now".into(), false, 5000)
        );
        assert_eq!(
            parse_wait_for("#runner-menu --gone --timeout-ms 250").unwrap(),
            ("#runner-menu".into(), true, 250)
        );
        assert!(parse_wait_for("--gone").is_err());
        assert!(parse_wait_for("#a --timeout-ms x").is_err());
    }

    #[test]
    fn quoted_selectors_in_act_tree_and_wait_for() {
        assert_eq!(
            parse_act(r#""textinput:Item title" set-value Cov node"#).unwrap(),
            (
                r#""textinput:Item title""#.into(),
                ActAction::SetValue("Cov node".into())
            )
        );
        assert_eq!(
            parse_act(r#"textinput:"Item title" set-value "quoted value""#).unwrap(),
            (
                r#"textinput:"Item title""#.into(),
                ActAction::SetValue("quoted value".into())
            )
        );
        // A quoted label may hold an action word.
        assert_eq!(
            parse_act(r#"button:"Click here to focus" focus"#).unwrap(),
            (r#"button:"Click here to focus""#.into(), ActAction::Focus)
        );
        assert_eq!(
            parse_act(r##""#a >> button:Go now""##).unwrap(),
            (r##""#a >> button:Go now""##.into(), ActAction::Click)
        );
        assert_eq!(
            parse_act(r#"button:"Run now" click"#).unwrap(),
            (r#"button:"Run now""#.into(), ActAction::Click)
        );
        assert!(parse_act(r#"button:"Run now" click extra"#).is_err());
        let q = parse_tree(r##"--label "two  words" --within "#a >> button:Go now" --role button"##).unwrap();
        assert_eq!(q.label.as_deref(), Some("two  words"));
        assert_eq!(q.within.as_deref(), Some(r##""#a >> button:Go now""##));
        assert_eq!(q.role.as_deref(), Some("button"));
        assert_eq!(
            parse_wait_for(r#"textinput:"Item  title" --gone --timeout-ms 9"#).unwrap(),
            (r#"textinput:"Item  title""#.into(), true, 9)
        );
    }

    #[test]
    fn parses_test_user_events() {
        let t = |l: &str| match parse_line(l).unwrap() {
            Command::TestUser(t) => t,
            other => panic!("{other:?}"),
        };
        assert_eq!(t("test-user move 1 2.5"), TestUser::Move { x: 1.0, y: 2.5 });
        assert_eq!(t("test-user click 3 4"), TestUser::Click { x: 3.0, y: 4.0 });
        assert_eq!(t("test-user key ctrl-j"), TestUser::Key("ctrl-j".into()));
        assert_eq!(t("test-user resize 900 600"), TestUser::Resize { w: 900.0, h: 600.0 });
        assert_eq!(t("test-user leave"), TestUser::Leave);
        assert_eq!(t("test-user peek"), TestUser::Peek);
        for bad in ["test-user", "test-user key", "test-user move 1", "test-user resize 0 5", "test-user nope"] {
            assert!(parse_line(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_mode_size_and_resize() {
        assert!(matches!(parse_line("mode").unwrap(), Command::Mode));
        assert!(matches!(parse_line("size").unwrap(), Command::Size));
        match parse_line("resize 1280 800").unwrap() {
            Command::Resize { w, h } => assert_eq!((w, h), (1280.0, 800.0)),
            other => panic!("{other:?}"),
        }
        assert!(parse_line("resize 1280").is_err());
        assert!(parse_line("resize 0 10").is_err());
        assert!(parse_line("mode now").is_err());
    }

    #[test]
    fn parses_close() {
        assert!(matches!(parse_line("close").unwrap(), Command::Close));
    }

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
            Command::Click { x, y, right } => {
                assert_eq!(x, 10.0);
                assert_eq!(y, 20.5);
                assert!(!right);
            }
            _ => panic!(),
        }
        // The same coordinates, right button: what opens a row's menu.
        let c = parse_line("rclick 10 20").unwrap();
        match c {
            Command::Click { x, y, right } => {
                assert_eq!((x, y), (10.0, 20.0));
                assert!(right);
            }
            _ => panic!(),
        }
        assert!(parse_line("rclick 10").is_err());
        let c = parse_line("scroll 10 20 0 -120.5").unwrap();
        match c {
            Command::Scroll { x, y, dx, dy } => assert_eq!((x, y, dx, dy), (10.0, 20.0, 0.0, -120.5)),
            _ => panic!(),
        }
        assert!(parse_line("scroll 10 20 5").is_err());
        assert!(parse_line("scroll 1 2 3 4 5").is_err());
        assert!(parse_line("scroll a 2 3 4").is_err());
        let c = parse_line("drag 10 20 30 44").unwrap();
        match c {
            Command::Drag { from, to } => {
                assert_eq!(from, (10.0, 20.0));
                assert_eq!(to, (30.0, 44.0));
            }
            _ => panic!(),
        }
        assert!(parse_line("drag 10 20 30").is_err());
        assert!(parse_line("drag 10 20 30 44 50").is_err());
        let c = parse_line("shot out.png 0 80 640 400").unwrap();
        match c {
            Command::Shot { path, crop } => {
                assert_eq!(path, PathBuf::from("out.png"));
                assert_eq!(crop, Some((0.0, 80.0, 640.0, 400.0)));
            }
            _ => panic!(),
        }
        assert!(matches!(parse_line("sync").unwrap(), Command::Sync));
        assert!(matches!(parse_line("quit").unwrap(), Command::Quit));
        assert!(parse_line("quit now").is_err());
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
    fn parses_tree_act_and_wait_for() {
        assert!(matches!(parse_line("tree").unwrap(), Command::Tree(_)));
        match parse_line("tree --role button --json").unwrap() {
            Command::Tree(q) => {
                assert_eq!(q.role.as_deref(), Some("button"));
                assert!(q.json);
            }
            _ => panic!(),
        }
        match parse_line("act button:Resume").unwrap() {
            Command::Act { selector, action } => {
                assert_eq!(selector, "button:Resume");
                assert_eq!(action, ActAction::Click);
            }
            _ => panic!(),
        }
        assert!(parse_line("act").is_err());
        match parse_line("wait-for #runner-menu --gone --timeout-ms 100").unwrap() {
            Command::WaitFor { selector, gone, timeout_ms } => {
                assert_eq!((selector.as_str(), gone, timeout_ms), ("#runner-menu", true, 100));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn rejects_removed_domain_helpers() {
        assert!(parse_line("shell launch task-1 cfg-1").is_err());
        assert!(parse_line("zed launch task-1 cfg-1").is_err());
        assert!(parse_line("terminal-agent launch cfg-1").is_err());
    }
}
