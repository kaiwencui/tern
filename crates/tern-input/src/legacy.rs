//! xterm's encoding, used while the program has not turned on the kitty
//! keyboard protocol.

use tern_types::{Key, KeypadKey, Modes, Mods};

use crate::key::{keypad_char, KeyKind, Resolved};
use crate::{push_char, push_decimal, CSI, ESC, SS3};

pub(crate) fn encode(key: &Resolved, modes: Modes) -> Vec<u8> {
    // xterm reports no releases and has no encoding for the lock keys or for
    // Command, which the app turns into actions before calling in.
    let mods = key.mods.held();
    if key.kind == KeyKind::Release || mods.cmd() {
        return Vec::new();
    }
    let mut out = Vec::new();
    match key.key {
        Key::Char(c) => return char_bytes(key, c),
        Key::Enter => {
            alt_prefix(&mut out, mods);
            enter(&mut out, modes);
        }
        Key::Tab => {
            alt_prefix(&mut out, mods);
            if mods.shift() {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                out.push(b'\t');
            }
        }
        Key::Backspace => {
            alt_prefix(&mut out, mods);
            out.push(if mods.ctrl() { 0x08 } else { 0x7f });
        }
        Key::Escape => {
            alt_prefix(&mut out, mods);
            out.push(ESC);
        }
        Key::Up => cursor(&mut out, b'A', mods, modes),
        Key::Down => cursor(&mut out, b'B', mods, modes),
        Key::Right => cursor(&mut out, b'C', mods, modes),
        Key::Left => cursor(&mut out, b'D', mods, modes),
        Key::Home => cursor(&mut out, b'H', mods, modes),
        Key::End => cursor(&mut out, b'F', mods, modes),
        Key::Insert => tilde(&mut out, 2, mods),
        Key::Delete => tilde(&mut out, 3, mods),
        Key::PageUp => tilde(&mut out, 5, mods),
        Key::PageDown => tilde(&mut out, 6, mods),
        // xterm sends Menu as the VT220's Do key, which is also F16.
        Key::Menu => tilde(&mut out, 29, mods),
        Key::F(n @ 1..=4) => {
            let last = b'P' + (n - 1);
            if mods.is_empty() {
                out.extend_from_slice(SS3);
            } else {
                // SS3 has no room for parameters, so a modified F1 to F4
                // becomes CSI 1;m P like the cursor keys.
                out.extend_from_slice(b"\x1b[1;");
                push_decimal(&mut out, modifier_param(mods));
            }
            out.push(last);
        }
        Key::F(n) => {
            if let Some(number) = function_key_number(n) {
                tilde(&mut out, number, mods);
            }
        }
        Key::Keypad(k) => keypad(&mut out, key, k, mods, modes),
        Key::CapsLock
        | Key::ShiftLeft
        | Key::ShiftRight
        | Key::ControlLeft
        | Key::ControlRight
        | Key::AltLeft
        | Key::AltRight
        | Key::SuperLeft
        | Key::SuperRight => {}
    }
    out
}

/// The bytes for a key that types the character c (its base character):
/// the OS text when no Ctrl or Alt is held, otherwise ESC for Alt and the C0
/// control for Ctrl. Empty when the key types nothing or Command is held.
pub(crate) fn char_bytes(key: &Resolved, c: char) -> Vec<u8> {
    let mods = key.mods.held();
    if mods.cmd() {
        return Vec::new();
    }
    if !mods.ctrl() && !mods.alt() {
        return key.text.map(|t| t.as_bytes().to_vec()).unwrap_or_default();
    }
    let c = key.modified_char(c);
    let mut out = Vec::with_capacity(5);
    alt_prefix(&mut out, mods);
    match control_byte(c) {
        Some(byte) if mods.ctrl() => out.push(byte),
        _ => push_char(&mut out, c),
    }
    out
}

