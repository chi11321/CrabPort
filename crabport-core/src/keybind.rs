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

/// The xterm modifier parameter for a key event: `1 + shift + 2*alt + 4*ctrl`.
/// `1` is the protocol's "no modifier" value.
fn modifier_parameter(m: &Modifiers) -> u8 {
    1 + (m.shift as u8) + 2 * (m.alt as u8) + 4 * (m.control as u8)
}

/// Encode a special (non-text) key the way xterm does, honouring the
/// program's DECCKM mode and the xterm modifier encoding:
///
/// - Arrow, Home and End use the CSI forms (`ESC [ A`, `ESC [ H`) in normal
///   mode and the SS3 forms (`ESC O A`, `ESC O H`) once the program enables
///   DECCKM (`ESC [ ? 1 h`) — what vim/less do at startup.
/// - With a modifier held, xterm switches to the parameterised CSI form
///   (`ESC [ 1;5C` for Ctrl+Right), so DECCKM no longer applies. F1–F4 use
///   SS3 unmodified (`ESC O P`) and CSI with modifiers (`ESC [ 1;2P`).
/// - PageUp/PageDown, Insert/Delete and F5–F12 are `~`-form sequences and are
///   unaffected by DECCKM.
///
/// Returns `None` for every other key, and when ⌘ is held (⌘ shortcuts stay
/// with the application, as the kitty-protocol path already does).
pub fn encode_special_key(key: &str, mods: &Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    if mods.platform || mods.function {
        return None;
    }
    let m = modifier_parameter(mods);

    let seq = match key {
        "up" => cursor('A', m, app_cursor),
        "down" => cursor('B', m, app_cursor),
        "right" => cursor('C', m, app_cursor),
        "left" => cursor('D', m, app_cursor),
        "home" => cursor('H', m, app_cursor),
        "end" => cursor('F', m, app_cursor),
        "pageup" => tilde(5, m),
        "pagedown" => tilde(6, m),
        "insert" => tilde(2, m),
        "delete" => tilde(3, m),
        "f1" => function('P', m),
        "f2" => function('Q', m),
        "f3" => function('R', m),
        "f4" => function('S', m),
        "f5" => tilde(15, m),
        "f6" => tilde(17, m),
        "f7" => tilde(18, m),
        "f8" => tilde(19, m),
        "f9" => tilde(20, m),
        "f10" => tilde(21, m),
        "f11" => tilde(23, m),
        "f12" => tilde(24, m),
        _ => return None,
    };
    Some(seq.into_bytes())
}

