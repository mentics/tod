use gpui::{Keystroke, Modifiers};

/// Selection-depth / camera commands. Physical keys live in [`Keymap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Move(Dir),
    MoveExtend(Dir),
    Pan(Dir),
    ZoomIn,
    ZoomOut,
    In,
    Out,
    Edit,
    Create,
    Delete,
    Connect,
    ToggleSelect,
    FitView,
    Undo,
    Redo,
    ExitEdit,
    CommitEdit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Depth {
    #[default]
    Graph,
    Node,
    Edit,
}

/// One remappable binding: GPUI keystroke spec → command.
#[derive(Clone, Debug)]
pub struct Binding {
    pub spec: &'static str,
    pub command: Command,
}

/// Default map from [keyboard.md](../../../../.local/projects/nov/visualization/keyboard.md).
/// Replace `bindings` to remap.
#[derive(Clone, Debug)]
pub struct Keymap {
    pub bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: vec![
                Binding {
                    spec: "w",
                    command: Command::Move(Dir::Up),
                },
                Binding {
                    spec: "a",
                    command: Command::Move(Dir::Left),
                },
                Binding {
                    spec: "s",
                    command: Command::Move(Dir::Down),
                },
                Binding {
                    spec: "d",
                    command: Command::Move(Dir::Right),
                },
                Binding {
                    spec: "up",
                    command: Command::Move(Dir::Up),
                },
                Binding {
                    spec: "down",
                    command: Command::Move(Dir::Down),
                },
                Binding {
                    spec: "left",
                    command: Command::Move(Dir::Left),
                },
                Binding {
                    spec: "right",
                    command: Command::Move(Dir::Right),
                },
                Binding {
                    spec: "shift-up",
                    command: Command::MoveExtend(Dir::Up),
                },
                Binding {
                    spec: "shift-down",
                    command: Command::MoveExtend(Dir::Down),
                },
                Binding {
                    spec: "shift-left",
                    command: Command::MoveExtend(Dir::Left),
                },
                Binding {
                    spec: "shift-right",
                    command: Command::MoveExtend(Dir::Right),
                },
                Binding {
                    spec: "shift-w",
                    command: Command::Pan(Dir::Up),
                },
                Binding {
                    spec: "shift-a",
                    command: Command::Pan(Dir::Left),
                },
                Binding {
                    spec: "shift-s",
                    command: Command::Pan(Dir::Down),
                },
                Binding {
                    spec: "shift-d",
                    command: Command::Pan(Dir::Right),
                },
                Binding {
                    spec: "r",
                    command: Command::In,
                },
                Binding {
                    spec: "enter",
                    command: Command::In,
                },
                Binding {
                    spec: "e",
                    command: Command::Out,
                },
                Binding {
                    spec: "shift-r",
                    command: Command::ZoomIn,
                },
                Binding {
                    spec: "shift-e",
                    command: Command::ZoomOut,
                },
                Binding {
                    spec: "q",
                    command: Command::Edit,
                },
                Binding {
                    spec: "f",
                    command: Command::Create,
                },
                Binding {
                    spec: "x",
                    command: Command::Delete,
                },
                Binding {
                    spec: "backspace",
                    command: Command::Delete,
                },
                Binding {
                    spec: "delete",
                    command: Command::Delete,
                },
                Binding {
                    spec: "c",
                    command: Command::Connect,
                },
                Binding {
                    spec: "ctrl-space",
                    command: Command::ToggleSelect,
                },
                Binding {
                    spec: "g",
                    command: Command::FitView,
                },
                Binding {
                    spec: "escape",
                    command: Command::ExitEdit,
                },
                Binding {
                    spec: "ctrl-enter",
                    command: Command::CommitEdit,
                },
                Binding {
                    spec: "cmd-z",
                    command: Command::Undo,
                },
                Binding {
                    spec: "ctrl-z",
                    command: Command::Undo,
                },
                Binding {
                    spec: "cmd-shift-z",
                    command: Command::Redo,
                },
                Binding {
                    spec: "ctrl-shift-z",
                    command: Command::Redo,
                },
            ],
        }
    }
}

impl Keymap {
    pub fn command_for(&self, keystroke: &Keystroke) -> Option<Command> {
        let normalized = normalize_keystroke(keystroke);
        for binding in &self.bindings {
            if let Ok(parsed) = Keystroke::parse(binding.spec) {
                if keystrokes_match(&normalized, &parsed) {
                    return Some(binding.command);
                }
            }
        }
        None
    }
}

fn normalize_keystroke(ks: &Keystroke) -> Keystroke {
    Keystroke {
        modifiers: ks.modifiers,
        key: ks.key.clone(),
        key_char: ks.key_char.clone(),
    }
}

fn keystrokes_match(typed: &Keystroke, bound: &Keystroke) -> bool {
    typed.key == bound.key && modifiers_match(typed.modifiers, bound.modifiers)
}

fn modifiers_match(a: Modifiers, b: Modifiers) -> bool {
    a.shift == b.shift
        && a.control == b.control
        && a.alt == b.alt
        && a.platform == b.platform
        && a.function == b.function
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(spec: &str) -> Keystroke {
        Keystroke::parse(spec).expect(spec)
    }

    #[test]
    fn wasd_move_and_shift_pan() {
        let map = Keymap::default();
        assert_eq!(map.command_for(&ks("w")), Some(Command::Move(Dir::Up)));
        assert_eq!(map.command_for(&ks("shift-w")), Some(Command::Pan(Dir::Up)));
        assert_eq!(
            map.command_for(&ks("shift-up")),
            Some(Command::MoveExtend(Dir::Up))
        );
        assert_eq!(map.command_for(&ks("r")), Some(Command::In));
        assert_eq!(map.command_for(&ks("shift-r")), Some(Command::ZoomIn));
        assert_eq!(map.command_for(&ks("e")), Some(Command::Out));
        assert_eq!(map.command_for(&ks("g")), Some(Command::FitView));
        assert_eq!(
            map.command_for(&ks("ctrl-space")),
            Some(Command::ToggleSelect)
        );
    }
}
