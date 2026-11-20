//! Key specs in requests: names for keys and modifiers, and the key
//! codes, flags and characters a real key event would carry.

use tern_types::Key;

use crate::keymap::{self, flag};

/// Modifier names to NSEventModifierFlags with the device bits a real
/// keyboard sets. Plain names mean the left key.
pub(super) fn modifier_flags(name: &str) -> Option<usize> {
    Some(match name {
        "shift" | "lshift" => flag::SHIFT | flag::LEFT_SHIFT,
        "rshift" => flag::SHIFT | flag::RIGHT_SHIFT,
        "ctrl" | "control" | "lctrl" => flag::CONTROL | flag::LEFT_CONTROL,
        "rctrl" => flag::CONTROL | flag::RIGHT_CONTROL,
        "alt" | "opt" | "option" | "lalt" | "lopt" => flag::OPTION | flag::LEFT_OPTION,
        "ralt" | "ropt" => flag::OPTION | flag::RIGHT_OPTION,
        "cmd" | "command" | "super" | "lcmd" => flag::COMMAND | flag::LEFT_COMMAND,
        "rcmd" => flag::COMMAND | flag::RIGHT_COMMAND,
        _ => return None,
    })
}

/// A modifier key by name: its key code and the flags it sets.
pub(super) fn modifier_key(name: &str) -> Option<(u16, usize)> {
    let key = match name {
        "shift" | "lshift" => Key::ShiftLeft,
        "rshift" => Key::ShiftRight,
        "ctrl" | "control" | "lctrl" => Key::ControlLeft,
        "rctrl" => Key::ControlRight,
        "alt" | "opt" | "option" | "lalt" | "lopt" => Key::AltLeft,
        "ralt" | "ropt" => Key::AltRight,
        "cmd" | "command" | "super" | "lcmd" => Key::SuperLeft,
        "rcmd" => Key::SuperRight,
        _ => return None,
    };
    Some((code_for(key)?, modifier_flags(name)?))
}

pub(super) fn parse_mods(spec: &str) -> Result<usize, String> {
    spec.split('+').try_fold(0, |acc, m| {
        modifier_flags(m)
            .map(|f| acc | f)
            .ok_or_else(|| format!("unknown modifier {m:?}"))
    })
}

/// A key press to synthesize.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KeySpec {
    pub code: u16,
    /// NSEventModifierFlags, including the bits the key itself carries
    /// (arrows set Function and NumericPad, for instance).
    pub flags: usize,
    /// The characters of a key that types no layout character; None means
    /// ask the keyboard layout.
    pub fixed: Option<char>,
    /// The key's characters on a US layout, unshifted and shifted, used if
    /// the layout cannot be asked.
    pub us: Option<(char, char)>,
}

pub(super) fn code_for(key: Key) -> Option<u16> {
    (0..0x80).find(|&code| keymap::key_for_code(code) == Some(key))
}

pub(super) fn parse_key(spec: &str) -> Result<KeySpec, String> {
    // "+" and "cmd++" name the + key.
    let (mods, key) = match spec.strip_suffix("++") {
        _ if spec == "+" => (None, "+"),
        Some(mods) => (Some(mods), "+"),
        None => match spec.rsplit_once('+') {
            Some((mods, key)) => (Some(mods), key),
            None => (None, spec),
        },
    };
    let mut flags = mods.map(parse_mods).transpose()?.unwrap_or(0);
    let named = |key: Key, fixed: char, extra: usize| -> Result<KeySpec, String> {
        let code = code_for(key).ok_or_else(|| format!("no key code for {key:?}"))?;
        Ok(KeySpec {
            code,
            flags: flags | extra,
            fixed: Some(fixed),
            us: None,
        })
    };
    let function = flag::FUNCTION;
    let arrow = flag::FUNCTION | flag::NUMERIC_PAD;
    let lower = key.to_ascii_lowercase();
    match lower.as_str() {
        "enter" | "return" => return named(Key::Enter, '\r', 0),
        "tab" => return named(Key::Tab, '\t', 0),
        "backspace" => return named(Key::Backspace, '\u{7f}', 0),
        "escape" | "esc" => return named(Key::Escape, '\u{1b}', 0),
        "delete" => return named(Key::Delete, '\u{F728}', function),
        "insert" | "help" => return named(Key::Insert, '\u{F746}', function),
        "home" => return named(Key::Home, '\u{F729}', function),
        "end" => return named(Key::End, '\u{F72B}', function),
        "pageup" => return named(Key::PageUp, '\u{F72C}', function),
        "pagedown" => return named(Key::PageDown, '\u{F72D}', function),
        "up" => return named(Key::Up, '\u{F700}', arrow),
        "down" => return named(Key::Down, '\u{F701}', arrow),
        "left" => return named(Key::Left, '\u{F702}', arrow),
        "right" => return named(Key::Right, '\u{F703}', arrow),
        "space" => {
            return Ok(KeySpec {
                code: 0x31,
                flags,
                fixed: None,
                us: Some((' ', ' ')),
            })
        }
        "kpenter" => {
            return named(
                Key::Keypad(tern_types::KeypadKey::Enter),
                '\u{3}',
                flag::NUMERIC_PAD,
            )
        }
        _ => {}
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
        && (1..=20).contains(&n) {
            let fixed = char::from_u32(0xF704 + u32::from(n) - 1).unwrap_or('\u{F704}');
            return named(Key::F(n), fixed, function);
        }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(format!("unknown key {key:?}"));
    };
    let (unshifted, shifted, code) = keymap::US_KEYS
        .iter()
        .copied()
        .find(|&(u, s, _)| u == c || s == c)
        .ok_or_else(|| format!("no US key types {c:?}"))?;
    if c == shifted && c != unshifted {
        // A shifted character names its key with Shift held.
        flags |= flag::SHIFT | flag::LEFT_SHIFT;
    }
    Ok(KeySpec {
        code,
        flags,
        fixed: None,
        us: Some((unshifted, shifted)),
    })
}

