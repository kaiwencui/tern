//! Mouse reports, and the wheel's arrow keys in full-screen programs that
//! have not taken the mouse.

use tern_types::{Key, Modes, Mods, MouseEncoding, MouseTracking};

use crate::key::{encode_key, KeyEvent, KeyKind, KeyOptions};
use crate::{push_char, push_decimal, CSI};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Press,
    Release,
    Motion,
    /// One wheel step; the button says which way.
    Wheel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// No button: motion with nothing held, or a release whose button is
    /// unknown.
    None,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// The button pressed or released; for motion, the button held; for the
    /// wheel, the direction.
    pub button: MouseButton,
    /// Zero-based cell column.
    pub col: u16,
    /// Zero-based cell row.
    pub row: u16,
    /// Shift, Alt and Ctrl are reported; the others are not.
    pub mods: Mods,
}

/// Whether the program has taken the mouse, so clicks and drags go to it
/// rather than to selection (the app lets Shift override this).
pub fn wants_mouse(modes: Modes) -> bool {
    modes.mouse != MouseTracking::Off
}

/// The report for a mouse event, or None when the program did not ask for
/// this kind of event or the position cannot be expressed in its encoding.
pub fn encode_mouse(event: MouseEvent, modes: Modes) -> Option<Vec<u8>> {
    let sgr = modes.mouse_encoding == MouseEncoding::Sgr;
    let button = match event.button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    };
    let code: u32 = match (event.kind, modes.mouse) {
        (_, MouseTracking::Off) => return None,
        (MouseKind::Press, _) => button?,
        (MouseKind::Release, _) => match event.button {
            // Only SGR can say which button came up; the older encodings
            // send 3 for any release.
            MouseButton::Left | MouseButton::Middle | MouseButton::Right if sgr => button?,
            MouseButton::Left | MouseButton::Middle | MouseButton::Right | MouseButton::None => 3,
            // Wheel steps have no release.
            _ => return None,
        },
        (MouseKind::Motion, MouseTracking::Click) => return None,
        (MouseKind::Motion, MouseTracking::Drag) => button? + 32,
        (MouseKind::Motion, MouseTracking::Motion) => match event.button {
            MouseButton::None => 3 + 32,
            _ => button? + 32,
        },
        (MouseKind::Wheel, _) => match event.button {
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
            _ => return None,
        },
    };
    let mods = event.mods;
    let code = code
        + if mods.shift() { 4 } else { 0 }
        + if mods.alt() { 8 } else { 0 }
        + if mods.ctrl() { 16 } else { 0 };
    let x = u32::from(event.col) + 1;
    let y = u32::from(event.row) + 1;
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(CSI);
    match modes.mouse_encoding {
        MouseEncoding::Default => {
            // One byte per value, offset by 32, so 223 is the last position.
            if x > 223 || y > 223 {
                return None;
            }
            out.push(b'M');
            out.extend([code, x, y].map(|v| (32 + v) as u8));
        }
        MouseEncoding::Utf8 => {
            // One character per value; xterm stops at two UTF-8 bytes.
            if x > 2015 || y > 2015 {
                return None;
            }
            out.push(b'M');
            for v in [code, x, y] {
                push_char(&mut out, char::from_u32(32 + v)?);
            }
        }
        MouseEncoding::Sgr => {
            out.push(b'<');
            push_decimal(&mut out, code);
            out.push(b';');
            push_decimal(&mut out, x);
            out.push(b';');
            push_decimal(&mut out, y);
            out.push(if event.kind == MouseKind::Release {
                b'm'
            } else {
                b'M'
            });
        }
    }
    Some(out)
}

/// The most arrows one call sends: more lines than any screen holds. It
/// stops a runaway delta (an infinite float saturates to i32::MAX when
/// cast) from freezing the app and flooding the program.
const MAX_SCROLL_LINES: u32 = 1_000;

