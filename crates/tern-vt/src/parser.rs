//! Tern's escape-sequence parser. It reads a program's output a byte at a
//! time and calls the terminal, through [Handler], for each character to
//! show and each control function to carry out.
//!
//! A UTF-8 character split across arbitrary PTY read boundaries is buffered
//! and decoded identically to the unsplit stream, so parser output never
//! depends on how incoming byte slices are chunked.
//!
//! Output inside a synchronized update (private mode 2026) is interpreted
//! immediately as it arrives rather than buffered, and the parser notes when
//! an update began for [Parser::take_began].
//!
//! The state machine follows Paul Williams' DEC ANSI parser
//! (`vt100.net/emu/dec_ansi_parser`) adapted for UTF-8 ground text, BEL- or
//! ST-terminated OSC strings, and 7-bit controls (C1 controls are ignored).

mod dispatch;
mod params;
#[cfg(test)]
mod tests;

use std::str;

use crate::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, PrivateMode, Rgb, StandardCharset,
    TabulationClearMode,
};
use crate::event::EventListener;
use crate::term::Term;
use params::Params;

/// Terminal callbacks invoked by the parser; each defaults to a no-op.
pub(crate) trait Handler {
    /// OSC 0 or 2: the window's title.
    fn set_title(&mut self, _title: Option<String>) {}

    /// DECSCUSR: the cursor's shape and blinking, or None for the default.
    fn set_cursor_style(&mut self, _style: Option<CursorStyle>) {}

    /// OSC 50: the cursor's shape alone.
    fn set_cursor_shape(&mut self, _shape: CursorShape) {}

    /// A character to show.
    fn input(&mut self, _c: char) {}

    /// CUP and HVP: the cursor to a line and a column, both from zero.
    fn goto(&mut self, _line: i32, _column: usize) {}

    /// VPA: the cursor to a line, from zero.
    fn goto_line(&mut self, _line: i32) {}

    /// CHA and HPA: the cursor to a column, from zero.
    fn goto_col(&mut self, _column: usize) {}

    /// ICH: blank cells inserted at the cursor.
    fn insert_blank(&mut self, _count: usize) {}

    /// CUU: the cursor up.
    fn move_up(&mut self, _lines: usize) {}

    /// CUD and VPR: the cursor down.
    fn move_down(&mut self, _lines: usize) {}

    /// DA and DECID: the terminal says what it is, primary attributes for
    /// None and secondary for `>`.
    fn identify_terminal(&mut self, _intermediate: Option<char>) {}

    /// DSR: a report of the terminal's status or the cursor's place.
    fn device_status(&mut self, _report: usize) {}

    /// CUF and HPR: the cursor right.
    fn move_forward(&mut self, _columns: usize) {}

    /// CUB: the cursor left.
    fn move_backward(&mut self, _columns: usize) {}

    /// CNL: the cursor down, to the first column.
    fn move_down_and_cr(&mut self, _lines: usize) {}

    /// CPL: the cursor up, to the first column.
    fn move_up_and_cr(&mut self, _lines: usize) {}

    /// HT: the cursor to the next tab stop, `count` times.
    fn put_tab(&mut self, _count: u16) {}

    /// BS: the cursor one column left.
    fn backspace(&mut self) {}

    /// CR: the cursor to the first column.
    fn carriage_return(&mut self) {}

    /// LF, VT, FF and IND: the cursor down a line, scrolling at the bottom.
    fn linefeed(&mut self) {}

    /// BEL.
    fn bell(&mut self) {}

    /// SUB: the parser has already cancelled the sequence.
    fn substitute(&mut self) {}

    /// HTS: a tab stop at the cursor's column.
    fn set_horizontal_tabstop(&mut self) {}

    /// SU: the scrolling region's lines up.
    fn scroll_up(&mut self, _lines: usize) {}

    /// SD: the scrolling region's lines down.
    fn scroll_down(&mut self, _lines: usize) {}

    /// IL: blank lines inserted at the cursor.
    fn insert_blank_lines(&mut self, _count: usize) {}

