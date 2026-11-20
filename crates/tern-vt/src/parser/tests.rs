//! Randomized chunk-invariance and golden-digest tests for [Parser].
//! Each generated stream is fed whole, byte-by-byte, and in random chunks;
//! every chunking must produce identical [Handler] calls, and the whole-stream
//! call log is verified against `parser.digests`.
//!
//! `TERN_PARSER_SEEDS` sets how many streams are tried (3,000 by default),
//! and `TERN_PARSER_SEED` tries the one stream of that seed alone.

use std::collections::BTreeSet;

use super::{Handler, Parser};
use crate::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, PrivateMode, Rgb, StandardCharset,
    TabulationClearMode,
};

/// Records a call as its name and `Debug`-formatted argument tuple for
/// FNV-1a digest verification against `parser.digests`.
macro_rules! record {
    ($log:expr, $name:literal $(, $arg:expr)*) => {
        $log.calls.push(format!("{} {:?}", $name, ($($arg,)*)))
    };
}

/// The calls this parser makes.
#[derive(Default)]
struct Log {
    calls: Vec<String>,
}

impl Handler for Log {
    fn set_title(&mut self, title: Option<String>) {
        record!(self, "set_title", title);
    }

    fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        record!(self, "set_cursor_style", style);
    }

    fn set_cursor_shape(&mut self, shape: CursorShape) {
        record!(self, "set_cursor_shape", shape);
    }

    fn input(&mut self, c: char) {
        record!(self, "input", c);
    }

    fn goto(&mut self, line: i32, column: usize) {
        record!(self, "goto", line, column);
    }

    fn goto_line(&mut self, line: i32) {
        record!(self, "goto_line", line);
    }

    fn goto_col(&mut self, column: usize) {
        record!(self, "goto_col", column);
    }

    fn insert_blank(&mut self, count: usize) {
        record!(self, "insert_blank", count);
    }

    fn move_up(&mut self, lines: usize) {
        record!(self, "move_up", lines);
    }

    fn move_down(&mut self, lines: usize) {
        record!(self, "move_down", lines);
    }

    fn identify_terminal(&mut self, intermediate: Option<char>) {
        record!(self, "identify_terminal", intermediate);
    }

    fn device_status(&mut self, report: usize) {
        record!(self, "device_status", report);
    }

    fn move_forward(&mut self, columns: usize) {
        record!(self, "move_forward", columns);
    }

    fn move_backward(&mut self, columns: usize) {
        record!(self, "move_backward", columns);
    }

    fn move_down_and_cr(&mut self, lines: usize) {
        record!(self, "move_down_and_cr", lines);
    }

    fn move_up_and_cr(&mut self, lines: usize) {
        record!(self, "move_up_and_cr", lines);
    }

    fn put_tab(&mut self, count: u16) {
        record!(self, "put_tab", count);
    }

    fn backspace(&mut self) {
        record!(self, "backspace");
    }

    fn carriage_return(&mut self) {
        record!(self, "carriage_return");
    }

    fn linefeed(&mut self) {
        record!(self, "linefeed");
    }

    fn bell(&mut self) {
        record!(self, "bell");
    }

    fn substitute(&mut self) {
        record!(self, "substitute");
    }

    fn set_horizontal_tabstop(&mut self) {
        record!(self, "set_horizontal_tabstop");
    }

    fn scroll_up(&mut self, lines: usize) {
        record!(self, "scroll_up", lines);
    }

    fn scroll_down(&mut self, lines: usize) {
        record!(self, "scroll_down", lines);
    }

    fn insert_blank_lines(&mut self, count: usize) {
        record!(self, "insert_blank_lines", count);
    }

    fn delete_lines(&mut self, count: usize) {
        record!(self, "delete_lines", count);
    }

    fn erase_chars(&mut self, count: usize) {
        record!(self, "erase_chars", count);
    }

    fn delete_chars(&mut self, count: usize) {
        record!(self, "delete_chars", count);
    }

    fn move_backward_tabs(&mut self, count: u16) {
        record!(self, "move_backward_tabs", count);
    }

    fn move_forward_tabs(&mut self, count: u16) {
        record!(self, "move_forward_tabs", count);
    }

    fn save_cursor_position(&mut self) {
        record!(self, "save_cursor_position");
    }

    fn restore_cursor_position(&mut self) {
        record!(self, "restore_cursor_position");
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        record!(self, "clear_line", mode);
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        record!(self, "clear_screen", mode);
    }

    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        record!(self, "clear_tabs", mode);
    }

    fn reset_state(&mut self) {
        record!(self, "reset_state");
    }

    fn reverse_index(&mut self) {
        record!(self, "reverse_index");
    }

    fn terminal_attribute(&mut self, attr: Attr) {
        record!(self, "terminal_attribute", attr);
    }

    fn set_mode(&mut self, mode: Mode) {
        record!(self, "set_mode", mode);
    }

    fn unset_mode(&mut self, mode: Mode) {
        record!(self, "unset_mode", mode);
    }

    fn report_mode(&mut self, mode: Mode) {
        record!(self, "report_mode", mode);
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        record!(self, "set_private_mode", mode);
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        record!(self, "unset_private_mode", mode);
    }

    fn report_private_mode(&mut self, mode: PrivateMode) {
        record!(self, "report_private_mode", mode);
    }

    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        record!(self, "set_scrolling_region", top, bottom);
    }

    fn set_keypad_application_mode(&mut self) {
        record!(self, "set_keypad_application_mode");
    }

    fn unset_keypad_application_mode(&mut self) {
        record!(self, "unset_keypad_application_mode");
    }

    fn set_active_charset(&mut self, index: CharsetIndex) {
        record!(self, "set_active_charset", index);
    }

    fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
        record!(self, "configure_charset", index, charset);
    }

    fn set_color(&mut self, index: usize, color: Rgb) {
        record!(self, "set_color", index, color);
    }

    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        record!(self, "dynamic_color_sequence", prefix, index, terminator);
    }

    fn reset_color(&mut self, index: usize) {
        record!(self, "reset_color", index);
    }

    fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]) {
        record!(self, "clipboard_store", clipboard, base64);
    }

    fn clipboard_load(&mut self, clipboard: u8, terminator: &str) {
        record!(self, "clipboard_load", clipboard, terminator);
    }

    fn decaln(&mut self) {
        record!(self, "decaln");
    }

    fn push_title(&mut self) {
        record!(self, "push_title");
    }

    fn pop_title(&mut self) {
        record!(self, "pop_title");
    }

    fn text_area_size_pixels(&mut self) {
        record!(self, "text_area_size_pixels");
    }

    fn text_area_size_chars(&mut self) {
        record!(self, "text_area_size_chars");
    }

    fn set_hyperlink(&mut self, link: Option<Hyperlink>) {
        record!(self, "set_hyperlink", link);
    }

    fn report_keyboard_mode(&mut self) {
        record!(self, "report_keyboard_mode");
    }

    fn push_keyboard_mode(&mut self, modes: KeyboardModes) {
        record!(self, "push_keyboard_mode", modes);
    }

    fn pop_keyboard_modes(&mut self, count: u16) {
        record!(self, "pop_keyboard_modes", count);
    }

    fn set_keyboard_mode(&mut self, modes: KeyboardModes, apply: KeyboardModesApplyBehavior) {
        record!(self, "set_keyboard_mode", modes, apply);
    }
}

