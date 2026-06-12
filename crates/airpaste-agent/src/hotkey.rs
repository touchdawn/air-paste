#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
pub use windows::spawn_hotkey_listener;

#[cfg(target_os = "macos")]
pub use macos::spawn_hotkey_listener;

/// A global hotkey the agent listens for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    /// Default Alt+V (Option+V on macOS) — paste remote content (files, or in isolated mode the
    /// inbox text).
    PasteRemote,
    /// Default Alt+C (Option+C on macOS) — capture the current selection into the AirPaste
    /// channel (isolated).
    CopyToAirPaste,
}

pub const DEFAULT_COPY_HOTKEY: &str = "alt+c";
pub const DEFAULT_PASTE_HOTKEY: &str = "alt+v";

/// A parsed global-hotkey chord: at least one modifier plus a letter, digit, or F-key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeySpec {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    /// Cmd on macOS, Win on Windows.
    pub meta: bool,
    pub key: HotkeyKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyKey {
    /// An ASCII letter or digit, stored uppercase.
    Char(u8),
    /// A function key, `F(1)`..=`F(12)`.
    F(u8),
}

/// The two chords the agent listens for, parsed and conflict-checked.
#[derive(Clone, Copy, Debug)]
pub struct HotkeyChords {
    pub copy: HotkeySpec,
    pub paste: HotkeySpec,
}

impl HotkeySpec {
    /// Parse a chord like `alt+c`, `ctrl+shift+f9` or `cmd+option+v` (case-insensitive).
    /// Accepted modifier aliases: alt/option/opt, ctrl/control, shift, cmd/command/meta/super/win.
    pub fn parse(spec: &str) -> anyhow::Result<Self> {
        let mut parsed = Self {
            alt: false,
            ctrl: false,
            shift: false,
            meta: false,
            key: HotkeyKey::Char(0),
        };
        let mut key = None;
        for part in spec.split('+') {
            let part = part.trim().to_ascii_lowercase();
            match part.as_str() {
                "alt" | "option" | "opt" => parsed.alt = true,
                "ctrl" | "control" => parsed.ctrl = true,
                "shift" => parsed.shift = true,
                "cmd" | "command" | "meta" | "super" | "win" => parsed.meta = true,
                "" => anyhow::bail!("hotkey \"{spec}\" has an empty part"),
                _ => {
                    if key.is_some() {
                        anyhow::bail!("hotkey \"{spec}\" has more than one non-modifier key");
                    }
                    key = Some(parse_key(spec, &part)?);
                }
            }
        }
        let Some(key) = key else {
            anyhow::bail!("hotkey \"{spec}\" is missing a key (e.g. alt+c)");
        };
        if !(parsed.alt || parsed.ctrl || parsed.shift || parsed.meta) {
            anyhow::bail!("hotkey \"{spec}\" needs at least one modifier (alt/ctrl/shift/cmd)");
        }
        parsed.key = key;
        Ok(parsed)
    }

    /// Human-readable chord with platform modifier names, e.g. `Option+C` / `Alt+C`.
    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.alt {
            parts.push(if cfg!(target_os = "macos") {
                "Option"
            } else {
                "Alt"
            });
        }
        if self.meta {
            parts.push(if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Win"
            });
        }
        let key = match self.key {
            HotkeyKey::Char(c) => (c as char).to_string(),
            HotkeyKey::F(n) => format!("F{n}"),
        };
        let mut label = parts.join("+");
        if !label.is_empty() {
            label.push('+');
        }
        label.push_str(&key);
        label
    }
}

fn parse_key(spec: &str, part: &str) -> anyhow::Result<HotkeyKey> {
    let mut chars = part.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => {
            Ok(HotkeyKey::Char(c.to_ascii_uppercase() as u8))
        }
        (Some('f'), Some(_)) => {
            let n: u8 = part[1..]
                .parse()
                .ok()
                .filter(|n| (1..=12).contains(n))
                .ok_or_else(|| {
                    anyhow::anyhow!("hotkey \"{spec}\": function keys go from f1 to f12")
                })?;
            Ok(HotkeyKey::F(n))
        }
        _ => anyhow::bail!(
            "hotkey \"{spec}\": unsupported key \"{part}\" (use a letter, digit, or f1-f12)"
        ),
    }
}

#[cfg(windows)]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = true;

#[cfg(target_os = "macos")]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = false;

#[cfg(not(any(windows, target_os = "macos")))]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = false;

#[cfg(not(any(windows, target_os = "macos")))]
pub fn spawn_hotkey_listener(
    _sender: tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
    _enable_copy: bool,
    _chords: HotkeyChords,
) -> anyhow::Result<()> {
    anyhow::bail!("global hotkeys MVP is currently implemented only on Windows and macOS")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_chords() {
        let copy = HotkeySpec::parse(DEFAULT_COPY_HOTKEY).expect("default copy parses");
        assert!(copy.alt && !copy.ctrl && !copy.shift && !copy.meta);
        assert_eq!(copy.key, HotkeyKey::Char(b'C'));
        let paste = HotkeySpec::parse(DEFAULT_PASTE_HOTKEY).expect("default paste parses");
        assert_eq!(paste.key, HotkeyKey::Char(b'V'));
    }

    #[test]
    fn parses_aliases_whitespace_and_case() {
        let spec = HotkeySpec::parse(" Ctrl + Shift + F9 ").expect("parses");
        assert!(spec.ctrl && spec.shift && !spec.alt && !spec.meta);
        assert_eq!(spec.key, HotkeyKey::F(9));

        let spec = HotkeySpec::parse("Option+1").expect("parses");
        assert!(spec.alt);
        assert_eq!(spec.key, HotkeyKey::Char(b'1'));

        let spec = HotkeySpec::parse("cmd+option+v").expect("parses");
        assert!(spec.meta && spec.alt);
        let spec2 = HotkeySpec::parse("WIN+ALT+V").expect("parses");
        assert_eq!(spec, spec2);
    }

    #[test]
    fn rejects_invalid_chords() {
        // No modifier: a bare global key would shadow normal typing.
        assert!(HotkeySpec::parse("c").is_err());
        assert!(HotkeySpec::parse("alt").is_err());
        assert!(HotkeySpec::parse("alt+c+v").is_err());
        assert!(HotkeySpec::parse("alt+f13").is_err());
        assert!(HotkeySpec::parse("alt+enter").is_err());
        assert!(HotkeySpec::parse("alt++c").is_err());
        assert!(HotkeySpec::parse("").is_err());
    }

    #[test]
    fn labels_are_human_readable() {
        let label = HotkeySpec::parse("ctrl+shift+f9").unwrap().label();
        assert_eq!(label, "Ctrl+Shift+F9");
        let label = HotkeySpec::parse("alt+c").unwrap().label();
        if cfg!(target_os = "macos") {
            assert_eq!(label, "Option+C");
        } else {
            assert_eq!(label, "Alt+C");
        }
    }
}