    /// DL: lines deleted at the cursor.
    fn delete_lines(&mut self, _count: usize) {}

    /// ECH: cells from the cursor on erased.
    fn erase_chars(&mut self, _count: usize) {}

    /// DCH: cells at the cursor deleted, the rest of the line moving left.
    fn delete_chars(&mut self, _count: usize) {}

    /// CBT: the cursor back `count` tab stops.
    fn move_backward_tabs(&mut self, _count: u16) {}

    /// CHT: the cursor on `count` tab stops.
    fn move_forward_tabs(&mut self, _count: u16) {}

    /// DECSC and SCOSC: the cursor saved.
    fn save_cursor_position(&mut self) {}

    /// DECRC and SCORC: the saved cursor restored.
    fn restore_cursor_position(&mut self) {}

    /// EL: part or all of the cursor's line erased.
    fn clear_line(&mut self, _mode: LineClearMode) {}

    /// ED: part or all of the screen erased, or the scrollback.
    fn clear_screen(&mut self, _mode: ClearMode) {}

    /// TBC: one tab stop cleared, or all.
    fn clear_tabs(&mut self, _mode: TabulationClearMode) {}

    /// RIS: the terminal reset.
    fn reset_state(&mut self) {}

    /// RI: the cursor up a line, scrolling at the top.
    fn reverse_index(&mut self) {}

    /// SGR: one change of the style text is written in.
    fn terminal_attribute(&mut self, _attr: Attr) {}

    /// SM.
    fn set_mode(&mut self, _mode: Mode) {}

    /// RM.
    fn unset_mode(&mut self, _mode: Mode) {}

    /// DECRQM: whether a mode is set.
    fn report_mode(&mut self, _mode: Mode) {}

    /// DECSET.
    fn set_private_mode(&mut self, _mode: PrivateMode) {}

    /// DECRST.
    fn unset_private_mode(&mut self, _mode: PrivateMode) {}

    /// DECRQM for a private mode.
    fn report_private_mode(&mut self, _mode: PrivateMode) {}

    /// DECSTBM: the scrolling region, its top and bottom lines from one.
    fn set_scrolling_region(&mut self, _top: usize, _bottom: Option<usize>) {}

    /// DECKPAM: the keypad sends application sequences.
    fn set_keypad_application_mode(&mut self) {}

    /// DECKPNM: the keypad sends what is printed on its keys.
    fn unset_keypad_application_mode(&mut self) {}

    /// SI and SO: which of G0 to G3 text is written in.
    fn set_active_charset(&mut self, _index: CharsetIndex) {}

    /// SCS: what one of G0 to G3 stands for.
    fn configure_charset(&mut self, _index: CharsetIndex, _charset: StandardCharset) {}

    /// OSC 4, 10, 11 and 12: a color of the palette, or one the terminal
    /// keeps for itself, set.
    fn set_color(&mut self, _index: usize, _color: Rgb) {}

    /// OSC 4, 10, 11 and 12 asked with `?`: a color reported, its reply
    /// beginning with `prefix` and ending with `terminator`.
    fn dynamic_color_sequence(&mut self, _prefix: String, _index: usize, _terminator: &str) {}

    /// OSC 104, 110, 111 and 112: a color back to what it was.
    fn reset_color(&mut self, _index: usize) {}

    /// OSC 52: text for a clipboard, in base64.
    fn clipboard_store(&mut self, _clipboard: u8, _base64: &[u8]) {}

    /// OSC 52 asked with `?`: a clipboard's text reported.
    fn clipboard_load(&mut self, _clipboard: u8, _terminator: &str) {}

    /// DECALN: the screen filled with E's.
    fn decaln(&mut self) {}

    /// XTWINOPS 22: the title saved.
    fn push_title(&mut self) {}

    /// XTWINOPS 23: the saved title restored.
    fn pop_title(&mut self) {}

    /// XTWINOPS 14: the size of the text area reported in pixels.
    fn text_area_size_pixels(&mut self) {}

