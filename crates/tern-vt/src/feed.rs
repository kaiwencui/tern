//! A shortcut for plain text.
//!
//! The emulator places output one character at a time: the escape parser
//! decodes a character and calls `Term::input`, which looks up its width,
//! checks the modes and writes one cell. Most of what a terminal is given in
//! bulk is runs of printable ASCII, where every one of those answers is the
//! same from one character to the next. [Feeder] spots such runs and writes
//! their cells directly. It also carries out what comes between runs most
//! often: a line feed, a carriage return, and the commonest control
//! sequences in their usual spelling (colors and style, moving the cursor,
//! erasing in the line or the screen). So plain and colored text, and most
//! of what a full-screen program sends to redraw itself, pass without the
//! parser, which is handed everything else.
//!
//! It must leave the terminal exactly as the parser would have. Three things
//! make that hold:
//!
//! - It takes the shortcut only where the parser says it is in the ground
//!   state and holds no part of a character (`Parser::is_ground`), where a
//!   printable byte means "print this character" and nothing else. A
//!   sequence the parser reads through to its end, and then stops
//!   (`Parser::advance_sequence`), so that what follows can take the
//!   shortcut, and so that the beginning or end of a synchronized update,
//!   which is always a sequence, is seen right after it.
//! - Before each run it checks everything `Term::input` checks, and leaves
//!   the run to the parser whenever any answer is unusual: insert mode, a
//!   line-drawing character set, a pending wrap, a hyperlink or colored
//!   underline in force, or a wide character under the cursor.
//! - The parser remembers the last character written, for the "repeat"
//!   sequence (CSI b). A character written here is given to it
//!   (`Parser::set_preceding`) before it parses anything more.
//!
//! # Other than ASCII
//!
//! Text in other scripts takes the shortcut too. A character is decoded
//! here by the rules of UTF-8, and written to one cell or two as
//! `Term::input` writes it, if it is one that takes a cell of its own and
//! the cursor's row has room for it as it is. Every other case is left to
//! the parser one character at a time: a character that joins the one
//! before it (an accent, a variation selector), a wide one in the last
//! column, one that would overwrite half of a wide one, and whatever is not
//! valid UTF-8.
//!
//! # A character cut in two
//!
//! Output arrives in pieces of a kilobyte, so now and then a piece ends in
//! the middle of a UTF-8 character. The parser keeps the bytes it has and
//! goes on with them when the rest comes (`src/parser.rs`), so a character
//! cut in two reads as one however the output is cut. The parser is not in
//! the ground state while it holds them, so the feeder leaves the rest of
//! the character to it.
//!
//! # Synchronized updates
//!
//! A program can wrap a redraw in a synchronized update (private mode 2026),
//! so that the screen is not shown half redrawn. Rather than buffering raw
//! bytes after the beginning of an update and parsing them all when the end
//! arrives, output inside an update is interpreted immediately as it arrives,
//! like any other, and the terminal notes that an update is under way
//! (`Term::sync_update`); the session shows no screen taken while one is.
//! [Feeder::advance_to_turn] stops where an update begins or ends
//! when asked to, for the session to take the screen there.
//!
//! # Fast row writes
//!
//! Runs of plain ASCII and UTF-8 are written straight into the active row
//! through `Row::write_ascii` and `Row::text`, carry out color changes on the
//! cursor template in place, and scan the sequence after each run only once:
//! every nanosecond spent on a kilobyte counts, because the reading thread
//! interprets output itself only while that stays quicker than the program
//! printing it (`io::Way`).

use std::time::{Duration, Instant};

use crate::grid::{Charsets, Dimensions};
use crate::index::Column;
use crate::term::cell::{Cell, Flags};
use crate::term::TermMode;
use crate::ansi::{CharsetIndex, ClearMode, Color, LineClearMode, NamedColor, Rgb, StandardCharset};
use crate::parser::Parser;
use crate::Term;
use memchr::memchr;
use unicode_width::UnicodeWidthChar;

use crate::engine::Queue as Listener;

const ESC: u8 = 0x1b;

/// The sequences that begin and end a synchronized update, in their usual
/// spelling. Spelled otherwise (with other modes in the same sequence, say)
/// they are the parser's, and do the same.
const BEGIN_SYNC: &[u8; 8] = b"\x1b[?2026h";
const END_SYNC: &[u8; 8] = b"\x1b[?2026l";
/// The length of the shortest sequence that begins a synchronized update.
pub const BEGIN_SYNC_LEN: usize = BEGIN_SYNC.len();
/// How long an update may last. One that has not ended by then is ended, so
/// that a program that never ends its update cannot freeze the screen. The
/// time runs from the latest beginning.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

/// Where `Feeder::advance_to_turn` stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Turn {
    /// At the end of the output it was given.
    None,
    /// Right after the beginning of a synchronized update, with none under
    /// way before.
    Began,
    /// Right after the end of the synchronized update that was under way.
    Ended,
}

/// The shortest run of printable ASCII written directly. Below this the
/// parser's own loop is as quick, and cutting its input into more pieces
/// would cost more than the shortcut saves.
const MIN_RUN: usize = 16;

#[inline]
fn printable(byte: u8) -> bool {
    (0x20..=0x7e).contains(&byte)
}

/// How many printable ASCII bytes `bytes` starts with.
#[inline]
fn printable_run(bytes: &[u8]) -> usize {
    let mut n = 0;
    // Sixteen at a time without a branch per byte, which the compiler turns
    // into vector compares.
    for chunk in bytes.chunks_exact(16) {
        let other = chunk
            .iter()
            .fold(false, |other, &b| other | (b.wrapping_sub(0x20) > 0x5e));
        if other {
            break;
        }
        n += 16;
    }
    n + bytes[n..].iter().take_while(|&&b| printable(b)).count()
}

/// The index of the first escape at or after `from`, or the length. Text
/// between sequences is mostly a few bytes long, which a plain loop covers
/// sooner than a search set up for long hauls.
#[inline]
fn next_escape(bytes: &[u8], from: usize) -> usize {
    let near = bytes.len().min(from + 16);
    if let Some(at) = bytes[from..near].iter().position(|&b| b == ESC) {
        return from + at;
    }
    memchr(ESC, &bytes[near..]).map_or(bytes.len(), |at| near + at)
}

/// The end of the ground-state output from `start` on that goes to the
/// parser: at least one byte, and on up to the next escape or the next long
/// run the shortcut can take.
fn ground_share(bytes: &[u8], start: usize) -> usize {
    let end = next_escape(bytes, start);
    // Never a cut at `start` itself: the caller has decided that the parser
    // takes what is there.
    let from = start + 1;
    if end >= from + MIN_RUN
        && let Some(run) = long_run(&bytes[..end], from)
    {
        return run;
    }
    end.max(from)
}

/// Where a run of at least [MIN_RUN] printable ASCII bytes starts in
/// `bytes`, looking from `from` on, if one is found. Runs are looked for
/// sixteen bytes at a time, so one shorter than two such steps can be missed;
/// it then goes to the parser, which is merely slower. The run found never
/// starts right after a byte that could leave a character unfinished.
fn long_run(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while at + 16 <= bytes.len() {
        let other = bytes[at..at + 16]
            .iter()
            .fold(false, |other, &b| other | (b.wrapping_sub(0x20) > 0x5e));
        if other {
            at += 16;
            continue;
        }
        // All sixteen are printable: the run reaches back from here.
        let mut run = at;
        while run > from && printable(bytes[run - 1]) {
            run -= 1;
        }
        // The byte before decides what the run's first byte means to the
        // parser; after the start of a UTF-8 sequence it belongs to that.
        if run > 0 && bytes[run - 1] >= 0x80 {
            run += 1;
        }
        return Some(run);
    }
    None
}

/// After a line feed at the bottom of the screen: asks for the memory the
/// next scrolls will reuse. With a full history each new line takes over
/// what the oldest line held, last touched thousands of lines ago and long
/// gone from the processor's caches, and waiting for it then is a good
/// part of what a scroll costs.
#[inline]
fn warm_next_rows(term: &Term<Listener>) {
    /// How many scrolls ahead; far enough for the memory to arrive.
    const AHEAD: usize = 12;
    let grid = term.grid();
    if grid.cursor.point.line.0 + 1 == grid.screen_lines() as i32 {
        grid.prefetch_scroll(AHEAD);
    }
}

/// The length of a sequence that names a character set.
const DESIGNATION_LEN: usize = 3;

/// Reads a sequence that names one of the four character sets (G0 to G3)
/// as ASCII or as line drawing from the front of `bytes`: `ESC ( B` and the
/// like, which full-screen programs send with nearly every change of
/// style. None for anything else, the other character sets included: those
/// are the parser's, which ignores them.
#[inline]
fn designation(bytes: &[u8]) -> Option<(CharsetIndex, StandardCharset)> {
    let [_, set, charset, ..] = bytes else {
        return None;
    };
    let index = match set {
        b'(' => CharsetIndex::G0,
        b')' => CharsetIndex::G1,
        b'*' => CharsetIndex::G2,
        b'+' => CharsetIndex::G3,
        _ => return None,
    };
    let charset = match charset {
        b'B' => StandardCharset::Ascii,
        b'0' => StandardCharset::SpecialCharacterAndLineDrawing,
        _ => return None,
    };
    Some((index, charset))
}

/// The most parameters a control sequence may have to be carried out here;
/// the parser itself stops at 32.
const MAX_PARAMS: usize = 16;

/// Reads a control sequence (CSI) in its usual spelling from the front of
/// `bytes`: `ESC [`, numbers of at most four digits separated by semicolons,
/// and a final byte. Returns its length, how many parameters went into
/// `params` (at least one: a missing number is zero, as it is to the
/// parser), and the final byte. None for any other spelling (colons,
/// private markers, intermediate bytes, longer numbers, more parameters) and
/// for a sequence the bytes end in the middle of; those are the parser's.
#[inline]
fn usual_csi(bytes: &[u8], params: &mut [u16; MAX_PARAMS]) -> Option<(usize, usize, u8)> {
    if bytes.len() < 3 || bytes[0] != ESC || bytes[1] != b'[' {
        return None;
    }
    let (mut count, mut value, mut digits) = (0, 0u16, 0);
    for (at, &byte) in bytes.iter().enumerate().skip(2) {
        match byte {
            b'0'..=b'9' => {
                digits += 1;
                if digits > 4 {
                    return None;
                }
                value = value * 10 + u16::from(byte - b'0');
            }
            b';' | 0x40..=0x7e => {
                if count == MAX_PARAMS {
                    return None;
                }
                params[count] = value;
                count += 1;
                if byte != b';' {
                    return Some((at + 1, count, byte));
                }
                (value, digits) = (0, 0);
            }
            _ => return None,
        }
    }
    None
}

/// Whether a control sequence with this final byte, in its usual spelling,
/// is carried out by [apply_csi].
#[inline]
fn is_direct(last: u8) -> bool {
    matches!(
        last,
        b'm' | b'H' | b'f' | b'K' | b'J' | b'A' | b'B' | b'e' | b'C' | b'a' | b'D' | b'E' | b'F' | b'G' | b'`' | b'd'
    )
}