/// What Ctrl turns a character into: X11's mapping, which xterm sends,
/// plus Ctrl+? as DEL like the VT100. Other characters are left alone.
fn control_byte(c: char) -> Option<u8> {
    match c {
        ' ' | '2' => Some(0),
        '@'..='~' => Some(c as u8 & 0x1f),
        '3'..='7' => Some(c as u8 - b'3' + 0x1b),
        '8' | '?' => Some(0x7f),
        '/' => Some(0x1f),
        _ => None,
    }
}

fn alt_prefix(out: &mut Vec<u8>, mods: Mods) {
    if mods.alt() {
        out.push(ESC);
    }
}

fn enter(out: &mut Vec<u8>, modes: Modes) {
    if modes.line_feed_new_line {
        out.extend_from_slice(b"\r\n");
    } else {
        out.push(b'\r');
    }
}

/// xterm's modifier parameter: 1 plus Shift 1, Alt 2, Ctrl 4. Command never
/// gets this far.
fn modifier_param(mods: Mods) -> u32 {
    1 + u32::from(mods.0)
}

/// Arrows, Home and End: SS3 in application cursor mode, CSI otherwise, and
/// CSI 1;m with modifiers in either mode.
fn cursor(out: &mut Vec<u8>, last: u8, mods: Mods, modes: Modes) {
    if mods.is_empty() {
        out.extend_from_slice(if modes.app_cursor { SS3 } else { CSI });
    } else {
        out.extend_from_slice(b"\x1b[1;");
        push_decimal(out, modifier_param(mods));
    }
    out.push(last);
}

/// CSI n ~, or CSI n;m ~ with modifiers.
fn tilde(out: &mut Vec<u8>, number: u32, mods: Mods) {
    out.extend_from_slice(CSI);
    push_decimal(out, number);
    if !mods.is_empty() {
        out.push(b';');
        push_decimal(out, modifier_param(mods));
    }
    out.push(b'~');
}

/// xterm's CSI number for F5 and up (its DEC function key table).
fn function_key_number(n: u8) -> Option<u32> {
    Some(match n {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        13 => 25,
        14 => 26,
        15 => 28,
        16 => 29,
        17 => 31,
        18 => 32,
        19 => 33,
        20 => 34,
        // No real terminal had keys past F20, so xterm numbers them on from 42.
        21..=35 => 42 + u32::from(n - 21),
        _ => return None,
    })
}

/// Keypad keys: SS3 codes in application keypad mode, with xterm's modifier
/// number between SS3 and the final byte; otherwise the character the key
/// types, with ESC for Alt (Ctrl and Shift change nothing, as in xterm).
fn keypad(out: &mut Vec<u8>, key: &Resolved, k: KeypadKey, mods: Mods, modes: Modes) {
    if modes.app_keypad {
        let last = match k {
            KeypadKey::Digit(d @ 0..=9) => b'p' + d,
            KeypadKey::Digit(_) => return,
            KeypadKey::Decimal => b'n',
            KeypadKey::Divide => b'o',
            KeypadKey::Multiply => b'j',
            KeypadKey::Subtract => b'm',
            KeypadKey::Add => b'k',
            KeypadKey::Enter => b'M',
            KeypadKey::Equal => b'X',
        };
        out.extend_from_slice(SS3);
        if !mods.is_empty() {
            push_decimal(out, modifier_param(mods));
        }
        out.push(last);
    } else if k == KeypadKey::Enter {
        alt_prefix(out, mods);
        enter(out, modes);
    } else if let Some(c) = keypad_char(k) {
        alt_prefix(out, mods);
        // The layout decides what the key types: the decimal key types a
        // comma on many European layouts.
        match key.text {
            Some(text) => out.extend_from_slice(text.as_bytes()),
            None => push_char(out, c),
        }
    }
}

#[cfg(test)]
mod tests {
    use tern_types::{Key, KeypadKey, Modes, Mods};

    use crate::key::{encode_key, keypad_char, KeyEvent, KeyKind, KeyOptions, OptionAsAlt};
    use crate::show;

    const ALT: KeyOptions = KeyOptions {
        option_as_alt: OptionAsAlt::Both,
    };