    /// XTWINOPS 18: the size of the text area reported in cells.
    fn text_area_size_chars(&mut self) {}

    /// OSC 8: the link text is written with, or None for none.
    fn set_hyperlink(&mut self, _link: Option<Hyperlink>) {}

    /// The kitty keyboard modes in force reported.
    fn report_keyboard_mode(&mut self) {}

    /// Kitty keyboard modes pushed onto their stack.
    fn push_keyboard_mode(&mut self, _modes: KeyboardModes) {}

    /// `count` kitty keyboard modes popped from their stack.
    fn pop_keyboard_modes(&mut self, _count: u16) {}

    /// The kitty keyboard modes in force changed.
    fn set_keyboard_mode(&mut self, _modes: KeyboardModes, _apply: KeyboardModesApplyBehavior) {}

    /// OSC 7: the shell reported its current working directory URI.
    fn set_working_directory(&mut self, _uri: String) {}

    /// OSC 133: semantic shell prompt/command/output mark (`A`, `B`, `C`, `D`).
    fn prompt_mark(&mut self, _kind: u8) {}

    /// OSC 9 / OSC 777: user notification from a running command.
    fn notify(&mut self, _title: Option<String>, _body: String) {}

    /// OSC 9;4: progress indicator state (ConEmu / Windows Terminal / Ghostty).
    fn set_progress(&mut self, _progress: crate::event::Progress) {}
}

/// Where the parser is in the output. Ground is text; the others are the
/// parts of an escape sequence or a control string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Text, and controls among it.
    Ground,
    /// After ESC.
    Escape,
    /// After ESC and one or more bytes from 0x20 to 0x2F.
    EscapeIntermediate,
    /// After CSI (ESC [).
    CsiEntry,
    /// In a control sequence's parameters.
    CsiParam,
    /// In a control sequence's intermediate bytes.
    CsiIntermediate,
    /// In a control sequence that is malformed, up to its final byte.
    CsiIgnore,
    /// After DCS (ESC P).
    DcsEntry,
    /// In a device control string's parameters.
    DcsParam,
    /// In a device control string's intermediate bytes.
    DcsIntermediate,
    /// In a device control string's data, up to its end.
    DcsPassthrough,
    /// In a device control string that is malformed, up to its end.
    DcsIgnore,
    /// In an operating system command (ESC ]).
    OscString,
    /// In a start of string, privacy message or application program
    /// command (ESC X, ESC ^, ESC _), whose contents are thrown away.
    SosPmApcString,
}

/// A control sequence keeps at most two intermediate bytes, a prefix such
/// as `?` among them. One with more is not carried out.
const MAX_INTERMEDIATES: usize = 2;

/// An OSC keeps at most this many parameters; what comes after the
/// separator that would begin one more is lost.
const MAX_OSC_PARAMS: usize = 16;

/// The longest OSC that is carried out. A longer one, such as a clipboard
/// far bigger than anyone copies, is read to its end and dropped, so output
/// that never ends its OSC cannot take memory without end.
const MAX_OSC_LEN: usize = 8 << 20;

/// What the OSC buffer may keep of its capacity after a long one.
const OSC_KEEP: usize = 4 << 10;

/// The character shown in place of bytes that are not valid UTF-8.
const REPLACEMENT: char = '\u{fffd}';

/// The byte that begins every escape sequence.
const ESC: u8 = 0x1b;

/// The escape-sequence parser: the state it is in, and what it has
/// gathered of the sequence it is in the middle of.
pub(crate) struct Parser {
    state: State,
    /// A control sequence's parameters.
    params: Params,
    /// The intermediate bytes of an escape or control sequence: the first
    /// `intermediate_len`.
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_len: usize,
    /// The control sequence has gone past what is kept of it, and is not
    /// carried out.
    ignoring: bool,
    /// The OSC so far, without the `;` between its parameters, up to
    /// [MAX_OSC_LEN].
    osc: Vec<u8>,
    /// The OSC went past [MAX_OSC_LEN]: what there was of it is gone, and
    /// it is not carried out.
    osc_too_long: bool,
    /// Where each of the OSC's finished parameters ends in `osc`: the first
    /// `osc_count`.
    osc_ends: [usize; MAX_OSC_PARAMS],
    osc_count: usize,
    /// The start of a UTF-8 character that the output so far ended in: the
    /// first `utf8_len` bytes, always the start of a valid one.
    utf8: [u8; 4],
    utf8_len: usize,
    /// The last character shown, which REP (`CSI b`) repeats.
    preceding: Option<char>,
    /// A synchronized update began since [Parser::take_began] was last
    /// called.
    began: bool,
}