/// Carries out a control sequence for which [is_direct] holds, as the parser
/// does: the same call for the same parameters, a parameter that is missing
/// or zero standing for the same thing, and nothing done where the parser
/// does nothing.
///
/// Returns whether what [modes_allow] looks at may have changed, which only
/// a color-and-style sequence can bring about.
fn apply_csi(term: &mut Term<Listener>, params: &[u16], last: u8) -> bool {
    // The parser's own way of reading the next parameter.
    let mut rest = params.iter().copied();
    let mut next_or = |default: u16| match rest.next() {
        Some(value) if value != 0 => value,
        _ => default,
    };
    match last {
        b'm' => return apply_sgr(term, params),
        b'H' | b'f' => {
            let line = next_or(1) as i32;
            let column = next_or(1) as usize;
            term.goto(line - 1, column - 1);
        }
        b'K' => match next_or(0) {
            0 => term.clear_line(LineClearMode::Right),
            1 => term.clear_line(LineClearMode::Left),
            2 => term.clear_line(LineClearMode::All),
            _ => {}
        },
        b'J' => match next_or(0) {
            0 => term.clear_screen(ClearMode::Below),
            1 => term.clear_screen(ClearMode::Above),
            2 => term.clear_screen(ClearMode::All),
            3 => term.clear_screen(ClearMode::Saved),
            _ => {}
        },
        b'A' => term.move_up(next_or(1) as usize),
        b'B' | b'e' => term.move_down(next_or(1) as usize),
        b'C' | b'a' => term.move_forward(next_or(1) as usize),
        b'D' => term.move_backward(next_or(1) as usize),
        b'E' => term.move_down_and_cr(next_or(1) as usize),
        b'F' => term.move_up_and_cr(next_or(1) as usize),
        b'G' | b'`' => term.goto_col(next_or(1) as usize - 1),
        b'd' => term.goto_line(next_or(1) as i32 - 1),
        _ => unreachable!("not a sequence carried out here"),
    }
    false
}

/// Carries out a color-and-style sequence as the parser does: the same
/// change to the cursor's template for each parameter, the same parameters
/// taken by an extended color, the same ones skipped. The changes are made
/// here, to the template itself, and are the ones `Term::terminal_attribute`
/// makes; output with a change of color every few characters spends much of
/// its time on them. Returns whether the template may have gained or lost
/// what [modes_allow] looks for, an underline color.
///
/// Kept out of [apply_csi], so that the sequences that only move the cursor
/// do not pay for the room this needs.
#[inline(never)]
fn apply_sgr(term: &mut Term<Listener>, params: &[u16]) -> bool {
    const NAMED: [NamedColor; 16] = [
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
    let named = |index: u16| Color::Named(NAMED[usize::from(index)]);
    let template = &mut term.grid_mut().cursor.template;
    let mut extra = false;
    let mut rest = params.iter().copied();
    while let Some(param) = rest.next() {
        match param {
            0 => {
                template.set_fg(Color::Named(NamedColor::Foreground));
                template.set_bg(Color::Named(NamedColor::Background));
                template.flags = Flags::empty();
                if template.has_extra() {
                    template.set_underline_color(None);
                    extra = true;
                }
            }
            1 => template.flags.insert(Flags::BOLD),
            2 => template.flags.insert(Flags::DIM),
            3 => template.flags.insert(Flags::ITALIC),
            4 => {
                template.flags.remove(Flags::ALL_UNDERLINES);
                template.flags.insert(Flags::UNDERLINE);
            }
            7 => template.flags.insert(Flags::INVERSE),
            8 => template.flags.insert(Flags::HIDDEN),
            9 => template.flags.insert(Flags::STRIKEOUT),
            21 => template.flags.remove(Flags::BOLD),
            22 => template.flags.remove(Flags::BOLD | Flags::DIM),
            23 => template.flags.remove(Flags::ITALIC),
            24 => template.flags.remove(Flags::ALL_UNDERLINES),
            27 => template.flags.remove(Flags::INVERSE),
            28 => template.flags.remove(Flags::HIDDEN),
            29 => template.flags.remove(Flags::STRIKEOUT),
            30..=37 => template.set_fg(named(param - 30)),
            39 => template.set_fg(Color::Named(NamedColor::Foreground)),
            40..=47 => template.set_bg(named(param - 40)),
            49 => template.set_bg(Color::Named(NamedColor::Background)),
            59 => {
                template.set_underline_color(None);
                extra = true;
            }
            90..=97 => template.set_fg(named(param - 90 + 8)),
            100..=107 => template.set_bg(named(param - 100 + 8)),
            38 | 48 | 58 => match extended_color(&mut rest) {
                Some(color) if param == 38 => template.set_fg(color),
                Some(color) if param == 48 => template.set_bg(color),
                Some(color) => {
                    template.set_underline_color(Some(color));
                    extra = true;
                }
                None => {}
            },
            // Blinking (5, 6, 25) is nothing to the terminal, and neither
            // is a number that stands for nothing.
            _ => {}
        }
    }
    extra
}

/// The color after 38, 48 or 58: `5;index` or `2;red;green;blue`. As in the
/// parser, a parameter it looks at is used up even when the color comes to
/// nothing.
fn extended_color(rest: &mut impl Iterator<Item = u16>) -> Option<Color> {
    let mut byte = || u8::try_from(rest.next()?).ok();
    match byte()? {
        2 => Some(Color::Spec(Rgb {
            r: byte()?,
            g: byte()?,
            b: byte()?,
        })),
        5 => Some(Color::Indexed(byte()?)),
        _ => None,
    }
}

/// What [write_ascii] did with the bytes it was given.
enum Wrote {
    /// This many, from the front. Fewer than given when the row ended or a
    /// wide character was in the way.
    Cells(usize),
    /// None: the next character needs the parser (a pending wrap, or half of
    /// a wide character under the cursor), the ones after it may not.
    OneSlow,
    /// None, and none of this run can be: a mode or attribute in force needs
    /// the parser for every character.
    Not,
}

/// Whether the modes and attributes in force let characters be written
/// directly. Only the parser changes them, so the answer holds until the
/// parser is next given something other than printable ASCII.
fn modes_allow(term: &Term<Listener>) -> bool {
    let cursor = &term.grid().cursor;
    // With all four sets plain ASCII no shift can select another; a cell
    // extra (hyperlink, underline color) would have to be shared per cell.
    !term.mode().contains(TermMode::INSERT)
        && cursor.charsets == Charsets::default()
        && !cursor.template.has_extra()
}

/// Writes printable ASCII at the cursor as `Term::input` would, one cell a
/// byte, and moves the cursor past it. `allowed` is [modes_allow]'s answer,
/// asked once per stretch of direct writing.
fn write_ascii(term: &mut Term<Listener>, bytes: &[u8], allowed: &mut Option<bool>) -> Wrote {
    if !*allowed.get_or_insert_with(|| modes_allow(term)) {
        return Wrote::Not;
    }
    let grid = term.grid_mut();
    let columns = grid.columns();
    let cursor = &grid.cursor;
    if cursor.input_needs_wrap {
        return Wrote::OneSlow;
    }
    let line = cursor.point.line;
    let col = cursor.point.column.0;
    let style = Cell::styled(' ', &cursor.template);
    let want = bytes.len().min(columns - col);
    let row = &mut grid[line];
    // Overwriting half of a wide character clears its other half, which is
    // Term::input's to do. Looked up first, without marking cells as used.
    let plain = row[Column(col)..Column(col + want)]
        .iter()
        .position(|cell| {
            cell.flags
                .intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
        })
        .unwrap_or(want);
    if plain == 0 {
        return Wrote::OneSlow;
    }
    row.write_ascii(col, &bytes[..plain], &style);
    if col + plain < columns {
        grid.cursor.point.column = Column(col + plain);
    } else {
        // The last column was written: the cursor stays on it and the next
        // character wraps first.
        grid.cursor.point.column = Column(columns - 1);
        grid.cursor.input_needs_wrap = true;
    }
    Wrote::Cells(plain)
}

/// What is at the front of some output, as text goes.
enum Next {
    /// A character that takes cells of its own: its length in bytes and
    /// its width in cells, one or two.
    Char(char, usize, usize),
    /// A whole character, this many bytes long, that takes no cell of its
    /// own or is not for printing: the parser places it.
    Other(usize),
    /// Not a whole, valid character: a control byte, the start of a
    /// sequence, bytes that are not UTF-8, or the end of the output.
    None,
}

/// Reads one character from the front of `bytes`, by the rules of UTF-8 as
/// the standard library keeps them: nothing overlong, no surrogates,
/// nothing past U+10FFFF.
#[inline(always)]
fn next_char(bytes: &[u8]) -> Next {
    let Some(&first) = bytes.first() else {
        return Next::None;
    };
    if first < 0x80 {
        return if printable(first) {
            Next::Char(first as char, 1, 1)
        } else {
            Next::None
        };
    }
    let follows = |at: usize, low: u8, high: u8| match bytes.get(at) {
        Some(&byte) if (low..=high).contains(&byte) => Some(u32::from(byte & 0x3f)),
        _ => None,
    };
    let lead = u32::from(first);
    let (code, len) = match first {
        0xc2..=0xdf => match follows(1, 0x80, 0xbf) {
            Some(b) => ((lead & 0x1f) << 6 | b, 2),
            None => return Next::None,
        },
        0xe0..=0xef => {
            let (low, high) = match first {
                0xe0 => (0xa0, 0xbf),
                0xed => (0x80, 0x9f),
                _ => (0x80, 0xbf),
            };
            match (follows(1, low, high), follows(2, 0x80, 0xbf)) {
                (Some(b), Some(c)) => ((lead & 0x0f) << 12 | b << 6 | c, 3),
                _ => return Next::None,
            }
        }
        0xf0..=0xf4 => {
            let (low, high) = match first {
                0xf0 => (0x90, 0xbf),
                0xf4 => (0x80, 0x8f),
                _ => (0x80, 0xbf),
            };
            match (follows(1, low, high), follows(2, 0x80, 0xbf), follows(3, 0x80, 0xbf)) {
                (Some(b), Some(c), Some(d)) => ((lead & 0x07) << 18 | b << 12 | c << 6 | d, 4),
                _ => return Next::None,
            }
        }
        _ => return Next::None,
    };
    // SAFETY: the ranges above admit exactly the scalar values: nothing
    // from U+D800 to U+DFFF and nothing above U+10FFFF.
    let c = unsafe { char::from_u32_unchecked(code) };
    match cells(c) {
        0 => Next::Other(len),
        width => Next::Char(c, len, width),
    }
}

/// How many cells a character takes, one or two, as `Term::input` has it;
/// zero for one that takes none of its own or is not for printing.
#[inline(always)]
fn cells(c: char) -> usize {
    match c as u32 {
        // The blocks most text is made of, in which every character has
        // the one width: Latin with its extensions, Greek, the letters of
        // Cyrillic, box drawing and blocks; the CJK ideographs, the kana
        // and the Hangul syllables.
        0xa1..=0xac | 0xae..=0x2ff | 0x370..=0x482 | 0x48a..=0x52f | 0x2500..=0x259f => 1,
        0x3041..=0x3096 | 0x30a1..=0x30fa | 0x4e00..=0x9fff | 0xac00..=0xd7a3 => 2,
        // Whatever else the table says: none, or more than two, is not for
        // writing here.
        _ => c.width().filter(|width| (1..=2).contains(width)).unwrap_or(0),
    }
}

/// What [write_chars] did.
enum Chars {
    /// Wrote the characters in this many bytes; the last of them.
    Wrote(usize, char),
    /// Nothing: the character at the front, this many bytes long, is the
    /// parser's to place. The ones after it may not be.
    OneSlow(usize),
    /// Nothing, and what is at the front is the parser's whatever it is: a
    /// mode in force needs the parser for every character, or what is
    /// there is not a whole valid character.
    Not,
}

/// Writes text at the cursor as `Term::input` would, a character at a time:
/// printable ASCII and whatever else takes one cell or two, as far as it
/// goes, wrapping to the next row where a row is full. `allowed` is
/// [modes_allow]'s answer, asked once per stretch of direct writing.
fn write_chars(term: &mut Term<Listener>, bytes: &[u8], allowed: &mut Option<bool>) -> Chars {
    if !*allowed.get_or_insert_with(|| modes_allow(term)) {
        return Chars::Not;
    }
    let wraps = term.mode().contains(TermMode::LINE_WRAP);
    let halves = Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER;
    let mut at = 0;
    let mut last = None;
    // The length of a character that has to go to the parser, if that is
    // what stopped the writing.
    let slow = 'rows: loop {
        // A row that is full wraps before the next character that takes a
        // cell, and not before any other.
        let len = match next_char(&bytes[at..]) {
            Next::Char(_, len, _) => len,
            Next::Other(len) => break Some(len),
            Next::None => break None,
        };
        let grid = term.grid_mut();
        if grid.cursor.input_needs_wrap {
            // As in `Feeder::direct`, from the last column only.
            if !wraps || grid.cursor.point.column.0 + 1 != grid.columns() {
                // It goes over the last column, which is `Term::input`'s to do.
                break Some(len);
            }
            let line = grid.cursor.point.line;
            grid[line].set_wrapline();
            term.linefeed();
            term.carriage_return();
            warm_next_rows(term);
            continue;
        }
        let columns = grid.columns();
        let line = grid.cursor.point.line;
        let style = Cell::styled(' ', &grid.cursor.template);
        let mut col = grid.cursor.point.column.0;
        let mut text = grid[line].text(col, &style);
        let stopped = loop {
            let (c, len, width) = match next_char(&bytes[at..]) {
                Next::Char(c, len, width) => (c, len, width),
                Next::Other(len) => break Some(Some(len)),
                Next::None => break Some(None),
            };
            if col + width > columns {
                // The row is full; or one cell is left and the character
                // takes two, which the parser fills in and wraps.
                break (col < columns).then_some(Some(len));
            }
            // Overwriting half of a wide character clears its other half,
            // which is Term::input's to do.
            if text.flags(col).intersects(halves)
                || (width == 2 && text.flags(col + 1).intersects(halves))
            {
                break Some(Some(len));
            }
            text.push(c, width == 2);
            col += width;
            at += len;
            last = Some(c);
        };
        drop(text);
        if col < columns {
            grid.cursor.point.column = Column(col);
        } else {
            // The last column was written: the cursor stays on it and the
            // next character wraps first.
            grid.cursor.point.column = Column(columns - 1);
            grid.cursor.input_needs_wrap = true;
        }
        match stopped {
            Some(slow) => break 'rows slow,
            // The row is full and the text goes on.
            None => continue,
        }
    };
    match (last, slow) {
        (Some(last), _) => Chars::Wrote(at, last),
        (None, Some(len)) => Chars::OneSlow(len),
        (None, None) => Chars::Not,
    }
}