/// Every method of [Handler] that a sequence can call.
const METHODS: &[&str] = &[
    "set_title", "set_cursor_style", "set_cursor_shape", "input", "goto", "goto_line",
    "goto_col", "insert_blank", "move_up", "move_down", "identify_terminal", "device_status",
    "move_forward", "move_backward", "move_down_and_cr", "move_up_and_cr", "put_tab",
    "backspace", "carriage_return", "linefeed", "bell", "substitute",
    "set_horizontal_tabstop", "scroll_up", "scroll_down", "insert_blank_lines", "delete_lines",
    "erase_chars", "delete_chars", "move_backward_tabs", "move_forward_tabs",
    "save_cursor_position", "restore_cursor_position", "clear_line", "clear_screen",
    "clear_tabs", "reset_state", "reverse_index", "terminal_attribute", "set_mode",
    "unset_mode", "report_mode", "set_private_mode", "unset_private_mode",
    "report_private_mode", "set_scrolling_region", "set_keypad_application_mode",
    "unset_keypad_application_mode", "set_active_charset", "configure_charset", "set_color",
    "dynamic_color_sequence", "reset_color", "clipboard_store", "clipboard_load", "decaln",
    "push_title", "pop_title", "text_area_size_pixels", "text_area_size_chars", "set_hyperlink",
    "report_keyboard_mode", "push_keyboard_mode", "pop_keyboard_modes", "set_keyboard_mode"
];

/// How the call that begins a synchronized update is recorded.
const SYNC_BEGINS: &str = "set_private_mode (Named(SyncUpdate),)";

/// What this parser calls for `stream` cut into pieces that end at `ends`.
/// After each piece [Parser::take_began] must say whether the piece began a
/// synchronized update; the pieces where it does not are returned too.
fn tern_calls(stream: &[u8], ends: &[usize]) -> (Vec<String>, Vec<usize>) {
    let mut parser = Parser::new();
    let mut log = Log::default();
    let mut wrong = Vec::new();
    let mut start = 0;
    for (piece, &end) in ends.iter().enumerate() {
        let before = log.calls.len();
        parser.advance(&mut log, &stream[start..end]);
        let began = log.calls[before..].iter().any(|call| call == SYNC_BEGINS);
        if parser.take_began() != began {
            wrong.push(piece);
        }
        start = end;
    }
    (log.calls, wrong)
}

/// Golden digests of whole-stream [Handler] call logs across 3,000 seeds plus
/// `long-hash`.
const RECORDED: &str = include_str!("parser.digests");

