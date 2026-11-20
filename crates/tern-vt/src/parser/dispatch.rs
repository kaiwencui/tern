//! Semantic dispatch for C0 controls, ESC sequences, CSI sequences, and OSC
//! commands. Unrecognized or malformed sequences are ignored.

use std::iter;

use super::Handler;
use super::params::{Iter, Params};
use crate::ansi::{
    Attr, CharsetIndex, ClearMode, Color, CursorShape, CursorStyle, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, NamedColor, NamedPrivateMode, PrivateMode,
    Rgb, StandardCharset, TabulationClearMode,
};

/// A C0 control. The others, and every C1 control, do nothing.
#[inline]
pub(super) fn execute<H: Handler>(handler: &mut H, byte: u8) {
    match byte {
        0x07 => handler.bell(),
        0x08 => handler.backspace(),
        0x09 => handler.put_tab(1),
        // LF, VT and FF all feed a line.
        0x0a..=0x0c => handler.linefeed(),
        0x0d => handler.carriage_return(),
        0x0e => handler.set_active_charset(CharsetIndex::G1),
        0x0f => handler.set_active_charset(CharsetIndex::G0),
        0x1a => handler.substitute(),
        _ => (),
    }
}

/// An escape sequence: ESC, its intermediate bytes, and `byte`. Only the
/// first two intermediates are kept, and none of these takes two.
pub(super) fn esc<H: Handler>(handler: &mut H, intermediates: &[u8], byte: u8) {
    // SCS: ESC, the set to designate, and what it is to be.
    let charset = match byte {
        b'B' => Some(StandardCharset::Ascii),
        b'0' => Some(StandardCharset::SpecialCharacterAndLineDrawing),
        _ => None,
    };
    if let Some(charset) = charset {
        let index = match intermediates {
            b"(" => CharsetIndex::G0,
            b")" => CharsetIndex::G1,
            b"*" => CharsetIndex::G2,
            b"+" => CharsetIndex::G3,
            _ => return,
        };
        return handler.configure_charset(index, charset);
    }
    match (intermediates, byte) {
        ([], b'D') => handler.linefeed(),
        ([], b'E') => {
            handler.linefeed();
            handler.carriage_return();
        },
        ([], b'H') => handler.set_horizontal_tabstop(),
        ([], b'M') => handler.reverse_index(),
        ([], b'Z') => handler.identify_terminal(None),
        ([], b'c') => handler.reset_state(),
        ([], b'7') => handler.save_cursor_position(),
        ([], b'8') => handler.restore_cursor_position(),
        (b"#", b'8') => handler.decaln(),
        ([], b'=') => handler.set_keypad_application_mode(),
        ([], b'>') => handler.unset_keypad_application_mode(),
        // ESC \ is ST, which ends a string and does nothing of its own.
        _ => (),
    }
}

/// The next parameter's first number, or `default` if there is none or it
/// is zero.
#[inline]
fn next_or(params: &mut Iter<'_>, default: u16) -> u16 {
    match params.next() {
        Some(&[number, ..]) if number != 0 => number,
        _ => default,
    }
}