impl KeySpec {
    /// The characters a US layout types, for when the layout cannot be asked.
    pub(super) fn us_characters(&self) -> (String, String) {
        let Some((unshifted, shifted)) = self.us else {
            let s = self.fixed.map(String::from).unwrap_or_default();
            return (s.clone(), s);
        };
        let shift = self.flags & flag::SHIFT != 0;
        let base = if shift { shifted } else { unshifted };
        let typed = if self.flags & flag::CONTROL != 0 && base.is_ascii_alphabetic() {
            char::from(base.to_ascii_uppercase() as u8 - b'@')
        } else {
            base
        };
        (typed.to_string(), base.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_specs() {
        let k = |s: &str| parse_key(s).unwrap_or_else(|e| panic!("{s}: {e}"));
        assert_eq!(
            k("a"),
            KeySpec {
                code: 0x00,
                flags: 0,
                fixed: None,
                us: Some(('a', 'A'))
            }
        );
        assert_eq!(k("shift+a").flags, flag::SHIFT | flag::LEFT_SHIFT);
        assert_eq!(k("A").flags, flag::SHIFT | flag::LEFT_SHIFT);
        assert_eq!(k("!").code, 0x12);
        assert_eq!(k("ctrl+c").flags, flag::CONTROL | flag::LEFT_CONTROL);
        assert_eq!(k("cmd+k").code, 0x28);
        let left = k("alt+left");
        assert_eq!((left.code, left.fixed), (0x7B, Some('\u{F702}')));
        assert_eq!(
            left.flags,
            flag::OPTION | flag::LEFT_OPTION | flag::FUNCTION | flag::NUMERIC_PAD
        );
        assert_eq!(
            k("ralt+left").flags & flag::RIGHT_OPTION,
            flag::RIGHT_OPTION
        );
        assert_eq!(k("escape").code, 0x35);
        assert_eq!(k("enter").fixed, Some('\r'));
        assert_eq!(k("tab").code, 0x30);
        assert_eq!(k("up").code, 0x7E);
        assert_eq!(
            k("f5"),
            KeySpec {
                code: 0x60,
                flags: flag::FUNCTION,
                fixed: Some('\u{F708}'),
                us: None
            }
        );
        assert_eq!(k("F12").code, 0x6F);
        assert_eq!(k("cmd++").code, 0x18);
        assert_eq!(k("cmd++").flags & flag::SHIFT, flag::SHIFT);
        assert_eq!(k("ctrl+tab").flags, flag::CONTROL | flag::LEFT_CONTROL);
        assert_eq!(k("space").code, 0x31);
        for bad in ["", "f0", "f21", "hyper+a", "é", "nope", "ctrl+"] {
            assert!(parse_key(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn plus_alone_names_the_plus_key() {
        let plus = parse_key("+").expect("parses");
        assert_eq!(plus.code, 0x18);
        assert_eq!(plus.flags, flag::SHIFT | flag::LEFT_SHIFT);
        assert_eq!(parse_key("cmd++").map(|k| k.code), Ok(0x18));
        assert!(parse_key("++").is_err());
    }

    #[test]
    fn us_characters_fallback() {
        let k = |s: &str| parse_key(s).map(|k| k.us_characters()).unwrap_or_default();
        assert_eq!(k("a"), ("a".into(), "a".into()));
        assert_eq!(k("shift+a"), ("A".into(), "A".into()));
        assert_eq!(k("ctrl+c"), ("\u{3}".into(), "c".into()));
        assert_eq!(k("shift+1"), ("!".into(), "!".into()));
        assert_eq!(k("enter"), ("\r".into(), "\r".into()));
    }
}