/// The record of `key`'s line, after the key.
fn recorded(key: &str) -> Option<&'static str> {
    let key = format!("{key} ");
    RECORDED.lines().find_map(|line| line.strip_prefix(key.as_str()))
}

#[test]
fn matches_recorded_digests_however_cut() {
    let number = |name: &str| std::env::var(name).ok().map(|n| n.parse::<u64>().expect(name));
    let seeds = match number("TERN_PARSER_SEED") {
        Some(seed) => seed..seed + 1,
        None => 0..number("TERN_PARSER_SEEDS").unwrap_or(3000),
    };
    let all = seeds.end - seeds.start > 1;
    let mut called = BTreeSet::new();
    let (mut cuttings, mut bytes, mut held, mut total) = (0, 0, 0, 0);
    for seed in seeds {
        let mut rng = Rng::new(seed);
        let stream = stream(&mut rng);
        let len = stream.len();
        let (whole, wrong) = tern_calls(&stream, &[len]);
        assert!(wrong.is_empty(), "seed {seed}, in one piece: take_began wrong");
        // Past the seeds recorded, the cuttings are held to one another.
        if let Some(record) = recorded(&seed.to_string()) {
            let begins = whole.iter().filter(|call| *call == SYNC_BEGINS).count();
            let mine = format!("{:016x} {} {begins}", digest(&whole), whole.len());
            assert_eq!(
                mine,
                record,
                "seed {seed}: parser digest mismatch\nstream: {}",
                stream.escape_ascii()
            );
            held += 1;
        }
        called.extend(whole.iter().map(|call| call.split(' ').next().unwrap().to_owned()));
        bytes += len;
        total += whole.len();
        let mut ways = vec![("a byte at a time", (1..=len).collect::<Vec<_>>())];
        for mean in [3, 40] {
            let mut ends = Vec::new();
            let mut at = 0;
            while at < len {
                at = (at + 1 + rng.below(2 * mean)).min(len);
                ends.push(at);
            }
            ways.push(("in random pieces", ends));
        }
        for (way, ends) in ways {
            let (got, wrong) = tern_calls(&stream, &ends);
            cuttings += 1;
            if got != whole {
                let at = got.iter().zip(&whole).take_while(|(a, b)| a == b).count();
                let around = |calls: &[String]| calls[at.saturating_sub(3)..(at + 3).min(calls.len())].to_vec();
                panic!(
                    "seed {seed}, {way}: call {at} differs\nwhole: {:#?}\n  cut: {:#?}\nstream: {}",
                    around(&whole),
                    around(&got),
                    stream.escape_ascii(),
                );
            }
            assert!(wrong.is_empty(), "seed {seed}, {way}: take_began wrong after pieces {wrong:?}");
        }
    }
    if all {
        let missing: Vec<_> =
            METHODS.iter().filter(|&&name| !called.contains(name)).collect();
        assert!(missing.is_empty(), "no stream called {missing:?}");
    }
    eprintln!(
        "{held} streams verified against digests, {cuttings} chunkings identical: {bytes} bytes, {total} calls"
    );
}

/// The calls the parser makes for `pieces`, given one after another.
fn calls(pieces: &[&[u8]]) -> Vec<String> {
    let mut parser = Parser::new();
    let mut log = Log::default();
    for piece in pieces {
        parser.advance(&mut log, piece);
    }
    log.calls
}

/// The calls that show each character of `text`.
fn shown(text: &str) -> Vec<String> {
    text.chars().map(|c| format!("input ({c:?},)")).collect()
}

/// Multi-byte UTF-8 ("né à") split mid-codepoint preserves following ASCII.
#[test]
fn split_character_loses_nothing() {
    let text = "né à".as_bytes();
    let whole = calls(&[text]);
    assert_eq!(whole, shown("né à"));
    for cut in 0..=text.len() {
        assert_eq!(calls(&[&text[..cut], &text[cut..]]), whole, "cut at {cut}");
    }
}

/// Text of every kind, valid and not, reads the same cut anywhere, twice.
#[test]
fn split_anything_reads_as_whole() {
    // Characters of every length, C1 controls spelled in UTF-8, invalid
    // and cut-off characters, one cut off by an escape.
    let text: &[u8] = b"a\xc3\xa9\xe4\xb8\xad\xf0\x9f\x98\x80b\xc2\x85\xe4\xbf\x1b[1m\xed\xa0\x80\
        \xf0\x9f\xc3\xa9\x80\xa0\xc0\xaf\xf4\x90\x80\x80z\xe4\xbf";
    let whole = calls(&[text]);
    assert_eq!(Some(format!("{:016x}", digest(&whole)).as_str()), recorded("split-anything"));
    for a in 0..=text.len() {
        for b in a..=text.len() {
            assert_eq!(calls(&[&text[..a], &text[a..b], &text[b..]]), whole, "cut at {a} and {b}");
        }
    }
    let bytes: Vec<&[u8]> = text.chunks(1).collect();
    assert_eq!(calls(&bytes), whole);
}