/// A control sequence that ends in `action`, `preceding` being the last
/// character shown. Returns whether it began a synchronized update.
pub(super) fn csi<H: Handler>(
    handler: &mut H,
    params: &Params,
    intermediates: &[u8],
    action: u8,
    preceding: Option<char>,
) -> bool {
    let mut began = false;
    let mut params = params.iter();
    let p = &mut params;
    match (intermediates, action) {
        ([], b'@') => handler.insert_blank(usize::from(next_or(p, 1))),
        ([], b'A') => handler.move_up(usize::from(next_or(p, 1))),
        ([], b'B' | b'e') => handler.move_down(usize::from(next_or(p, 1))),
        // REP: the last character shown, again; nothing if none was.
        ([], b'b') => {
            if let Some(c) = preceding {
                for _ in 0..next_or(p, 1) {
                    handler.input(c);
                }
            }
        },
        ([], b'C' | b'a') => handler.move_forward(usize::from(next_or(p, 1))),
        // DA, with any intermediates; the first says which attributes.
        (_, b'c') if next_or(p, 0) == 0 => {
            handler.identify_terminal(intermediates.first().map(|&b| char::from(b)))
        },
        ([], b'D') => handler.move_backward(usize::from(next_or(p, 1))),
        ([], b'd') => handler.goto_line(i32::from(next_or(p, 1)) - 1),
        ([], b'E') => handler.move_down_and_cr(usize::from(next_or(p, 1))),
        ([], b'F') => handler.move_up_and_cr(usize::from(next_or(p, 1))),
        ([], b'G' | b'`') => handler.goto_col(usize::from(next_or(p, 1)) - 1),
        ([], b'g') => match next_or(p, 0) {
            0 => handler.clear_tabs(TabulationClearMode::Current),
            3 => handler.clear_tabs(TabulationClearMode::All),
            _ => (),
        },
        ([], b'H' | b'f') => {
            let line = i32::from(next_or(p, 1)) - 1;
            let column = usize::from(next_or(p, 1)) - 1;
            handler.goto(line, column);
        },
        // SM, RM, DECSET and DECRST take each parameter's first number,
        // and there is always at least one, zero if none was given.
        ([], b'h') => params.for_each(|param| handler.set_mode(Mode::from_raw(param[0]))),
        (b"?", b'h') => {
            for param in params {
                if param[0] == NamedPrivateMode::SyncUpdate as u16 {
                    began = true;
                }
                handler.set_private_mode(PrivateMode::from_raw(param[0]));
            }
        },
        ([], b'l') => params.for_each(|param| handler.unset_mode(Mode::from_raw(param[0]))),
        (b"?", b'l') => {
            params.for_each(|param| handler.unset_private_mode(PrivateMode::from_raw(param[0])))
        },
        ([], b'I') => handler.move_forward_tabs(next_or(p, 1)),
        ([], b'J') => {
            let mode = match next_or(p, 0) {
                0 => ClearMode::Below,
                1 => ClearMode::Above,
                2 => ClearMode::All,
                3 => ClearMode::Saved,
                _ => return began,
            };
            handler.clear_screen(mode);
        },
        ([], b'K') => {
            let mode = match next_or(p, 0) {
                0 => LineClearMode::Right,
                1 => LineClearMode::Left,
                2 => LineClearMode::All,
                _ => return began,
            };
            handler.clear_line(mode);
        },
        ([], b'L') => handler.insert_blank_lines(usize::from(next_or(p, 1))),
        ([], b'M') => handler.delete_lines(usize::from(next_or(p, 1))),
        ([], b'm') => sgr(handler, p),
        ([], b'n') => handler.device_status(usize::from(next_or(p, 0))),
        (b"?", b'n') if next_or(p, 0) == 996 => handler.report_color_scheme(),
        ([], b'P') => handler.delete_chars(usize::from(next_or(p, 1))),
        (b"$", b'p') => handler.report_mode(Mode::from_raw(next_or(p, 0))),
        (b"?$", b'p') => handler.report_private_mode(PrivateMode::from_raw(next_or(p, 0))),
        // XTVERSION (`CSI > q` / `CSI > 0 q`).
        (b">", b'q') if next_or(p, 0) == 0 => handler.report_terminal_version(),
        // DECSCUSR: odd numbers blink, and zero is the default.
        (b" ", b'q') => {
            let number = next_or(p, 0);
            let shape = match number {
                0 => None,
                1 | 2 => Some(CursorShape::Block),
                3 | 4 => Some(CursorShape::Underline),
                5 | 6 => Some(CursorShape::Beam),
                _ => return began,
            };
            let blinking = number % 2 == 1;
            handler.set_cursor_style(shape.map(|shape| CursorStyle { shape, blinking }));
        },
        ([], b'r') => {
            let top = usize::from(next_or(p, 1));
            let bottom = p.next().map(|param| usize::from(param[0])).filter(|&bottom| bottom != 0);
            handler.set_scrolling_region(top, bottom);
        },
        ([], b'S') => handler.scroll_up(usize::from(next_or(p, 1))),
        ([], b's') => handler.save_cursor_position(),
        ([], b'T') => handler.scroll_down(usize::from(next_or(p, 1))),
        ([], b't') => match next_or(p, 1) {
            14 => handler.text_area_size_pixels(),
            18 => handler.text_area_size_chars(),
            22 => handler.push_title(),
            23 => handler.pop_title(),
            _ => (),
        },
        ([], b'u') => handler.restore_cursor_position(),
        (b"?", b'u') => handler.report_keyboard_mode(),
        // The modes are a byte: higher bits of the number are dropped.
        (b"=", b'u') => {
            let modes = KeyboardModes::from_bits_truncate(next_or(p, 0) as u8);
            let apply = match next_or(p, 1) {
                2 => KeyboardModesApplyBehavior::Union,
                3 => KeyboardModesApplyBehavior::Difference,
                _ => KeyboardModesApplyBehavior::Replace,
            };
            handler.set_keyboard_mode(modes, apply);
        },
        (b">", b'u') => {
            handler.push_keyboard_mode(KeyboardModes::from_bits_truncate(next_or(p, 0) as u8))
        },
        (b"<", b'u') => handler.pop_keyboard_modes(next_or(p, 1)),
        ([], b'X') => handler.erase_chars(usize::from(next_or(p, 1))),
        ([], b'Z') => handler.move_backward_tabs(next_or(p, 1)),
        _ => (),
    }
    began
}

