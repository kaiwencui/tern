//! Packs the input-relevant terminal modes into one word so the parser thread
//! can publish them and the UI can read them without taking the terminal lock.

use tern_vt::engine::TermMode;
use tern_types::{Modes, MouseEncoding, MouseTracking};

const APP_CURSOR: u32 = 1 << 0;
const APP_KEYPAD: u32 = 1 << 1;
const BRACKETED_PASTE: u32 = 1 << 2;
const FOCUS_EVENTS: u32 = 1 << 3;
const ALT_SCREEN: u32 = 1 << 4;
const ALTERNATE_SCROLL: u32 = 1 << 5;
const LINE_FEED_NEW_LINE: u32 = 1 << 6;
const MOUSE_SHIFT: u32 = 8;
const ENCODING_SHIFT: u32 = 10;
const KITTY_SHIFT: u32 = 16;

pub(crate) fn pack(mode: TermMode) -> u32 {
    let bit = |m: TermMode, b: u32| if mode.contains(m) { b } else { 0 };
    let mouse: u32 = if mode.contains(TermMode::MOUSE_MOTION) {
        3
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        2
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        1
    } else {
        0
    };
    let encoding: u32 = if mode.contains(TermMode::SGR_MOUSE) {
        2
    } else if mode.contains(TermMode::UTF8_MOUSE) {
        1
    } else {
        0
    };
    let kitty = bit(TermMode::DISAMBIGUATE_ESC_CODES, 1)
        | bit(TermMode::REPORT_EVENT_TYPES, 2)
        | bit(TermMode::REPORT_ALTERNATE_KEYS, 4)
        | bit(TermMode::REPORT_ALL_KEYS_AS_ESC, 8)
        | bit(TermMode::REPORT_ASSOCIATED_TEXT, 16);
    bit(TermMode::APP_CURSOR, APP_CURSOR)
        | bit(TermMode::APP_KEYPAD, APP_KEYPAD)
        | bit(TermMode::BRACKETED_PASTE, BRACKETED_PASTE)
        | bit(TermMode::FOCUS_IN_OUT, FOCUS_EVENTS)
        | bit(TermMode::ALT_SCREEN, ALT_SCREEN)
        | bit(TermMode::ALTERNATE_SCROLL, ALTERNATE_SCROLL)
        | bit(TermMode::LINE_FEED_NEW_LINE, LINE_FEED_NEW_LINE)
        | mouse << MOUSE_SHIFT
        | encoding << ENCODING_SHIFT
        | kitty << KITTY_SHIFT
}

pub(crate) fn unpack(bits: u32) -> Modes {
    Modes {
        app_cursor: bits & APP_CURSOR != 0,
        app_keypad: bits & APP_KEYPAD != 0,
        bracketed_paste: bits & BRACKETED_PASTE != 0,
        focus_events: bits & FOCUS_EVENTS != 0,
        alt_screen: bits & ALT_SCREEN != 0,
        alternate_scroll: bits & ALTERNATE_SCROLL != 0,
        mouse: match (bits >> MOUSE_SHIFT) & 3 {
            1 => MouseTracking::Click,
            2 => MouseTracking::Drag,
            3 => MouseTracking::Motion,
            _ => MouseTracking::Off,
        },
        mouse_encoding: match (bits >> ENCODING_SHIFT) & 3 {
            1 => MouseEncoding::Utf8,
            2 => MouseEncoding::Sgr,
            _ => MouseEncoding::Default,
        },
        kitty_flags: ((bits >> KITTY_SHIFT) & 0xff) as u8,
        line_feed_new_line: bits & LINE_FEED_NEW_LINE != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_round_trips() {
        let m = unpack(pack(TermMode::default()));
        assert!(m.alternate_scroll);
        assert!(!m.app_cursor && !m.bracketed_paste && !m.alt_screen);
        assert_eq!(m.mouse, MouseTracking::Off);
        assert_eq!(m.mouse_encoding, MouseEncoding::Default);
        assert_eq!(m.kitty_flags, 0);
    }

    #[test]
    fn every_field_round_trips() {
        let mode = TermMode::APP_CURSOR
            | TermMode::APP_KEYPAD
            | TermMode::BRACKETED_PASTE
            | TermMode::FOCUS_IN_OUT
            | TermMode::ALT_SCREEN
            | TermMode::LINE_FEED_NEW_LINE
            | TermMode::MOUSE_DRAG
            | TermMode::MOUSE_REPORT_CLICK
            | TermMode::SGR_MOUSE
            | TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_ALL_KEYS_AS_ESC;
        let m = unpack(pack(mode));
        assert!(m.app_cursor && m.app_keypad && m.bracketed_paste && m.focus_events);
        assert!(m.alt_screen && m.line_feed_new_line && !m.alternate_scroll);
        assert_eq!(m.mouse, MouseTracking::Drag);
        assert_eq!(m.mouse_encoding, MouseEncoding::Sgr);
        assert_eq!(m.kitty_flags, 1 | 8);
    }

    #[test]
    fn motion_wins_over_click() {
        let m = unpack(pack(
            TermMode::MOUSE_MOTION | TermMode::MOUSE_REPORT_CLICK | TermMode::UTF8_MOUSE,
        ));
        assert_eq!(m.mouse, MouseTracking::Motion);
        assert_eq!(m.mouse_encoding, MouseEncoding::Utf8);
    }
}