/// Writes text at the start of the cursor's line, which a scroll was asked
/// to leave as it was (`Grid::spare_reset` with `usize::MAX`), and resets
/// what is left of the line after the text. It writes what [write_chars]
/// would write there, with one difference: what the cells hold counts for
/// nothing, being left over from the line this row was thousands of lines
/// ago. Returns how many bytes it took and the last character written.
///
/// Resetting a line and then writing over the start of it writes those
/// cells twice, and for a line of text that is most of them.
fn write_fresh(term: &mut Term<Listener>, bytes: &[u8]) -> (usize, Option<char>) {
    let grid = term.grid_mut();
    let columns = grid.columns();
    let line = grid.cursor.point.line;
    let template = grid.cursor.template.clone();
    let style = Cell::styled(' ', &template);
    let row = &mut grid[line];
    let (mut at, mut col, mut last) = (0, 0, None);
    let mut text = row.text(0, &style);
    while let Next::Char(c, len, width) = next_char(&bytes[at..]) {
        if col + width > columns {
            break;
        }
        text.push(c, width == 2);
        col += width;
        at += len;
        last = Some(c);
    }
    drop(text);
    row.reset_after(&template, col);
    if col < columns {
        grid.cursor.point.column = Column(col);
    } else {
        // As in write_ascii: the last column was written.
        grid.cursor.point.column = Column(columns - 1);
        grid.cursor.input_needs_wrap = true;
    }
    (at, last)
}

/// A control sequence that [usual_csi] has read, for which [is_direct]
/// holds: where it starts, its length, the number of its parameters and
/// its final byte.
#[derive(Clone, Copy)]
struct Csi {
    at: usize,
    len: usize,
    count: usize,
    last: u8,
}

impl Csi {
    /// Where no sequence starts: the mark of none.
    const NOWHERE: usize = usize::MAX;

    /// No sequence.
    const NONE: Csi = Csi {
        at: Csi::NOWHERE,
        len: 0,
        count: 0,
        last: 0,
    };

    /// Counts the sequence, if there is one, from `start` bytes earlier.
    #[inline(always)]
    fn from(&mut self, start: usize) {
        self.at = self.at.saturating_add(start);
    }
}

/// How many bytes at the front of `bytes` are text to write directly in one
/// go; zero when they are the parser's. `fits` is how many cells the
/// cursor's row has left, and `params` is scratch space.
///
/// Text that reaches past the end of the row is written to the end of the
/// row, and what comes after that is not looked at until the next row.
/// When what follows a run of printable bytes is done directly as well (a
/// line end, a control sequence [is_direct] holds for) or is not known yet
/// (the end of the bytes at hand), the run is written whole, whatever its
/// length: the parser is then not needed at all. Before anything else a
/// short run is the parser's, and so is a long run's last character,
/// together with what follows: the parser has then seen it should a repeat
/// come next, at no extra call.
///
/// A control sequence found after the run, to be carried out directly, is
/// noted in `next` as [usual_csi] returned it, with its parameters left in
/// `params`: so that it is not read a second time when its turn comes.
/// `next` starts [Csi::NOWHERE] otherwise.
#[inline]
fn direct_text(
    bytes: &[u8],
    params: &mut [u16; MAX_PARAMS],
    fits: usize,
    next: &mut Csi,
) -> usize {
    next.at = Csi::NOWHERE;
    if !bytes.first().is_some_and(|&b| printable(b)) {
        return 0;
    }
    let run = printable_run(&bytes[..bytes.len().min(fits + 1)]);
    if run > fits {
        return fits;
    }
    let after = &bytes[run..];
    let whole = match after.first() {
        Some(b'\n' | b'\r') | None => true,
        Some(&ESC) => match usual_csi(after, params) {
            Some((len, count, last)) if is_direct(last) => {
                *next = Csi {
                    at: run,
                    len,
                    count,
                    last,
                };
                true
            }
            _ => false,
        },
        // Text in another script, which is written directly as well.
        Some(&byte) => byte >= 0x80,
    };
    if !whole && run < MIN_RUN {
        0
    } else {
        run - usize::from(!whole)
    }
}

/// Writes printable ASCII over the cells at the start of the cursor's line
/// that a scroll was asked to leave as they were (`Grid::spare_reset`), one
/// cell a byte, and moves the cursor past them. Every cell is written
/// whatever it holds: what it holds is left over from the line this row was
/// thousands of lines ago.
fn overwrite_spared(term: &mut Term<Listener>, bytes: &[u8]) {
    let grid = term.grid_mut();
    let columns = grid.columns();
    let line = grid.cursor.point.line;
    let style = Cell::styled(' ', &grid.cursor.template);
    grid[line].write_ascii(0, bytes, &style);
    if bytes.len() < columns {
        grid.cursor.point.column = Column(bytes.len());
    } else {
        // As in write_ascii: the last column was written.
        grid.cursor.point.column = Column(columns - 1);
        grid.cursor.input_needs_wrap = true;
    }
}

/// The escape parser with the shortcut in front of it.
pub(crate) struct Feeder {
    parser: Parser,
    /// The last character written directly, which the parser has not seen.
    unseen: Option<char>,
    /// The beginning of a synchronized update was carried out here since
    /// the time limit was last looked at; the parser notes its own.
    began: bool,
    /// When the synchronized update under way is ended for lasting too
    /// long; None when none is under way.
    until: Option<Instant>,
    /// A synchronized update has ended since this was last asked for.
    ended: bool,
}

impl Feeder {
    pub(crate) fn new() -> Feeder {
        Feeder {
            parser: Parser::new(),
            unseen: None,
            began: false,
            until: None,
            ended: false,
        }
    }

    /// When the synchronized update under way is to be ended for lasting
    /// too long, as of the last output interpreted.
    pub(crate) fn sync_deadline(&self) -> Option<Instant> {
        self.until
    }

    /// Ends the synchronized update under way, if there is one.
    pub(crate) fn stop_sync(&mut self, term: &mut Term<Listener>) {
        term.set_sync_update(false);
        self.until = None;
    }

    /// Whether a synchronized update has ended since this was last asked.
    pub(crate) fn take_ended(&mut self) -> bool {
        std::mem::take(&mut self.ended)
    }

    /// Interprets output, changing the terminal as the parser alone would.
    #[cfg(test)]
    pub(crate) fn advance(&mut self, term: &mut Term<Listener>, bytes: &[u8]) {
        let (taken, turn) = self.advance_to_turn(term, bytes, false);
        debug_assert_eq!((taken, turn), (bytes.len(), Turn::None));
    }

    /// Interprets output, changing the terminal as the parser alone would.
    /// With `watch`, only as far as the next beginning or end of a
    /// synchronized update: returns how many bytes were interpreted, the
    /// sequence that begins or ends the update last among them, and which
    /// of the two it was. Without it, all of the output is interpreted.
    pub(crate) fn advance_to_turn(
        &mut self,
        term: &mut Term<Listener>,
        bytes: &[u8],
        watch: bool,
    ) -> (usize, Turn) {
        let mut turn = Turn::None;
        let mut i = 0;
        while i < bytes.len() {
            let was = term.sync_update();
            if self.parser.is_ground() && bytes[i] != ESC {
                i += self.direct(term, &bytes[i..], watch);
                if watch && term.sync_update() != was {
                    turn = if was { Turn::Ended } else { Turn::Began };
                    break;
                }
                if i == bytes.len() || bytes[i] == ESC {
                    continue;
                }
                // What the shortcut leaves of the text, up to the next
                // escape or the next run it can take, goes to the parser.
                // That can end in the middle of a character, which the
                // parser then holds.
                let end = ground_share(bytes, i);
                self.parse(term, &bytes[i..end]);
                i = end;
                continue;
            }
            // A sequence, or the rest of a character: the parser reads it
            // through and stops where it is back in the ground state. Every
            // beginning and end of an update is a sequence, so it is here
            // that one is seen, right after it.
            self.tell_unseen();
            i += self.parser.advance_sequence(term, &bytes[i..]);
            if term.sync_update() != was {
                self.ended |= was;
                if watch {
                    turn = if was { Turn::Ended } else { Turn::Began };
                    break;
                }
            }
        }
        // The time limit runs from the latest beginning, and is looked at
        // only if the update outlasts the output at hand.
        let began = self.parser.take_began() | std::mem::take(&mut self.began);
        if !term.sync_update() {
            self.until = None;
        } else if began || self.until.is_none() {
            self.until = Some(Instant::now() + SYNC_TIMEOUT);
        }
        (i, turn)
    }

