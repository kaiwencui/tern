//! Key events and the choice between the legacy (xterm) and kitty encodings.

use tern_types::{Key, KeypadKey, Modes, Mods};

use crate::{kitty, legacy};

/// Whether a key went down, repeated while held, or came up.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum KeyKind {
    #[default]
    Press,
    Repeat,
    Release,
}

/// Which Option keys act as Alt (Meta, sending ESC before the key) rather
/// than typing the layout's special characters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OptionAsAlt {
    /// Option types characters, as everywhere else on macOS: Option+e starts
    /// an accent, Option+a types å.
    #[default]
    None,
    Left,
    Right,
    Both,
}

/// User settings that change key encoding.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KeyOptions {
    pub option_as_alt: OptionAsAlt,
}

/// One key event as the app reads it from the OS.
///
/// Do not call [`encode_key`] for a key the input method swallowed while
/// composing (a dead key, a key typed into marked text). A dead key's accent
/// arrives with the next key, as that key's `text` (`NSEvent.characters`
/// already holds the accented letter). Text an input method composed from
/// several keys, such as a CJK conversion, goes through [`encode_text`]
/// instead: the key that committed it did not type it, and under the kitty
/// protocol's report-all-keys flag that key's event would report the key
/// rather than the text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub key: Key,
    /// Every modifier in effect, lock keys included. For a modifier key's own
    /// event, the state after the event (what `NSEvent.modifierFlags`
    /// reports in flagsChanged), so releasing one Shift while the other is held
    /// still has Shift set.
    pub mods: Mods,
    pub kind: KeyKind,
    /// What the OS says the key types with the current modifiers, after dead
    /// keys and input methods. None for keys that type nothing. Text that
    /// starts with a control character counts as nothing, and so does
    /// AppKit's private-use character for a function key (U+F700 to U+F8FF)
    /// on a key that types no character.
    pub text: Option<String>,
    /// The character this key types with Shift on the current layout, when
    /// known.
    pub shifted: Option<char>,
    /// The character on the same physical key in the US layout, when known.
    /// Lets Ctrl+С on a Russian layout send ^C, and is reported to programs
    /// that ask for alternate keys.
    pub base_layout: Option<char>,
    /// The key is on the numeric keypad. Implied by [`Key::Keypad`]; set it
    /// also when the app reports a keypad key as [`Key::Enter`] or as a
    /// [`Key::Char`] digit or operator. Ignored on other keys, so it may
    /// come straight from `NSEventModifierFlagNumericPad`, which macOS also
    /// sets on the arrow keys.
    pub keypad: bool,
    /// Which Option key is down (the device-dependent bits of
    /// `NSEvent.modifierFlags`), for [`OptionAsAlt::Left`] and
    /// [`OptionAsAlt::Right`].
    pub left_option: bool,
    pub right_option: bool,
}

/// The bytes to send for a key event; empty when the key sends nothing.
///
/// With [`Modes::kitty_flags`] at zero (or only the flags that refine other
/// flags' escape codes) this is xterm's encoding; otherwise the kitty
/// keyboard protocol's.
pub fn encode_key(event: &KeyEvent, modes: Modes, options: KeyOptions) -> Vec<u8> {
    let key = Resolved::new(event, options);
    if let Some(text) = key.text {
        // Enter or an arrow that arrives with text is an input method
        // committing what it composed: the key itself was spent on that.
        if !types_char(key.key) && key.kind != KeyKind::Release {
            return encode_text(text, modes);
        }
    }
    if kitty::active(modes.kitty_flags) {
        kitty::encode(&key, modes.kitty_flags)
    } else {
        legacy::encode(&key, modes)
    }
}

/// The bytes for text an input method committed rather than a key typed,
/// such as a finished CJK conversion.
///
/// Under the kitty protocol's report-all-keys plus associated-text flags this
/// is the protocol's key-less text event (CSI 0;;codepoints u), which
/// carries no control characters and so is empty when the text has nothing
/// else; everywhere else the text goes as UTF-8, as typed text always does.
pub fn encode_text(text: &str, modes: Modes) -> Vec<u8> {
    if kitty::reports_text(modes.kitty_flags) {
        kitty::text_event(text)
    } else {
        text.as_bytes().to_vec()
    }
}

