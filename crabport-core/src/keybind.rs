use gpui::{Keystroke, Modifiers};

// ---------------------------------------------------------------
// Key action
// ---------------------------------------------------------------

/// What happens when a key combination is matched.
#[derive(Clone, Debug)]
pub enum KeyAction {
    /// Send raw bytes to the PTY.
    Bytes(Vec<u8>),
    /// Invoke a named UI action (copy, paste, etc.).
    Action(TerminalAction),
}

/// Named terminal UI actions that are triggered by keybinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalAction {
    Copy,
    Paste,
    Search,
    // Future: ScrollUp, ScrollDown, etc.
}

// ---------------------------------------------------------------
// Key binding
// ---------------------------------------------------------------

/// A keybinding maps a `Modifiers + key` pair to something.
pub struct Binding {
    pub modifiers: Modifiers,
    pub key: &'static str, // matched against keystroke.key
    pub action: KeyAction,
}

// ---------------------------------------------------------------
// Default bindings
// ---------------------------------------------------------------

/// Return the default set of terminal key bindings.
///
/// The first matching binding wins, so put more-specific rules before
/// fallback rules.
pub fn default_bindings() -> Vec<Binding> {
    let ctrl = Modifiers {
        control: true,
        ..Modifiers::default()
    };

    let ctrl_shift = Modifiers {
        control: true,
        shift: true,
        ..Modifiers::default()
    };

    let platform = Modifiers {
        platform: true,
        ..Modifiers::default()
    };

    vec![
        // ---- Platform shortcuts (GUI, not sent to PTY) ----
        Binding {
            modifiers: platform,
            key: "c",
            action: KeyAction::Action(TerminalAction::Copy),
        },
        Binding {
            modifiers: platform,
            key: "v",
            action: KeyAction::Action(TerminalAction::Paste),
        },
        // ---- Search (platform key) ----
        // Cmd+F on macOS, Ctrl+F on Linux/Windows. This overrides the
        // Ctrl+F → 0x06 control-byte binding below so the search overlay
        // opens instead of sending ACK to the PTY.
        Binding {
            modifiers: platform,
            key: "f",
            action: KeyAction::Action(TerminalAction::Search),
        },
        // ---- Ctrl+Shift alternates ----
        Binding {
            modifiers: ctrl_shift,
            key: "c",
            action: KeyAction::Action(TerminalAction::Copy),
        },
        Binding {
            modifiers: ctrl_shift,
            key: "v",
            action: KeyAction::Action(TerminalAction::Paste),
        },
        // ---- Ctrl+letter → control characters 0x01–0x1a ----
        binding_ctrl("a", 0x01),
        binding_ctrl("b", 0x02),
        binding_ctrl("c", 0x03),
        binding_ctrl("d", 0x04),
        binding_ctrl("e", 0x05),
        binding_ctrl("f", 0x06),
        binding_ctrl("g", 0x07),
        binding_ctrl("h", 0x08),
        binding_ctrl("i", 0x09), // Tab (also handled by `tab` key below)
        binding_ctrl("j", 0x0a),
        binding_ctrl("k", 0x0b),
        binding_ctrl("l", 0x0c),
        binding_ctrl("m", 0x0d), // Carriage return
        binding_ctrl("n", 0x0e),
        binding_ctrl("o", 0x0f),
        binding_ctrl("p", 0x10),
        binding_ctrl("q", 0x11),
        binding_ctrl("r", 0x12),
        binding_ctrl("s", 0x13),
        binding_ctrl("t", 0x14),
        binding_ctrl("u", 0x15),
        binding_ctrl("v", 0x16),
        binding_ctrl("w", 0x17),
        binding_ctrl("x", 0x18),
        binding_ctrl("y", 0x19),
        binding_ctrl("z", 0x1a),
        // Ctrl+other keys
        // ---- Ctrl+other keys ----
        Binding {
            modifiers: ctrl,
            key: "space",
            action: KeyAction::Bytes(vec![0x00]),
        },
        Binding {
            modifiers: ctrl,
            key: "[",
            action: KeyAction::Bytes(vec![0x1b]),
        },
        Binding {
            modifiers: ctrl,
            key: "]",
            action: KeyAction::Bytes(vec![0x1d]),
        },
        Binding {
            modifiers: ctrl,
            key: "\\",
            action: KeyAction::Bytes(vec![0x1c]),
        },
        // ---- Special keys ----
        Binding {
            modifiers: Modifiers::default(),
            key: "enter",
            action: KeyAction::Bytes(vec![b'\r']),
        },
        Binding {
            modifiers: Modifiers::default(),
            key: "backspace",
            action: KeyAction::Bytes(vec![0x7f]),
        },
        Binding {
            modifiers: Modifiers::default(),
            key: "tab",
            action: KeyAction::Bytes(vec![b'\t']),
        },
        Binding {
            modifiers: Modifiers::default(),
            key: "escape",
            action: KeyAction::Bytes(vec![0x1b]),
        },
        // Cursor keys, Home/End, PageUp/PageDown, Insert/Delete and F1–F12
        // are encoded by [`encode_special_key`] at keypress time, so they can
        // follow the program's DECCKM mode and the xterm modifier encoding —
        // a static table can only ever send the unmodified CSI forms, which
        // is what made the cursor keys wrong inside vim/less.
    ]
}

// Helper: create a Ctrl+letter binding.
fn binding_ctrl(letter: &'static str, byte: u8) -> Binding {
    Binding {
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        key: letter,
        action: KeyAction::Bytes(vec![byte]),
    }
}

// ---------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------

/// Match a keystroke against the binding list.  Returns the first
/// binding whose modifiers are a subset of the event's modifiers
/// and whose key matches.
///
/// We use a subset match so that e.g. `Ctrl+Shift+C` doesn't
/// accidentally trigger the `Ctrl+C` binding.
pub fn resolve<'a>(keystroke: &Keystroke, bindings: &'a [Binding]) -> Option<&'a KeyAction> {
    let ev = &keystroke.modifiers;

    bindings
        .iter()
        .find(|b| {
            if keystroke.key.as_str() != b.key {
                return false;
            }
            // All modifiers in the binding must be present in the event.
            // Any extra modifiers in the event are ok (they won't match
            // a more-specific rule first — the list is ordered).
            let bm = &b.modifiers;
            (!bm.control || ev.control)
                && (!bm.shift || ev.shift)
                && (!bm.alt || ev.alt)
                && (!bm.platform || ev.platform)
                && (!bm.function || ev.function)
        })
        .map(|b| &b.action)
}