impl Parser {
    /// A parser in the ground state, which has shown nothing.
    pub(crate) fn new() -> Parser {
        Parser {
            state: State::Ground,
            params: Params::default(),
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_len: 0,
            ignoring: false,
            osc: Vec::new(),
            osc_too_long: false,
            osc_ends: [0; MAX_OSC_PARAMS],
            osc_count: 0,
            utf8: [0; 4],
            utf8_len: 0,
            preceding: None,
            began: false,
        }
    }

    /// Reads `bytes`, the next piece of output, calling `handler` for what
    /// they say. A sequence or character the piece ends in the middle of is
    /// kept and taken up by the next call.
    #[inline]
    pub(crate) fn advance<H: Handler>(&mut self, handler: &mut H, bytes: &[u8]) {
        let mut i = 0;
        if self.utf8_len != 0 {
            i = self.finish_char(handler, bytes);
        }
        while i < bytes.len() {
            i = match self.state {
                State::Ground => self.ground(handler, bytes, i),
                State::OscString => self.osc_string(handler, bytes, i),
                State::DcsPassthrough | State::DcsIgnore | State::SosPmApcString => {
                    self.skip_string(handler, bytes, i)
                },
                _ => {
                    self.step(handler, bytes[i]);
                    i + 1
                },
            };
        }
    }

    /// Reads a sequence, from the escape that begins it or from where the
    /// last piece left it, or the rest of a character the last piece ended
    /// in the middle of, and stops where the parser is back in the ground
    /// state: for a caller that takes what follows itself. Returns how many
    /// bytes it took, all of `bytes` if the sequence goes on past them.
    #[inline]
    pub(crate) fn advance_sequence<H: Handler>(&mut self, handler: &mut H, bytes: &[u8]) -> usize {
        let mut i = 0;
        if self.utf8_len != 0 {
            i = self.finish_char(handler, bytes);
        } else if self.state == State::Ground && bytes.first() == Some(&ESC) {
            self.begin_escape();
            i = 1;
        }
        while i < bytes.len() && self.state != State::Ground {
            i = match self.state {
                State::Ground => unreachable!("the loop ends in the ground state"),
                State::OscString => self.osc_string(handler, bytes, i),
                State::DcsPassthrough | State::DcsIgnore | State::SosPmApcString => {
                    self.skip_string(handler, bytes, i)
                },
                _ => {
                    self.step(handler, bytes[i]);
                    i + 1
                },
            };
        }
        i
    }

    /// Makes `c` the character REP repeats, for a caller that has shown
    /// text without the parser.
    pub(crate) fn set_preceding(&mut self, c: char) {
        self.preceding = Some(c);
    }

    /// Whether the parser is in the ground state and holds no part of a
    /// character: whether the next byte, if printable, is shown as it is.
    pub(crate) fn is_ground(&self) -> bool {
        self.state == State::Ground && self.utf8_len == 0
    }

    /// Whether a synchronized update began (`CSI ? 2026 h`, the mode alone
    /// or among others) since this was last called.
    pub(crate) fn take_began(&mut self) -> bool {
        std::mem::take(&mut self.began)
    }