/// The sixteen colors of the palette, in order.
const PALETTE: [NamedColor; 16] = [
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::White,
    NamedColor::BrightBlack,
    NamedColor::BrightRed,
    NamedColor::BrightGreen,
    NamedColor::BrightYellow,
    NamedColor::BrightBlue,
    NamedColor::BrightMagenta,
    NamedColor::BrightCyan,
    NamedColor::BrightWhite,
];

/// SGR: each parameter changes the style, in order. One that is not
/// understood is passed over.
fn sgr<H: Handler>(handler: &mut H, params: &mut Iter<'_>) {
    while let Some(param) = params.next() {
        let attr = match *param {
            [code] => match code {
                // A color in the parameters that follow, which it takes up.
                38 => sgr_color(&mut params.by_ref().map(|param| param[0])).map(Attr::Foreground),
                48 => sgr_color(&mut params.by_ref().map(|param| param[0])).map(Attr::Background),
                58 => sgr_color(&mut params.by_ref().map(|param| param[0]))
                    .map(|color| Attr::UnderlineColor(Some(color))),
                code => plain_attr(code),
            },
            // The kind of underline, given as a subparameter.
            [4, kind] => Some(match kind {
                0 => Attr::CancelUnderline,
                2 => Attr::DoubleUnderline,
                3 => Attr::Undercurl,
                4 => Attr::DottedUnderline,
                5 => Attr::DashedUnderline,
                _ => Attr::Underline,
            }),
            [4, ..] => Some(Attr::Underline),
            // A color in subparameters.
            [38, ref color @ ..] => colon_color(color).map(Attr::Foreground),
            [48, ref color @ ..] => colon_color(color).map(Attr::Background),
            [58, ref color @ ..] => {
                colon_color(color).map(|color| Attr::UnderlineColor(Some(color)))
            },
            _ => None,
        };
        if let Some(attr) = attr {
            handler.terminal_attribute(attr);
        }
    }
}

/// What an SGR parameter of one number, and no color, asks for.
fn plain_attr(code: u16) -> Option<Attr> {
    let named = |base: u16| PALETTE[usize::from(code - base)];
    Some(match code {
        0 => Attr::Reset,
        1 => Attr::Bold,
        2 => Attr::Dim,
        3 => Attr::Italic,
        4 => Attr::Underline,
        5 => Attr::BlinkSlow,
        6 => Attr::BlinkFast,
        7 => Attr::Reverse,
        8 => Attr::Hidden,
        9 => Attr::Strike,
        21 => Attr::CancelBold,
        22 => Attr::CancelBoldDim,
        23 => Attr::CancelItalic,
        24 => Attr::CancelUnderline,
        25 => Attr::CancelBlink,
        27 => Attr::CancelReverse,
        28 => Attr::CancelHidden,
        29 => Attr::CancelStrike,
        30..=37 => Attr::Foreground(Color::Named(named(30))),
        39 => Attr::Foreground(Color::Named(NamedColor::Foreground)),
        40..=47 => Attr::Background(Color::Named(named(40))),
        49 => Attr::Background(Color::Named(NamedColor::Background)),
        59 => Attr::UnderlineColor(None),
        90..=97 => Attr::Foreground(Color::Named(named(90 - 8))),
        100..=107 => Attr::Background(Color::Named(named(100 - 8))),
        _ => return None,
    })
}

/// A color given in subparameters after 38, 48 or 58: its kind, then its
/// numbers. With more than four, the second is a color space, passed over.
fn colon_color(numbers: &[u16]) -> Option<Color> {
    let skip = if numbers.len() > 4 { 2 } else { 1 };
    sgr_color(&mut iter::once(numbers[0]).chain(numbers[skip..].iter().copied()))
}

/// A color given as its kind and numbers, taken from `numbers` only as far
/// as they are needed, and no further than the first that is wrong: 2 and
/// red, green and blue, or 5 and an index of the palette.
fn sgr_color(numbers: &mut dyn Iterator<Item = u16>) -> Option<Color> {
    let kind = numbers.next()?;
    let mut byte = || numbers.next().and_then(|number| u8::try_from(number).ok());
    match kind {
        2 => Some(Color::Spec(Rgb { r: byte()?, g: byte()?, b: byte()? })),
        5 => Some(Color::Indexed(byte()?)),
        _ => None,
    }
}