/// REP repeats a character a caller says it showed without the parser.
#[test]
fn rep_repeats_what_a_caller_showed() {
    let mut parser = Parser::new();
    let mut log = Log::default();
    // Nothing has been shown, so there is nothing to repeat.
    parser.advance(&mut log, b"\x1b[2b");
    assert!(log.calls.is_empty());

    parser.set_preceding('x');
    parser.advance(&mut log, b"\x1b[3b");
    assert_eq!(log.calls, shown("xxx"));

    // A character the parser shows takes its place.
    log.calls.clear();
    parser.advance(&mut log, b"y\x1b[b");
    assert_eq!(log.calls, shown("yy"));

    // And a control in between changes nothing.
    log.calls.clear();
    parser.set_preceding('z');
    parser.advance(&mut log, b"\n\x1b[2b");
    assert_eq!(log.calls, ["linefeed ()", "input ('z',)", "input ('z',)"]);
}

/// What `is_ground` and `take_began` say along the way.
#[test]
fn ground_and_began() {
    let mut parser = Parser::new();
    let mut log = Log::default();
    assert!(parser.is_ground());
    for (bytes, ground) in [(&b"a\x1b"[..], false), (b"[", false), (b"m", true), (b"\xc3", false), (b"\xa9", true)] {
        parser.advance(&mut log, bytes);
        assert_eq!(parser.is_ground(), ground, "after {}", bytes.escape_ascii());
    }

    assert!(!parser.take_began());
    parser.advance(&mut log, b"\x1b[?25;2026h");
    assert!(parser.take_began());
    assert!(!parser.take_began());
    parser.advance(&mut log, b"\x1b[?2026l\x1b[2026h\x1b[?2026$p");
    assert!(!parser.take_began());
}

/// `#` colors with 17+ hex digits per channel wrap shifts deterministically.
#[test]
fn long_hash_colors_wrap_deterministically() {
    let stream = long_hash_colors();
    let tern = calls(&[&stream]);
    assert!(tern.iter().filter(|call| call.starts_with("set_color")).count() > 30);
    assert_eq!(Some(format!("{:016x}", digest(&tern)).as_str()), recorded("long-hash"));
}

/// A small random number generator (xorshift64*), so that every seed gives
/// the same stream on every run.
struct Rng(u64);

impl Rng {
    /// The generator for `seed`.
    fn new(seed: u64) -> Rng {
        let mut rng = Rng((seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xd1b5_4a32_d192_ed03).max(1));
        rng.next();
        rng
    }

    /// The next number, of 64 random bits.
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number below `n`.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// True once in `n` times.
    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    /// A number from `low` to `high`, both included.
    fn between(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }

    /// One of `items`.
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

/// The byte that begins every escape sequence.
const ESC: u8 = 0x1b;

/// A stream of output: text and sequences of every kind, well and badly
/// formed, one after another.
fn stream(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..rng.between(1, 60) {
        match rng.below(100) {
            0..=21 => ascii(rng, &mut out),
            22..=31 => utf8(rng, &mut out),
            32..=37 => invalid_utf8(rng, &mut out),
            38..=43 => control(rng, &mut out),
            44..=54 => csi_random(rng, &mut out),
            55..=62 => csi_known(rng, &mut out),
            63..=69 => sgr(rng, &mut out),
            70..=73 => rep(rng, &mut out),
            74..=79 => esc(rng, &mut out),
            80..=88 => osc(rng, &mut out),
            89..=91 => dcs(rng, &mut out),
            92..=93 => sos_pm_apc(rng, &mut out),
            94..=96 => sync(rng, &mut out),
            _ => random_bytes(rng, &mut out, 12),
        }
    }
    out
}

/// Printable ASCII, now and then with DEL.
fn ascii(rng: &mut Rng, out: &mut Vec<u8>) {
    for _ in 0..rng.between(1, 20) {
        out.push(if rng.one_in(40) { 0x7f } else { rng.between(0x20, 0x7e) as u8 });
    }
}

/// Characters of every length, from the ends of the ranges of each length,
/// joining ones, C1 controls, and any at all.
const SAMPLES: &[char] = &[
    'é', 'à', 'ß', '中', '文', '😀', '🦀', '\u{301}', '\u{200d}', '\u{fe0f}', '\u{85}', '\u{9b}',
    '\u{80}', '\u{9f}', '\u{a0}', '\u{fffd}', '\u{10ffff}', '\u{7ff}', '\u{800}', '\u{ffff}',
    '\u{10000}', '\u{2028}', '\u{3000}', '\u{d7ff}', '\u{e000}',
];

/// One to six characters, of the samples or any.
fn utf8(rng: &mut Rng, out: &mut Vec<u8>) {
    for _ in 0..rng.between(1, 6) {
        let c = if rng.one_in(4) {
            char::from_u32(rng.below(0x11_0000) as u32).unwrap_or('x')
        } else {
            rng.pick(SAMPLES)
        };
        out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
    }
}

/// Bytes that are not UTF-8: lone continuation bytes, characters cut off,
/// spelled too long, surrogates, past the last character, and bytes that
/// never begin one.
fn invalid_utf8(rng: &mut Rng, out: &mut Vec<u8>) {
    const BAD: &[&[u8]] = &[
        b"\x80", b"\xbf", b"\x9b", b"\xa0", b"\xc0\x80", b"\xc1\xbf", b"\xc2", b"\xe0\x80\x80",
        b"\xe0\x9f\xbf", b"\xe4\xbf", b"\xe4", b"\xed\xa0\x80", b"\xed\xbf\xbf", b"\xef\xbf",
        b"\xf0\x80\x80\x80", b"\xf0\x8f\xbf\xbf", b"\xf0\x9f", b"\xf0\x9f\x98", b"\xf4\x90\x80\x80",
        b"\xf5", b"\xf8\x88\x80\x80\x80", b"\xfe", b"\xff", b"\xc3\x28", b"\xe2\x28\xa1",
        b"\xf0\x28\x8c\xbc",
    ];
    if rng.one_in(3) {
        // The start of a character whose end never comes.
        let c = rng.pick(&['é', '中', '😀', '\u{10ffff}', '\u{85}']);
        let mut bytes = [0; 4];
        let len = c.encode_utf8(&mut bytes).len();
        out.extend_from_slice(&bytes[..rng.between(1, len - 1)]);
    } else {
        out.extend_from_slice(rng.pick(BAD));
    }
}

/// A C0 control, DEL, or a C1 control as a byte or a character.
fn control(rng: &mut Rng, out: &mut Vec<u8>) {
    match rng.below(10) {
        0 => out.push(0x7f),
        1 => out.push(rng.between(0x80, 0x9f) as u8),
        2 => out.extend_from_slice(&[0xc2, rng.between(0x80, 0x9f) as u8]),
        3 => out.push(rng.pick(&[0x18, 0x1a, ESC])),
        _ => out.push(rng.below(0x20) as u8),
    }
}

/// A number for a parameter: none, small, one that means something, or
/// past what a parameter holds.
fn number(rng: &mut Rng, out: &mut Vec<u8>, small: bool) {
    let n = if small {
        if rng.one_in(5) {
            return;
        }
        rng.below(6).to_string()
    } else {
        match rng.below(20) {
            0 => return,
            1 => "65535".to_owned(),
            2 => "65536".to_owned(),
            3 => "99999999999999999999".to_owned(),
            4 => rng
                .pick(&[2026, 1049, 2004, 25, 1000, 1006, 14, 18, 22, 23, 38, 48, 58, 4, 5, 2, 255, 256, 300])
                .to_string(),
            5..=9 => rng.below(301).to_string(),
            _ => rng.below(10).to_string(),
        }
    };
    out.extend_from_slice(n.as_bytes());
}

/// Recognized CSI final bytes.
const FINALS: &[u8] = b"@ABCDEFGHIJKLMPSTXZabcdefghlmnqrstux`pWk";

/// Bytes that break into a sequence: controls carried out in its middle,
/// those that end it, DEL, and bytes above ASCII.
const BREAKS: &[u8] = &[0x00, 0x07, 0x08, 0x0a, 0x0d, 0x0e, 0x1f, 0x18, 0x1a, ESC, 0x7f, 0x9c, 0xc3, 0xff];

/// A control sequence put together at random.
fn csi_random(rng: &mut Rng, out: &mut Vec<u8>) {
    let final_byte = match rng.below(30) {
        0 => None,
        1..=4 => Some(rng.between(0x40, 0x7e) as u8),
        _ => Some(rng.pick(FINALS)),
    };
    // A long repeat is a great many calls; only `rep` asks for one.
    let small = final_byte == Some(b'b');
    let mut seq = vec![ESC, b'['];
    if rng.one_in(3) {
        seq.push(rng.pick(b"?><="));
    }
    if rng.one_in(25) {
        seq.push(rng.pick(b"?><="));
    }
    let count = if rng.one_in(12) { rng.between(28, 40) } else { rng.below(7) };
    for k in 0..count {
        if k > 0 {
            seq.push(if rng.one_in(5) { b':' } else { b';' });
        }
        number(rng, &mut seq, small);
    }
    if rng.one_in(6) {
        for _ in 0..rng.between(1, 3) {
            seq.push(rng.pick(b" $!\"'"));
        }
        if rng.one_in(10) {
            seq.push(rng.pick(b"1;?"));
        }
    }
    if rng.one_in(12) {
        let at = rng.between(2, seq.len());
        seq.insert(at, rng.pick(BREAKS));
    }
    seq.extend(final_byte);
    out.extend_from_slice(&seq);
}

/// Control sequences in the forms the terminal knows, `#` standing for a
/// number.
const KNOWN: &[&str] = &[
    "?#h", "?#l", "#h", "#l", "h", "?2026h", "?2026l", "?2026;25h", "?25;2026h", "?2026:1h",
    "?1049h", "?1049l", "?2004h", "?1;3;6;7;12;25h", "=#;#u", "=#u", ">#u", "<#u", "<u", "?u", "u",
    "s", "# q", " q", "#$p", "?#$p", "$p", "#t", "14t", "18t", "22t", "23t", "#g", "3g", "#J",
    "#K", "#;#r", "r", ";#r", "#;#H", "#;#f", "H", ">c", "c", "?c", "1c", ">1c", "#n", "6n", "#@",
    "#A", "#B", "#C", "#D", "#E", "#F", "#G", "#`", "#a", "#d", "#e", "#I", "#Z", "#L", "#M", "#P",
    "#S", "#T", "#X", "?5W", "#k", "1 k", "?4m", ">4;#m", "#:#H", "#;#;#H", "?#;#$p",
];

/// One of the known forms, its numbers filled in.
fn csi_known(rng: &mut Rng, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    for &b in rng.pick(KNOWN).as_bytes() {
        if b == b'#' {
            let small = !rng.one_in(6);
            number(rng, out, small);
        } else {
            out.push(b);
        }
    }
}

/// SGR with every kind of parameter: plain ones, colors after `;` and after
/// `:`, whole and cut short, and underline kinds.
fn sgr(rng: &mut Rng, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    let byte = |rng: &mut Rng| if rng.one_in(10) { rng.between(256, 300) } else { rng.below(256) };
    for k in 0..rng.between(1, 6) {
        if k > 0 {
            out.push(b';');
        }
        let base = rng.pick(&[38, 48, 58]);
        let part = match rng.below(14) {
            0..=3 => rng
                .pick(&[
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 21, 22, 23, 24, 25, 27, 28, 29, 30, 37, 39, 40, 47,
                    49, 59, 90, 97, 100, 107,
                ])
                .to_string(),
            4 => rng.below(120).to_string(),
            5 => format!("{base};5;{}", byte(rng)),
            6 => format!("{base};2;{};{};{}", byte(rng), byte(rng), byte(rng)),
            7 => format!("{base};2;{}", byte(rng)),
            8 => format!("{base}:5:{}", byte(rng)),
            9 => format!("{base}:2::{}:{}:{}", byte(rng), byte(rng), byte(rng)),
            10 => format!("{base}:2:{}:{}:{}", byte(rng), byte(rng), byte(rng)),
            11 => format!("{base}:{}:{}:{}:{}:{}", rng.below(6), rng.below(3), byte(rng), byte(rng), byte(rng)),
            12 => format!("4:{}", rng.below(7)),
            _ => rng.pick(&["4:", "4:3:1", "38", "48;5", "58:2", ":", "0:1", "38;5;1:2"]).to_owned(),
        };
        out.extend_from_slice(part.as_bytes());
    }
    out.push(b'm');
}

/// REP after text, after a control, after nothing.
fn rep(rng: &mut Rng, out: &mut Vec<u8>) {
    match rng.below(6) {
        0 | 1 => ascii(rng, out),
        2 => utf8(rng, out),
        3 => control(rng, out),
        4 => invalid_utf8(rng, out),
        _ => (),
    }
    out.extend_from_slice(b"\x1b[");
    if rng.one_in(1000) {
        out.extend_from_slice(rng.pick(&[&b"65535"[..], b"99999"]));
    } else if !rng.one_in(5) {
        out.extend_from_slice(rng.below(6).to_string().as_bytes());
    }
    out.push(b'b');
}

/// Escape sequences: character sets, the ones the terminal knows, and any.
fn esc(rng: &mut Rng, out: &mut Vec<u8>) {
    out.push(ESC);
    match rng.below(9) {
        0 | 1 => {
            out.push(rng.pick(b"()*+"));
            out.push(if rng.one_in(4) { rng.pick(b"A<%5") } else { rng.pick(b"0B") });
        },
        2 | 3 => out.push(rng.pick(b"78=>DEHMcZ\\")),
        4 => out.extend_from_slice(rng.pick(&[&b"#8"[..], b"#3", b"(#0", b"#(8", b" 8"])),
        5 => out.push(rng.between(0x20, 0x7e) as u8),
        6 => {
            for _ in 0..rng.between(2, 3) {
                out.push(rng.between(0x20, 0x2f) as u8);
            }
            out.push(rng.between(0x30, 0x7e) as u8);
        },
        7 => out.push(rng.pick(&[0x7f, 0x80, 0x9c, 0xc3, 0xff, ESC])),
        _ => out.push(rng.pick(&[0x07, 0x0a, 0x0d, 0x18, 0x1a, 0x00])),
    }
}

/// Hex digits, upper and lower case.
const HEX: &[u8] = b"0123456789abcdefABCDEF";

/// A color as OSC 4 and 10 to 12 take one, or something like one.
fn color_spec(rng: &mut Rng, out: &mut Vec<u8>) {
    match rng.below(8) {
        0 | 1 => {
            out.push(b'#');
            // Cap generated hex length so 3 channels fit inside `usize::from_str_radix`.
            let len = if rng.one_in(10) { rng.below(41) } else { rng.pick(&[0, 1, 2, 3, 4, 5, 6, 9, 12]) };
            for _ in 0..len {
                out.push(if rng.one_in(30) { b'+' } else { rng.pick(HEX) });
            }
        },
        2 | 3 => {
            out.extend_from_slice(b"rgb:");
            let parts = if rng.one_in(6) { rng.pick(&[2, 4]) } else { 3 };
            for k in 0..parts {
                if k > 0 {
                    out.push(b'/');
                }
                for _ in 0..rng.below(6) {
                    out.push(if rng.one_in(30) { b'+' } else { rng.pick(HEX) });
                }
            }
        },
        4 => out.push(b'?'),
        5 => ascii(rng, out),
        6 => out.extend_from_slice("rgb:é/1/2".as_bytes()),
        _ => out.extend_from_slice(rng.pick(&[&b"#zzz"[..], b"#\xc3\xa9", b"rgbi:1/1/1", b"#-1-1-1"])),
    }
}

/// Text inside a string: printable, `;`, characters, invalid bytes, and
/// controls a string passes over.
fn payload(rng: &mut Rng, out: &mut Vec<u8>, len: usize) {
    for _ in 0..len {
        match rng.below(20) {
            0 => out.push(b';'),
            1 => utf8(rng, out),
            2 => invalid_utf8(rng, out),
            3 => out.push(rng.pick(&[0x01, 0x06, 0x08, 0x0a, 0x19, 0x1c, 0x1f, 0x7f, 0x9c])),
            4 => out.push(b' '),
            _ => out.push(rng.between(0x21, 0x7e) as u8),
        }
    }
}

/// An operating system command of every number the terminal carries out,
/// and others, ended in every way.
fn osc(rng: &mut Rng, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b]");
    let text = |rng: &mut Rng, out: &mut Vec<u8>| {
        let len = rng.below(12);
        payload(rng, out, len);
    };
    match rng.below(24) {
        0 | 1 => {
            out.extend_from_slice(rng.pick(&[&b"0"[..], b"2"]));
            if !rng.one_in(8) {
                out.push(b';');
                if rng.one_in(3) {
                    out.extend_from_slice(b"  ");
                }
                text(rng, out);
            }
        },
        2 | 3 => {
            out.push(b'4');
            let pairs = rng.below(4);
            for _ in 0..pairs {
                out.push(b';');
                if rng.one_in(8) {
                    out.extend_from_slice(rng.pick(&[&b""[..], b"x", b"300", b"1a"]));
                } else {
                    out.extend_from_slice(rng.below(256).to_string().as_bytes());
                }
                if !rng.one_in(10) {
                    out.push(b';');
                    color_spec(rng, out);
                }
            }
        },
        4 | 5 => {
            out.extend_from_slice(b"8;");
            out.extend_from_slice(rng.pick(&[
                &b""[..], b"id=a", b"id=link1:x=y", b"x=y:id=b", b"id=", b"id=\xff", b"x:id=c:id=d",
            ]));
            if !rng.one_in(8) {
                out.push(b';');
                if !rng.one_in(4) {
                    out.extend_from_slice(rng.pick(&[&b"http://x.org/"[..], b"file:///a;b", b"a\xffb", b";"]));
                    text(rng, out);
                }
            }
        },
        6..=8 => {
            out.extend_from_slice(rng.pick(&[&b"10"[..], b"11", b"12"]));
            for _ in 0..rng.below(5) {
                out.push(b';');
                color_spec(rng, out);
            }
        },
        9 => out.extend_from_slice(rng.pick(&[&b"22;pointer"[..], b"22;text", b"22"])),
        10 => {
            out.extend_from_slice(b"50;");
            out.extend_from_slice(rng.pick(&[
                &b"CursorShape=0"[..], b"CursorShape=1", b"CursorShape=2", b"CursorShape=3", b"CursorShape=",
                b"CursorShape", b"cursorshape=1", b"CursorShape=12",
            ]));
        },
        11..=13 => {
            out.extend_from_slice(b"52");
            if !rng.one_in(8) {
                out.push(b';');
                out.extend_from_slice(rng.pick(&[&b"c"[..], b"p", b"", b"s0", b"\xc3\xa9"]));
                if !rng.one_in(8) {
                    out.push(b';');
                    if rng.one_in(3) {
                        out.push(b'?');
                    } else {
                        text(rng, out);
                    }
                }
            }
        },
        14 | 15 => {
            out.extend_from_slice(b"104");
            if !rng.one_in(6) {
                for _ in 0..rng.between(1, 3) {
                    out.push(b';');
                    if rng.one_in(6) {
                        out.extend_from_slice(rng.pick(&[&b"x"[..], b"256", b""]));
                    } else {
                        out.extend_from_slice(rng.below(256).to_string().as_bytes());
                    }
                }
            }
        },
        16 => out.extend_from_slice(rng.pick(&[&b"110"[..], b"111", b"112", b"110;x", b"112;"])),
        17 => {
            // More parameters than are kept.
            out.extend_from_slice(rng.pick(&[&b"0"[..], b"4", b"104", b"10"]));
            for _ in 0..rng.between(14, 25) {
                out.push(b';');
                if rng.one_in(2) {
                    color_spec(rng, out);
                } else {
                    out.extend_from_slice(rng.below(20).to_string().as_bytes());
                }
            }
        },
        18 => {
            // Multi-kilobyte OSC payloads.
            out.extend_from_slice(rng.pick(&[&b"2;"[..], b"52;c;", b"8;;http://"]));
            let len = rng.between(1100, 3000);
            payload(rng, out, len);
        },
        19 => (),
        20 => out.push(b';'),
        _ => {
            out.extend_from_slice(rng.below(200).to_string().as_bytes());
            out.push(b';');
            text(rng, out);
        },
    }
    end_string(rng, out, true);
}

