//! NSEvent key data to tern_types::Key and Mods.
//!
//! The functions here take the raw numbers an NSEvent carries, so they can
//! be tested without AppKit.

use tern_types::{Key, KeypadKey, Mods};

/// NSEventModifierFlags bits.
pub mod flag {
    pub const CAPS_LOCK: usize = 1 << 16;
    pub const SHIFT: usize = 1 << 17;
    pub const CONTROL: usize = 1 << 18;
    pub const OPTION: usize = 1 << 19;
    pub const COMMAND: usize = 1 << 20;
    pub const NUMERIC_PAD: usize = 1 << 21;
    pub const FUNCTION: usize = 1 << 23;

    // Device-dependent bits in the low word (NX_DEVICE*KEYMASK in IOLLEvent.h).
    // AppKit does not name them, but they are the only way to tell the left
    // and right keys apart.
    pub const LEFT_CONTROL: usize = 0x0001;
    pub const LEFT_SHIFT: usize = 0x0002;
    pub const RIGHT_SHIFT: usize = 0x0004;
    pub const LEFT_COMMAND: usize = 0x0008;
    pub const RIGHT_COMMAND: usize = 0x0010;
    pub const LEFT_OPTION: usize = 0x0020;
    pub const RIGHT_OPTION: usize = 0x0040;
    pub const RIGHT_CONTROL: usize = 0x2000;
}

/// Which Option keys are held. Kept apart from [Mods] because a terminal may
/// treat only one of them as Alt and let the other compose characters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct OptionKeys {
    pub left: bool,
    pub right: bool,
}

/// A key press translated from an NSEvent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyPress {
    pub key: Key,
    pub mods: Mods,
    pub option: OptionKeys,
}

pub fn mods_from_flags(flags: usize) -> Mods {
    let mut m = Mods::NONE;
    for (bit, mods) in [
        (flag::SHIFT, Mods::SHIFT),
        (flag::OPTION, Mods::ALT),
        (flag::CONTROL, Mods::CTRL),
        (flag::COMMAND, Mods::SUPER),
        (flag::CAPS_LOCK, Mods::CAPS_LOCK),
    ] {
        if flags & bit != 0 {
            m = m | mods;
        }
    }
    m
}

pub fn option_keys(flags: usize) -> OptionKeys {
    let left = flags & flag::LEFT_OPTION != 0;
    let right = flags & flag::RIGHT_OPTION != 0;
    // Events without device bits (from other software) still say Option.
    if flags & flag::OPTION != 0 && !left && !right {
        return OptionKeys {
            left: true,
            right: false,
        };
    }
    OptionKeys { left, right }
}

/// The keys whose meaning does not depend on the keyboard layout, by
/// virtual key code (kVK_* in HIToolbox/Events.h).
pub fn key_for_code(code: u16) -> Option<Key> {
    use KeypadKey as K;
    Some(match code {
        0x24 => Key::Enter,
        0x30 => Key::Tab,
        0x33 => Key::Backspace,
        0x35 => Key::Escape,
        0x75 => Key::Delete,
        0x72 => Key::Insert, // Help, where PC keyboards have Insert
        0x73 => Key::Home,
        0x77 => Key::End,
        0x74 => Key::PageUp,
        0x79 => Key::PageDown,
        0x7B => Key::Left,
        0x7C => Key::Right,
        0x7D => Key::Down,
        0x7E => Key::Up,
        0x7A => Key::F(1),
        0x78 => Key::F(2),
        0x63 => Key::F(3),
        0x76 => Key::F(4),
        0x60 => Key::F(5),
        0x61 => Key::F(6),
        0x62 => Key::F(7),
        0x64 => Key::F(8),
        0x65 => Key::F(9),
        0x6D => Key::F(10),
        0x67 => Key::F(11),
        0x6F => Key::F(12),
        0x69 => Key::F(13),
        0x6B => Key::F(14),
        0x71 => Key::F(15),
        0x6A => Key::F(16),
        0x40 => Key::F(17),
        0x4F => Key::F(18),
        0x50 => Key::F(19),
        0x5A => Key::F(20),
        0x52 => Key::Keypad(K::Digit(0)),
        0x53 => Key::Keypad(K::Digit(1)),
        0x54 => Key::Keypad(K::Digit(2)),
        0x55 => Key::Keypad(K::Digit(3)),
        0x56 => Key::Keypad(K::Digit(4)),
        0x57 => Key::Keypad(K::Digit(5)),
        0x58 => Key::Keypad(K::Digit(6)),
        0x59 => Key::Keypad(K::Digit(7)),
        0x5B => Key::Keypad(K::Digit(8)),
        0x5C => Key::Keypad(K::Digit(9)),
        0x41 => Key::Keypad(K::Decimal),
        0x4B => Key::Keypad(K::Divide),
        0x43 => Key::Keypad(K::Multiply),
        0x4E => Key::Keypad(K::Subtract),
        0x45 => Key::Keypad(K::Add),
        0x4C => Key::Keypad(K::Enter),
        0x51 => Key::Keypad(K::Equal),
        0x39 => Key::CapsLock,
        0x6E => Key::Menu,
        0x38 => Key::ShiftLeft,
        0x3C => Key::ShiftRight,
        0x3B => Key::ControlLeft,
        0x3E => Key::ControlRight,
        0x3A => Key::AltLeft,
        0x3D => Key::AltRight,
        0x37 => Key::SuperLeft,
        0x36 => Key::SuperRight,
        _ => return None,
    })
}