    /// Reads text from `bytes[i]` on, up to the escape that ends it or the
    /// end of the piece, and returns where it stopped. Printable ASCII is
    /// shown byte by byte; other characters are decoded where they start.
    #[inline]
    fn ground<H: Handler>(&mut self, handler: &mut H, bytes: &[u8], mut i: usize) -> usize {
        // Kept here while the loop runs, and given back when it ends.
        let mut preceding = self.preceding;
        while let Some(&byte) = bytes.get(i) {
            match byte {
                // DEL (0x7f) is passed to `input` in ground state; Term::input ignores it.
                0x20..=0x7f => {
                    let c = byte as char;
                    handler.input(c);
                    preceding = Some(c);
                    i += 1;
                },
                ESC => {
                    self.preceding = preceding;
                    self.begin_escape();
                    return i + 1;
                },
                0x00..=0x1f => {
                    dispatch::execute(handler, byte);
                    i += 1;
                },
                _ => {
                    let decoded = decode(&bytes[i..]);
                    let len = match decoded {
                        Decoded::Char(_, len) | Decoded::Invalid(len) => len,
                        Decoded::Partial => {
                            // The piece ends inside the character: what
                            // there is of it waits for the next piece.
                            let rest = &bytes[i..];
                            self.utf8[..rest.len()].copy_from_slice(rest);
                            self.utf8_len = rest.len();
                            i = bytes.len();
                            break;
                        },
                    };
                    show(handler, &mut preceding, decoded, byte);
                    i += len;
                },
            }
        }
        self.preceding = preceding;
        i
    }

    /// Goes on with the character the last piece ended in the middle of,
    /// with the first bytes of `bytes`. Returns how many of them it took:
    /// none when the first does not go on with the character, which is
    /// then shown as invalid and the byte read afresh.
    fn finish_char<H: Handler>(&mut self, handler: &mut H, bytes: &[u8]) -> usize {
        let held = self.utf8_len;
        let taken = bytes.len().min(4 - held);
        let mut char_bytes = self.utf8;
        char_bytes[held..held + taken].copy_from_slice(&bytes[..taken]);
        let decoded = decode(&char_bytes[..held + taken]);
        let len = match decoded {
            Decoded::Char(_, len) | Decoded::Invalid(len) => len,
            Decoded::Partial => {
                self.utf8 = char_bytes;
                self.utf8_len = held + taken;
                return taken;
            },
        };
        self.utf8_len = 0;
        // What was held is the start of a valid character, so whatever ends
        // it ends no sooner than where the held bytes do.
        debug_assert!(len >= held);
        show(handler, &mut self.preceding, decoded, char_bytes[0]);
        len - held
    }

    /// Reads one byte in an escape sequence, a control sequence, or the
    /// start of a device control string.
    fn step<H: Handler>(&mut self, handler: &mut H, byte: u8) {
        // Three bytes mean the same wherever these states are: CAN and SUB
        // break the sequence off and are carried out, and ESC begins a new
        // one.
        match byte {
            0x18 | 0x1a => {
                dispatch::execute(handler, byte);
                self.state = State::Ground;
                return;
            },
            ESC => return self.begin_escape(),
            _ => (),
        }
        match self.state {
            State::Escape => self.escape(handler, byte),
            State::EscapeIntermediate => self.escape_intermediate(handler, byte),
            State::CsiEntry | State::CsiParam | State::CsiIntermediate | State::CsiIgnore => {
                self.csi(handler, byte)
            },
            State::DcsEntry | State::DcsParam | State::DcsIntermediate => self.dcs_head(byte),
            State::Ground | State::DcsPassthrough | State::DcsIgnore | State::OscString
            | State::SosPmApcString => unreachable!("read in bulk by advance"),
        }
    }

    /// A byte right after ESC.
    fn escape<H: Handler>(&mut self, handler: &mut H, byte: u8) {
        match byte {
            0x00..=0x1f => dispatch::execute(handler, byte),
            0x20..=0x2f => {
                self.collect(byte);
                self.state = State::EscapeIntermediate;
            },
            b'P' => {
                self.clear_sequence();
                self.state = State::DcsEntry;
            },
            b'[' => {
                self.clear_sequence();
                self.state = State::CsiEntry;
            },
            b']' => {
                self.osc.clear();
                self.osc_count = 0;
                self.osc_too_long = false;
                self.state = State::OscString;
            },
            b'X' | b'^' | b'_' => self.state = State::SosPmApcString,
            0x30..=0x7e => {
                dispatch::esc(handler, &[], byte);
                self.state = State::Ground;
            },
            // DEL and bytes above ASCII are passed over.
            _ => (),
        }
    }