/// The end of a string: BEL (if `bell` ends it), ST spelled with ESC, ESC
/// and something else, CAN, SUB, or nothing yet.
fn end_string(rng: &mut Rng, out: &mut Vec<u8>, bell: bool) {
    match rng.below(20) {
        0..=7 if bell => out.push(0x07),
        0..=15 => out.extend_from_slice(b"\x1b\\"),
        16 => out.extend_from_slice(rng.pick(&[&b"\x1b[m"[..], b"\x1b7", b"\x1b"])),
        17 => out.push(0x18),
        18 => out.push(0x1a),
        _ => (),
    }
}

/// A device control string, its head well or badly formed, ended in every
/// way, a lone ST byte among them.
fn dcs(rng: &mut Rng, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1bP");
    for _ in 0..rng.below(5) {
        out.push(if rng.one_in(8) { rng.pick(BREAKS) } else { rng.between(0x20, 0x3f) as u8 });
    }
    if !rng.one_in(10) {
        out.push(rng.between(0x40, 0x7e) as u8);
    }
    let len = rng.below(20);
    payload(rng, out, len);
    if rng.one_in(4) {
        out.push(0x9c);
    } else {
        end_string(rng, out, false);
    }
}

/// SOS, PM and APC, which nothing but CAN, SUB and ESC end.
fn sos_pm_apc(rng: &mut Rng, out: &mut Vec<u8>) {
    out.push(ESC);
    out.push(rng.pick(b"X^_"));
    let len = rng.below(16);
    payload(rng, out, len);
    if rng.one_in(4) {
        out.push(rng.pick(&[0x07, 0x9c]));
    }
    end_string(rng, out, false);
}

