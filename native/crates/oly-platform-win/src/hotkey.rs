//! Keyboard chords, without a global hook.
//!
//! The shipped build registered application-wide hotkeys through a Tauri plugin
//! and then retired them: the config store's migration removes the whole
//! `hotkeys` section, so a port that resurrected them would apply settings the
//! user can no longer see or change. What remains is the in-window chord
//! matcher the lyrics window uses for its own shortcuts — a pure function, so it
//! is tested without a message loop.

use std::fmt;

/// Modifier state of one key event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
    };

    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Self::NONE
        }
    }

    pub fn shift() -> Self {
        Self {
            shift: true,
            ..Self::NONE
        }
    }

    fn label(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.meta {
            parts.push("Meta");
        }
        parts.join("+")
    }
}

/// A non-modifier key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Function(u8),
    Left,
    Right,
    Up,
    Down,
    Space,
    Enter,
    Escape,
    Tab,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
    Delete,
}

impl Key {
    fn parse(token: &str) -> Option<Self> {
        let upper = token.to_ascii_uppercase();
        if let Some(number) = upper.strip_prefix('F') {
            if let Ok(index) = number.parse::<u8>() {
                if (1..=24).contains(&index) {
                    return Some(Self::Function(index));
                }
            }
        }
        match upper.as_str() {
            "LEFT" => Some(Self::Left),
            "RIGHT" => Some(Self::Right),
            "UP" => Some(Self::Up),
            "DOWN" => Some(Self::Down),
            "SPACE" => Some(Self::Space),
            "ENTER" | "RETURN" => Some(Self::Enter),
            "ESC" | "ESCAPE" => Some(Self::Escape),
            "TAB" => Some(Self::Tab),
            "HOME" => Some(Self::Home),
            "END" => Some(Self::End),
            "PAGEUP" | "PGUP" => Some(Self::PageUp),
            "PAGEDOWN" | "PGDN" => Some(Self::PageDown),
            "BACKSPACE" => Some(Self::Backspace),
            "DELETE" | "DEL" => Some(Self::Delete),
            _ => {
                let mut chars = token.chars();
                let ch = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                Some(Self::Char(ch.to_ascii_uppercase()))
            }
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Char(ch) => write!(f, "{ch}"),
            Key::Function(index) => write!(f, "F{index}"),
            Key::Left => f.write_str("Left"),
            Key::Right => f.write_str("Right"),
            Key::Up => f.write_str("Up"),
            Key::Down => f.write_str("Down"),
            Key::Space => f.write_str("Space"),
            Key::Enter => f.write_str("Enter"),
            Key::Escape => f.write_str("Escape"),
            Key::Tab => f.write_str("Tab"),
            Key::Home => f.write_str("Home"),
            Key::End => f.write_str("End"),
            Key::PageUp => f.write_str("PageUp"),
            Key::PageDown => f.write_str("PageDown"),
            Key::Backspace => f.write_str("Backspace"),
            Key::Delete => f.write_str("Delete"),
        }
    }
}

/// A modifier set plus one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    pub key: Key,
}

impl Hotkey {
    /// Parses `Ctrl+Shift+L`, `Alt+F4`, `F11`, `Escape`.
    ///
    /// Every token except the last must be a modifier; an unknown token makes
    /// the whole chord invalid rather than being silently ignored, because a
    /// half-parsed shortcut is one the user believes they set.
    pub fn parse(spec: &str) -> Option<Self> {
        let tokens: Vec<&str> = spec
            .split('+')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .collect();
        let (key_token, modifier_tokens) = tokens.split_last()?;
        let mut modifiers = Modifiers::NONE;
        for token in modifier_tokens {
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers.ctrl = true,
                "alt" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                "meta" | "win" | "super" | "cmd" => modifiers.meta = true,
                _ => return None,
            }
        }
        Some(Self {
            modifiers,
            key: Key::parse(key_token)?,
        })
    }

    /// Exact match: a chord set to `Ctrl+L` must not fire while `Ctrl+Shift+L`
    /// is held, or every shifted variant would trigger its unshifted action.
    pub fn matches(&self, modifiers: Modifiers, key: Key) -> bool {
        self.modifiers == modifiers && self.key == key
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = self.modifiers.label();
        if prefix.is_empty() {
            write!(f, "{}", self.key)
        } else {
            write!(f, "{prefix}+{}", self.key)
        }
    }
}

/// The lyrics-window shortcuts that survive the hotkey retirement, in the order
/// the help sheet lists them.
pub const WINDOW_SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+W", "Toggle the wide panel"),
    ("Ctrl+M", "Cycle compact shapes"),
    ("Ctrl+,", "Open settings"),
    ("Ctrl+O", "Open lyrics picker"),
    ("Space", "Play or pause the followed player"),
    ("Left", "Seek back five seconds"),
    ("Right", "Seek forward five seconds"),
    ("Escape", "Close the topmost layer"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_needs_every_modifier_it_was_parsed_with() {
        let chord = Hotkey::parse("Ctrl+Shift+L").expect("valid");
        assert_eq!(chord.key, Key::Char('L'));
        assert!(chord.matches(
            Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::NONE
            },
            Key::Char('L')
        ));
        assert!(!chord.matches(Modifiers::ctrl(), Key::Char('L')));
        assert!(!chord.matches(
            Modifiers {
                ctrl: true,
                shift: true,
                alt: true,
                ..Modifiers::NONE
            },
            Key::Char('L')
        ));
    }

    #[test]
    fn an_unparsable_token_invalidates_the_whole_chord() {
        assert!(Hotkey::parse("Ctrl+Nope+L").is_none());
        assert!(Hotkey::parse("Ctrl+").is_none());
        assert!(Hotkey::parse("").is_none());
        assert!(Hotkey::parse("Hyper+L").is_none());
    }

    #[test]
    fn function_keys_and_named_keys_round_trip() {
        for spec in ["F11", "Ctrl+F5", "PageDown", "Alt+Enter", "Escape"] {
            let chord = Hotkey::parse(spec).expect(spec);
            assert_eq!(chord.to_string().to_ascii_lowercase(), spec.to_ascii_lowercase());
        }
    }

    #[test]
    fn the_published_shortcut_table_parses_without_a_registration_api() {
        assert!(WINDOW_SHORTCUTS.len() >= 6);
        for (spec, label) in WINDOW_SHORTCUTS {
            assert!(Hotkey::parse(spec).is_some(), "{spec}");
            assert!(!label.is_empty());
        }
    }
}
