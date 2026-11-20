//! The kitty keyboard protocol
//! (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>), used while the
//! program has turned it on.

use tern_types::{Key, KeypadKey};

use crate::key::{keypad_char, KeyKind, Resolved};
use crate::{legacy, push_decimal, CSI, ESC};

const DISAMBIGUATE: u8 = 1;
const EVENT_TYPES: u8 = 2;
const ALTERNATE_KEYS: u8 = 4;
const ALL_KEYS: u8 = 8;
const ASSOCIATED_TEXT: u8 = 16;

/// Whether the flags change anything. Alternate keys and associated text
/// only refine escape codes the other flags produce, so on their own the
/// legacy encoding stays.
pub(crate) fn active(flags: u8) -> bool {
    flags & (DISAMBIGUATE | EVENT_TYPES | ALL_KEYS) != 0
}

/// Whether text is reported inside escape codes; the spec leaves associated
/// text undefined without report-all-keys.
pub(crate) fn reports_text(flags: u8) -> bool {
    flags & (ALL_KEYS | ASSOCIATED_TEXT) == ALL_KEYS | ASSOCIATED_TEXT
}

pub(crate) fn encode(key: &Resolved, flags: u8) -> Vec<u8> {
    let disambiguate = flags & DISAMBIGUATE != 0;
    let all_keys = flags & ALL_KEYS != 0;
    let release = key.kind == KeyKind::Release;
    if release && flags & EVENT_TYPES == 0 {
        return Vec::new();
    }
    // Keypad keys are their own keys only when disambiguating or reporting
    // every key; otherwise they are the keys they copy.
    let k = if disambiguate || all_keys {
        key.key
    } else {
        main_key(key.key)
    };
    if !all_keys {
        // Modifier keys are only reported with report-all-keys. Enter, Tab
        // and Backspace releases are held back too, so that a shell left in
        // this mode by a crashed program still takes "reset" and Enter.
        if is_modifier(k) || (release && matches!(k, Key::Enter | Key::Tab | Key::Backspace)) {
            return Vec::new();
        }
    }
    let held = key.mods.held();
    if !all_keys && !release {
        // Text with no Ctrl, Alt or Command is sent as text. Lock keys and
        // Shift do not count: they only change what is typed.
        if let Some(text) = key.text
            && !held.ctrl() && !held.alt() && !held.cmd() {
                return text.as_bytes().to_vec();
            }
        if held.is_empty() {
            match k {
                Key::Enter => return b"\r".to_vec(),
                Key::Tab => return b"\t".to_vec(),
                Key::Backspace => return vec![0x7f],
                _ => {}
            }
        }
        if !disambiguate {
            // Reporting event types alone leaves presses as they were.
            match k {
                Key::Escape if held.is_empty() => return vec![ESC],
                Key::Char(c) => {
                    let bytes = legacy::char_bytes(key, c);
                    if !bytes.is_empty() {
                        return bytes;
                    }
                }
                _ => {}
            }
        }
    }
    escape_code(key, k, flags)
}

/// CSI 0;;codepoints u: text with no key, under report-all-keys plus
/// associated text.
pub(crate) fn text_event(text: &str) -> Vec<u8> {
    // With nothing printable this would be an event for no key and no text.
    if text.chars().all(char::is_control) {
        return Vec::new();
    }
    let mut out = Vec::new();
    serialize(
        &mut out,
        &Fields {
            code: 0,
            text: Some(text),
            ..Fields::default()
        },
        b'u',
    );
    out
}

fn escape_code(key: &Resolved, k: Key, flags: u8) -> Vec<u8> {
    let Some((code, last)) = key_code(k) else {
        return Vec::new();
    };
    let event_type = match key.kind {
        _ if flags & EVENT_TYPES == 0 => None,
        KeyKind::Press => None,
        KeyKind::Repeat => Some(2),
        KeyKind::Release => Some(3),
    };
    // Only keys that type characters have alternates. The shifted key is
    // only reported with Shift held.
    let (shifted, base_layout) = match k {
        Key::Char(c) if flags & ALTERNATE_KEYS != 0 => (
            key.shifted.filter(|&s| key.mods.shift() && s != c),
            key.base_layout.filter(|&b| b != c),
        ),
        _ => (None, None),
    };
    let text = if reports_text(flags) && key.kind != KeyKind::Release {
        key.text
    } else {
        None
    };
    let fields = Fields {
        code,
        shifted,
        base_layout,
        mods: key.mods.0,
        event_type,
        text,
    };
    let mut out = Vec::with_capacity(16);
    serialize(&mut out, &fields, last);
    out
}

#[derive(Default)]
struct Fields<'a> {
    code: u32,
    shifted: Option<char>,
    base_layout: Option<char>,
    mods: u8,
    event_type: Option<u8>,
    text: Option<&'a str>,
}