    const NORMAL: Modes = Modes {
        app_cursor: false,
        app_keypad: false,
        bracketed_paste: false,
        focus_events: false,
        alt_screen: false,
        alternate_scroll: false,
        mouse: tern_types::MouseTracking::Off,
        mouse_encoding: tern_types::MouseEncoding::Default,
        kitty_flags: 0,
        line_feed_new_line: false,
    };
    const APP_CURSOR: Modes = Modes {
        app_cursor: true,
        ..NORMAL
    };
    const APP_KEYPAD: Modes = Modes {
        app_keypad: true,
        ..NORMAL
    };

    /// Every combination of Shift, Alt and Ctrl with xterm's parameter for it.
    const MODIFIED: [(Mods, u32); 7] = [
        (Mods::SHIFT, 2),
        (Mods::ALT, 3),
        (Mods(Mods::SHIFT.0 | Mods::ALT.0), 4),
        (Mods::CTRL, 5),
        (Mods(Mods::SHIFT.0 | Mods::CTRL.0), 6),
        (Mods(Mods::ALT.0 | Mods::CTRL.0), 7),
        (Mods(Mods::SHIFT.0 | Mods::ALT.0 | Mods::CTRL.0), 8),
    ];

    fn enc(event: &KeyEvent, modes: Modes) -> String {
        show(&encode_key(event, modes, ALT))
    }

    fn key(k: Key, mods: Mods) -> KeyEvent {
        KeyEvent::new(k).mods(mods)
    }

    #[test]
    fn cursor_keys() {
        let keys = [
            (Key::Up, 'A'),
            (Key::Down, 'B'),
            (Key::Right, 'C'),
            (Key::Left, 'D'),
            (Key::Home, 'H'),
            (Key::End, 'F'),
        ];
        for (k, last) in keys {
            assert_eq!(
                enc(&key(k, Mods::NONE), NORMAL),
                format!("\\x1b[{last}"),
                "{k:?}"
            );
            assert_eq!(
                enc(&key(k, Mods::NONE), APP_CURSOR),
                format!("\\x1bO{last}"),
                "{k:?}"
            );
            for (mods, m) in MODIFIED {
                let want = format!("\\x1b[1;{m}{last}");
                assert_eq!(enc(&key(k, mods), NORMAL), want, "{k:?} {mods:?}");
                assert_eq!(enc(&key(k, mods), APP_CURSOR), want, "{k:?} {mods:?} app");
            }
        }
    }