    /// A byte after ESC and an intermediate byte.
    fn escape_intermediate<H: Handler>(&mut self, handler: &mut H, byte: u8) {
        match byte {
            0x00..=0x1f => dispatch::execute(handler, byte),
            0x20..=0x2f => self.collect(byte),
            0x30..=0x7e => {
                dispatch::esc(handler, &self.intermediates[..self.intermediate_len], byte);
                self.state = State::Ground;
            },
            _ => (),
        }
    }

    /// A byte of a control sequence, in any of its states.
    fn csi<H: Handler>(&mut self, handler: &mut H, byte: u8) {
        let state = self.state;
        match byte {
            // Controls are carried out in the middle of a sequence.
            0x00..=0x1f => dispatch::execute(handler, byte),
            0x40..=0x7e => {
                if state != State::CsiIgnore {
                    self.csi_final(handler, byte);
                }
                self.state = State::Ground;
            },
            _ if state == State::CsiIgnore => (),
            0x20..=0x2f => {
                self.collect(byte);
                self.state = State::CsiIntermediate;
            },
            // Parameter bytes after an intermediate make it malformed.
            0x30..=0x3f if state == State::CsiIntermediate => self.state = State::CsiIgnore,
            b'0'..=b'9' => {
                if self.params.is_full() {
                    self.ignoring = true;
                } else {
                    self.params.digit(byte - b'0');
                }
                self.state = State::CsiParam;
            },
            b':' | b';' => {
                if self.params.is_full() {
                    self.ignoring = true;
                } else {
                    self.params.end_number(byte == b';');
                }
                self.state = State::CsiParam;
            },
            // A private marker such as `?` counts as an intermediate, and
            // only first.
            0x3c..=0x3f => {
                if state == State::CsiEntry {
                    self.collect(byte);
                    self.state = State::CsiParam;
                } else {
                    self.state = State::CsiIgnore;
                }
            },
            // DEL and bytes above ASCII are passed over.
            _ => (),
        }
    }