/// US ANSI key positions: (unshifted, shifted, virtual key code).
pub const US_KEYS: [(char, char, u16); 48] = [
    ('a', 'A', 0x00),
    ('s', 'S', 0x01),
    ('d', 'D', 0x02),
    ('f', 'F', 0x03),
    ('h', 'H', 0x04),
    ('g', 'G', 0x05),
    ('z', 'Z', 0x06),
    ('x', 'X', 0x07),
    ('c', 'C', 0x08),
    ('v', 'V', 0x09),
    ('b', 'B', 0x0B),
    ('q', 'Q', 0x0C),
    ('w', 'W', 0x0D),
    ('e', 'E', 0x0E),
    ('r', 'R', 0x0F),
    ('y', 'Y', 0x10),
    ('t', 'T', 0x11),
    ('1', '!', 0x12),
    ('2', '@', 0x13),
    ('3', '#', 0x14),
    ('4', '$', 0x15),
    ('6', '^', 0x16),
    ('5', '%', 0x17),
    ('=', '+', 0x18),
    ('9', '(', 0x19),
    ('7', '&', 0x1A),
    ('-', '_', 0x1B),
    ('8', '*', 0x1C),
    ('0', ')', 0x1D),
    (']', '}', 0x1E),
    ('o', 'O', 0x1F),
    ('u', 'U', 0x20),
    ('[', '{', 0x21),
    ('i', 'I', 0x22),
    ('p', 'P', 0x23),
    ('l', 'L', 0x25),
    ('j', 'J', 0x26),
    ('\'', '"', 0x27),
    ('k', 'K', 0x28),
    (';', ':', 0x29),
    ('\\', '|', 0x2A),
    (',', '<', 0x2B),
    ('/', '?', 0x2C),
    ('n', 'N', 0x2D),
    ('m', 'M', 0x2E),
    ('.', '>', 0x2F),
    ('\x60', '~', 0x32),
    (' ', ' ', 0x31),
];

/// What the key at this virtual key code types on a US layout, unshifted
/// and shifted; None for keys that type no character there.
pub fn us_characters(code: u16) -> Option<(char, char)> {
    US_KEYS
        .iter()
        .find(|&&(_, _, c)| c == code)
        .map(|&(unshifted, shifted, _)| (unshifted, shifted))
}

/// Translates a key event. base is what the key types with no modifiers
/// (NSEvent charactersByApplyingModifiers: with none, or
/// charactersIgnoringModifiers when that is unavailable). None for keys
/// with no meaning to a terminal, such as the Fn key or media keys.
pub fn translate(code: u16, flags: usize, base: &str) -> Option<KeyPress> {
    let key = match key_for_code(code) {
        Some(k) => k,
        None => {
            let mut chars = base.chars();
            let c = chars.next()?;
            // AppKit puts unnamed function keys in the private use area, and
            // dead keys produce nothing or a control character.
            if ('\u{F700}'..='\u{F8FF}').contains(&c) || c.is_control() {
                return None;
            }
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(l), None) => Key::Char(l),
                _ => Key::Char(c),
            }
        }
    };
    Some(KeyPress {
        key,
        mods: mods_from_flags(flags),
        option: option_keys(flags),
    })
}