    /// Carries out what can be done without the parser from the front of
    /// `bytes`: text in runs the shortcut takes, line feeds, carriage returns
    /// and the commonest control sequences. Returns how many bytes that was; the next one, if any, is
    /// the parser's. Called in the ground state, and leaves it so.
    ///
    /// With `watch`, it also returns right after carrying out the sequence
    /// that begins a synchronized update when none was under way, or ends
    /// the one that was.
    fn direct(&mut self, term: &mut Term<Listener>, bytes: &[u8], watch: bool) -> usize {
        let mut allowed = None;
        let mut params = [0; MAX_PARAMS];
        // The control sequence that the last look ahead from a run of text
        // found after it, counted from the start of `bytes`; `params` holds
        // its parameters until [usual_csi] is next called.
        let mut next = Csi::NONE;
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                // What the parser calls for these in the ground state.
                b'\n' => {
                    // The line a scroll brings in is reset cell by cell, and
                    // then the text after the line feed is written over
                    // those very cells. When that text is at hand and would
                    // be written here, its cells are spared the reset.
                    i += 1;
                    let columns = term.grid().columns();
                    // Not with a wrap pending, which a backward tab can
                    // leave at the start of a line: the parser wraps before
                    // the next character, where this wrote it on the line.
                    let at_start = term.grid().cursor.point.column.0 == 0
                        && !term.grid().cursor.input_needs_wrap;
                    let ahead = if at_start {
                        direct_text(&bytes[i..], &mut params, columns, &mut next)
                    } else {
                        next.at = Csi::NOWHERE;
                        0
                    };
                    next.from(i);
                    // Text in another script: how many cells it will take
                    // is not known until it is written, so the line is
                    // left as it is, written, and reset from there on.
                    let other = ahead == 0 && at_start && bytes.get(i).is_some_and(|&b| b >= 0x80);
                    if other && *allowed.get_or_insert_with(|| modes_allow(term)) {
                        term.grid_mut().spare_reset(usize::MAX);
                        term.linefeed();
                        if term.grid_mut().spared_reset() > 0 {
                            let (len, last) = write_fresh(term, &bytes[i..]);
                            if last.is_some() {
                                self.unseen = last;
                            }
                            i += len;
                        }
                    } else if ahead > 0 && *allowed.get_or_insert_with(|| modes_allow(term)) {
                        term.grid_mut().spare_reset(ahead);
                        term.linefeed();
                        // Nonzero only if the line feed scrolled, and then
                        // the cursor is at the start of the line brought in.
                        let spared = term.grid_mut().spared_reset();
                        if spared > 0 {
                            overwrite_spared(term, &bytes[i..i + spared]);
                            self.unseen = Some(bytes[i + spared - 1] as char);
                            i += spared;
                        }
                    } else {
                        term.linefeed();
                    }
                    warm_next_rows(term);
                }
                b'\r' => {
                    term.carriage_return();
                    i += 1;
                }
                ESC => {
                    // A sequence already read, when the text before it was
                    // looked at: no other kind of sequence, then.
                    if next.at == i {
                        if apply_csi(term, &params[..next.count], next.last) {
                            allowed = None;
                        }
                        i += next.len;
                        next.at = Csi::NOWHERE;
                        continue;
                    }
                    // All there is to the beginning and the end of a
                    // synchronized update is the terminal's note of it.
                    if bytes[i..].starts_with(BEGIN_SYNC) {
                        i += BEGIN_SYNC.len();
                        self.began = true;
                        if !term.sync_update() {
                            term.set_sync_update(true);
                            if watch {
                                break;
                            }
                        }
                        continue;
                    }
                    if bytes[i..].starts_with(END_SYNC) {
                        i += END_SYNC.len();
                        if term.sync_update() {
                            term.set_sync_update(false);
                            self.ended = true;
                            if watch {
                                break;
                            }
                        }
                        continue;
                    }
                    // Naming a character set changes what the printable
                    // bytes after it may mean, so that is asked again.
                    if let Some((index, charset)) = designation(&bytes[i..]) {
                        term.configure_charset(index, charset);
                        allowed = None;
                        i += DESIGNATION_LEN;
                        continue;
                    }
                    let Some((len, count, last)) = usual_csi(&bytes[i..], &mut params) else {
                        break;
                    };
                    if !is_direct(last) {
                        break;
                    }
                    // An underline color changes what a cell carries.
                    if apply_csi(term, &params[..count], last) {
                        allowed = None;
                    }
                    i += len;
                }
                0x20..=0x7e => {
                    let grid = term.grid();
                    let columns = grid.columns();
                    // From the last column only, where the parser marks
                    // the row as wrapped: a wrap left pending elsewhere, as a
                    // backward tab leaves one, is the parser's to make.
                    if grid.cursor.input_needs_wrap
                        && grid.cursor.point.column.0 + 1 == columns
                        && term.mode().contains(TermMode::LINE_WRAP)
                        && *allowed.get_or_insert_with(|| modes_allow(term))
                    {
                        // The row is full and the text goes on: it wraps to
                        // the next row, as the parser would have it before
                        // it places the next character.
                        let ahead = direct_text(&bytes[i..], &mut params, columns, &mut next);
                        next.from(i);
                        if ahead == 0 {
                            break;
                        }
                        let grid = term.grid_mut();
                        let line = grid.cursor.point.line;
                        grid[line].set_wrapline();
                        // As at a line feed: the row this may bring in is
                        // spared the reset of the cells about to be written.
                        grid.spare_reset(ahead);
                        term.linefeed();
                        let spared = term.grid_mut().spared_reset();
                        term.carriage_return();
                        warm_next_rows(term);
                        if spared > 0 {
                            overwrite_spared(term, &bytes[i..i + spared]);
                            self.unseen = Some(bytes[i + spared - 1] as char);
                            i += spared;
                        }
                        continue;
                    }
                    let fits = columns - grid.cursor.point.column.0;
                    let len = direct_text(&bytes[i..], &mut params, fits, &mut next);
                    next.from(i);
                    if len == 0 {
                        break;
                    }
                    let text = &bytes[i..i + len];
                    match write_ascii(term, text, &mut allowed) {
                        Wrote::Cells(n) => {
                            self.unseen = Some(text[n - 1] as char);
                            i += n;
                        }
                        // Printable bytes leave the parser in the ground
                        // state and the modes as they are.
                        Wrote::OneSlow => {
                            self.parse(term, &bytes[i..i + 1]);
                            i += 1;
                        }
                        Wrote::Not => {
                            let run = printable_run(&bytes[i..]);
                            self.parse(term, &bytes[i..i + run]);
                            i += run;
                        }
                    }
                }
                0x80.. => match write_chars(term, &bytes[i..], &mut allowed) {
                    Chars::Wrote(len, last) => {
                        self.unseen = Some(last);
                        i += len;
                    }
                    // A whole character leaves the parser in the ground
                    // state, with nothing kept.
                    Chars::OneSlow(len) => {
                        self.parse(term, &bytes[i..i + len]);
                        i += len;
                    }
                    Chars::Not => break,
                },
                _ => break,
            }
        }
        i
    }

    /// Gives ground-state output to the parser.
    fn parse(&mut self, term: &mut Term<Listener>, bytes: &[u8]) {
        self.tell_unseen();
        self.parser.advance(term, bytes);
    }

    /// Tells the parser the last character written past it, which is what
    /// a repeat (CSI b) repeats.
    #[inline]
    fn tell_unseen(&mut self) {
        if let Some(last) = self.unseen.take() {
            self.parser.set_preceding(last);
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::engine::Options;
    use crate::term::TermDamage;

    use crate::index::Line;

    use super::*;

    /// The visible screen as text, one line per row, trailing blanks
    /// removed, as the session gives it.
    fn screen_text(term: &Term<Listener>) -> String {
        let grid = term.grid();
        let offset = grid.display_offset() as i32;
        let mut out = String::new();
        for row in 0..term.screen_lines() as i32 {
            let start = out.len();
            for cell in grid.line(Line(row - offset)).cells() {
                if cell.flags().intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                out.push(if cell.c() == '\t' { ' ' } else { cell.c() });
                out.extend(cell.zerowidth());
            }
            let trimmed = out[start..].trim_end_matches(' ').len();
            out.truncate(start + trimmed);
            out.push('\n');
        }
        out
    }

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }

        fn screen_lines(&self) -> usize {
            self.1
        }

        fn columns(&self) -> usize {
            self.0
        }
    }

    fn term(cols: usize, rows: usize) -> Term<Listener> {
        let options = Options {
            scrollback_lines: 50,
            ..Options::default()
        };
        Term::new(
            options,
            &Size(cols, rows),
            Listener::default(),
        )
    }

    /// The parser alone, with no shortcut: the measure of what the feeder
    /// should do.
    struct Plain {
        parser: Parser,
    }

    impl Plain {
        fn new() -> Plain {
            Plain { parser: Parser::new() }
        }

        fn advance(&mut self, term: &mut Term<Listener>, bytes: &[u8]) {
            self.parser.advance(term, bytes);
        }

        /// As the feeder ends an update that lasted too long.
        fn stop_sync(&mut self, term: &mut Term<Listener>) {
            term.unset_private_mode(crate::ansi::NamedPrivateMode::SyncUpdate.into());
        }
    }

    /// The usual way of honoring a synchronized update, as the parser
    /// this was first built on did it: what comes after the beginning of
    /// one is set aside, and interpreted all at once when the end comes.
    #[derive(Default)]
    struct Aside {
        plain: Option<Plain>,
        held: Vec<u8>,
        holding: bool,
        /// The last bytes interpreted, to see a beginning in.
        tail: Vec<u8>,
    }

    impl Aside {
        fn advance(&mut self, term: &mut Term<Listener>, bytes: &[u8]) {
            let plain = self.plain.get_or_insert_with(Plain::new);
            for &byte in bytes {
                if self.holding {
                    self.held.push(byte);
                    if self.held.ends_with(END_SYNC) {
                        plain.advance(term, &self.held);
                        self.held.clear();
                        self.holding = false;
                    }
                } else {
                    plain.advance(term, &[byte]);
                    self.tail.push(byte);
                    if self.tail.len() > BEGIN_SYNC.len() {
                        self.tail.remove(0);
                    }
                    self.holding = self.tail.ends_with(BEGIN_SYNC);
                }
            }
        }

        fn stop_sync(&mut self, term: &mut Term<Listener>) {
            let plain = self.plain.get_or_insert_with(Plain::new);
            plain.advance(term, &self.held);
            self.held.clear();
            self.holding = false;
            plain.stop_sync(term);
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn pick<'a, T: ?Sized>(&mut self, items: &[&'a T]) -> &'a T {
            items[self.below(items.len())]
        }
    }

    /// Everything a frame or a later sequence could show a difference in.
    fn state(term: &mut Term<Listener>) -> String {
        let mut out = String::new();
        let damage = match term.damage() {
            TermDamage::Full => "full".to_string(),
            TermDamage::Partial(lines) => lines
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(" "),
        };
        term.reset_damage();
        out.push_str(&format!(
            "damage {damage}\nmode {:?}\nupdate under way {}\ncursor {:?}\noffset {} history {}\n",
            term.mode(),
            term.sync_update(),
            term.grid().cursor,
            term.grid().display_offset(),
            term.grid().history_size(),
        ));
        let grid = term.grid();
        for line in grid.topmost_line().0..=grid.bottommost_line().0 {
            let cells: Vec<_> = grid.line(Line(line)).cells().collect();
            out.push_str(&format!("{line}: {cells:?}\n"));
        }
        // A hyperlink without an id of its own is numbered from a counter
        // the two terminals share, so its number differs between them.
        let mut same = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(at) = rest.find("id: \"") {
            let (before, after) = rest.split_at(at + 5);
            same.push_str(before);
            rest = after.trim_start_matches(|c: char| c.is_ascii_digit());
        }
        same.push_str(rest);
        same
    }

    /// A stream that mixes long and short text with everything that changes
    /// what a printable byte means or where it goes.
    fn stream(rng: &mut Rng, tokens: usize) -> Vec<u8> {
        const TEXT: &[u8] = b"abcdefghijklmnopqrstuvwxyz ABCDEFGHIJ 0123456789 _`{|}~!@#$%";
        let mut out = Vec::new();
        for _ in 0..tokens {
            match rng.below(43) {
                0..=11 => {
                    let len = *rng.pick(&[&1usize, &3, &9, &15, &16, &17, &40, &79, &80, &81, &200]);
                    for _ in 0..len + rng.below(3) {
                        out.push(TEXT[rng.below(TEXT.len())]);
                    }
                }
                12..=15 => out.extend_from_slice(rng.pick::<[u8]>(&[b"\n", b"\r\n", b"\r", b"\n\n"])),
                16 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\t", b"\x08", b"\x07", b"\x7f", b"\x0b", b"\x0c", b"\x0e", b"\x0f", b"\x18",
                    b"\x1a", b"\x00",
                ])),
                17..=18 => out.extend_from_slice(
                    rng.pick::<str>(&["\u{4e2d}\u{6587}", "e\u{301}", "\u{1f600}", "\u{e9}", "\u{3042}"])
                        .as_bytes(),
                ),
                // Runs of text in other scripts, long enough to wrap, with
                // ASCII among them and without.
                39..=41 => {
                    const SCRIPTS: [&str; 8] = [
                        "\u{4e2d}\u{6587}\u{5b57}\u{6f22}\u{8a9e}\u{65e5}\u{672c}\u{3042}\u{30a2}\u{d55c}",
                        "\u{e9}\u{e0}\u{fc}\u{df}\u{f1}\u{141}\u{17c}\u{3b1}\u{3b2}\u{3c9}\u{436}\u{44f}",
                        "\u{1f600}\u{1f680}\u{1f44d}\u{1f3fd}\u{2764}\u{fe0f}\u{1f469}\u{200d}\u{1f52c}\u{2728}",
                        "\u{2500}\u{2502}\u{250c}\u{2510}\u{2514}\u{2518}\u{2588}\u{2591}\u{2580}\u{28ff}",
                        "e\u{301}a\u{308}\u{323}\u{4e2d}\u{301}\u{ff46}\u{ff55}\u{ad}\u{85}\u{a0}\u{200b}",
                        "a\u{4e2d}b\u{e9}c \u{6587}d\u{1f600}e ",
                        "\u{e9} \u{e0} \u{fc}a\u{f1} ",
                        "\u{4e2d}",
                    ];
                    let script: Vec<char> = SCRIPTS[rng.below(SCRIPTS.len())].chars().collect();
                    let len = *rng.pick(&[&1usize, &2, &5, &9, &10, &19, &20, &21, &45, &90]);
                    let mut utf8 = [0; 4];
                    for _ in 0..len + rng.below(3) {
                        let c = script[rng.below(script.len())];
                        out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
                    }
                }
                19 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\xff", b"\xc3", b"\xe4\xb8", b"\x9b", b"\x9c", b"\x80", b"\xf0\x9f",
                ])),
                20..=21 => out.extend_from_slice(
                    format!(
                        "\x1b[{}m",
                        rng.pick::<str>(&["1", "0", "31", "38;5;196", "48;2;1;2;3", "4:3", "7", ""])
                    )
                    .as_bytes(),
                ),
                // Colors and styles in every spelling, valid or not.
                22..=23 => {
                    const VALUES: &[u32] = &[
                        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 33,
                        37, 38, 39, 40, 44, 47, 48, 49, 50, 58, 59, 60, 89, 90, 97, 98, 100, 107,
                        108, 255, 256, 300, 9999, 10000, 65535, 65536,
                    ];
                    let count = *rng.pick(&[&0usize, &1, &1, &2, &3, &5, &8, &16, &17, &33]);
                    out.extend_from_slice(b"\x1b[");
                    for at in 0..count {
                        if at > 0 {
                            out.push(*rng.pick(&[&b';', &b';', &b';', &b';', &b':']));
                        }
                        if rng.below(12) > 0 {
                            let value = VALUES[rng.below(VALUES.len())];
                            let zeros = if rng.below(10) == 0 { "00" } else { "" };
                            out.extend_from_slice(format!("{zeros}{value}").as_bytes());
                        }
                    }
                    out.extend_from_slice(rng.pick::<[u8]>(&[b"m", b"m", b"m", b"m", b" m", b"", b"M"]));
                }
                24..=25 => out.extend_from_slice(
                    format!("\x1b[{};{}H", rng.below(12) + 1, rng.below(30) + 1).as_bytes(),
                ),
                26 => {
                    let number = rng.pick::<str>(&["", "", "0", "1", "2", "3", "4", "7", "40", "999", "010"]);
                    let last = rng.pick::<str>(&[
                        "A", "B", "C", "D", "E", "F", "G", "H", "J", "K", "L", "M", "P", "@", "X", "S", "T",
                        "a", "d", "e", "f", "`",
                    ]);
                    let second = rng.pick::<str>(&["", "", "", ";", ";3", ";0", ";25;7", ";999"]);
                    out.extend_from_slice(format!("\x1b[{number}{second}{last}").as_bytes());
                }
                27 => out.extend_from_slice(format!("\x1b[{}b", rng.below(6)).as_bytes()),
                28 => out.extend_from_slice(rng.pick::<[u8]>(&[b"\x1b[4h", b"\x1b[4l", b"\x1b[4l"])),
                29 => out.extend_from_slice(rng.pick::<[u8]>(&[b"\x1b[?7l", b"\x1b[?7h", b"\x1b[?7h"])),
                30 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\x1b[?1049h",
                    b"\x1b[?1049l",
                    b"\x1b[?6h",
                    b"\x1b[?6l",
                    b"\x1b[20h",
                    b"\x1b[20l",
                ])),
                31 => out.extend_from_slice(
                    format!("\x1b[{};{}r", rng.below(6) + 1, rng.below(6) + 7).as_bytes(),
                ),
                32 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\x1b(0", b"\x1b(B", b"\x1b)0", b"\x1b)B", b"\x1b(B", b"\x1b)B",
                ])),
                33 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\x1b7", b"\x1b8", b"\x1bM", b"\x1bD", b"\x1bE", b"\x1bH", b"\x1b=", b"\x1bc",
                ])),
                34 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\x1b]0;a title\x07",
                    b"\x1b]2;another\x1b\\",
                    b"\x1b]8;;http://example.com\x1b\\",
                    b"\x1b]8;;\x1b\\",
                    b"\x1b]8;;http://example.com\x07",
                    b"\x1b]8;;\x07",
                ])),
                35 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\x1b[58:5:196m",
                    b"\x1b[58;5;196m",
                    b"\x1b[4;58;2;1;2;3m",
                    b"\x1b[59m",
                    b"\x1b[59m",
                    b"\x1b[0m",
                ])),
                36 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\x1b[?2026h",
                    b"\x1b[?2026l",
                    b"\x1b[?2026l",
                    b"\x1bP=1s\x1b\\",
                    b"\x1bP=2s\x1b\\",
                    b"\x1bP=2s\x1b\\",
                ])),
                37 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\x1bPq#0;2;0;0;0\x1b\\",
                    b"\x1b_an application command\x1b\\",
                    b"\x1b^private\x1b\\",
                    b"\x1bXstring\x1b\\",
                    b"\x1bP1$r\x9c",
                ])),
                // Sequences left unfinished, and stray bytes inside them.
                38 => out.extend_from_slice(rng.pick::<[u8]>(&[
                    b"\x1b", b"\x1b[", b"\x1b[38;5", b"\x1b]0;unfinished", b"\x1bP", b"\x1b(", b"\x1b[1\x18",
                    b"\x1b[3\n1m", b"\x1b[\xe4m", b"\x1b\x7f[1m", b"\x1b[ q", b"\x1b[1 q",
                ])),
                _ => {
                    for _ in 0..rng.below(4) + 1 {
                        out.push(rng.below(256) as u8);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn the_shortcut_leaves_the_terminal_as_the_parser_would() {
        for seed in 1..=400u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (cols, rows) = (*rng.pick(&[&20usize, &33, &80]), 12);
            let bytes = stream(&mut rng, 300);
            let mut plain = term(cols, rows);
            let mut plain_parser = Plain::new();
            let mut fast = term(cols, rows);
            let mut feeder = Feeder::new();
            let mut at = 0;
            while at < bytes.len() {
                let len = (*rng.pick(&[&1usize, &2, &7, &64, &500, &4000])).min(bytes.len() - at);
                let chunk = &bytes[at..at + len];
                plain_parser.advance(&mut plain, chunk);
                feeder.advance(&mut fast, chunk);
                at += len;
                let (want, got) = (state(&mut plain), state(&mut fast));
                if want != got {
                    let differing: Vec<String> = want
                        .lines()
                        .zip(got.lines())
                        .filter(|(a, b)| a != b)
                        .take(3)
                        .map(|(a, b)| format!("parser:   {a}\nshortcut: {b}"))
                        .collect();
                    panic!(
                        "seed {seed}, after byte {at} of {} (chunk {:?}):\n{}",
                        bytes.len(),
                        String::from_utf8_lossy(chunk),
                        differing.join("\n")
                    );
                }
            }
            // An update left under way is ended the same.
            plain_parser.stop_sync(&mut plain);
            feeder.stop_sync(&mut fast);
            assert_eq!(state(&mut plain), state(&mut fast), "seed {seed}, at the end");
        }
    }

    /// Lines as programs mostly print them: plain text short and long, some
    /// with tabs, a few words in color, an erase to the end of the line,
    /// text in another script. These are the lines whose compact form is
    /// built as they are written, or nearly.
    fn lines(rng: &mut Rng) -> Vec<u8> {
        const WORDS: [&str; 12] = [
            "fn", "main()", "{", "let", "value", "=", "compute(&input);", "}", "//", "warning:",
            "\u{e9}t\u{e9}", "\u{65e5}\u{672c}\u{8a9e}",
        ];
        let mut out = Vec::new();
        for _ in 0..1 + rng.below(6) {
            out.extend(std::iter::repeat_n(b'\t', rng.below(3)));
            let most = *rng.pick(&[&4usize, &12, &40]);
            for _ in 0..rng.below(most) {
                match rng.below(14) {
                    0 => out.extend_from_slice(b"\x1b[31m"),
                    1 => out.extend_from_slice(b"\x1b[1;38;5;208m"),
                    2 => out.extend_from_slice(b"\x1b[0m"),
                    3 => out.extend_from_slice(b"\x1b[44m\x1b[K"),
                    4 => out.extend_from_slice(b"\x1b[m\x1b[K"),
                    5 => out.push(b'\t'),
                    6 => out.push(b'\r'),
                    _ => {
                        // The last two are not ASCII, and come up less.
                        let plain = 2 * rng.below(2);
                        let word = WORDS[rng.below(WORDS.len() - plain)];
                        out.extend_from_slice(word.as_bytes());
                        out.push(b' ');
                    }
                }
            }
            out.extend_from_slice(rng.pick::<[u8]>(&[b"\r\n", b"\r\n", b"\n"]));
        }
        out
    }

    /// A terminal of `cols` by `rows` whose scrollback of 50 lines keeps
    /// `ring` lines as cells and packs the rest into pages of `page` bytes.
    fn stored(cols: usize, rows: usize, ring: usize, page: usize) -> Term<Listener> {
        let options = Options { scrollback_lines: 50, ring_rows: ring, page_bytes: page, ..Options::default() };
        Term::new(options, &Size(cols, rows), Listener::default())
    }

    /// The scrollback is kept packed, and this is what makes sure nothing
    /// can tell: a terminal whose ring of lines kept as cells grows with the
    /// output and is packed at odd moments, one that packs lines into tiny
    /// pages as soon as it can, and one whose ring grows a little past the
    /// few dozen it keeps and is packed whole now and then, are given the
    /// same output, the same scrolling back and the same changes of size,
    /// and have to be the same in every line all the way through.
    #[test]
    fn the_scrollback_is_the_same_packed_or_not() {
        use crate::grid::Scroll;

        let mut packed = 0;
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (cols, rows) = (*rng.pick(&[&20usize, &33, &80]), 12);
            let mut bytes = Vec::new();
            for _ in 0..40 {
                match rng.below(4) {
                    0 => bytes.extend(stream(&mut rng, 8)),
                    _ => bytes.extend(lines(&mut rng)),
                }
            }
            let mut usual = stored(cols, rows, usize::MAX, crate::grid::PAGE_BYTES);
            let mut tiny = stored(cols, rows, 0, 64);
            let mut whole = stored(cols, rows, 40, 256);
            let mut feeders = [Feeder::new(), Feeder::new(), Feeder::new()];
            let mut at = 0;
            while at < bytes.len() {
                let len = (*rng.pick(&[&1usize, &7, &64, &500, &4000])).min(bytes.len() - at);
                let chunk = &bytes[at..at + len];
                feeders[0].advance(&mut usual, chunk);
                feeders[1].advance(&mut tiny, chunk);
                feeders[2].advance(&mut whole, chunk);
                at += len;
                match rng.below(8) {
                    0..=2 => {
                        let delta = rng.below(40) as i32 - 20;
                        for term in [&mut usual, &mut tiny, &mut whole] {
                            term.scroll_display(Scroll::Delta(delta));
                        }
                    }
                    // Lines left as cells are packed, in part or all.
                    4 => {
                        let lines = 1 + rng.below(40);
                        usual.settle(lines);
                        whole.settle(usize::MAX);
                    }
                    3 if rng.below(3) == 0 => {
                        let cols = *rng.pick(&[&7usize, &20, &33, &80, &100]);
                        let rows = *rng.pick(&[&3usize, &12, &12, &30]);
                        for term in [&mut usual, &mut tiny, &mut whole] {
                            term.resize(Size(cols, rows));
                        }
                    }
                    _ => {}
                }
                let want = state(&mut whole);
                same(&want, &state(&mut usual), &format!("seed {seed}, byte {at}, packed after a while"));
                same(&want, &state(&mut tiny), &format!("seed {seed}, byte {at}, packed at once"));
            }
            // All but the newest line, which stays as cells.
            let (history, all, _) = tiny.history_stats();
            packed += usize::from(history > 1 && all + 1 == history);
        }
        // The test is of something: the scrollback was packed.
        assert!(packed > 200, "the scrollback was packed in {packed} of 300 runs");
    }

    /// Panics with the lines two states differ in, if they differ.
    #[track_caller]
    fn same_state(plain: &mut Term<Listener>, fast: &mut Term<Listener>, when: &str) {
        let (want, got) = (state(plain), state(fast));
        same(&want, &got, when);
    }

    /// Panics with the lines two states, as [state] gives them, differ in,
    /// if they differ.
    #[track_caller]
    fn same(want: &str, got: &str, when: &str) {
        if want != got {
            let differing: Vec<String> = want
                .lines()
                .zip(got.lines())
                .filter(|(a, b)| a != b)
                .take(3)
                .map(|(a, b)| {
                    let at = a.bytes().zip(b.bytes()).position(|(x, y)| x != y).unwrap_or(0);
                    let from = at.saturating_sub(60);
                    format!(
                        "parser:   ...{}\nshortcut: ...{}",
                        a.get(from..(at + 120).min(a.len())).unwrap_or(a),
                        b.get(from..(at + 120).min(b.len())).unwrap_or(b)
                    )
                })
                .collect();
            panic!("{when}:\n{}", differing.join("\n"));
        }
    }

    /// Output that is mostly synchronized updates: begun and ended as usual
    /// and in other spellings, one inside another, one right after
    /// another, begun after half a character, ended twice or never, with
    /// the time running out on some, and all of it cut anywhere.
    const UPDATES: [&[u8]; 22] = [
        b"\x1b[?2026h",
        b"\x1b[?2026h",
        b"\x1b[?2026h",
        b"\x1b[?2026l",
        b"\x1b[?2026l",
        b"\x1b[?2026l",
        b"\x1b[?2026;25h",
        b"\x1b[?25;2026h",
        b"\x1b[?2026;7l",
        b"\x1b[?02026h",
        b"\x1b[?2026$p",
        b"\x1b[?2026h\x1b[?2026l",
        b"\x1b[?2026l\x1b[?2026h",
        b"\xe4\xb8\x1b[?2026h",
        b"\xc3\x1b[?2026l",
        b"\x1b",
        b"\x1b[?20",
        b"a line of ordinary text, long enough for the shortcut\r\n",
        b"\x1b[1;31mred\x1b[0m \x1b[3;5Hthere \x1b[K\x1b[2Aup\r\n",
        "wide \u{4e16}\u{754c} and e\u{301}\r\n".as_bytes(),
        b"\x1b[2J\x1b[H",
        b"z\x1b[3b",
    ];


    #[test]
    fn updates_leave_the_terminal_as_the_parser_would() {
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut bytes = Vec::new();
            for _ in 0..rng.below(60) + 5 {
                bytes.extend_from_slice(UPDATES[rng.below(UPDATES.len())]);
            }
            let mut plain = term(33, 8);
            let mut plain_parser = Plain::new();
            let mut fast = term(33, 8);
            let mut feeder = Feeder::new();
            let mut at = 0;
            let mut cuts = Vec::new();
            while at < bytes.len() {
                let len = (*rng.pick(&[&1usize, &3, &8, &9, &40, &200, &4000])).min(bytes.len() - at);
                let chunk = &bytes[at..at + len];
                plain_parser.advance(&mut plain, chunk);
                // Watching for the turns or not, the outcome is the same.
                let watch = rng.below(2) == 0;
                let mut rest = chunk;
                while !rest.is_empty() {
                    let was = fast.sync_update();
                    let (taken, turn) = feeder.advance_to_turn(&mut fast, rest, watch);
                    rest = &rest[taken..];
                    match turn {
                        Turn::None => assert!(rest.is_empty()),
                        Turn::Began => assert!(watch && !was && fast.sync_update()),
                        Turn::Ended => assert!(watch && was && !fast.sync_update()),
                    }
                }
                at += len;
                cuts.push(at);
                same_state(
                    &mut plain,
                    &mut fast,
                    &format!("seed {seed}, after byte {at} of {}; all of it {:?}, cut after {cuts:?}", bytes.len(), String::from_utf8_lossy(&bytes)),
                );
                // A time limit is kept for as long as an update is under way.
                assert_eq!(feeder.sync_deadline().is_some(), fast.sync_update(), "seed {seed}");
                // The update's time runs out.
                if rng.below(9) == 0 {
                    plain_parser.stop_sync(&mut plain);
                    feeder.stop_sync(&mut fast);
                    assert!(!fast.sync_update() && feeder.sync_deadline().is_none());
                    same_state(&mut plain, &mut fast, &format!("seed {seed}, timed out at {at}"));
                }
            }
            plain_parser.stop_sync(&mut plain);
            feeder.stop_sync(&mut fast);
            same_state(&mut plain, &mut fast, &format!("seed {seed}, at the end"));
        }
    }

    /// Setting an update's output aside and interpreting it when the update
    /// ends ([Aside], as the parser this was first built on did) gives,
    /// whenever no update is under way, the very screen that interpreting
    /// everything as it comes gives: only what can be seen in the meantime
    /// differs, which the session sees to.
    #[test]
    fn between_updates_the_screen_is_what_setting_them_aside_would_give() {
        // As above, without the other spellings: set aside, an update ends
        // only at an end spelled the usual way.
        let pieces: Vec<&[u8]> = UPDATES
            .iter()
            .copied()
            .filter(|p| {
                let odd: [&[u8]; 4] = [b"\x1b[?2026;25h", b"\x1b[?25;2026h", b"\x1b[?2026;7l", b"\x1b[?02026h"];
                !odd.contains(p)
            })
            .collect();
        // What both terminals show, apart from the note of an update.
        let shown = |term: &mut Term<Listener>| {
            state(term)
                .lines()
                .filter(|l| !l.starts_with("update under way"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut compared = 0;
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut bytes = Vec::new();
            for _ in 0..rng.below(60) + 5 {
                bytes.extend_from_slice(pieces[rng.below(pieces.len())]);
            }
            let mut aside = term(33, 8);
            let mut aside_parser = Aside::default();
            let mut fast = term(33, 8);
            let mut feeder = Feeder::new();
            let mut at = 0;
            while at < bytes.len() {
                let len = (*rng.pick(&[&1usize, &3, &8, &9, &40, &200, &4000])).min(bytes.len() - at);
                let chunk = &bytes[at..at + len];
                aside_parser.advance(&mut aside, chunk);
                feeder.advance(&mut fast, chunk);
                at += len;
                let holding = aside_parser.holding;
                assert_eq!(holding, fast.sync_update(), "seed {seed}, after byte {at}");
                if !holding {
                    assert_eq!(shown(&mut aside), shown(&mut fast), "seed {seed}, after byte {at}");
                    compared += 1;
                }
            }
            aside_parser.stop_sync(&mut aside);
            feeder.stop_sync(&mut fast);
            assert_eq!(shown(&mut aside), shown(&mut fast), "seed {seed}, at the end");
        }
        assert!(compared > 500, "{compared} screens compared");
    }

    fn fed(cols: usize, bytes: &[u8]) -> (Term<Listener>, Term<Listener>) {
        let mut plain = term(cols, 8);
        let mut parser = Plain::new();
        parser.advance(&mut plain, bytes);
        let mut fast = term(cols, 8);
        Feeder::new().advance(&mut fast, bytes);
        (plain, fast)
    }

    /// A line that fills its row leaves a wrap pending, and a backward tab
    /// takes the cursor to the start of the row without settling it: the
    /// text after a line feed then wraps first, here as in the parser,
    /// whichever row the line feed brings in.
    #[test]
    fn a_wrap_pending_at_the_start_of_a_row_is_made_after_a_line_feed() {
        for text in [&b"abcde\x1b[Z\n\nB"[..], b"abcde\x1b[Z\nxy\r\n", b"\x1b[8;1Habcde\x1b[Z\n\xc3\xa9"] {
            for rows in [1, 8] {
                let mut plain = term(5, rows);
                Plain::new().advance(&mut plain, b"\x1b[8;1H\n");
                Plain::new().advance(&mut plain, text);
                let mut fast = term(5, rows);
                let mut feeder = Feeder::new();
                feeder.advance(&mut fast, b"\x1b[8;1H\n");
                feeder.advance(&mut fast, text);
                same_state(&mut plain, &mut fast, &format!("{text:?} in {rows} rows"));
            }
        }
    }

    /// A piece of output can end anywhere, also inside a character. What
    /// comes out must not depend on where: the parser given all of the
    /// text at once is the measure, as it then never has to put a
    /// character together from two pieces.
    #[test]
    fn text_cut_anywhere_comes_out_as_the_text_whole() {
        let texts: [&[u8]; 16] = [
            "n\u{e9} \u{e0} la carte, s'il vous pla\u{ee}t".as_bytes(),
            "\u{e9}A\u{e9}".as_bytes(),
            "\u{e9}\u{e9}A\u{e9}B\u{e9}".as_bytes(),
            "se\u{f1}or, a\u{f1}o, ma\u{f1}ana \u{bf}s\u{ed}?".as_bytes(),
            "Gr\u{fc}\u{df}e aus K\u{f6}ln".as_bytes(),
            "\u{4e2d}\u{6587} and \u{65e5}\u{672c}\u{8a9e} with ascii between".as_bytes(),
            "a\u{4e2d}b\u{6587}c\u{5b57}d".as_bytes(),
            "emoji \u{1f600} and \u{1f44d}\u{1f3fd} and \u{1f469}\u{200d}\u{1f52c} ok".as_bytes(),
            "e\u{301}a\u{308}\u{323}o\u{302} combining".as_bytes(),
            "\u{395}\u{3bb}\u{3bb}\u{3b7}\u{3bd}\u{3b9}\u{3ba}\u{3ac} \u{3ba}\u{3b5}\u{3af}\u{3bc}\u{3b5}\u{3bd}\u{3bf}".as_bytes(),
            "\u{250c}\u{2500}\u{2500}\u{2510}\r\n\u{2502}ab\u{2502}\r\n\u{2514}\u{2500}\u{2500}\u{2518}".as_bytes(),
            "\u{ff46}\u{ff55}\u{ff4c}\u{ff4c} width and a soft\u{ad}hyphen \u{85}".as_bytes(),
            // And what is not valid, which has to come out as wrong either way.
            b"\xc3\xa9A\xff then \xe4\xb8 cut \xe4\xb8\xad\xb8 short",
            b"\xf0\x9f\x98 \xf0\x9f\x98\x80\x80 \xed\xa0\x80 \xe0\x80\x80",
            b"\xc3\x1b[1m\xa9 \xe4\x1b[0m\xb8\xad",
            b"\xc0\x80 \xc1\xbf \xf5\x80\x80\x80 \x80\xbf",
        ];
        for text in texts {
            let mut whole = term(20, 6);
            Plain::new().advance(&mut whole, text);
            let want = state(&mut whole);
            // In two pieces, and in three.
            for first in 0..=text.len() {
                for second in first..=text.len().min(first + 5) {
                    let mut fast = term(20, 6);
                    let mut feeder = Feeder::new();
                    for piece in [&text[..first], &text[first..second], &text[second..]] {
                        feeder.advance(&mut fast, piece);
                    }
                    assert_eq!(
                        want,
                        state(&mut fast),
                        "{:?} cut at {first} and {second}",
                        String::from_utf8_lossy(text)
                    );
                }
            }
        }
    }

    #[test]
    fn every_character_is_as_wide_here_as_the_terminal_has_it() {
        for c in '\0'..=char::MAX {
            let want = c.width().filter(|width| (1..=2).contains(width)).unwrap_or(0);
            assert_eq!(cells(c), want, "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn characters_are_decoded_as_the_standard_library_decodes_them() {
        // What the standard library makes of the front of some bytes: a
        // character and its length, if there is a whole valid one.
        let std_first = |bytes: &[u8]| {
            (1..=bytes.len().min(4)).find_map(|len| {
                let c = std::str::from_utf8(&bytes[..len]).ok()?.chars().next()?;
                Some((c, len))
            })
        };
        let check = |bytes: &[u8]| {
            let got = match next_char(bytes) {
                Next::Char(c, len, width) => {
                    assert_eq!(Some(width), c.width(), "{bytes:x?}");
                    Some((c, len))
                }
                Next::Other(len) => {
                    let (c, std_len) = std_first(bytes).expect("a character");
                    assert!(!matches!(c.width(), Some(1 | 2)) && std_len == len, "{bytes:x?}");
                    Some((c, len))
                }
                Next::None => None,
            };
            // Printable ASCII and everything past it; the controls and DEL
            // are not text.
            let want = std_first(bytes).filter(|(c, _)| printable(*c as u8) || *c as u32 >= 0x80);
            assert_eq!(got, want, "{bytes:x?}");
        };
        // Every first and second byte, with the third and fourth at the
        // edges of every range that matters, and each cut short.
        let edges = [0x00, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xff];
        for first in 0..=255u8 {
            check(&[first]);
            for second in 0..=255u8 {
                check(&[first, second]);
                for third in edges {
                    check(&[first, second, third]);
                    for fourth in edges {
                        check(&[first, second, third, fourth]);
                    }
                }
            }
        }
        check(&[]);
        // And every character there is, with something after it.
        let mut utf8 = [0; 5];
        for c in '\u{80}'..=char::MAX {
            let len = c.encode_utf8(&mut utf8).len();
            utf8[len] = b'x';
            check(&utf8[..len + 1]);
        }
    }

    #[test]
    fn a_repeat_after_a_run_repeats_its_last_character() {
        for bytes in [
            &b"a long run of plain text ends in z\x1b[3b"[..],
            b"a long run of plain text ends in z\n\x1b[3b",
            b"a long run of plain text ends in z\r\n\t\x1b[2b",
        ] {
            let (mut plain, mut fast) = fed(60, bytes);
            assert_eq!(state(&mut plain), state(&mut fast));
            assert!(state(&mut fast).contains("c: 'z'"));
        }
    }

    #[test]
    fn text_after_an_unfinished_character_goes_to_the_parser_first() {
        // The parser holds the start of a character when the first piece
        // ends; the text that follows turns it into a replacement character,
        // which must land before that text.
        for start in [&b"\xe4\xb8"[..], b"\xc3", b"\xf0\x9f\x98"] {
            let rest = b"then a long run of ordinary text follows it";
            let mut plain = term(60, 6);
            let mut parser = Plain::new();
            let mut fast = term(60, 6);
            let mut feeder = Feeder::new();
            for piece in [start, &rest[..]] {
                parser.advance(&mut plain, piece);
                feeder.advance(&mut fast, piece);
            }
            assert_eq!(state(&mut plain), state(&mut fast));
            assert!(state(&mut fast).contains("c: '\u{fffd}'"));
        }
    }

    #[test]
    fn line_feeds_scroll_the_smallest_grids() {
        // Fewer rows in all, history included, than the scrolls looked
        // ahead for.
        for (cols, rows, history) in [(2, 1, 0), (2, 1, 1), (2, 1, 3), (5, 2, 9), (5, 2, 10), (5, 3, 40)] {
            let options = Options {
                scrollback_lines: history,
                ..Options::default()
            };
            let listener = || Listener::default();
            let mut plain = Term::new(options, &Size(cols, rows), listener());
            let mut fast = Term::new(options, &Size(cols, rows), listener());
            let mut parser = Plain::new();
            let mut feeder = Feeder::new();
            for piece in [&b"a\nb\n\n"[..], b"\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n", b"cd\r\n\n\n"] {
                for _ in 0..20 {
                    parser.advance(&mut plain, piece);
                    feeder.advance(&mut fast, piece);
                }
                assert_eq!(state(&mut plain), state(&mut fast), "{cols}x{rows}, history {history}");
            }
        }
    }

    #[test]
    fn text_over_a_reused_line_leaves_nothing_of_the_old_one() {
        // Lines of every kind are written first: wide characters, colored
        // backgrounds, links, combining characters, full width and empty,
        // and lines brought in while the background was blue, which are
        // blue from end to end though nothing was written to them.
        let mut old = Vec::new();
        for n in 0..40 {
            old.extend_from_slice(match n % 6 {
                0 => "\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}".as_bytes(),
                1 => b"\x1b[41m a line on red, cleared to its end\x1b[K\x1b[m",
                2 => b"\x1b]8;;http://example.com\x1b\\a link across the line\x1b]8;;\x1b\\",
                3 => "e\u{301}e\u{301}e\u{301} accents \u{1f600}".as_bytes(),
                4 => b"0123456789012345678901234567890123456789",
                _ => b"",
            });
            old.extend_from_slice(b"\r\n");
        }
        old.extend_from_slice(b"\x1b[44m\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\x1b[m");
        // The engine uses a row again only once the history (50 lines here)
        // is full and the oldest row comes around. So: plain lines until the
        // last of them have come around to the rows written above and are
        // what the screen and the history hold at the end. Those take plain text of every length, with the
        // reset of the cells it overwrites spared.
        // And text in other scripts of every length, wide and narrow,
        // alone and after ASCII, with the line left as it was until the
        // text is written.
        const LINES: usize = 1012;
        const OTHER: [&str; 4] = [
            "\u{4e2d}\u{6587}\u{5b57}\u{6f22}\u{8a9e}",
            "\u{e9}\u{e0}\u{fc}\u{3b1}\u{3c9}",
            "\u{4e2d}a\u{e9}\u{1f600}b",
            "\u{e9}\u{301}\u{4e2d}\u{200d}x",
        ];
        for width in [0usize, 1, 2, 15, 16, 17, 29, 30, 31, 45, 101, 102, 103, 114, 115, 116, 130] {
            let mut new = Vec::new();
            for n in 0..LINES {
                if width < 100 {
                    new.extend((0..width).map(|at| b'a' + ((n + at) % 26) as u8));
                } else {
                    // Widths past a hundred stand for the other scripts.
                    let script: Vec<char> = OTHER[n % OTHER.len()].chars().collect();
                    let text: String = (0..width - 100).map(|at| script[(n + at) % script.len()]).collect();
                    new.extend_from_slice(text.as_bytes());
                }
                new.extend_from_slice(if n % 9 == 8 { b"\n" } else { b"\r\n" });
            }
            let lines: Vec<&[u8]> = new.split_inclusive(|&b| b == b'\n').collect();
            // In one piece; a few lines to a piece, which looks ahead within
            // a piece and not beyond it; and a line to a piece, which leaves
            // nothing to look ahead to.
            for together in [LINES, 4, 1] {
                let mut plain = term(30, 6);
                let mut fast = term(30, 6);
                let mut parser = Plain::new();
                let mut feeder = Feeder::new();
                parser.advance(&mut plain, &old);
                feeder.advance(&mut fast, &old);
                for (at, piece) in lines.chunks(together).enumerate() {
                    let piece = piece.concat();
                    parser.advance(&mut plain, &piece);
                    feeder.advance(&mut fast, &piece);
                    // Over the last lines, long after the rows have come
                    // around, after every piece.
                    if at * together >= LINES - 80 {
                        assert_eq!(
                            state(&mut plain),
                            state(&mut fast),
                            "width {width}, {together} lines a piece, piece {at}"
                        );
                    }
                }
                assert_eq!(state(&mut plain), state(&mut fast), "width {width}");
            }
        }
    }

    #[test]
    fn lines_scroll_inside_a_region_wherever_the_rows_are_kept() {
        // The engine keeps rows in a ring, and moves the rows of a scrolling
        // region in one piece unless they straddle the ring's end. `before`
        // lines scrolled off first put the screen at every place in the
        // ring, both sides of that end.
        for before in 0..24 {
            let mut term = term(10, 8);
            let mut parser = Plain::new();
            for _ in 0..before {
                parser.advance(&mut term, b"\r\n");
            }
            let mut feed = b"\x1b[H".to_vec();
            let mut rows: Vec<String> = (0..8).map(|row| format!("r{row}")).collect();
            feed.extend_from_slice(rows.join("\r\n").as_bytes());
            // Rows three to six scroll; the cursor goes to the last of them.
            feed.extend_from_slice(b"\x1b[3;6r\x1b[6;1H");
            parser.advance(&mut term, &feed);
            for step in 0..9 {
                let line = format!("n{step}");
                parser.advance(&mut term, format!("\r\n{line}").as_bytes());
                rows[2..6].rotate_left(1);
                rows[5] = line;
                let shown = screen_text(&term);
                assert_eq!(
                    shown.lines().collect::<Vec<_>>(),
                    rows,
                    "{before} lines before, step {step}"
                );
            }
            // And by more than one line at a time.
            parser.advance(&mut term, b"\x1b[2S");
            rows[2..6].rotate_left(2);
            rows[4] = String::new();
            rows[5] = String::new();
            let shown = screen_text(&term);
            assert_eq!(shown.lines().collect::<Vec<_>>(), rows, "{before} lines before");
        }
    }

    #[test]
    fn control_sequences_carried_out_here_do_what_the_parser_does() {
        // A screen full of text with no two rows or columns alike, so that
        // every cursor position and every erased stretch shows.
        let mut screen = b"\x1b[H".to_vec();
        for row in 0..8u8 {
            screen.extend((0..20u8).map(|col| b'a' + (row * 3 + col) % 26));
            if row < 7 {
                screen.extend_from_slice(b"\r\n");
            }
        }
        let first = ["", "0", "1", "2", "3", "5", "999"];
        let second = ["", ";", ";0", ";1", ";4", ";999"];
        // Every sequence carried out directly, some the parser keeps, and
        // look-alikes with a private marker or an intermediate byte, which
        // mean something else or nothing.
        let ends = [
            "m", "H", "f", "K", "J", "A", "B", "e", "C", "a", "D", "E", "F", "G", "`", "d", "X", "P", "@", "L", "M", "S",
            "T", "b",
        ];
        let before = ["", "?", ">", "=", "<"];
        let between = ["", " ", "!", "$"];
        for last in ends {
            for one in first {
                for two in second {
                    for (marker, middle) in before.iter().flat_map(|m| between.iter().map(move |b| (m, b))) {
                        // Markers and intermediates with every sequence
                        // would be thousands; a few of each do.
                        if (!marker.is_empty() || !middle.is_empty()) && (one.len() + two.len()) % 3 != 1 {
                            continue;
                        }
                        let sequence = format!("\x1b[{marker}{one}{two}{middle}{last}");
                        for at in ["\x1b[4;7H", "\x1b[1;1H", "\x1b[8;20H"] {
                            let mut feed = screen.clone();
                            feed.extend_from_slice(at.as_bytes());
                            feed.extend_from_slice(sequence.as_bytes());
                            // Text after it shows where the cursor went.
                            feed.extend_from_slice(b"Z");
                            let (mut plain, mut fast) = fed(20, &feed);
                            assert_eq!(state(&mut plain), state(&mut fast), "{sequence:?} after {at:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn character_sets_named_here_are_the_ones_the_parser_would_name() {
        // Every pair of bytes after the escape that could be taken for one,
        // with text before and after that reads differently in each set.
        for set in *b"()*+-./$%" {
            for charset in *b"B0AK<>12`a" {
                let sequence = [ESC, set, charset];
                for cut in 0..=sequence.len() {
                    let before = b"plain text, then lines: lqqqk\r\n".as_slice();
                    let after = b"lqqqqqqqqqqqqqqqqqqqqk and \x0elqk\x0f shifted\r\n".as_slice();
                    let first = [before, &sequence[..cut]].concat();
                    let second = [&sequence[cut..], after].concat();
                    let mut plain = term(60, 6);
                    let mut parser = Plain::new();
                    let mut fast = term(60, 6);
                    let mut feeder = Feeder::new();
                    for piece in [&first, &second] {
                        parser.advance(&mut plain, piece);
                        feeder.advance(&mut fast, piece);
                        same_state(
                            &mut plain,
                            &mut fast,
                            &format!("ESC {} {} cut at {cut}", set as char, charset as char),
                        );
                    }
                }
            }
        }
        // Line drawing in the set in use, and the text after it takes the
        // parser's way; ASCII again, and the shortcut is back.
        let mut fast = term(60, 6);
        let mut feeder = Feeder::new();
        feeder.advance(&mut fast, b"\x1b(0a long run of text drawn as lines qqqq");
        assert!(feeder.unseen.is_none());
        assert_eq!(fast.grid()[Line(0)][Column(0)].c(), '\u{2592}');
        feeder.advance(&mut fast, b"\x1b(Ba long run of text that is text again");
        assert_eq!(feeder.unseen, Some('n'));
    }

    #[test]
    fn long_lines_wrap_where_the_parser_wraps_them() {
        let text: Vec<u8> = (0..500).map(|i| b'a' + (i % 26) as u8).collect();
        let (mut plain, mut fast) = fed(37, &text);
        assert_eq!(state(&mut plain), state(&mut fast));
    }

    /// The session takes the screen where an update begins and where one
    /// ends, so the feeder has to stop exactly there: with the sequence
    /// carried out and nothing after it, however the sequence is spelled
    /// and wherever the output is cut.
    #[test]
    fn the_feeder_stops_exactly_where_an_update_begins_or_ends() {
        // The first row's text at each stop, and how the stop came about.
        let stops = |pieces: &[&[u8]], under_way: bool| {
            let mut fast = term(60, 4);
            let mut feeder = Feeder::new();
            if under_way {
                feeder.advance(&mut fast, BEGIN_SYNC);
            }
            let mut stops = Vec::new();
            for piece in pieces {
                let mut rest = *piece;
                while !rest.is_empty() {
                    let (taken, turn) = feeder.advance_to_turn(&mut fast, rest, true);
                    rest = &rest[taken..];
                    if turn != Turn::None {
                        let row: String = fast.grid()[Line(0)][..].iter().map(|c| c.c()).collect();
                        stops.push((turn, row.trim_end().to_string()));
                    }
                }
            }
            stops
        };
        let before: &[u8] = b"before ";
        let after: &[u8] = b"and then a long run of ordinary text";
        let cut_everywhere = |sequence: &[u8], under_way: bool, want: &[(Turn, &str)]| {
            let whole = [before, sequence, after].concat();
            for cut in 0..=whole.len() {
                let (first, second) = whole.split_at(cut);
                let got = stops(&[first, second], under_way);
                let got: Vec<(Turn, &str)> = got.iter().map(|(t, row)| (*t, row.as_str())).collect();
                assert_eq!(got, want, "{:?} cut at {cut}", String::from_utf8_lossy(sequence));
            }
        };
        for begins in [
            &b"\x1b[?2026h"[..],
            b"\x1b[?2026;25h",
            b"\x1b[?25;2026h",
            b"\x1b[?02026h",
            b"\x1b[?25;2026;7h",
        ] {
            cut_everywhere(begins, false, &[(Turn::Began, "before")]);
            // With one under way already, another beginning is no turn.
            cut_everywhere(begins, true, &[]);
        }
        for ends in [&b"\x1b[?2026l"[..], b"\x1b[?2026;25l", b"\x1b[?7;2026l", b"\x1b[?02026l"] {
            cut_everywhere(ends, true, &[(Turn::Ended, "before")]);
            cut_everywhere(ends, false, &[]);
        }
        for other in [
            &b"\x1b[?25h"[..],
            b"\x1b[?1049l",
            b"\x1b[2026h",
            b"\x1b[?1;2004h",
            b"\x1b[?2004;1l",
            b"\x1b[4l",
        ] {
            cut_everywhere(other, false, &[]);
            cut_everywhere(other, true, &[]);
        }
        // Cut twice, with many parameters, and with something other than a
        // parameter in the middle.
        let turn = [(Turn::Began, "before".to_string())];
        assert_eq!(stops(&[b"before \x1b[?", b"20", b"26;1h after"], false), turn);
        assert_eq!(stops(&[b"before \x1b[?", b"20", b"04;1h after"], false), []);
        let long = [&b"before \x1b[?1"[..], &b";1".repeat(20), b";2026h after"].concat();
        assert_eq!(stops(&[&long[..30], &long[30..]], false), turn);
        // One right after another: each is a stop of its own.
        let got = stops(&[b"a\x1b[?2026hb\x1b[?2026l\x1b[?2026hc\x1b[?2026ld"], false);
        let want = [(Turn::Began, "a"), (Turn::Ended, "ab"), (Turn::Began, "ab"), (Turn::Ended, "abc")];
        assert_eq!(got.len(), want.len());
        for ((turn, row), (want_turn, want_row)) in got.iter().zip(want) {
            assert_eq!((*turn, row.as_str()), (want_turn, want_row));
        }

    }

    #[test]
    fn an_update_that_never_ends_has_a_time_limit_from_its_latest_beginning() {
        let mut fast = term(40, 4);
        let mut feeder = Feeder::new();
        feeder.advance(&mut fast, b"shown\x1b[?2026hnot yet");
        let first = feeder.sync_deadline().expect("a limit");
        assert!(first > Instant::now() && first <= Instant::now() + SYNC_TIMEOUT);
        // More output inside it does not move the limit.
        feeder.advance(&mut fast, b" more");
        assert_eq!(feeder.sync_deadline(), Some(first));
        // Another beginning does, spelled either way.
        std::thread::sleep(Duration::from_millis(2));
        feeder.advance(&mut fast, b"\x1b[?2026h");
        let second = feeder.sync_deadline().expect("a limit");
        assert!(second > first);
        std::thread::sleep(Duration::from_millis(2));
        feeder.advance(&mut fast, b"\x1b[?25;2026h");
        assert!(feeder.sync_deadline().expect("a limit") > second);
        // Its end takes the limit away, and is noted once.
        assert!(!feeder.take_ended());
        feeder.advance(&mut fast, b"\x1b[?2026l");
        assert!(feeder.sync_deadline().is_none() && !fast.sync_update());
        assert!(feeder.take_ended() && !feeder.take_ended());
        // So does being ended for lasting too long.
        feeder.advance(&mut fast, b"\x1b[?2026h");
        feeder.stop_sync(&mut fast);
        assert!(feeder.sync_deadline().is_none() && !fast.sync_update());
    }

    #[test]
    fn a_look_ahead_names_the_sequence_after_the_text_or_none() {
        let stale = Csi {
            at: 7,
            len: 3,
            count: 1,
            last: b'm',
        };
        let mut params = [0; MAX_PARAMS];

        // A sequence carried out directly: where it starts, counted from the text, and
        // what it says.
        let mut next = stale;
        assert_eq!(direct_text(b"red\x1b[1;31mmore", &mut params, 80, &mut next), 3);
        assert_eq!((next.at, next.len, next.count, next.last), (3, 7, 2, b'm'));
        assert_eq!(params[..2], [1, 31]);
        next.from(100);
        assert_eq!(next.at, 103);

        // Whatever else follows the text, or nothing, leaves none: a line end, a sequence
        // that is the parser's, text in another script, the end of the output, the end of
        // the row.
        for (bytes, fits) in [
            (&b"plain text\r\n"[..], 80),
            (b"a run long enough..\x1b[?25h", 80),
            (b"caf\xc3\xa9", 80),
            (b"the end", 80),
            (b"longer than the row", 4),
            (b"\x1b[31m", 80),
            (b"", 80),
        ] {
            let mut next = stale;
            direct_text(bytes, &mut params, fits, &mut next);
            assert_eq!(next.at, Csi::NOWHERE, "{:?}", String::from_utf8_lossy(bytes));
            // And none it stays, wherever the text is counted from.
            next.from(12);
            assert_eq!(next.at, Csi::NOWHERE);
        }
    }

    #[test]
    fn printable_runs_are_measured() {
        assert_eq!(printable_run(b""), 0);
        assert_eq!(printable_run(b"\n"), 0);
        assert_eq!(printable_run(b"abc\n"), 3);
        assert_eq!(printable_run(b"0123456789abcdef0123\x1b"), 20);
        assert_eq!(printable_run(b"0123456789abcdef"), 16);
        assert_eq!(printable_run(b"0123456789abcde\x7f"), 15);
        assert_eq!(printable_run("0123456789abcdefgh\u{e9}".as_bytes()), 18);
    }
}