/// The beginning and end of a synchronized update, spelled in several ways.
fn sync(rng: &mut Rng, out: &mut Vec<u8>) {
    out.extend_from_slice(rng.pick(&[
        &b"\x1b[?2026h"[..], b"\x1b[?2026l", b"\x1b[?2026;2026h", b"\x1b[?1;2026h", b"\x1b[?2026$p",
        b"\x1b[2026h", b"\x1b[?2026h\x1b[?2026l",
    ]));
}

/// Bytes of any value.
fn random_bytes(rng: &mut Rng, out: &mut Vec<u8>, most: usize) {
    for _ in 0..rng.between(1, most) {
        out.push(rng.below(256) as u8);
    }
}

/// `#` color specs with 17–40 hex digits per channel.
fn long_hash_colors() -> Vec<u8> {
    let mut stream = Vec::new();
    for len in [17, 18, 20, 31, 32, 40] {
        for last in ["f", "ab", "7c", "1", "+9"] {
            let stretch = format!("{}{last}", "0".repeat(len - last.len()));
            let color = format!("#{stretch}{stretch}{stretch}");
            stream.extend_from_slice(format!("\x1b]4;1;{color}\x07\x1b]10;{color}0\x1b\\").as_bytes());
        }
    }
    stream
}

/// FNV-1a digest of serialized [Handler] calls.
fn digest(calls: &[String]) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    for call in calls {
        for &byte in call.as_bytes().iter().chain(b"\n") {
            digest = (digest ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    digest
}


#[test]
fn an_osc_too_long_is_dropped_and_its_memory_given_back() {
    let mut parser = Parser::new();
    let mut log = Log::default();
    parser.advance(&mut log, b"\x1b]0;");
    // In pieces, as output comes, and never ended by anything until here.
    let piece = vec![b'a'; 1 << 20];
    for _ in 0..(super::MAX_OSC_LEN >> 20) + 2 {
        parser.advance(&mut log, &piece);
    }
    assert!(parser.osc.capacity() <= super::MAX_OSC_LEN);
    parser.advance(&mut log, b"\x07\x1b]0;ok\x07");
    assert_eq!(log.calls, vec![format!("set_title {:?}", (Some("ok".to_owned()),))]);
    assert!(parser.osc.capacity() <= super::OSC_KEEP);

    // One just under the limit is carried out, and its memory given back.
    let long = "b".repeat(super::MAX_OSC_LEN - 2);
    log.calls.clear();
    parser.advance(&mut log, format!("\x1b]0;{long}\x07").as_bytes());
    assert_eq!(log.calls.len(), 1);
    assert!(parser.osc.capacity() <= super::OSC_KEEP);
}