    /// The final byte of a control sequence: the sequence is carried out,
    /// unless it went past what is kept of it.
    fn csi_final<H: Handler>(&mut self, handler: &mut H, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.end_number(true);
        }
        if !self.ignoring {
            let intermediates = &self.intermediates[..self.intermediate_len];
            self.began |= dispatch::csi(handler, &self.params, intermediates, byte, self.preceding);
        }
    }

    /// A byte of the start of a device control string, before its data.
    /// Nothing of the string reaches the terminal, but which state it is in
    /// decides whether a lone ST byte (0x9C) ends it.
    fn dcs_head(&mut self, byte: u8) {
        let state = self.state;
        self.state = match byte {
            0x40..=0x7e => State::DcsPassthrough,
            0x20..=0x2f => State::DcsIntermediate,
            0x30..=0x3f if state == State::DcsIntermediate => State::DcsIgnore,
            0x3c..=0x3f if state != State::DcsEntry => State::DcsIgnore,
            0x30..=0x3f => State::DcsParam,
            // Controls, DEL and bytes above ASCII are passed over.
            _ => state,
        };
    }

    /// Passes over the contents of a device control string, or of an SOS,
    /// PM or APC, from `bytes[i]` on, up to what ends it. Returns where it
    /// stopped.
    fn skip_string<H: Handler>(&mut self, handler: &mut H, bytes: &[u8], i: usize) -> usize {
        // A lone ST byte ends only the data of a device control string.
        let st_ends = self.state == State::DcsPassthrough;
        let rest = &bytes[i..];
        let found = rest
            .iter()
            .position(|&b| matches!(b, 0x18 | 0x1a | ESC) || (st_ends && b == 0x9c));
        let Some(at) = found else { return bytes.len() };
        match rest[at] {
            ESC => self.begin_escape(),
            0x9c => self.state = State::Ground,
            byte => {
                dispatch::execute(handler, byte);
                self.state = State::Ground;
            },
        }
        i + at + 1
    }

    /// Reads an OSC from `bytes[i]` on, up to what ends it or the end of the
    /// piece, and returns where it stopped.
    fn osc_string<H: Handler>(&mut self, handler: &mut H, bytes: &[u8], mut i: usize) -> usize {
        loop {
            // Everything but controls and `;` is part of the string, DEL and
            // bytes above ASCII included.
            let rest = &bytes[i..];
            let plain = rest.iter().position(|&b| b < 0x20 || b == b';').unwrap_or(rest.len());
            if !self.osc_too_long {
                if self.osc.len() + plain > MAX_OSC_LEN {
                    self.osc_too_long = true;
                    self.osc = Vec::new();
                } else {
                    self.osc.extend_from_slice(&rest[..plain]);
                }
            }
            i += plain;
            let Some(&byte) = bytes.get(i) else { return i };
            i += 1;
            match byte {
                b';' => self.osc_separator(),
                0x07 => {
                    self.osc_end(handler, true);
                    self.state = State::Ground;
                    return i;
                },
                // CAN and SUB end the string, which is carried out, and are
                // carried out after it.
                0x18 | 0x1a => {
                    self.osc_end(handler, false);
                    dispatch::execute(handler, byte);
                    self.state = State::Ground;
                    return i;
                },
                // ESC ends it as ST would, whatever follows the ESC.
                ESC => {
                    self.osc_end(handler, false);
                    self.begin_escape();
                    return i;
                },
                // Other controls are passed over.
                _ => (),
            }
        }
    }

    /// Ends the OSC parameter being read, unless there are as many as are
    /// kept.
    fn osc_separator(&mut self) {
        if self.osc_count < MAX_OSC_PARAMS && !self.osc_too_long {
            self.osc_ends[self.osc_count] = self.osc.len();
            self.osc_count += 1;
        }
    }

    /// Carries out the OSC read so far and forgets it. `bell` says whether
    /// BEL ended it, which is how the terminal ends its reply.
    fn osc_end<H: Handler>(&mut self, handler: &mut H, bell: bool) {
        if self.osc_too_long {
            self.osc_too_long = false;
            self.osc_count = 0;
            return;
        }
        self.osc_separator();
        let mut params: [&[u8]; MAX_OSC_PARAMS] = [&[]; MAX_OSC_PARAMS];
        let mut start = 0;
        for (param, &end) in params.iter_mut().zip(&self.osc_ends[..self.osc_count]) {
            *param = &self.osc[start..end];
            start = end;
        }
        dispatch::osc(handler, &params[..self.osc_count], bell);
        self.osc.clear();
        self.osc.shrink_to(OSC_KEEP);
        self.osc_count = 0;
    }

    /// Notes an intermediate byte, or that there are too many.
    fn collect(&mut self, byte: u8) {
        if self.intermediate_len == MAX_INTERMEDIATES {
            self.ignoring = true;
        } else {
            self.intermediates[self.intermediate_len] = byte;
            self.intermediate_len += 1;
        }
    }

    /// Forgets the sequence gathered so far.
    fn clear_sequence(&mut self) {
        self.intermediate_len = 0;
        self.ignoring = false;
        self.params.clear();
    }

    /// ESC: a new escape sequence begins, whatever came before.
    fn begin_escape(&mut self) {
        self.clear_sequence();
        self.state = State::Escape;
    }
}

/// What the bytes at the start of a slice are as UTF-8.
#[derive(Clone, Copy)]
enum Decoded {
    /// A character, and its length in bytes.
    Char(char, usize),
    /// Bytes that are not valid UTF-8 and are shown as one replacement
    /// character: as many as make the longest start of a valid character,
    /// or one.
    Invalid(usize),
    /// The start of a valid character, cut off by the end of the slice.
    Partial,
}