/// CSI code:shifted:base ; mods+1:event ; text u, leaving out what is
/// empty, and the code too when it is the 1 of CSI 1 ; mods A.
fn serialize(out: &mut Vec<u8>, f: &Fields, last: u8) {
    // The text must not carry control codes.
    let mut text = f
        .text
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .peekable();
    let has_text = text.peek().is_some();
    let has_alternates = f.shifted.is_some() || f.base_layout.is_some();
    let has_mods = f.mods != 0 || f.event_type.is_some();
    out.extend_from_slice(CSI);
    if f.code != 1 || has_alternates || has_mods || has_text {
        push_decimal(out, f.code);
    }
    if has_alternates {
        out.push(b':');
        if let Some(shifted) = f.shifted {
            push_decimal(out, u32::from(shifted));
        }
        if let Some(base_layout) = f.base_layout {
            out.push(b':');
            push_decimal(out, u32::from(base_layout));
        }
    }
    if has_mods || has_text {
        out.push(b';');
    }
    if has_mods {
        push_decimal(out, u32::from(f.mods) + 1);
        if let Some(event_type) = f.event_type {
            out.push(b':');
            push_decimal(out, u32::from(event_type));
        }
    }
    if has_text {
        out.push(b';');
        for (i, c) in text.enumerate() {
            if i > 0 {
                out.push(b':');
            }
            push_decimal(out, u32::from(c));
        }
    }
    out.push(last);
}

fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::CapsLock
            | Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

/// The key a keypad key copies.
fn main_key(key: Key) -> Key {
    match key {
        Key::Keypad(KeypadKey::Enter) => Key::Enter,
        Key::Keypad(k) => keypad_char(k).map_or(key, Key::Char),
        _ => key,
    }
}

/// The spec's number and final byte for a key; None for keys it has none
/// for.
fn key_code(key: Key) -> Option<(u32, u8)> {
    Some(match key {
        Key::Char(c) => (u32::from(c), b'u'),
        Key::Escape => (27, b'u'),
        Key::Enter => (13, b'u'),
        Key::Tab => (9, b'u'),
        Key::Backspace => (127, b'u'),
        Key::Insert => (2, b'~'),
        Key::Delete => (3, b'~'),
        Key::Left => (1, b'D'),
        Key::Right => (1, b'C'),
        Key::Up => (1, b'A'),
        Key::Down => (1, b'B'),
        Key::PageUp => (5, b'~'),
        Key::PageDown => (6, b'~'),
        Key::Home => (1, b'H'),
        Key::End => (1, b'F'),
        Key::CapsLock => (57358, b'u'),
        Key::Menu => (57363, b'u'),
        Key::F(1) => (1, b'P'),
        Key::F(2) => (1, b'Q'),
        // Not CSI R, which a program would take for a cursor position report.
        Key::F(3) => (13, b'~'),
        Key::F(4) => (1, b'S'),
        Key::F(5) => (15, b'~'),
        Key::F(6) => (17, b'~'),
        Key::F(7) => (18, b'~'),
        Key::F(8) => (19, b'~'),
        Key::F(9) => (20, b'~'),
        Key::F(10) => (21, b'~'),
        Key::F(11) => (23, b'~'),
        Key::F(12) => (24, b'~'),
        Key::F(n @ 13..=35) => (57376 + u32::from(n - 13), b'u'),
        Key::F(_) => return None,
        Key::Keypad(k) => (
            match k {
                KeypadKey::Digit(d @ 0..=9) => 57399 + u32::from(d),
                KeypadKey::Digit(_) => return None,
                KeypadKey::Decimal => 57409,
                KeypadKey::Divide => 57410,
                KeypadKey::Multiply => 57411,
                KeypadKey::Subtract => 57412,
                KeypadKey::Add => 57413,
                KeypadKey::Enter => 57414,
                KeypadKey::Equal => 57415,
            },
            b'u',
        ),
        Key::ShiftLeft => (57441, b'u'),
        Key::ControlLeft => (57442, b'u'),
        Key::AltLeft => (57443, b'u'),
        Key::SuperLeft => (57444, b'u'),
        Key::ShiftRight => (57447, b'u'),
        Key::ControlRight => (57448, b'u'),
        Key::AltRight => (57449, b'u'),
        Key::SuperRight => (57450, b'u'),
    })
}

#[cfg(test)]
mod tests {
    use tern_types::{Key, KeypadKey, Modes, Mods};

    use crate::key::{encode_key, KeyEvent, KeyKind, KeyOptions, OptionAsAlt};
    use crate::show;

    const ALT_BOTH: KeyOptions = KeyOptions {
        option_as_alt: OptionAsAlt::Both,
    };
    const ALT_NONE: KeyOptions = KeyOptions {
        option_as_alt: OptionAsAlt::None,
    };

    const D: u8 = 0b1;
    const E: u8 = 0b10;
    const A: u8 = 0b100;
    const K: u8 = 0b1000;
    const T: u8 = 0b10000;

    fn enc_with(event: &KeyEvent, flags: u8, app_cursor: bool, options: KeyOptions) -> String {
        let modes = Modes {
            kitty_flags: flags,
            app_cursor,
            app_keypad: app_cursor,
            ..Modes::default()
        };
        show(&encode_key(event, modes, options))
    }