/// An operating system command, as the parameters between its `;`.
/// `bell` says whether BEL ended it, for a reply to end the same way.
pub(super) fn osc<H: Handler>(handler: &mut H, params: &[&[u8]], bell: bool) {
    let terminator = if bell { "\x07" } else { "\x1b\\" };
    let Some((&command, args)) = params.split_first() else { return };
    match command {
        // The title, from parameters that may themselves hold `;`. Those
        // that are not UTF-8 are left out.
        b"0" | b"2" if !args.is_empty() => {
            let parts: Vec<&str> = args.iter().filter_map(|arg| str::from_utf8(arg).ok()).collect();
            handler.set_title(Some(parts.join(";").trim().to_owned()));
        },
        // Colors of the palette, in pairs of an index and a color or `?`.
        b"4" if args.len() % 2 == 0 && !args.is_empty() => {
            for pair in args.chunks_exact(2) {
                let Some(index) = decimal(pair[0]) else { continue };
                if let Some(color) = parse_color(pair[1]) {
                    handler.set_color(usize::from(index), color);
                } else if pair[1] == b"?" {
                    let prefix = format!("4;{index}");
                    handler.dynamic_color_sequence(prefix, usize::from(index), terminator);
                }
            }
        },
        b"7" if !args.is_empty() => {
            let parts: Vec<&str> = args.iter().filter_map(|arg| str::from_utf8(arg).ok()).collect();
            handler.set_working_directory(parts.join(";"));
        },
        b"8" if args.len() >= 2 => hyperlink(handler, args),
        // ConEmu / Ghostty progress indicator (`OSC 9 ; 4 ; state [; pct]`)
        // and iTerm2 desktop notifications (`OSC 9 ; msg`).
        b"9" if !args.is_empty() => {
            if args[0] == b"4" {
                if let Some(&state) = args.get(1) {
                    let pct = args
                        .get(2)
                        .and_then(|p| decimal(p))
                        .unwrap_or(0)
                        .min(100);
                    let progress = match state {
                        b"0" => Some(crate::event::Progress::Hidden),
                        b"1" => Some(crate::event::Progress::Set(pct)),
                        b"2" => Some(crate::event::Progress::Error(pct)),
                        b"3" => Some(crate::event::Progress::Indeterminate),
                        b"4" => Some(crate::event::Progress::Pause(pct)),
                        _ => None,
                    };
                    if let Some(p) = progress {
                        handler.set_progress(p);
                    }
                }
            } else {
                let parts: Vec<&str> =
                    args.iter().filter_map(|arg| str::from_utf8(arg).ok()).collect();
                let body = parts.join(";");
                let trimmed = body.trim();
                if !trimmed.is_empty() {
                    handler.notify(None, trimmed.to_owned());
                }
            }
        },
        // The foreground, background and cursor colors, from the one named
        // on: each further parameter is the next color.
        b"10" | b"11" | b"12" if !args.is_empty() => {
            let first = if command == b"10" { 10 } else if command == b"11" { 11 } else { 12 };
            for (number, arg) in (first..=12u8).zip(args) {
                let index = NamedColor::Foreground as usize + usize::from(number - 10);
                if let Some(color) = parse_color(arg) {
                    handler.set_color(index, color);
                } else if *arg == b"?" {
                    handler.dynamic_color_sequence(number.to_string(), index, terminator);
                }
            }
        },
        // The cursor's shape, in the spelling of iTerm2 and Konsole.
        b"50" => {
            let shape = match args.first().and_then(|arg| arg.strip_prefix(b"CursorShape=")) {
                Some([b'0', ..]) => CursorShape::Block,
                Some([b'1', ..]) => CursorShape::Beam,
                Some([b'2', ..]) => CursorShape::Underline,
                _ => return,
            };
            handler.set_cursor_shape(shape);
        },
        // A clipboard, `c` if none is named: `?` asks for its text, and
        // anything else is text for it.
        b"52" if args.len() >= 2 => {
            let clipboard = args[0].first().copied().unwrap_or(b'c');
            match args[1] {
                b"?" => handler.clipboard_load(clipboard, terminator),
                text => handler.clipboard_store(clipboard, text),
            }
        },
        // Colors of the palette back to what they were: all of them if none
        // is named.
        b"104" => {
            if args.first().is_none_or(|arg| arg.is_empty()) {
                (0..256).for_each(|index| handler.reset_color(index));
            } else {
                args.iter()
                    .filter_map(|arg| decimal(arg))
                    .for_each(|index| handler.reset_color(usize::from(index)));
            }
        },
        b"110" => handler.reset_color(NamedColor::Foreground as usize),
        b"111" => handler.reset_color(NamedColor::Background as usize),
        b"112" => handler.reset_color(NamedColor::Cursor as usize),
        // FinalTerm / shell semantic prompt marks (`OSC 133 ; A|B|C|D`).
        b"133" if !args.is_empty() => {
            if let Some(&kind) = args[0].first()
                && matches!(kind, b'A' | b'B' | b'C' | b'D')
            {
                handler.prompt_mark(kind);
            }
        },
        // rxvt / Ghostty / WezTerm notification (`OSC 777 ; notify ; title ; body`).
        b"777" if args.len() >= 2 && args[0] == b"notify" => {
            if args.len() == 2 {
                if let Ok(body) = str::from_utf8(args[1]) {
                    let trimmed = body.trim();
                    if !trimmed.is_empty() {
                        handler.notify(None, trimmed.to_owned());
                    }
                }
            } else {
                let title = str::from_utf8(args[1])
                    .ok()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                let parts: Vec<&str> =
                    args[2..].iter().filter_map(|arg| str::from_utf8(arg).ok()).collect();
                let body = parts.join(";");
                let trimmed = body.trim();
                if !trimmed.is_empty() || title.is_some() {
                    handler.notify(title, trimmed.to_owned());
                }
            }
        },
        _ => (),
    }
}