/// Decodes the character `bytes` starts with, whose first byte is above
/// ASCII. Valid characters are decoded inline; malformed prefixes fall back
/// to `std::str::from_utf8` so maximal-subpart error lengths follow UTF-8
/// standard rules.
#[inline]
fn decode(bytes: &[u8]) -> Decoded {
    let continues = |at: usize| bytes.get(at).is_some_and(|&b| b & 0xc0 == 0x80);
    let low = |at: usize| u32::from(bytes[at] & 0x3f);
    let first = u32::from(bytes[0]);
    let (code, len) = match bytes[0] {
        0xc2..=0xdf if continues(1) => ((first & 0x1f) << 6 | low(1), 2),
        0xe0..=0xef if continues(1) && continues(2) => {
            ((first & 0x0f) << 12 | low(1) << 6 | low(2), 3)
        },
        0xf0..=0xf4 if continues(1) && continues(2) && continues(3) => {
            ((first & 0x07) << 18 | low(1) << 12 | low(2) << 6 | low(3), 4)
        },
        _ => return decode_strictly(bytes),
    };
    // Too long a spelling of a smaller character, a surrogate, or past the
    // last character is invalid, and left to the strict decoder.
    let shortest = [0, 0, 0x80, 0x800, 0x1_0000][len];
    match char::from_u32(code) {
        Some(c) if code >= shortest => Decoded::Char(c, len),
        _ => decode_strictly(bytes),
    }
}

/// Decodes the character `bytes` starts with by the standard library's
/// rules for what is valid.
#[cold]
fn decode_strictly(bytes: &[u8]) -> Decoded {
    let bytes = &bytes[..bytes.len().min(4)];
    let valid = match str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if error.valid_up_to() > 0 => {
            // SAFETY: the standard library found these bytes valid.
            unsafe { str::from_utf8_unchecked(&bytes[..error.valid_up_to()]) }
        },
        Err(error) => {
            return match error.error_len() {
                Some(len) => Decoded::Invalid(len),
                None => Decoded::Partial,
            };
        },
    };
    let c = valid.chars().next().expect("a valid stretch is not empty");
    Decoded::Char(c, c.len_utf8())
}

/// Emits a decoded character or U+FFFD replacement: C1 controls (whether
/// UTF-8 U+0080..=U+009F or a lone 0x80..=0x9F byte) are ignored.
#[inline]
fn show<H: Handler>(handler: &mut H, preceding: &mut Option<char>, decoded: Decoded, first: u8) {
    let c = match decoded {
        Decoded::Char(c, _) if ('\u{80}'..='\u{9f}').contains(&c) => return,
        Decoded::Char(c, _) => c,
        Decoded::Invalid(1) if first <= 0x9f => return,
        Decoded::Invalid(_) => REPLACEMENT,
        Decoded::Partial => return,
    };
    handler.input(c);
    *preceding = Some(c);
}

/// The parser on its own, driving a terminal without the feeder in front of
/// it: for the parse benchmark (`examples/parse.rs`) and for tests outside
/// the crate (`tests/extras.rs`).
#[doc(hidden)]
pub struct BareParser(Parser);

impl Default for BareParser {
    fn default() -> BareParser {
        BareParser(Parser::new())
    }
}

impl BareParser {
    /// Reads a piece of output into `term`.
    pub fn advance<T: EventListener>(&mut self, term: &mut Term<T>, bytes: &[u8]) {
        self.0.advance(term, bytes);
    }

    /// Reads a piece of output into a handler that only adds the characters
    /// it is given to `count`: the cost of parsing alone.
    pub fn advance_counting(&mut self, count: &mut u64, bytes: &[u8]) {
        self.0.advance(&mut Count(count), bytes);
    }
}

/// A handler that counts the characters it is shown and does nothing else.
struct Count<'a>(&'a mut u64);

impl Handler for Count<'_> {
    #[inline]
    fn input(&mut self, _c: char) {
        *self.0 += 1;
    }
}