    /// Encodes under the flags, checking application cursor and keypad
    /// modes change nothing.
    fn enc(event: &KeyEvent, flags: u8) -> String {
        let got = enc_with(event, flags, false, ALT_BOTH);
        assert_eq!(
            enc_with(event, flags, true, ALT_BOTH),
            got,
            "app modes changed {event:?}"
        );
        got
    }

    fn key(k: Key, mods: Mods) -> KeyEvent {
        KeyEvent::new(k).mods(mods)
    }

    fn check(flags: u8, rows: &[(KeyEvent, &str)]) {
        for (event, want) in rows {
            assert_eq!(enc(event, flags), *want, "flags {flags:#b} {event:?}");
        }
    }

    /// Modifier combinations with the protocol's parameter for each.
    const MODIFIED: [(Mods, u32); 8] = [
        (Mods::SHIFT, 2),
        (Mods::ALT, 3),
        (Mods(Mods::SHIFT.0 | Mods::ALT.0), 4),
        (Mods::CTRL, 5),
        (Mods(Mods::SHIFT.0 | Mods::CTRL.0), 6),
        (Mods(Mods::ALT.0 | Mods::CTRL.0), 7),
        (Mods::SUPER, 9),
        (
            Mods(Mods::SHIFT.0 | Mods::ALT.0 | Mods::CTRL.0 | Mods::SUPER.0),
            16,
        ),
    ];

    /// Each key that sends a code, with what it sends unmodified under the
    /// disambiguate flag and the shape it takes with modifiers.
    fn functional_keys() -> Vec<(KeyEvent, String, String)> {
        let named = [
            (KeyEvent::new(Key::Escape), "\\x1b[27u", "\\x1b[27;{m}u"),
            (KeyEvent::new(Key::Enter), "\\r", "\\x1b[13;{m}u"),
            (KeyEvent::new(Key::Tab), "\\t", "\\x1b[9;{m}u"),
            (KeyEvent::new(Key::Backspace), "\\x7f", "\\x1b[127;{m}u"),
            (KeyEvent::new(Key::Insert), "\\x1b[2~", "\\x1b[2;{m}~"),
            (KeyEvent::new(Key::Delete), "\\x1b[3~", "\\x1b[3;{m}~"),
            (KeyEvent::new(Key::Left), "\\x1b[D", "\\x1b[1;{m}D"),
            (KeyEvent::new(Key::Right), "\\x1b[C", "\\x1b[1;{m}C"),
            (KeyEvent::new(Key::Up), "\\x1b[A", "\\x1b[1;{m}A"),
            (KeyEvent::new(Key::Down), "\\x1b[B", "\\x1b[1;{m}B"),
            (KeyEvent::new(Key::PageUp), "\\x1b[5~", "\\x1b[5;{m}~"),
            (KeyEvent::new(Key::PageDown), "\\x1b[6~", "\\x1b[6;{m}~"),
            (KeyEvent::new(Key::Home), "\\x1b[H", "\\x1b[1;{m}H"),
            (KeyEvent::new(Key::End), "\\x1b[F", "\\x1b[1;{m}F"),
            (KeyEvent::new(Key::Menu), "\\x1b[57363u", "\\x1b[57363;{m}u"),
            (KeyEvent::new(Key::F(1)), "\\x1b[P", "\\x1b[1;{m}P"),
            (KeyEvent::new(Key::F(2)), "\\x1b[Q", "\\x1b[1;{m}Q"),
            (KeyEvent::new(Key::F(3)), "\\x1b[13~", "\\x1b[13;{m}~"),
            (KeyEvent::new(Key::F(4)), "\\x1b[S", "\\x1b[1;{m}S"),
            (KeyEvent::new(Key::F(5)), "\\x1b[15~", "\\x1b[15;{m}~"),
            (KeyEvent::new(Key::F(6)), "\\x1b[17~", "\\x1b[17;{m}~"),
            (KeyEvent::new(Key::F(7)), "\\x1b[18~", "\\x1b[18;{m}~"),
            (KeyEvent::new(Key::F(8)), "\\x1b[19~", "\\x1b[19;{m}~"),
            (KeyEvent::new(Key::F(9)), "\\x1b[20~", "\\x1b[20;{m}~"),
            (KeyEvent::new(Key::F(10)), "\\x1b[21~", "\\x1b[21;{m}~"),
            (KeyEvent::new(Key::F(11)), "\\x1b[23~", "\\x1b[23;{m}~"),
            (KeyEvent::new(Key::F(12)), "\\x1b[24~", "\\x1b[24;{m}~"),
            (
                KeyEvent::new(Key::F(13)),
                "\\x1b[57376u",
                "\\x1b[57376;{m}u",
            ),
            (
                KeyEvent::new(Key::F(20)),
                "\\x1b[57383u",
                "\\x1b[57383;{m}u",
            ),
            (
                KeyEvent::new(Key::F(35)),
                "\\x1b[57398u",
                "\\x1b[57398;{m}u",
            ),
        ];
        let keypad = [
            (KeypadKey::Digit(0), 57399),
            (KeypadKey::Digit(1), 57400),
            (KeypadKey::Digit(2), 57401),
            (KeypadKey::Digit(3), 57402),
            (KeypadKey::Digit(4), 57403),
            (KeypadKey::Digit(5), 57404),
            (KeypadKey::Digit(6), 57405),
            (KeypadKey::Digit(7), 57406),
            (KeypadKey::Digit(8), 57407),
            (KeypadKey::Digit(9), 57408),
            (KeypadKey::Decimal, 57409),
            (KeypadKey::Divide, 57410),
            (KeypadKey::Multiply, 57411),
            (KeypadKey::Subtract, 57412),
            (KeypadKey::Add, 57413),
            (KeypadKey::Enter, 57414),
            (KeypadKey::Equal, 57415),
        ];
        let mut rows: Vec<_> = named
            .into_iter()
            .map(|(e, plain, modded)| (e, plain.to_owned(), modded.to_owned()))
            .collect();
        // Keypad keys without text: Enter has none, and the digits are
        // tested here as codes.
        rows.extend(keypad.into_iter().map(|(k, code)| {
            (
                KeyEvent::new(Key::Keypad(k)),
                format!("\\x1b[{code}u"),
                format!("\\x1b[{code};{{m}}u"),
            )
        }));
        rows
    }