/// For a flagsChanged: event: which modifier key changed and whether it is
/// now down. None for codes that are not modifier keys.
pub fn modifier_change(code: u16, flags: usize) -> Option<(Key, bool)> {
    let key = key_for_code(code)?;
    let (device, other_side, generic) = match key {
        Key::ShiftLeft => (flag::LEFT_SHIFT, flag::RIGHT_SHIFT, flag::SHIFT),
        Key::ShiftRight => (flag::RIGHT_SHIFT, flag::LEFT_SHIFT, flag::SHIFT),
        Key::ControlLeft => (flag::LEFT_CONTROL, flag::RIGHT_CONTROL, flag::CONTROL),
        Key::ControlRight => (flag::RIGHT_CONTROL, flag::LEFT_CONTROL, flag::CONTROL),
        Key::AltLeft => (flag::LEFT_OPTION, flag::RIGHT_OPTION, flag::OPTION),
        Key::AltRight => (flag::RIGHT_OPTION, flag::LEFT_OPTION, flag::OPTION),
        Key::SuperLeft => (flag::LEFT_COMMAND, flag::RIGHT_COMMAND, flag::COMMAND),
        Key::SuperRight => (flag::RIGHT_COMMAND, flag::LEFT_COMMAND, flag::COMMAND),
        // Caps Lock reports its lock state, not whether the key is held.
        Key::CapsLock => return Some((key, flags & flag::CAPS_LOCK != 0)),
        _ => return None,
    };
    let down = if flags & (device | other_side) != 0 {
        flags & device != 0
    } else {
        // Without device bits the generic bit is all there is.
        flags & generic != 0
    };
    Some((key, down))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(code: u16, flags: usize, base: &str) -> Option<(Key, Mods)> {
        translate(code, flags, base).map(|k| (k.key, k.mods))
    }

    #[test]
    fn letters_take_the_lowercase_base_character() {
        assert_eq!(t(0x00, 0, "a"), Some((Key::Char('a'), Mods::NONE)));
        assert_eq!(
            t(0x00, flag::SHIFT | flag::LEFT_SHIFT, "A"),
            Some((Key::Char('a'), Mods::SHIFT))
        );
        assert_eq!(
            t(0x08, flag::CONTROL | flag::LEFT_CONTROL, "c"),
            Some((Key::Char('c'), Mods::CTRL))
        );
        assert_eq!(
            t(0x28, flag::COMMAND | flag::LEFT_COMMAND, "k"),
            Some((Key::Char('k'), Mods::SUPER))
        );
        assert_eq!(
            t(0x12, flag::SHIFT, "1"),
            Some((Key::Char('1'), Mods::SHIFT))
        );
        assert_eq!(t(0x31, 0, " "), Some((Key::Char(' '), Mods::NONE)));
        // Other layouts: the base character is whatever the layout types.
        assert_eq!(t(0x0C, 0, "é"), Some((Key::Char('é'), Mods::NONE)));
        assert_eq!(t(0x0C, 0, "Ä"), Some((Key::Char('ä'), Mods::NONE)));
    }

    #[test]
    fn named_keys_come_from_the_key_code() {
        let none = 0;
        assert_eq!(t(0x24, none, "\r"), Some((Key::Enter, Mods::NONE)));
        assert_eq!(t(0x30, none, "\t"), Some((Key::Tab, Mods::NONE)));
        assert_eq!(t(0x30, flag::CONTROL, "\t"), Some((Key::Tab, Mods::CTRL)));
        assert_eq!(t(0x35, none, "\u{1b}"), Some((Key::Escape, Mods::NONE)));
        assert_eq!(t(0x33, none, "\u{7f}"), Some((Key::Backspace, Mods::NONE)));
        assert_eq!(
            t(0x75, flag::FUNCTION, "\u{F728}"),
            Some((Key::Delete, Mods::NONE))
        );
        let arrow = flag::FUNCTION | flag::NUMERIC_PAD;
        assert_eq!(t(0x7E, arrow, "\u{F700}"), Some((Key::Up, Mods::NONE)));
        assert_eq!(t(0x7D, arrow, "\u{F701}"), Some((Key::Down, Mods::NONE)));
        assert_eq!(
            t(0x7B, arrow | flag::OPTION, "\u{F702}"),
            Some((Key::Left, Mods::ALT))
        );
        assert_eq!(t(0x7C, arrow, "\u{F703}"), Some((Key::Right, Mods::NONE)));
        assert_eq!(
            t(0x60, flag::FUNCTION, "\u{F708}"),
            Some((Key::F(5), Mods::NONE))
        );
        assert_eq!(
            t(0x7A, flag::FUNCTION, "\u{F704}"),
            Some((Key::F(1), Mods::NONE))
        );
        assert_eq!(
            t(0x6F, flag::FUNCTION, "\u{F70F}"),
            Some((Key::F(12), Mods::NONE))
        );
        assert_eq!(
            t(0x73, flag::FUNCTION, "\u{F729}"),
            Some((Key::Home, Mods::NONE))
        );
        assert_eq!(
            t(0x79, flag::FUNCTION, "\u{F72D}"),
            Some((Key::PageDown, Mods::NONE))
        );
        assert_eq!(
            t(0x4C, flag::NUMERIC_PAD, "\u{3}"),
            Some((Key::Keypad(KeypadKey::Enter), Mods::NONE))
        );
        assert_eq!(
            t(0x5C, flag::NUMERIC_PAD, "9"),
            Some((Key::Keypad(KeypadKey::Digit(9)), Mods::NONE))
        );
    }

    #[test]
    fn unknown_keys_are_dropped() {
        // Fn alone, an unnamed function key, a dead key, nothing at all.
        assert_eq!(t(0x3F, flag::FUNCTION, ""), None);
        assert_eq!(t(0x0A, 0, "\u{F739}"), None);
        assert_eq!(t(0x0E, flag::OPTION, ""), None);
        assert_eq!(t(0x0E, 0, "\u{0}"), None);
    }

    #[test]
    fn modifiers() {
        let all = flag::SHIFT | flag::CONTROL | flag::OPTION | flag::COMMAND | flag::CAPS_LOCK;
        let m = mods_from_flags(all | flag::FUNCTION | flag::NUMERIC_PAD);
        assert_eq!(
            m,
            Mods::SHIFT | Mods::CTRL | Mods::ALT | Mods::SUPER | Mods::CAPS_LOCK
        );
        assert_eq!(m.held(), Mods::SHIFT | Mods::CTRL | Mods::ALT | Mods::SUPER);
        assert_eq!(mods_from_flags(0), Mods::NONE);
    }

    #[test]
    fn left_and_right_option() {
        let left = flag::OPTION | flag::LEFT_OPTION;
        let right = flag::OPTION | flag::RIGHT_OPTION;
        assert_eq!(
            option_keys(left),
            OptionKeys {
                left: true,
                right: false
            }
        );
        assert_eq!(
            option_keys(right),
            OptionKeys {
                left: false,
                right: true
            }
        );
        assert_eq!(
            option_keys(left | right),
            OptionKeys {
                left: true,
                right: true
            }
        );
        assert_eq!(
            option_keys(flag::OPTION),
            OptionKeys {
                left: true,
                right: false
            }
        );
        assert_eq!(option_keys(0), OptionKeys::default());
        let k = translate(0x7B, right | flag::FUNCTION, "\u{F702}").unwrap();
        assert_eq!(
            (k.key, k.mods, k.option.right),
            (Key::Left, Mods::ALT, true)
        );
    }

    #[test]
    fn modifier_key_changes() {
        assert_eq!(
            modifier_change(0x3A, flag::OPTION | flag::LEFT_OPTION),
            Some((Key::AltLeft, true))
        );
        assert_eq!(modifier_change(0x3A, 0), Some((Key::AltLeft, false)));
        // Releasing right Option while left Option is still held.
        assert_eq!(
            modifier_change(0x3D, flag::OPTION | flag::LEFT_OPTION),
            Some((Key::AltRight, false))
        );
        assert_eq!(
            modifier_change(0x38, flag::SHIFT),
            Some((Key::ShiftLeft, true))
        );
        assert_eq!(
            modifier_change(0x3E, flag::CONTROL | flag::RIGHT_CONTROL),
            Some((Key::ControlRight, true))
        );
        assert_eq!(
            modifier_change(0x37, flag::COMMAND | flag::LEFT_COMMAND),
            Some((Key::SuperLeft, true))
        );
        assert_eq!(
            modifier_change(0x39, flag::CAPS_LOCK),
            Some((Key::CapsLock, true))
        );
        assert_eq!(modifier_change(0x00, 0), None);
    }
}