/// OSC 8: `args` are the link's parameters, then its URI, which may hold
/// `;` of its own. An empty URI ends the link.
fn hyperlink<H: Handler>(handler: &mut H, args: &[&[u8]]) {
    let text = |part: &[u8]| str::from_utf8(part).unwrap_or_default().to_owned();
    let uri = args[1..].iter().map(|part| text(part)).collect::<Vec<_>>().join(";");
    if uri.is_empty() {
        return handler.set_hyperlink(None);
    }
    // The parameters are `key=value` pairs separated by `:`; only the first
    // `id` counts, and only if it is UTF-8.
    let id = args[0]
        .split(|&b| b == b':')
        .find_map(|pair| pair.strip_prefix(b"id="))
        .and_then(|id| str::from_utf8(id).ok())
        .map(str::to_owned);
    handler.set_hyperlink(Some(Hyperlink { id, uri }));
}

/// A number of a byte's size in decimal digits, or None.
fn decimal(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u8, |number, &digit| {
        if !digit.is_ascii_digit() {
            return None;
        }
        number.checked_mul(10)?.checked_add(digit - b'0')
    })
}

/// A color in one of the spellings of X11: `#` and three equal stretches of
/// hex digits, or `rgb:` and three stretches of one to four hex digits
/// separated by `/`.
fn parse_color(spec: &[u8]) -> Option<Rgb> {
    if let Some(digits) = spec.strip_prefix(b"#") {
        hash_color(digits)
    } else if let Some(parts) = spec.strip_prefix(b"rgb:") {
        rgb_color(parts)
    } else {
        None
    }
}

/// `#` colors: each third of the digits is one channel, of which the two
/// most significant hex digits count, and a single digit is the high one.
/// The last third takes what does not divide evenly.
fn hash_color(digits: &[u8]) -> Option<Rgb> {
    let third = digits.len() / 3;
    let channel = |part: &[u8]| -> Option<u8> {
        let value = usize::from_str_radix(str::from_utf8(part).ok()?, 16).ok()? << 4;
        // Use wrapping_shr so stretches of 17+ hex digits wrap cleanly in debug and release.
        let shift = (4 * part.len().saturating_sub(1)) as u32;
        Some(value.wrapping_shr(shift) as u8)
    };
    Some(Rgb {
        r: channel(&digits[..third])?,
        g: channel(&digits[third..2 * third])?,
        b: channel(&digits[2 * third..])?,
    })
}

/// `rgb:` colors: each channel is scaled from as many hex digits as it has
/// to a byte.
fn rgb_color(parts: &[u8]) -> Option<Rgb> {
    let parts: Vec<&str> = str::from_utf8(parts).ok()?.split('/').collect();
    let [r, g, b] = parts[..] else { return None };
    let channel = |digits: &str| -> Option<u8> {
        if digits.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        let most = 16u32.pow(digits.len() as u32) - 1;
        Some((255 * value / most) as u8)
    };
    Some(Rgb { r: channel(r)?, g: channel(g)?, b: channel(b)? })
}