    #[test]
    fn disambiguate_functional_keys() {
        for (event, plain, modded) in functional_keys() {
            assert_eq!(enc(&event, D), plain, "{event:?}");
            for (mods, m) in MODIFIED {
                let want = modded.replace("{m}", &m.to_string());
                assert_eq!(
                    enc(&event.clone().mods(mods), D),
                    want,
                    "{event:?} {mods:?}"
                );
            }
        }
    }

    #[test]
    fn disambiguate() {
        let a = KeyEvent::new(Key::Char('a')).shifted('A');
        check(
            D,
            &[
                (a.clone().text("a"), "a"),
                (a.clone().mods(Mods::SHIFT).text("A"), "A"),
                (a.clone().mods(Mods::CTRL).text("\u{1}"), "\\x1b[97;5u"),
                (a.clone().mods(Mods::ALT).text("å"), "\\x1b[97;3u"),
                (a.clone().mods(Mods::CTRL | Mods::SHIFT), "\\x1b[97;6u"),
                (a.clone().mods(Mods::ALT | Mods::SHIFT), "\\x1b[97;4u"),
                (a.clone().mods(Mods::CTRL | Mods::ALT), "\\x1b[97;7u"),
                (a.clone().mods(Mods::SUPER).text("a"), "\\x1b[97;9u"),
                (key(Key::Char(' '), Mods::CTRL), "\\x1b[32;5u"),
                (key(Key::Char(' '), Mods::NONE).text(" "), " "),
                (key(Key::Char('['), Mods::ALT), "\\x1b[91;3u"),
                (key(Key::Char('i'), Mods::CTRL), "\\x1b[105;5u"),
                (key(Key::Char('3'), Mods::CTRL), "\\x1b[51;5u"),
                (
                    key(Key::Keypad(KeypadKey::Digit(0)), Mods::NONE).text("0"),
                    "0",
                ),
                (
                    key(Key::Keypad(KeypadKey::Digit(0)), Mods::SHIFT).text("0"),
                    "0",
                ),
                (key(Key::Char('é'), Mods::NONE).text("é"), "\\xc3\\xa9"),
            ],
        );
    }

    #[test]
    fn lock_modifiers() {
        let locks = Mods::CAPS_LOCK | Mods::NUM_LOCK;
        // Not reported for keys that type text, so programs that only ask to
        // disambiguate still get text with Caps Lock on.
        check(
            D,
            &[
                (key(Key::Char('a'), Mods::CAPS_LOCK).text("A"), "A"),
                (
                    key(Key::Char('a'), Mods::CAPS_LOCK | Mods::CTRL),
                    "\\x1b[97;69u",
                ),
                (key(Key::Escape, Mods::CAPS_LOCK), "\\x1b[27;65u"),
                (key(Key::Up, Mods::NUM_LOCK), "\\x1b[1;129A"),
                (key(Key::Enter, locks), "\\r"),
                (key(Key::Tab, locks), "\\t"),
                (key(Key::Backspace, locks), "\\x7f"),
                (key(Key::Enter, locks | Mods::SHIFT), "\\x1b[13;194u"),
            ],
        );
        check(
            D | E,
            &[
                (key(Key::Enter, locks), "\\r"),
                (key(Key::Enter, locks).kind(KeyKind::Release), ""),
            ],
        );
        check(
            K,
            &[(
                key(Key::Char('a'), Mods::CAPS_LOCK).text("A"),
                "\\x1b[97;65u",
            )],
        );
    }