    #[test]
    fn editing_keys() {
        let keys = [
            (Key::Insert, 2),
            (Key::Delete, 3),
            (Key::PageUp, 5),
            (Key::PageDown, 6),
            (Key::Menu, 29),
        ];
        for (k, n) in keys {
            for modes in [NORMAL, APP_CURSOR, APP_KEYPAD] {
                assert_eq!(
                    enc(&key(k, Mods::NONE), modes),
                    format!("\\x1b[{n}~"),
                    "{k:?}"
                );
                for (mods, m) in MODIFIED {
                    assert_eq!(
                        enc(&key(k, mods), modes),
                        format!("\\x1b[{n};{m}~"),
                        "{k:?} {mods:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn function_keys() {
        for (n, last) in [(1, 'P'), (2, 'Q'), (3, 'R'), (4, 'S')] {
            for modes in [NORMAL, APP_CURSOR] {
                assert_eq!(
                    enc(&key(Key::F(n), Mods::NONE), modes),
                    format!("\\x1bO{last}")
                );
                for (mods, m) in MODIFIED {
                    let want = format!("\\x1b[1;{m}{last}");
                    assert_eq!(enc(&key(Key::F(n), mods), modes), want, "F{n} {mods:?}");
                }
            }
        }
        let tilde_keys = [
            (5, 15),
            (6, 17),
            (7, 18),
            (8, 19),
            (9, 20),
            (10, 21),
            (11, 23),
            (12, 24),
            (13, 25),
            (14, 26),
            (15, 28),
            (16, 29),
            (17, 31),
            (18, 32),
            (19, 33),
            (20, 34),
            (21, 42),
            (24, 45),
            (35, 56),
        ];
        for (n, number) in tilde_keys {
            for modes in [NORMAL, APP_CURSOR, APP_KEYPAD] {
                let plain = format!("\\x1b[{number}~");
                assert_eq!(enc(&key(Key::F(n), Mods::NONE), modes), plain, "F{n}");
                for (mods, m) in MODIFIED {
                    let want = format!("\\x1b[{number};{m}~");
                    assert_eq!(enc(&key(Key::F(n), mods), modes), want, "F{n} {mods:?}");
                }
            }
        }
        assert_eq!(enc(&key(Key::F(0), Mods::NONE), NORMAL), "");
        assert_eq!(enc(&key(Key::F(36), Mods::NONE), NORMAL), "");
    }

    /// The C0 keys, from the kitty spec's table of legacy encodings.
    #[test]
    fn c0_keys() {
        let shift_alt = Mods::SHIFT | Mods::ALT;
        let ctrl_shift = Mods::CTRL | Mods::SHIFT;
        let ctrl_alt = Mods::CTRL | Mods::ALT;
        let columns = [
            Mods::NONE,
            Mods::CTRL,
            Mods::ALT,
            Mods::SHIFT,
            ctrl_shift,
            shift_alt,
            ctrl_alt,
        ];
        let rows: [(KeyEvent, [&str; 7]); 5] = [
            (
                KeyEvent::new(Key::Enter),
                [
                    "\\r", "\\r", "\\x1b\\r", "\\r", "\\r", "\\x1b\\r", "\\x1b\\r",
                ],
            ),
            (
                KeyEvent::new(Key::Escape),
                [
                    "\\x1b",
                    "\\x1b",
                    "\\x1b\\x1b",
                    "\\x1b",
                    "\\x1b",
                    "\\x1b\\x1b",
                    "\\x1b\\x1b",
                ],
            ),
            (
                KeyEvent::new(Key::Backspace),
                [
                    "\\x7f",
                    "\\x08",
                    "\\x1b\\x7f",
                    "\\x7f",
                    "\\x08",
                    "\\x1b\\x7f",
                    "\\x1b\\x08",
                ],
            ),
            (
                KeyEvent::new(Key::Tab),
                [
                    "\\t",
                    "\\t",
                    "\\x1b\\t",
                    "\\x1b[Z",
                    "\\x1b[Z",
                    "\\x1b\\x1b[Z",
                    "\\x1b\\t",
                ],
            ),
            (
                KeyEvent::new(Key::Char(' ')).text(" ").shifted(' '),
                [" ", "\\x00", "\\x1b ", " ", "\\x00", "\\x1b ", "\\x1b\\x00"],
            ),
        ];
        for (event, wants) in rows {
            for (mods, want) in columns.iter().zip(wants) {
                // Shift alone leaves the OS text in place; Ctrl and Alt
                // replace it.
                let event = KeyEvent {
                    mods: *mods,
                    left_option: mods.alt(),
                    ..event.clone()
                };
                assert_eq!(enc(&event, NORMAL), want, "{:?} {mods:?}", event.key);
                assert_eq!(
                    enc(&event, APP_CURSOR),
                    want,
                    "{:?} {mods:?} app",
                    event.key
                );
            }
        }
    }

    #[test]
    fn enter_under_line_feed_mode() {
        let lnm = Modes {
            line_feed_new_line: true,
            ..NORMAL
        };
        assert_eq!(enc(&key(Key::Enter, Mods::NONE), lnm), "\\r\\n");
        assert_eq!(enc(&key(Key::Enter, Mods::ALT), lnm), "\\x1b\\r\\n");
        let kp_enter = key(Key::Keypad(KeypadKey::Enter), Mods::NONE);
        assert_eq!(enc(&kp_enter, lnm), "\\r\\n");
    }

    /// Every key of the US layout: base, shifted, then what Ctrl sends for
    /// each (X11's control mapping, which xterm uses, plus ? as DEL).
    const US_KEYS: [(char, char, &str, &str); 48] = [
        ('`', '~', "\\x00", "\\x1e"),
        ('1', '!', "1", "!"),
        ('2', '@', "\\x00", "\\x00"),
        ('3', '#', "\\x1b", "#"),
        ('4', '$', "\\x1c", "$"),
        ('5', '%', "\\x1d", "%"),
        ('6', '^', "\\x1e", "\\x1e"),
        ('7', '&', "\\x1f", "&"),
        ('8', '*', "\\x7f", "*"),
        ('9', '(', "9", "("),
        ('0', ')', "0", ")"),
        ('-', '_', "-", "\\x1f"),
        ('=', '+', "=", "+"),
        ('[', '{', "\\x1b", "\\x1b"),
        (']', '}', "\\x1d", "\\x1d"),
        ('\\', '|', "\\x1c", "\\x1c"),
        (';', ':', ";", ":"),
        ('\'', '"', "\\'", "\\\""),
        (',', '<', ",", "<"),
        ('.', '>', ".", ">"),
        ('/', '?', "\\x1f", "\\x7f"),
        (' ', ' ', "\\x00", "\\x00"),
        ('a', 'A', "\\x01", "\\x01"),
        ('b', 'B', "\\x02", "\\x02"),
        ('c', 'C', "\\x03", "\\x03"),
        ('d', 'D', "\\x04", "\\x04"),
        ('e', 'E', "\\x05", "\\x05"),
        ('f', 'F', "\\x06", "\\x06"),
        ('g', 'G', "\\x07", "\\x07"),
        ('h', 'H', "\\x08", "\\x08"),
        ('i', 'I', "\\t", "\\t"),
        ('j', 'J', "\\n", "\\n"),
        ('k', 'K', "\\x0b", "\\x0b"),
        ('l', 'L', "\\x0c", "\\x0c"),
        ('m', 'M', "\\r", "\\r"),
        ('n', 'N', "\\x0e", "\\x0e"),
        ('o', 'O', "\\x0f", "\\x0f"),
        ('p', 'P', "\\x10", "\\x10"),
        ('q', 'Q', "\\x11", "\\x11"),
        ('r', 'R', "\\x12", "\\x12"),
        ('s', 'S', "\\x13", "\\x13"),
        ('t', 'T', "\\x14", "\\x14"),
        ('u', 'U', "\\x15", "\\x15"),
        ('v', 'V', "\\x16", "\\x16"),
        ('w', 'W', "\\x17", "\\x17"),
        ('x', 'X', "\\x18", "\\x18"),
        ('y', 'Y', "\\x19", "\\x19"),
        ('z', 'Z', "\\x1a", "\\x1a"),
    ];

    #[test]
    fn printable_keys() {
        for (base, shifted, ctrl, ctrl_shift) in US_KEYS {
            let plain = show(base.encode_utf8(&mut [0; 4]).as_bytes());
            let upper = show(shifted.encode_utf8(&mut [0; 4]).as_bytes());
            let k = KeyEvent::new(Key::Char(base)).shifted(shifted);
            let rows = [
                (k.clone().text(&base.to_string()), plain.clone()),
                (
                    k.clone().mods(Mods::SHIFT).text(&shifted.to_string()),
                    upper.clone(),
                ),
                (k.clone().mods(Mods::ALT), format!("\\x1b{plain}")),
                (
                    k.clone().mods(Mods::ALT | Mods::SHIFT),
                    format!("\\x1b{upper}"),
                ),
                (k.clone().mods(Mods::CTRL), ctrl.to_owned()),
                (
                    k.clone().mods(Mods::CTRL | Mods::ALT),
                    format!("\\x1b{ctrl}"),
                ),
                (
                    k.clone().mods(Mods::CTRL | Mods::SHIFT),
                    ctrl_shift.to_owned(),
                ),
                (
                    k.clone().mods(Mods::CTRL | Mods::ALT | Mods::SHIFT),
                    format!("\\x1b{ctrl_shift}"),
                ),
            ];
            for (event, want) in rows {
                for modes in [NORMAL, APP_CURSOR, APP_KEYPAD] {
                    assert_eq!(enc(&event, modes), want, "{base:?} {:?}", event.mods);
                }
            }
        }
    }

    #[test]
    fn printable_keys_without_a_known_shifted_character() {
        // Shift on a letter still means uppercase.
        let a = KeyEvent::new(Key::Char('a'));
        assert_eq!(
            enc(&a.clone().mods(Mods::ALT | Mods::SHIFT), NORMAL),
            "\\x1bA"
        );
        assert_eq!(
            enc(&a.clone().mods(Mods::CTRL | Mods::SHIFT), NORMAL),
            "\\x01"
        );
        // A French AZERTY key: ':' with '/' on Shift.
        let colon = KeyEvent::new(Key::Char(':')).shifted('/');
        assert_eq!(enc(&colon.mods(Mods::ALT | Mods::SHIFT), NORMAL), "\\x1b/");
    }

    #[test]
    fn lock_keys_change_nothing() {
        let locks = Mods::CAPS_LOCK | Mods::NUM_LOCK;
        let rows = [
            (key(Key::Char(' '), Mods::CTRL | locks), "\\x00"),
            (key(Key::Up, locks), "\\x1b[A"),
            (key(Key::Up, Mods::CTRL | locks), "\\x1b[1;5A"),
            (key(Key::F(5), locks), "\\x1b[15~"),
            (key(Key::Enter, locks), "\\r"),
            (key(Key::Char('a'), Mods::CAPS_LOCK).text("A"), "A"),
        ];
        for (event, want) in rows {
            assert_eq!(enc(&event, NORMAL), want, "{event:?}");
        }
    }

    #[test]
    fn keys_that_send_nothing() {
        let rows = [
            key(Key::Char('c'), Mods::SUPER).text("c"),
            key(Key::Up, Mods::SUPER),
            key(Key::Enter, Mods::SUPER | Mods::SHIFT),
            key(Key::Char('a'), Mods::NONE)
                .text("a")
                .kind(KeyKind::Release),
            key(Key::Enter, Mods::NONE).kind(KeyKind::Release),
            key(Key::CapsLock, Mods::CAPS_LOCK),
            key(Key::ShiftLeft, Mods::SHIFT),
            key(Key::ShiftRight, Mods::SHIFT),
            key(Key::ControlLeft, Mods::CTRL),
            key(Key::ControlRight, Mods::CTRL),
            key(Key::AltLeft, Mods::ALT),
            key(Key::AltRight, Mods::ALT),
            key(Key::SuperLeft, Mods::SUPER),
            key(Key::SuperRight, Mods::SUPER),
            // A dead key's press types nothing yet.
            key(Key::Char('\''), Mods::NONE),
            key(Key::Keypad(KeypadKey::Digit(10)), Mods::NONE),
        ];
        for event in rows {
            for modes in [NORMAL, APP_CURSOR, APP_KEYPAD] {
                assert_eq!(enc(&event, modes), "", "{event:?}");
            }
        }
    }

    #[test]
    fn repeat_is_a_press() {
        let rows = [
            (key(Key::Char('a'), Mods::NONE).text("a"), "a"),
            (key(Key::Up, Mods::NONE), "\\x1b[A"),
            (key(Key::Char('a'), Mods::CTRL), "\\x01"),
        ];
        for (event, want) in rows {
            assert_eq!(enc(&event.kind(KeyKind::Repeat), NORMAL), want);
        }
    }

    #[test]
    fn text_goes_as_typed() {
        let rows = [
            (key(Key::Char('a'), Mods::NONE).text("a"), "a"),
            (key(Key::Char('a'), Mods::SHIFT).text("A"), "A"),
            (key(Key::Char('e'), Mods::NONE).text("é"), "\\xc3\\xa9"),
            (
                key(Key::Char('j'), Mods::NONE).text("日本"),
                "\\xe6\\x97\\xa5\\xe6\\x9c\\xac",
            ),
            (
                key(Key::Char('q'), Mods::NONE).text("😀"),
                "\\xf0\\x9f\\x98\\x80",
            ),
        ];
        for (event, want) in rows {
            assert_eq!(enc(&event, NORMAL), want);
        }
    }

    #[test]
    fn keypad_numeric_mode() {
        let rows = [
            (KeypadKey::Digit(0), "0"),
            (KeypadKey::Digit(5), "5"),
            (KeypadKey::Digit(9), "9"),
            (KeypadKey::Decimal, "."),
            (KeypadKey::Divide, "/"),
            (KeypadKey::Multiply, "*"),
            (KeypadKey::Subtract, "-"),
            (KeypadKey::Add, "+"),
            (KeypadKey::Equal, "="),
            (KeypadKey::Enter, "\\r"),
        ];
        for (k, want) in rows {
            assert_eq!(enc(&key(Key::Keypad(k), Mods::NONE), NORMAL), want, "{k:?}");
            // As in xterm, Shift and Ctrl change nothing and Alt adds ESC.
            assert_eq!(
                enc(&key(Key::Keypad(k), Mods::SHIFT), NORMAL),
                want,
                "{k:?}"
            );
            assert_eq!(enc(&key(Key::Keypad(k), Mods::CTRL), NORMAL), want, "{k:?}");
            assert_eq!(
                enc(&key(Key::Keypad(k), Mods::ALT), NORMAL),
                format!("\\x1b{want}"),
                "{k:?}"
            );
        }
        // The layout's text wins: a comma on the decimal key.
        let comma = key(Key::Keypad(KeypadKey::Decimal), Mods::NONE).text(",");
        assert_eq!(enc(&comma, NORMAL), ",");
    }

    #[test]
    fn keypad_application_mode() {
        let rows = [
            (KeypadKey::Digit(0), 'p'),
            (KeypadKey::Digit(1), 'q'),
            (KeypadKey::Digit(2), 'r'),
            (KeypadKey::Digit(3), 's'),
            (KeypadKey::Digit(4), 't'),
            (KeypadKey::Digit(5), 'u'),
            (KeypadKey::Digit(6), 'v'),
            (KeypadKey::Digit(7), 'w'),
            (KeypadKey::Digit(8), 'x'),
            (KeypadKey::Digit(9), 'y'),
            (KeypadKey::Decimal, 'n'),
            (KeypadKey::Divide, 'o'),
            (KeypadKey::Multiply, 'j'),
            (KeypadKey::Subtract, 'm'),
            (KeypadKey::Add, 'k'),
            (KeypadKey::Enter, 'M'),
            (KeypadKey::Equal, 'X'),
        ];
        for (k, last) in rows {
            // With the text the key types, which application mode replaces.
            let mut event = key(Key::Keypad(k), Mods::NONE);
            event.text = keypad_char(k).map(String::from);
            assert_eq!(enc(&event, APP_KEYPAD), format!("\\x1bO{last}"), "{k:?}");
            for (mods, m) in MODIFIED {
                let event = key(Key::Keypad(k), mods);
                assert_eq!(
                    enc(&event, APP_KEYPAD),
                    format!("\\x1bO{m}{last}"),
                    "{k:?} {mods:?}"
                );
            }
        }
    }

    #[test]
    fn command_combinations_send_nothing() {
        let keys = [
            Key::Char('a'),
            Key::Char('1'),
            Key::Char(' '),
            Key::Enter,
            Key::Tab,
            Key::Backspace,
            Key::Escape,
            Key::Up,
            Key::Home,
            Key::PageDown,
            Key::Delete,
            Key::Menu,
            Key::F(1),
            Key::F(12),
            Key::Keypad(KeypadKey::Digit(1)),
            Key::Keypad(KeypadKey::Enter),
        ];
        for k in keys {
            for others in [Mods::NONE, Mods::SHIFT, Mods::ALT, Mods::CTRL, Mods(7)] {
                // Text as macOS reports it: only for keys that type.
                let mut event = key(k, Mods::SUPER | others);
                if matches!(k, Key::Char(_) | Key::Keypad(KeypadKey::Digit(_))) {
                    event = event.text("a");
                }
                for modes in [NORMAL, APP_CURSOR, APP_KEYPAD] {
                    assert_eq!(enc(&event, modes), "", "{k:?} {others:?}");
                }
            }
        }
    }
}