/// A key event after deciding what Option means for it and whether the OS
/// text counts.
pub(crate) struct Resolved<'a> {
    /// The key, with keypad digits, operators and Enter as [`Key::Keypad`].
    pub key: Key,
    /// The modifiers still in play: Alt is gone when the OS spent Option on
    /// composing the text.
    pub mods: Mods,
    pub kind: KeyKind,
    /// What the key types; None when it types nothing, or when Option acts
    /// as Alt and the OS text was typed with Option as a character modifier.
    pub text: Option<&'a str>,
    pub shifted: Option<char>,
    pub base_layout: Option<char>,
}

impl<'a> Resolved<'a> {
    pub(crate) fn new(event: &'a KeyEvent, options: KeyOptions) -> Resolved<'a> {
        let key = keypad_form(event.key, event.keypad);
        let option_is_alt = match options.option_as_alt {
            OptionAsAlt::None => false,
            OptionAsAlt::Left => event.left_option,
            OptionAsAlt::Right => event.right_option,
            OptionAsAlt::Both => true,
        };
        let mut text = event.text.as_deref().filter(|t| is_text(t, key));
        let mut mods = event.mods;
        if mods.alt() {
            if option_is_alt {
                // The OS read Option as a character modifier (Option+a gave
                // å), but here it means Alt and the plain key.
                text = None;
            } else if text.is_some() {
                // The OS spent Option on the text, so the program sees text,
                // not an Alt combination. When Option typed nothing (Option
                // with an arrow, or with Ctrl), it still counts as Alt.
                mods = mods.without(Mods::ALT);
            }
        }
        Resolved {
            key,
            mods,
            kind: event.kind,
            text,
            shifted: event.shifted,
            base_layout: event.base_layout,
        }
    }

    /// The character Ctrl and Alt apply to for a key whose base character is
    /// c: Shift and Caps Lock applied, as xterm does. When that is not ASCII
    /// and the US key at the same spot is, the US key is used instead, as
    /// kitty does, so Ctrl+С on a Russian layout still sends ^C.
    pub(crate) fn modified_char(&self, c: char) -> char {
        let shift = self.mods.shift();
        let upper = shift || self.mods.contains(Mods::CAPS_LOCK);
        let typed = match self.shifted {
            Some(shifted) if shift => shifted,
            _ if upper => uppercase(c),
            _ => c,
        };
        match self.base_layout {
            Some(us) if !typed.is_ascii() && us.is_ascii() => {
                if upper {
                    us.to_ascii_uppercase()
                } else {
                    us
                }
            }
            _ => typed,
        }
    }
}

/// Whether the key types a character, as opposed to Enter, arrows and the
/// other keys that send codes.
fn types_char(key: Key) -> bool {
    match key {
        Key::Char(_) => true,
        Key::Keypad(k) => k != KeypadKey::Enter,
        _ => false,
    }
}

fn is_text(text: &str, key: Key) -> bool {
    match text.chars().next() {
        None => false,
        Some(c) if c.is_control() => false,
        // AppKit's characters for arrows, function keys and the like.
        Some('\u{F700}'..='\u{F8FF}') => types_char(key),
        Some(_) => true,
    }
}

fn uppercase(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// The key in the form the encoders use: keypad digits, operators and
/// Enter as [`Key::Keypad`]. The flag means nothing on other keys: a Mac
/// keypad has no navigation keys, and macOS sets its numeric-pad flag on the
/// arrows too.
fn keypad_form(key: Key, keypad: bool) -> Key {
    match key {
        Key::Enter if keypad => Key::Keypad(KeypadKey::Enter),
        Key::Char(c) if keypad => keypad_key(c).map_or(key, Key::Keypad),
        _ => key,
    }
}

/// The character a keypad key types; None for Enter and invalid digits.
pub(crate) fn keypad_char(key: KeypadKey) -> Option<char> {
    match key {
        KeypadKey::Digit(d) => char::from_digit(u32::from(d), 10),
        KeypadKey::Decimal => Some('.'),
        KeypadKey::Divide => Some('/'),
        KeypadKey::Multiply => Some('*'),
        KeypadKey::Subtract => Some('-'),
        KeypadKey::Add => Some('+'),
        KeypadKey::Equal => Some('='),
        KeypadKey::Enter => None,
    }
}

fn keypad_key(c: char) -> Option<KeypadKey> {
    Some(match c {
        '0'..='9' => KeypadKey::Digit(c as u8 - b'0'),
        '.' => KeypadKey::Decimal,
        '/' => KeypadKey::Divide,
        '*' => KeypadKey::Multiply,
        '-' => KeypadKey::Subtract,
        '+' => KeypadKey::Add,
        '=' => KeypadKey::Equal,
        _ => return None,
    })
}

/// Builders for test events.
#[cfg(test)]
impl KeyEvent {
    pub(crate) fn new(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            mods: Mods::NONE,
            kind: KeyKind::Press,
            text: None,
            shifted: None,
            base_layout: None,
            keypad: false,
            left_option: false,
            right_option: false,
        }
    }

    /// Alt comes from the left Option key unless the test says otherwise.
    pub(crate) fn mods(mut self, mods: Mods) -> KeyEvent {
        self.mods = mods;
        if mods.alt() && !self.left_option && !self.right_option {
            self.left_option = true;
        }
        self
    }

    pub(crate) fn text(mut self, text: &str) -> KeyEvent {
        self.text = Some(text.to_owned());
        self
    }

    pub(crate) fn shifted(mut self, c: char) -> KeyEvent {
        self.shifted = Some(c);
        self
    }

    pub(crate) fn base_layout(mut self, c: char) -> KeyEvent {
        self.base_layout = Some(c);
        self
    }

    pub(crate) fn kind(mut self, kind: KeyKind) -> KeyEvent {
        self.kind = kind;
        self
    }

    pub(crate) fn keypad(mut self) -> KeyEvent {
        self.keypad = true;
        self
    }

    pub(crate) fn right_option(mut self) -> KeyEvent {
        self.left_option = false;
        self.right_option = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::show;

    const ALT_BOTH: KeyOptions = KeyOptions {
        option_as_alt: OptionAsAlt::Both,
    };
    const ALT_NONE: KeyOptions = KeyOptions {
        option_as_alt: OptionAsAlt::None,
    };

    fn enc(event: &KeyEvent, options: KeyOptions) -> String {
        show(&encode_key(event, Modes::default(), options))
    }

    fn kitty(flags: u8) -> Modes {
        Modes {
            kitty_flags: flags,
            ..Modes::default()
        }
    }

    #[test]
    fn option_as_alt_picks_the_side() {
        // Option+a as macOS reports it: the text is å.
        let left = KeyEvent::new(Key::Char('a')).mods(Mods::ALT).text("å");
        let right = left.clone().right_option();
        let both = KeyEvent {
            left_option: true,
            right_option: true,
            ..left.clone()
        };
        let rows = [
            (&left, OptionAsAlt::None, "\\xc3\\xa5"),
            (&left, OptionAsAlt::Left, "\\x1ba"),
            (&left, OptionAsAlt::Right, "\\xc3\\xa5"),
            (&left, OptionAsAlt::Both, "\\x1ba"),
            (&right, OptionAsAlt::None, "\\xc3\\xa5"),
            (&right, OptionAsAlt::Left, "\\xc3\\xa5"),
            (&right, OptionAsAlt::Right, "\\x1ba"),
            (&right, OptionAsAlt::Both, "\\x1ba"),
            (&both, OptionAsAlt::Left, "\\x1ba"),
            (&both, OptionAsAlt::Right, "\\x1ba"),
        ];
        for (event, option_as_alt, want) in rows {
            let got = enc(event, KeyOptions { option_as_alt });
            assert_eq!(got, want, "{option_as_alt:?} with {event:?}");
        }
    }

    #[test]
    fn option_without_alt_sends_the_os_text() {
        // Option+e on the US layout types the accent the OS composes.
        let accent = KeyEvent::new(Key::Char('e')).mods(Mods::ALT).text("´");
        assert_eq!(enc(&accent, ALT_NONE), "\\xc2\\xb4");
        let composed = KeyEvent::new(Key::Char('e')).text("é");
        assert_eq!(enc(&composed, ALT_NONE), "\\xc3\\xa9");
        // Option+Shift+k types the Apple logo, a private-use character.
        let apple = KeyEvent::new(Key::Char('k'))
            .mods(Mods::ALT | Mods::SHIFT)
            .text("\u{F8FF}");
        assert_eq!(enc(&apple, ALT_NONE), "\\xef\\xa3\\xbf");
        // Option+Space types a no-break space.
        let space = KeyEvent::new(Key::Char(' ')).mods(Mods::ALT).text("\u{a0}");
        assert_eq!(enc(&space, ALT_NONE), "\\xc2\\xa0");
    }

    #[test]
    fn option_that_types_nothing_still_counts_as_alt() {
        let rows = [
            (KeyEvent::new(Key::Left).mods(Mods::ALT), "\\x1b[1;3D"),
            (KeyEvent::new(Key::Backspace).mods(Mods::ALT), "\\x1b\\x7f"),
            (KeyEvent::new(Key::Enter).mods(Mods::ALT), "\\x1b\\r"),
            // Ctrl+Option+a: macOS gives the control character, not text.
            (
                KeyEvent::new(Key::Char('a'))
                    .mods(Mods::ALT | Mods::CTRL)
                    .text("\u{1}"),
                "\\x1b\\x01",
            ),
        ];
        for (event, want) in rows {
            assert_eq!(enc(&event, ALT_NONE), want, "{event:?}");
        }
    }

    #[test]
    fn option_as_alt_uses_the_key_not_the_os_text() {
        let rows = [
            (
                KeyEvent::new(Key::Char('a')).mods(Mods::ALT).text("å"),
                "\\x1ba",
            ),
            (
                KeyEvent::new(Key::Char('a'))
                    .mods(Mods::ALT | Mods::SHIFT)
                    .text("Å")
                    .shifted('A'),
                "\\x1bA",
            ),
            (
                KeyEvent::new(Key::Char('1'))
                    .mods(Mods::ALT | Mods::SHIFT)
                    .text("⁄")
                    .shifted('!'),
                "\\x1b!",
            ),
            (KeyEvent::new(Key::Char('e')).mods(Mods::ALT), "\\x1be"),
            // Caps Lock makes letters uppercase, as typing them would.
            (
                KeyEvent::new(Key::Char('a'))
                    .mods(Mods::ALT | Mods::CAPS_LOCK)
                    .text("å"),
                "\\x1bA",
            ),
        ];
        for (event, want) in rows {
            assert_eq!(enc(&event, ALT_BOTH), want, "{event:?}");
        }
    }

    #[test]
    fn non_latin_layouts_fall_back_to_the_us_key_for_ctrl_and_alt() {
        let es = KeyEvent::new(Key::Char('с')).base_layout('c');
        let rows = [
            (es.clone().text("с"), "\\xd1\\x81"),
            (es.clone().mods(Mods::CTRL).text("\u{3}"), "\\x03"),
            (
                es.clone().mods(Mods::CTRL | Mods::SHIFT).shifted('С'),
                "\\x03",
            ),
            (es.clone().mods(Mods::ALT), "\\x1bc"),
            (
                es.clone().mods(Mods::ALT | Mods::SHIFT).shifted('С'),
                "\\x1bC",
            ),
            // Without a US key to fall back on, the layout's character goes.
            (
                KeyEvent::new(Key::Char('с')).mods(Mods::ALT),
                "\\x1b\\xd1\\x81",
            ),
            (KeyEvent::new(Key::Char('с')).mods(Mods::CTRL), "\\xd1\\x81"),
            // An ASCII key keeps its own meaning even when the US key differs
            // (AZERTY's a sits where the US q is).
            (
                KeyEvent::new(Key::Char('a'))
                    .base_layout('q')
                    .mods(Mods::CTRL),
                "\\x01",
            ),
        ];
        for (event, want) in rows {
            assert_eq!(enc(&event, ALT_BOTH), want, "{event:?}");
        }
    }

    #[test]
    fn text_that_is_not_text() {
        let rows = [
            // Control characters are what macOS reports for Ctrl combinations.
            (KeyEvent::new(Key::Char('a')).text("\u{1}"), ""),
            (KeyEvent::new(Key::Char('a')).text(""), ""),
            // AppKit's private-use characters for arrows and function keys.
            (KeyEvent::new(Key::Up).text("\u{F700}"), "\\x1b[A"),
            (KeyEvent::new(Key::F(1)).text("\u{F704}"), "\\x1bOP"),
            (KeyEvent::new(Key::Enter).text("\r"), "\\r"),
            (KeyEvent::new(Key::Escape).text("\u{1b}"), "\\x1b"),
        ];
        for (event, want) in rows {
            assert_eq!(enc(&event, ALT_NONE), want, "{event:?}");
        }
    }

    #[test]
    fn input_method_commits_on_enter_send_the_text() {
        let commit = KeyEvent::new(Key::Enter).text("日本");
        assert_eq!(enc(&commit, ALT_NONE), "\\xe6\\x97\\xa5\\xe6\\x9c\\xac");
        let under_kitty = encode_key(&commit, kitty(0b11000), ALT_NONE);
        assert_eq!(show(&under_kitty), "\\x1b[0;;26085:26412u");
        let release = commit.clone().kind(KeyKind::Release);
        assert_eq!(
            show(&encode_key(&release, kitty(0b1010), ALT_NONE)),
            "\\x1b[13;1:3u"
        );
    }

    #[test]
    fn committed_text() {
        let rows = [
            (0, "\\xc3\\xa9"),
            (0b1, "\\xc3\\xa9"),
            (0b1000, "\\xc3\\xa9"),
            (0b10000, "\\xc3\\xa9"),
            (0b11000, "\\x1b[0;;233u"),
            (0b11111, "\\x1b[0;;233u"),
        ];
        for (flags, want) in rows {
            assert_eq!(
                show(&encode_text("é", kitty(flags))),
                want,
                "flags {flags:#b}"
            );
        }
        assert_eq!(
            show(&encode_text("a\u{7}b", kitty(0b11000))),
            "\\x1b[0;;97:98u"
        );
    }

    #[test]
    fn committed_text_with_nothing_printable_sends_nothing() {
        // CSI 0 u would be a key event for no key and no text.
        for flags in [0, 0b1000, 0b11000, 0b11111] {
            assert_eq!(show(&encode_text("", kitty(flags))), "", "{flags:#b}");
        }
        assert_eq!(show(&encode_text("\u{7}\u{1b}", kitty(0b11000))), "");
    }

    #[test]
    fn keypad_flag_on_navigation_keys_changes_nothing() {
        // macOS sets its numeric-pad modifier flag on the arrow keys, and a
        // Mac keypad has no navigation keys, so the flag must not turn an
        // arrow into the keypad's arrow (CSI 57419 u under the kitty
        // protocol), which programs do not take for an arrow.
        let keys = [
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Insert,
            Key::Delete,
        ];
        for k in keys {
            for flags in 0..32 {
                for app in [false, true] {
                    let modes = Modes {
                        kitty_flags: flags,
                        app_cursor: app,
                        app_keypad: app,
                        ..Modes::default()
                    };
                    for mods in [Mods::NONE, Mods::SHIFT, Mods::CTRL | Mods::ALT] {
                        for kind in [KeyKind::Press, KeyKind::Release] {
                            let plain = KeyEvent::new(k).mods(mods).kind(kind);
                            let flagged = plain.clone().keypad();
                            assert_eq!(
                                show(&encode_key(&flagged, modes, ALT_NONE)),
                                show(&encode_key(&plain, modes, ALT_NONE)),
                                "{k:?} {mods:?} {kind:?} flags {flags:#b} app {app}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn keypad_flag_marks_keypad_keys() {
        let app = Modes {
            app_keypad: true,
            ..Modes::default()
        };
        let rows = [
            (KeyEvent::new(Key::Char('5')).keypad().text("5"), "\\x1bOu"),
            (KeyEvent::new(Key::Enter).keypad(), "\\x1bOM"),
            (KeyEvent::new(Key::Char(',')).keypad().text(","), ","),
            (KeyEvent::new(Key::Up).keypad(), "\\x1b[A"),
        ];
        for (event, want) in rows {
            assert_eq!(show(&encode_key(&event, app, ALT_NONE)), want, "{event:?}");
        }
        let enter = KeyEvent::new(Key::Enter).keypad();
        assert_eq!(
            show(&encode_key(&enter, kitty(1), ALT_NONE)),
            "\\x1b[57414u"
        );
        let tab = KeyEvent::new(Key::Tab).keypad();
        assert_eq!(show(&encode_key(&tab, kitty(1), ALT_NONE)), "\\t");
    }
}