    #[test]
    fn disambiguate_reports_no_releases_and_no_modifier_keys() {
        for flags in [D, D | A, D | T] {
            check(
                flags,
                &[
                    (key(Key::Char('a'), Mods::NONE).kind(KeyKind::Release), ""),
                    (key(Key::Up, Mods::NONE).kind(KeyKind::Release), ""),
                    (
                        key(Key::Char('a'), Mods::CTRL).kind(KeyKind::Repeat),
                        "\\x1b[97;5u",
                    ),
                    (key(Key::ShiftLeft, Mods::SHIFT), ""),
                    (key(Key::ControlRight, Mods::CTRL), ""),
                    (key(Key::CapsLock, Mods::CAPS_LOCK), ""),
                ],
            );
        }
    }

    #[test]
    fn event_types() {
        let release = KeyKind::Release;
        let repeat = KeyKind::Repeat;
        let a = key(Key::Char('a'), Mods::NONE);
        // Event types alone leave presses as legacy bytes.
        check(
            E,
            &[
                (a.clone().text("a"), "a"),
                (a.clone().text("a").kind(repeat), "a"),
                (a.clone().kind(repeat), "\\x1b[97;1:2u"),
                (a.clone().text("a").kind(release), "\\x1b[97;1:3u"),
                (a.clone().mods(Mods::SHIFT).kind(release), "\\x1b[97;2:3u"),
                (a.clone().mods(Mods::CTRL), "\\x01"),
                (a.clone().mods(Mods::CTRL).kind(release), "\\x1b[97;5:3u"),
                (a.clone().mods(Mods::ALT), "\\x1ba"),
                (a.clone().mods(Mods::SUPER), "\\x1b[97;9u"),
                (key(Key::Escape, Mods::NONE), "\\x1b"),
                (key(Key::Escape, Mods::NONE).kind(release), "\\x1b[27;1:3u"),
                (key(Key::Escape, Mods::ALT), "\\x1b[27;3u"),
                (key(Key::Enter, Mods::NONE), "\\r"),
                (key(Key::Enter, Mods::CTRL), "\\x1b[13;5u"),
                (key(Key::Up, Mods::NONE), "\\x1b[A"),
                (key(Key::Up, Mods::NONE).kind(repeat), "\\x1b[1;1:2A"),
                (key(Key::Up, Mods::NONE).kind(release), "\\x1b[1;1:3A"),
                (key(Key::F(1), Mods::NONE), "\\x1b[P"),
                (key(Key::F(1), Mods::NONE).kind(release), "\\x1b[1;1:3P"),
                (key(Key::F(5), Mods::SHIFT).kind(release), "\\x1b[15;2:3~"),
                // The keypad is reported as the keys it copies.
                (key(Key::Keypad(KeypadKey::Enter), Mods::NONE), "\\r"),
                (
                    key(Key::Keypad(KeypadKey::Enter), Mods::NONE).kind(release),
                    "",
                ),
                (
                    key(Key::Keypad(KeypadKey::Digit(5)), Mods::NONE).text("5"),
                    "5",
                ),
                (
                    key(Key::Keypad(KeypadKey::Digit(5)), Mods::NONE).kind(release),
                    "\\x1b[53;1:3u",
                ),
                (
                    key(Key::Up, Mods::NONE).keypad().kind(release),
                    "\\x1b[1;1:3A",
                ),
            ],
        );
        check(
            D | E,
            &[
                (a.clone().text("a"), "a"),
                (a.clone().text("a").kind(repeat), "a"),
                (a.clone().kind(release), "\\x1b[97;1:3u"),
                (a.clone().mods(Mods::CTRL), "\\x1b[97;5u"),
                (a.clone().mods(Mods::CTRL).kind(repeat), "\\x1b[97;5:2u"),
                (a.clone().mods(Mods::CTRL).kind(release), "\\x1b[97;5:3u"),
                (key(Key::Escape, Mods::NONE).kind(release), "\\x1b[27;1:3u"),
                (key(Key::Backspace, Mods::NONE), "\\x7f"),
                // Enter, Tab and Backspace have no releases without
                // report-all-keys, even modified.
                (key(Key::Backspace, Mods::NONE).kind(release), ""),
                (key(Key::Enter, Mods::NONE).kind(release), ""),
                (key(Key::Tab, Mods::SHIFT).kind(release), ""),
                (key(Key::Enter, Mods::CTRL).kind(repeat), "\\x1b[13;5:2u"),
                (
                    key(Key::Keypad(KeypadKey::Enter), Mods::NONE).kind(release),
                    "\\x1b[57414;1:3u",
                ),
                (key(Key::ShiftLeft, Mods::SHIFT), ""),
                (key(Key::ShiftLeft, Mods::NONE).kind(release), ""),
            ],
        );
    }