/// One of the cursor-family keys: CSI `X` (or SS3 `X` in application mode)
/// unmodified, parameterised CSI with a modifier held.
fn cursor(final_byte: char, m: u8, app_cursor: bool) -> String {
    if m == 1 {
        if app_cursor {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[{final_byte}")
        }
    } else {
        format!("\x1b[1;{m}{final_byte}")
    }
}

/// One of the `~`-form keys (PageUp/PageDown, Insert/Delete, F5–F12).
fn tilde(number: u8, m: u8) -> String {
    if m == 1 {
        format!("\x1b[{number}~")
    } else {
        format!("\x1b[{number};{m}~")
    }
}

/// F1–F4: SS3 unmodified, parameterised CSI with a modifier held.
fn function(final_byte: char, m: u8) -> String {
    if m == 1 {
        format!("\x1bO{final_byte}")
    } else {
        format!("\x1b[1;{m}{final_byte}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: &str, m: Modifiers) -> Keystroke {
        Keystroke {
            key: key.to_owned(),
            key_char: None,
            modifiers: m,
        }
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            control: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn letters_map_to_their_control_byte() {
        assert_eq!(control_byte(&key("c", ctrl())), Some(0x03));
        assert_eq!(control_byte(&key("u", ctrl())), Some(0x15));
        assert_eq!(control_byte(&key("z", ctrl())), Some(0x1a));
    }

    /// #88: gpui reports `Ctrl+Shift+-` as `key = "_"` with `shift` cleared,
    /// so the key name is what has to map, and it must become US (0x1f) —
    /// nano's go-to-line.
    #[test]
    fn shifted_punctuation_maps_by_its_key_name() {
        assert_eq!(control_byte(&key("_", ctrl())), Some(0x1f));
        assert_eq!(control_byte(&key("[", ctrl())), Some(0x1b));
        assert_eq!(control_byte(&key("\\", ctrl())), Some(0x1c));
        assert_eq!(control_byte(&key("]", ctrl())), Some(0x1d));
        assert_eq!(control_byte(&key("^", ctrl())), Some(0x1e));
        assert_eq!(control_byte(&key("@", ctrl())), Some(0x00));
    }

    #[test]
    fn question_mark_is_del_not_us() {
        assert_eq!(control_byte(&key("?", ctrl())), Some(0x7f));
    }

    #[test]
    fn undefined_keys_fall_through() {
        // `-` without Shift has no control byte (sending CR here would act
        // like Enter in the shell).
        assert_eq!(control_byte(&key("-", ctrl())), None);
        assert_eq!(control_byte(&key("/", ctrl())), None);
        assert_eq!(control_byte(&key("1", ctrl())), None);
    }

    #[test]
    fn other_modifiers_never_map() {
        let mut m = ctrl();
        m.platform = true;
        assert_eq!(control_byte(&key("_", m)), None);

        let mut m = ctrl();
        m.alt = true;
        assert_eq!(control_byte(&key("_", m)), None);

        let mut m = ctrl();
        m.function = true;
        assert_eq!(control_byte(&key("_", m)), None);

        // Without Ctrl there is nothing to map.
        assert_eq!(control_byte(&key("_", Modifiers::default())), None);
    }

    #[test]
    fn multi_character_and_named_keys_do_not_map() {
        assert_eq!(control_byte(&key("home", ctrl())), None);
        assert_eq!(control_byte(&key("ab", ctrl())), None);
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Modifiers::default()
        }
    }

    // -----------------------------------------------------------------
    // encode_special_key
    // -----------------------------------------------------------------

    #[test]
    fn arrows_use_csi_outside_application_mode() {
        assert_eq!(
            encode_special_key("up", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[A".as_slice())
        );
        assert_eq!(
            encode_special_key("down", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[B".as_slice())
        );
        assert_eq!(
            encode_special_key("right", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[C".as_slice())
        );
        assert_eq!(
            encode_special_key("left", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[D".as_slice())
        );
    }

    /// In vim/less (DECCKM on) the unmodified cursor keys must use the SS3
    /// forms, and Home/End follow the same rule.
    #[test]
    fn application_mode_switches_to_ss3() {
        assert_eq!(
            encode_special_key("up", &Modifiers::default(), true).as_deref(),
            Some(b"\x1bOA".as_slice())
        );
        assert_eq!(
            encode_special_key("left", &Modifiers::default(), true).as_deref(),
            Some(b"\x1bOD".as_slice())
        );
        assert_eq!(
            encode_special_key("home", &Modifiers::default(), true).as_deref(),
            Some(b"\x1bOH".as_slice())
        );
        assert_eq!(
            encode_special_key("end", &Modifiers::default(), true).as_deref(),
            Some(b"\x1bOF".as_slice())
        );
    }

    #[test]
    fn modifiers_use_the_parameterised_csi_form_even_in_application_mode() {
        // 1 + shift = 2.
        assert_eq!(
            encode_special_key("up", &shift(), false).as_deref(),
            Some(b"\x1b[1;2A".as_slice())
        );
        // 1 + alt = 3.
        assert_eq!(
            encode_special_key("left", &alt(), true).as_deref(),
            Some(b"\x1b[1;3D".as_slice())
        );
        // 1 + ctrl = 5.
        assert_eq!(
            encode_special_key("right", &ctrl(), false).as_deref(),
            Some(b"\x1b[1;5C".as_slice())
        );
        // 1 + shift + ctrl = 6.
        let mut m = ctrl();
        m.shift = true;
        assert_eq!(
            encode_special_key("home", &m, true).as_deref(),
            Some(b"\x1b[1;6H".as_slice())
        );
    }

    #[test]
    fn home_and_end_use_csi_outside_application_mode() {
        assert_eq!(
            encode_special_key("home", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[H".as_slice())
        );
        assert_eq!(
            encode_special_key("end", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[F".as_slice())
        );
    }

    #[test]
    fn tilde_keys_ignore_application_mode() {
        assert_eq!(
            encode_special_key("pageup", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[5~".as_slice())
        );
        assert_eq!(
            encode_special_key("pagedown", &Modifiers::default(), true).as_deref(),
            Some(b"\x1b[6~".as_slice())
        );
        assert_eq!(
            encode_special_key("insert", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[2~".as_slice())
        );
        assert_eq!(
            encode_special_key("delete", &shift(), false).as_deref(),
            Some(b"\x1b[3;2~".as_slice())
        );
    }

    #[test]
    fn function_keys_use_ss3_then_csi() {
        assert_eq!(
            encode_special_key("f1", &Modifiers::default(), false).as_deref(),
            Some(b"\x1bOP".as_slice())
        );
        assert_eq!(
            encode_special_key("f4", &Modifiers::default(), true).as_deref(),
            Some(b"\x1bOS".as_slice())
        );
        assert_eq!(
            encode_special_key("f1", &shift(), false).as_deref(),
            Some(b"\x1b[1;2P".as_slice())
        );
        assert_eq!(
            encode_special_key("f5", &Modifiers::default(), false).as_deref(),
            Some(b"\x1b[15~".as_slice())
        );
        assert_eq!(
            encode_special_key("f12", &ctrl(), false).as_deref(),
            Some(b"\x1b[24;5~".as_slice())
        );
    }

    #[test]
    fn cmd_shortcuts_and_unknown_keys_are_left_to_the_app() {
        let mut m = Modifiers::default();
        m.platform = true;
        assert_eq!(encode_special_key("up", &m, false), None);
        assert_eq!(
            encode_special_key("enter", &Modifiers::default(), false),
            None
        );
        assert_eq!(encode_special_key("a", &Modifiers::default(), false), None);
    }
}

/// The control byte xterm would send for a key with `Ctrl` held and no ⌘,
/// Option or Fn — the xterm/ECMA-48 control-character table:
///
/// | key | byte | key | byte |
/// | --- | --- | --- | --- |
/// | `a`–`z` | 0x01–0x1a | `@` `[` `\` `]` `^` `_` | 0x00–0x1f |
/// | `?` | 0x7f (DEL) | | |
///
/// Keys outside that table (`-`, `/`, digits, …) have no defined control
/// byte and yield `None`, so the key falls through unchanged.
///
/// This reads `key`, not `key_char`: gpui leaves `key_char` unset whenever
/// Ctrl is held, and reports the shifted character as the key name with
/// `shift` cleared — `Ctrl+Shift+-` arrives as `key = "_"` — so a binding
/// named `ctrl-shift--` could never match and the key was silently dropped
/// (#88). `Ctrl+_` and therefore `Ctrl+Shift+-` now send US (0x1f), which is
/// nano's go-to-line.
pub fn control_byte(keystroke: &Keystroke) -> Option<u8> {
    let m = &keystroke.modifiers;
    if !m.control || m.platform || m.alt || m.function {
        return None;
    }
    let mut chars = keystroke.key.chars();
    let (c, single) = match (chars.next(), chars.next()) {
        (Some(c), None) => (c, true),
        _ => return None,
    };
    if !single {
        return None;
    }
    let lower = c.to_ascii_lowercase() as u8;
    match lower {
        // Ctrl+A–Z → 0x01–0x1a.
        b'a'..=b'z' => Some(lower - b'a' + 0x01),
        // Ctrl+@ [ \ ] ^ _ → 0x00–0x1f.
        b'@'..=b'_' => Some(lower - b'@'),
        // Ctrl+? is DEL, not US: 0x1f already belongs to Ctrl+_.
        b'?' => Some(0x7f),
        _ => None,
    }
}