/// The arrow keys to send for a wheel turn of lines (positive scrolls up,
/// toward older output) when the program is on the alternate screen with
/// alternate scroll on (mode 1007) and has not taken the mouse. Empty
/// otherwise.
///
/// Each line is an unmodified Up or Down press, encoded as the keyboard
/// would send it (SS3 in application cursor mode, CSI otherwise and under
/// the kitty protocol), followed by its release when the program asked the
/// kitty protocol for key releases. At most 1000 lines are sent.
pub fn alternate_scroll_bytes(lines: i32, modes: Modes) -> Vec<u8> {
    if lines == 0 || !modes.alt_screen || !modes.alternate_scroll || wants_mouse(modes) {
        return Vec::new();
    }
    let mut arrow = KeyEvent {
        key: if lines > 0 { Key::Up } else { Key::Down },
        mods: Mods::NONE,
        kind: KeyKind::Press,
        text: None,
        shifted: None,
        base_layout: None,
        keypad: false,
        left_option: false,
        right_option: false,
    };
    let mut one = encode_key(&arrow, modes, KeyOptions::default());
    arrow.kind = KeyKind::Release;
    one.extend(encode_key(&arrow, modes, KeyOptions::default()));
    one.repeat(lines.unsigned_abs().min(MAX_SCROLL_LINES) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::show;

    fn modes(mouse: MouseTracking, mouse_encoding: MouseEncoding) -> Modes {
        Modes {
            mouse,
            mouse_encoding,
            ..Modes::default()
        }
    }

    fn event(kind: MouseKind, button: MouseButton) -> MouseEvent {
        MouseEvent {
            kind,
            button,
            col: 0,
            row: 0,
            mods: Mods::NONE,
        }
    }

    fn enc(event: MouseEvent, modes: Modes) -> Option<String> {
        encode_mouse(event, modes).map(|b| show(&b))
    }

    const TRACKING: [MouseTracking; 3] = [
        MouseTracking::Click,
        MouseTracking::Drag,
        MouseTracking::Motion,
    ];
    const ENCODINGS: [MouseEncoding; 3] = [
        MouseEncoding::Default,
        MouseEncoding::Utf8,
        MouseEncoding::Sgr,
    ];

    #[test]
    fn wants_mouse_follows_tracking() {
        assert!(!wants_mouse(Modes::default()));
        for mouse in TRACKING {
            assert!(wants_mouse(Modes {
                mouse,
                ..Modes::default()
            }));
        }
    }

    #[test]
    fn nothing_when_tracking_is_off() {
        let kinds = [
            MouseKind::Press,
            MouseKind::Release,
            MouseKind::Motion,
            MouseKind::Wheel,
        ];
        let buttons = [
            MouseButton::Left,
            MouseButton::Middle,
            MouseButton::Right,
            MouseButton::None,
            MouseButton::WheelUp,
            MouseButton::WheelDown,
            MouseButton::WheelLeft,
            MouseButton::WheelRight,
        ];
        for encoding in ENCODINGS {
            for kind in kinds {
                for button in buttons {
                    let got = enc(event(kind, button), modes(MouseTracking::Off, encoding));
                    assert_eq!(got, None, "{kind:?} {button:?} {encoding:?}");
                }
            }
        }
    }

    /// Which events each tracking mode reports, as SGR codes.
    #[test]
    fn tracking_modes() {
        let rows: [(MouseKind, MouseButton, [Option<&str>; 3]); 14] = [
            (
                MouseKind::Press,
                MouseButton::Left,
                [Some("0"), Some("0"), Some("0")],
            ),
            (
                MouseKind::Press,
                MouseButton::Middle,
                [Some("1"), Some("1"), Some("1")],
            ),
            (
                MouseKind::Press,
                MouseButton::Right,
                [Some("2"), Some("2"), Some("2")],
            ),
            (
                MouseKind::Release,
                MouseButton::Left,
                [Some("0"), Some("0"), Some("0")],
            ),
            (
                MouseKind::Release,
                MouseButton::Right,
                [Some("2"), Some("2"), Some("2")],
            ),
            (
                MouseKind::Wheel,
                MouseButton::WheelUp,
                [Some("64"), Some("64"), Some("64")],
            ),
            (
                MouseKind::Wheel,
                MouseButton::WheelDown,
                [Some("65"), Some("65"), Some("65")],
            ),
            (
                MouseKind::Wheel,
                MouseButton::WheelLeft,
                [Some("66"), Some("66"), Some("66")],
            ),
            (
                MouseKind::Wheel,
                MouseButton::WheelRight,
                [Some("67"), Some("67"), Some("67")],
            ),
            (
                MouseKind::Motion,
                MouseButton::Left,
                [None, Some("32"), Some("32")],
            ),
            (
                MouseKind::Motion,
                MouseButton::Middle,
                [None, Some("33"), Some("33")],
            ),
            (
                MouseKind::Motion,
                MouseButton::Right,
                [None, Some("34"), Some("34")],
            ),
            (
                MouseKind::Motion,
                MouseButton::None,
                [None, None, Some("35")],
            ),
            (
                MouseKind::Release,
                MouseButton::None,
                [Some("3"), Some("3"), Some("3")],
            ),
        ];
        for (kind, button, wants) in rows {
            for (mouse, want) in TRACKING.into_iter().zip(wants) {
                let got = enc(event(kind, button), modes(mouse, MouseEncoding::Sgr));
                let last = if kind == MouseKind::Release { 'm' } else { 'M' };
                let want = want.map(|code| format!("\\x1b[<{code};1;1{last}"));
                assert_eq!(got, want, "{kind:?} {button:?} {mouse:?}");
            }
        }
    }

    #[test]
    fn meaningless_events_send_nothing() {
        let rows = [
            (MouseKind::Press, MouseButton::None),
            (MouseKind::Press, MouseButton::WheelUp),
            (MouseKind::Release, MouseButton::WheelDown),
            (MouseKind::Motion, MouseButton::WheelLeft),
            (MouseKind::Wheel, MouseButton::Left),
            (MouseKind::Wheel, MouseButton::None),
        ];
        for (kind, button) in rows {
            for encoding in ENCODINGS {
                for mouse in TRACKING {
                    assert_eq!(
                        enc(event(kind, button), modes(mouse, encoding)),
                        None,
                        "{kind:?} {button:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn default_encoding() {
        let m = modes(MouseTracking::Motion, MouseEncoding::Default);
        let at = |kind, button, col, row| MouseEvent {
            kind,
            button,
            col,
            row,
            mods: Mods::NONE,
        };
        let rows = [
            (
                at(MouseKind::Press, MouseButton::Left, 0, 0),
                Some("\\x1b[M !!"),
            ),
            (
                at(MouseKind::Press, MouseButton::Middle, 1, 2),
                Some("\\x1b[M!\\\"#"),
            ),
            (
                at(MouseKind::Press, MouseButton::Right, 9, 4),
                Some("\\x1b[M\\\"*%"),
            ),
            // Releases say 3, whichever button it was.
            (
                at(MouseKind::Release, MouseButton::Left, 0, 0),
                Some("\\x1b[M#!!"),
            ),
            (
                at(MouseKind::Release, MouseButton::Right, 0, 0),
                Some("\\x1b[M#!!"),
            ),
            (
                at(MouseKind::Motion, MouseButton::Left, 0, 0),
                Some("\\x1b[M@!!"),
            ),
            (
                at(MouseKind::Motion, MouseButton::None, 0, 0),
                Some("\\x1b[MC!!"),
            ),
            (
                at(MouseKind::Wheel, MouseButton::WheelUp, 0, 0),
                Some("\\x1b[M\u{60}!!"),
            ),
            (
                at(MouseKind::Wheel, MouseButton::WheelDown, 0, 0),
                Some("\\x1b[Ma!!"),
            ),
            (
                at(MouseKind::Wheel, MouseButton::WheelLeft, 0, 0),
                Some("\\x1b[Mb!!"),
            ),
            (
                at(MouseKind::Wheel, MouseButton::WheelRight, 0, 0),
                Some("\\x1b[Mc!!"),
            ),
            // Column and row 223 are the last that fit in a byte.
            (
                at(MouseKind::Press, MouseButton::Left, 222, 222),
                Some("\\x1b[M \\xff\\xff"),
            ),
            (at(MouseKind::Press, MouseButton::Left, 223, 0), None),
            (at(MouseKind::Press, MouseButton::Left, 0, 223), None),
            (at(MouseKind::Release, MouseButton::Left, 300, 0), None),
            (
                at(MouseKind::Press, MouseButton::Left, u16::MAX, u16::MAX),
                None,
            ),
        ];
        for (event, want) in rows {
            assert_eq!(enc(event, m), want.map(str::to_owned), "{event:?}");
        }
    }

    #[test]
    fn utf8_encoding() {
        let m = modes(MouseTracking::Motion, MouseEncoding::Utf8);
        let at = |col, row| MouseEvent {
            kind: MouseKind::Press,
            button: MouseButton::Left,
            col,
            row,
            mods: Mods::NONE,
        };
        let rows = [
            (at(0, 0), Some("\\x1b[M !!")),
            (at(94, 0), Some("\\x1b[M \\x7f!")),
            // Past 95 the values take two bytes.
            (at(95, 0), Some("\\x1b[M \\xc2\\x80!")),
            (at(222, 300), Some("\\x1b[M \\xc3\\xbf\\xc5\\x8d")),
            (at(2014, 2014), Some("\\x1b[M \\xdf\\xbf\\xdf\\xbf")),
            (at(2015, 0), None),
            (at(0, 2015), None),
        ];
        for (event, want) in rows {
            assert_eq!(enc(event, m), want.map(str::to_owned), "{event:?}");
        }
        let release = MouseEvent {
            kind: MouseKind::Release,
            ..at(0, 0)
        };
        assert_eq!(enc(release, m).as_deref(), Some("\\x1b[M#!!"));
        let wheel = MouseEvent {
            kind: MouseKind::Wheel,
            button: MouseButton::WheelDown,
            ..at(0, 0)
        };
        assert_eq!(enc(wheel, m).as_deref(), Some("\\x1b[Ma!!"));
    }

    #[test]
    fn sgr_encoding() {
        let m = modes(MouseTracking::Motion, MouseEncoding::Sgr);
        let at = |kind, button, col, row| MouseEvent {
            kind,
            button,
            col,
            row,
            mods: Mods::NONE,
        };
        let rows = [
            (
                at(MouseKind::Press, MouseButton::Left, 0, 0),
                "\\x1b[<0;1;1M",
            ),
            (
                at(MouseKind::Release, MouseButton::Left, 0, 0),
                "\\x1b[<0;1;1m",
            ),
            (
                at(MouseKind::Release, MouseButton::Middle, 4, 5),
                "\\x1b[<1;5;6m",
            ),
            (
                at(MouseKind::Motion, MouseButton::Right, 0, 0),
                "\\x1b[<34;1;1M",
            ),
            (
                at(MouseKind::Wheel, MouseButton::WheelUp, 7, 8),
                "\\x1b[<64;8;9M",
            ),
            // No limit on positions.
            (
                at(MouseKind::Press, MouseButton::Left, 1233, 5677),
                "\\x1b[<0;1234;5678M",
            ),
            (
                at(MouseKind::Press, MouseButton::Left, u16::MAX, u16::MAX),
                "\\x1b[<0;65536;65536M",
            ),
        ];
        for (event, want) in rows {
            assert_eq!(enc(event, m).as_deref(), Some(want), "{event:?}");
        }
    }

    #[test]
    fn modifier_bits() {
        let rows = [
            (Mods::SHIFT, 4),
            (Mods::ALT, 8),
            (Mods::CTRL, 16),
            (Mods::SHIFT | Mods::ALT | Mods::CTRL, 28),
            // Command and the lock keys are not reported.
            (Mods::SUPER | Mods::CAPS_LOCK | Mods::NUM_LOCK, 0),
            (Mods::CTRL | Mods::SUPER, 16),
        ];
        let kinds = [
            (MouseKind::Press, MouseButton::Right, 2, 'M'),
            (MouseKind::Release, MouseButton::Right, 2, 'm'),
            (MouseKind::Motion, MouseButton::Right, 34, 'M'),
            (MouseKind::Wheel, MouseButton::WheelDown, 65, 'M'),
        ];
        for (mods, bits) in rows {
            for (kind, button, code, last) in kinds {
                let event = MouseEvent {
                    kind,
                    button,
                    col: 0,
                    row: 0,
                    mods,
                };
                let sgr = enc(event, modes(MouseTracking::Motion, MouseEncoding::Sgr));
                assert_eq!(
                    sgr,
                    Some(format!("\\x1b[<{};1;1{last}", code + bits)),
                    "{mods:?} {kind:?}"
                );
                // The older encodings carry the same bits in their first byte.
                let raw = encode_mouse(event, modes(MouseTracking::Motion, MouseEncoding::Default));
                let base = if kind == MouseKind::Release { 3 } else { code };
                assert_eq!(
                    raw.map(|b| b[3]),
                    Some(32 + base + bits),
                    "{mods:?} {kind:?}"
                );
            }
        }
    }

    fn scroll_modes() -> Modes {
        Modes {
            alt_screen: true,
            alternate_scroll: true,
            ..Modes::default()
        }
    }

    #[test]
    fn alternate_scroll_is_bounded() {
        // A runaway delta (an infinite float saturates to i32::MAX when cast)
        // must not freeze the app sending billions of arrows.
        let on = scroll_modes();
        for lines in [5_000, -5_000, i32::MAX, i32::MIN] {
            let bytes = alternate_scroll_bytes(lines, on);
            assert_eq!(bytes.len(), 3 * 1_000, "{lines}");
        }
        assert_eq!(alternate_scroll_bytes(1_000, on).len(), 3 * 1_000);
        assert_eq!(alternate_scroll_bytes(999, on).len(), 3 * 999);
    }

    #[test]
    fn alternate_scroll() {
        let on = scroll_modes();
        let app = Modes {
            app_cursor: true,
            ..on
        };
        let rows = [
            (on, 1, "\\x1b[A"),
            (on, 3, "\\x1b[A\\x1b[A\\x1b[A"),
            (on, -2, "\\x1b[B\\x1b[B"),
            (on, 0, ""),
            (app, 2, "\\x1bOA\\x1bOA"),
            (app, -1, "\\x1bOB"),
            // The kitty protocol has no SS3 arrows.
            (
                Modes {
                    kitty_flags: 1,
                    ..app
                },
                1,
                "\\x1b[A",
            ),
            (
                Modes {
                    kitty_flags: 4,
                    ..app
                },
                1,
                "\\x1bOA",
            ),
            (
                Modes {
                    alt_screen: false,
                    ..on
                },
                1,
                "",
            ),
            (
                Modes {
                    alternate_scroll: false,
                    ..on
                },
                1,
                "",
            ),
            (
                Modes {
                    mouse: MouseTracking::Click,
                    ..on
                },
                1,
                "",
            ),
            // A program that asked for key releases gets one after each
            // arrow, so it never sees the arrow as held down.
            (
                Modes {
                    kitty_flags: 0b10,
                    ..app
                },
                1,
                "\\x1b[A\\x1b[1;1:3A",
            ),
            (
                Modes {
                    kitty_flags: 0b1011,
                    ..on
                },
                -2,
                "\\x1b[B\\x1b[1;1:3B\\x1b[B\\x1b[1;1:3B",
            ),
        ];
        for (modes, lines, want) in rows {
            assert_eq!(
                show(&alternate_scroll_bytes(lines, modes)),
                want,
                "{lines} {modes:?}"
            );
        }
    }
}