    #[test]
    fn alternate_keys() {
        let a = KeyEvent::new(Key::Char('a')).shifted('A');
        let es = KeyEvent::new(Key::Char('с')).shifted('С').base_layout('c');
        check(
            D | A,
            &[
                (a.clone().text("a"), "a"),
                (a.clone().mods(Mods::SHIFT).text("A"), "A"),
                // The shifted key only with Shift held.
                (a.clone().mods(Mods::CTRL), "\\x1b[97;5u"),
                (a.clone().mods(Mods::CTRL | Mods::SHIFT), "\\x1b[97:65;6u"),
                (
                    a.clone().mods(Mods::CTRL | Mods::SHIFT).base_layout('b'),
                    "\\x1b[97:65:98;6u",
                ),
                (
                    a.clone().mods(Mods::CTRL).base_layout('b'),
                    "\\x1b[97::98;5u",
                ),
                // Nothing to report when the US key is the same key.
                (a.clone().mods(Mods::CTRL).base_layout('a'), "\\x1b[97;5u"),
                (es.clone().mods(Mods::CTRL), "\\x1b[1089::99;5u"),
                (
                    es.clone().mods(Mods::CTRL | Mods::SHIFT),
                    "\\x1b[1089:1057:99;6u",
                ),
                (es.clone().text("с"), "\\xd1\\x81"),
                // Keys that send codes have no alternates.
                (
                    key(Key::Up, Mods::SHIFT).shifted('A').base_layout('b'),
                    "\\x1b[1;2A",
                ),
                (key(Key::Escape, Mods::NONE).base_layout('b'), "\\x1b[27u"),
            ],
        );
        check(
            D | E | A,
            &[(
                a.clone().mods(Mods::SHIFT).text("A").kind(KeyKind::Release),
                "\\x1b[97:65;2:3u",
            )],
        );
    }

    #[test]
    fn report_all_keys() {
        let release = KeyKind::Release;
        let repeat = KeyKind::Repeat;
        check(
            K,
            &[
                (key(Key::Char('a'), Mods::NONE).text("a"), "\\x1b[97u"),
                (
                    key(Key::Char('a'), Mods::NONE).text("a").kind(repeat),
                    "\\x1b[97u",
                ),
                (key(Key::Char('a'), Mods::NONE).text("a").kind(release), ""),
                (key(Key::Char('a'), Mods::SHIFT).text("A"), "\\x1b[97;2u"),
                (key(Key::Char('a'), Mods::CTRL), "\\x1b[97;5u"),
                (key(Key::Char(' '), Mods::NONE).text(" "), "\\x1b[32u"),
                (key(Key::Char('\''), Mods::NONE), "\\x1b[39u"),
                (key(Key::Up, Mods::NONE), "\\x1b[A"),
                (key(Key::Enter, Mods::NONE), "\\x1b[13u"),
                (key(Key::Enter, Mods::CTRL), "\\x1b[13;5u"),
                (key(Key::Tab, Mods::NONE), "\\x1b[9u"),
                (key(Key::Backspace, Mods::NONE), "\\x1b[127u"),
                (key(Key::Escape, Mods::NONE), "\\x1b[27u"),
                (
                    key(Key::Keypad(KeypadKey::Digit(5)), Mods::NONE).text("5"),
                    "\\x1b[57404u",
                ),
                (key(Key::ShiftLeft, Mods::SHIFT), "\\x1b[57441;2u"),
                (key(Key::ControlLeft, Mods::CTRL), "\\x1b[57442;5u"),
                (key(Key::AltLeft, Mods::ALT), "\\x1b[57443;3u"),
                (key(Key::SuperLeft, Mods::SUPER), "\\x1b[57444;9u"),
                (key(Key::ShiftRight, Mods::SHIFT), "\\x1b[57447;2u"),
                (key(Key::ControlRight, Mods::CTRL), "\\x1b[57448;5u"),
                (key(Key::AltRight, Mods::ALT), "\\x1b[57449;3u"),
                (key(Key::SuperRight, Mods::SUPER), "\\x1b[57450;9u"),
                (key(Key::CapsLock, Mods::CAPS_LOCK), "\\x1b[57358;65u"),
                (key(Key::ShiftLeft, Mods::NONE).kind(release), ""),
            ],
        );
        check(
            K | E,
            &[
                (
                    key(Key::Char('a'), Mods::NONE).text("a").kind(repeat),
                    "\\x1b[97;1:2u",
                ),
                (
                    key(Key::Char('a'), Mods::NONE).kind(release),
                    "\\x1b[97;1:3u",
                ),
                (key(Key::Enter, Mods::NONE).kind(release), "\\x1b[13;1:3u"),
                (key(Key::Tab, Mods::NONE).kind(release), "\\x1b[9;1:3u"),
                (
                    key(Key::Backspace, Mods::NONE).kind(release),
                    "\\x1b[127;1:3u",
                ),
                // The modifier's own bit follows the event...
                (
                    key(Key::ShiftLeft, Mods::NONE).kind(release),
                    "\\x1b[57441;1:3u",
                ),
                // ...and stays set while the other Ctrl is held.
                (
                    key(Key::ControlRight, Mods::CTRL).kind(release),
                    "\\x1b[57448;5:3u",
                ),
                (
                    key(Key::CapsLock, Mods::NONE).kind(release),
                    "\\x1b[57358;1:3u",
                ),
            ],
        );
    }

    #[test]
    fn associated_text() {
        let a = KeyEvent::new(Key::Char('a')).shifted('A');
        check(
            K | T,
            &[
                (a.clone().text("a"), "\\x1b[97;;97u"),
                // The spec's example: shift+a -> CSI 97 ; 2 ; 65 u.
                (a.clone().mods(Mods::SHIFT).text("A"), "\\x1b[97;2;65u"),
                (a.clone().mods(Mods::SHIFT).text("AB"), "\\x1b[97;2;65:66u"),
                (a.clone().mods(Mods::CTRL).text("\u{1}"), "\\x1b[97;5u"),
                (a.clone().text("a").kind(KeyKind::Release), ""),
                (key(Key::Enter, Mods::NONE).text("\r"), "\\x1b[13u"),
                (
                    key(Key::Keypad(KeypadKey::Digit(5)), Mods::NONE).text("5"),
                    "\\x1b[57404;;53u",
                ),
                (key(Key::Char('e'), Mods::NONE).text("é"), "\\x1b[101;;233u"),
            ],
        );
        // Option typing å: the OS spent Option, so only the text is new. The
        // spec's own example reports such text with no modifiers.
        let option_a = a.clone().mods(Mods::ALT).text("å");
        assert_eq!(
            enc_with(&option_a, K | T, false, ALT_NONE),
            "\\x1b[97;;229u"
        );
        assert_eq!(enc_with(&option_a, K | T, false, ALT_BOTH), "\\x1b[97;3u");
        assert_eq!(enc_with(&option_a, D, false, ALT_NONE), "\\xc3\\xa5");
        // Only the Option key the setting names acts as Alt.
        let left_only = KeyOptions {
            option_as_alt: OptionAsAlt::Left,
        };
        let right_option_a = option_a.clone().right_option();
        assert_eq!(enc_with(&option_a, D, false, left_only), "\\x1b[97;3u");
        assert_eq!(enc_with(&right_option_a, D, false, left_only), "\\xc3\\xa5");
        assert_eq!(
            enc_with(&right_option_a, K | T, false, left_only),
            "\\x1b[97;;229u"
        );
        check(
            K | E | T,
            &[
                (a.clone().text("a").kind(KeyKind::Release), "\\x1b[97;1:3u"),
                (
                    a.clone().text("a").kind(KeyKind::Repeat),
                    "\\x1b[97;1:2;97u",
                ),
            ],
        );
        check(
            D | E | A | K | T,
            &[
                (
                    a.clone().mods(Mods::SHIFT).text("A").base_layout('q'),
                    "\\x1b[97:65:113;2;65u",
                ),
                (
                    a.clone().mods(Mods::SHIFT).text("A").kind(KeyKind::Release),
                    "\\x1b[97:65;2:3u",
                ),
            ],
        );
        // Without report-all-keys the text flag is undefined; it changes
        // nothing.
        check(
            D | T,
            &[
                (a.clone().text("a"), "a"),
                (a.clone().mods(Mods::CTRL), "\\x1b[97;5u"),
            ],
        );
    }

    #[test]
    fn refinement_flags_alone_keep_the_legacy_encoding() {
        let rows = [
            (key(Key::Char('a'), Mods::CTRL), "\\x01", "\\x01"),
            (key(Key::Escape, Mods::NONE), "\\x1b", "\\x1b"),
            (key(Key::Up, Mods::NONE), "\\x1b[A", "\\x1bOA"),
            (
                key(Key::Keypad(KeypadKey::Digit(1)), Mods::NONE),
                "1",
                "\\x1bOq",
            ),
            (
                key(Key::Char('a'), Mods::NONE)
                    .text("a")
                    .kind(KeyKind::Release),
                "",
                "",
            ),
            (
                key(Key::Char('a'), Mods::SHIFT)
                    .shifted('A')
                    .base_layout('q'),
                "",
                "",
            ),
        ];
        for flags in [A, T, A | T] {
            for (event, normal, app) in &rows {
                assert_eq!(
                    enc_with(event, flags, false, ALT_BOTH),
                    *normal,
                    "{flags:#b} {event:?}"
                );
                assert_eq!(
                    enc_with(event, flags, true, ALT_BOTH),
                    *app,
                    "{flags:#b} {event:?} app"
                );
            }
        }
    }

    #[test]
    fn keys_with_no_number() {
        for flags in [D, K, D | E | A | K | T] {
            check(
                flags,
                &[
                    (key(Key::F(0), Mods::NONE), ""),
                    (key(Key::F(36), Mods::CTRL), ""),
                    (key(Key::Keypad(KeypadKey::Digit(10)), Mods::CTRL), ""),
                ],
            );
        }
    }

    /// Every key the app can report, with the text macOS gives it unmodified.
    fn every_key() -> Vec<(Key, Option<&'static str>)> {
        let mut keys = vec![
            (Key::Char('a'), Some("a")),
            (Key::Char('1'), Some("1")),
            (Key::Char(' '), Some(" ")),
            (Key::Char('['), Some("[")),
            (Key::Char('é'), Some("é")),
            (Key::Enter, None),
            (Key::Tab, None),
            (Key::Backspace, None),
            (Key::Escape, None),
            (Key::Up, None),
            (Key::Down, None),
            (Key::Left, None),
            (Key::Right, None),
            (Key::Home, None),
            (Key::End, None),
            (Key::PageUp, None),
            (Key::PageDown, None),
            (Key::Insert, None),
            (Key::Delete, None),
            (Key::Menu, None),
            (Key::CapsLock, None),
            (Key::ShiftLeft, None),
            (Key::ShiftRight, None),
            (Key::ControlLeft, None),
            (Key::ControlRight, None),
            (Key::AltLeft, None),
            (Key::AltRight, None),
            (Key::SuperLeft, None),
            (Key::SuperRight, None),
            (Key::Keypad(KeypadKey::Enter), None),
        ];
        keys.extend((1..=35).map(|n| (Key::F(n), None)));
        let digits = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
        keys.extend((0..=9).map(|d| {
            (
                Key::Keypad(KeypadKey::Digit(d)),
                Some(digits[usize::from(d)]),
            )
        }));
        keys.extend([
            (Key::Keypad(KeypadKey::Decimal), Some(".")),
            (Key::Keypad(KeypadKey::Divide), Some("/")),
            (Key::Keypad(KeypadKey::Multiply), Some("*")),
            (Key::Keypad(KeypadKey::Subtract), Some("-")),
            (Key::Keypad(KeypadKey::Add), Some("+")),
            (Key::Keypad(KeypadKey::Equal), Some("=")),
        ]);
        keys
    }

    /// Every key under every combination of modifiers and event kind.
    fn every_event() -> Vec<KeyEvent> {
        let mut events = Vec::new();
        for (k, text) in every_key() {
            for bits in 0..16u8 {
                for locks in [Mods::NONE, Mods::CAPS_LOCK] {
                    let mods = Mods(bits) | locks;
                    for kind in [KeyKind::Press, KeyKind::Repeat, KeyKind::Release] {
                        let mut event = KeyEvent::new(k).mods(mods).kind(kind);
                        // macOS gives control characters, not text, with Ctrl.
                        if !mods.ctrl() {
                            event.text = text.map(str::to_owned);
                        }
                        events.push(event);
                    }
                }
            }
        }
        events
    }

    /// One of the spec's escape code forms: CSI parameters, then u, ~ or a
    /// letter.
    fn is_escape_code(bytes: &[u8]) -> bool {
        match bytes {
            [0x1b, b'[', params @ .., last] => {
                b"u~ABCDEFHPQS".contains(last)
                    && params
                        .iter()
                        .all(|b| b.is_ascii_digit() || *b == b';' || *b == b':')
            }
            _ => false,
        }
    }

    #[test]
    fn disambiguated_output_has_one_reading() {
        // The spec: with this flag every key event that does not type text is
        // CSI number;modifier u or CSI 1;modifier [~ABCDEFHPQS], except
        // Enter, Tab and Backspace, which keep their bytes.
        for flags in [D, D | E, D | A, D | E | A, D | T] {
            for event in every_event() {
                let bytes = encode_key(
                    &event,
                    Modes {
                        kitty_flags: flags,
                        ..Modes::default()
                    },
                    ALT_BOTH,
                );
                let held = event.mods.held();
                let plain_text = event
                    .text
                    .as_deref()
                    .filter(|_| !held.alt() && !held.ctrl() && !held.cmd());
                let legacy_byte = match (event.key, held.is_empty()) {
                    (Key::Enter, true) => Some(&b"\r"[..]),
                    (Key::Tab, true) => Some(&b"\t"[..]),
                    (Key::Backspace, true) => Some(&b"\x7f"[..]),
                    _ => None,
                };
                let fine = bytes.is_empty()
                    || is_escape_code(&bytes)
                    || plain_text.is_some_and(|t| t.as_bytes() == bytes)
                    || legacy_byte == Some(&bytes[..]);
                assert!(fine, "flags {flags:#b} {event:?} gave {}", show(&bytes));
                if event.kind == KeyKind::Release {
                    assert!(
                        plain_text.is_none_or(|text| text.as_bytes() != bytes),
                        "{event:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn report_all_keys_reports_every_key_as_a_code() {
        for flags in [K, K | E, K | T, D | E | A | K | T] {
            for event in every_event() {
                let bytes = encode_key(
                    &event,
                    Modes {
                        kitty_flags: flags,
                        ..Modes::default()
                    },
                    ALT_BOTH,
                );
                let silent = event.kind == KeyKind::Release && flags & E == 0;
                if silent {
                    assert_eq!(show(&bytes), "", "flags {flags:#b} {event:?}");
                } else {
                    assert!(
                        is_escape_code(&bytes),
                        "flags {flags:#b} {event:?} gave {}",
                        show(&bytes)
                    );
                }
            }
        }
    }
}
