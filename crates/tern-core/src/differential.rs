//! Randomized state-machine and storage-regime regression tests for `tern-vt`.
//!
//! Each seed drives a terminal of random dimensions and scrollback capacity
//! through randomized escape streams, resizes, viewport scrolls, selections,
//! theme changes, and idle history settles—hashing frames, modes, PTY replies,
//! synchronized-update stops, selected text, and cell hyperlinks after every
//! step against `engine.digests` across default, tiny-page, and grown-ring
//! storage configurations.
//!
//! `TERN_DIFF_SEEDS=n` runs `n` seeds instead of the default count (seeds past
//! those recorded in `engine.digests` fuzz invariants and panic-freedom),
//! `TERN_DIFF_SEED=s` runs seed `s` alone, and `TERN_DIFF_LOG=1` prints every
//! step before a panic.

use tern_types::{CursorShape, Frame, Modes, Theme};
use tern_vt::engine::{
    CursorStyle, Event, Options, SelectionKind, Side, Terminal, Turn, ViewScroll, WindowSize,
};

use crate::blink::{self, BlinkModeScan};
use crate::io::{translate, Outgoing};
use crate::modes;
use crate::session::engine_shape;
use crate::snapshot::{self, TermState};

/// What the session would have sent or told the app.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Out {
    Write(Vec<u8>),
    Title(Option<String>),
    Bell,
    ClipboardStore(String),
    /// The reply to a request to read the clipboard, made with the text the
    /// app is assumed to give: [CLIPBOARD].
    ClipboardLoad(String),
}

/// What the app is assumed to have on the clipboard.
const CLIPBOARD: &str = "clip\u{e9}board";

/// The engine as it is, driven as the session drives it.
struct Live {
    state: TermState,
    /// The size of the ring and of a page: see [Storage].
    storage: Storage,
    blink_mode: BlinkModeScan,
    size: WindowSize,
    frame: Frame,
    events: Vec<Event>,
    scrollback: usize,
}

impl Live {
    fn new(cols: u16, rows: u16, scrollback: usize, storage: Storage) -> Live {
        let options = options(scrollback, CursorShape::Block, false, storage);
        Live {
            state: TermState::new(Terminal::new(&options, cols as usize, rows as usize), Theme::default()),
            storage,
            blink_mode: BlinkModeScan::default(),
            size: WindowSize { num_lines: rows, num_cols: cols, cell_width: 8, cell_height: 16 },
            frame: Frame::default(),
            events: Vec::new(),
            scrollback,
        }
    }

    /// As `io::interpret` interprets a piece.
    fn interpret(&mut self, piece: &[u8], watch: bool) -> Vec<(usize, Turn)> {
        let mut stops = Vec::new();
        let mut at = 0;
        while let Some(i) = self.blink_mode.find(&piece[at..]) {
            self.feed(piece, at, at + i, watch, &mut stops);
            let style = self.state.term.cursor_style();
            self.feed(piece, at + i, at + i + 1, watch, &mut stops);
            blink::keep_blinking(&mut self.state.term, style);
            at += i + 1;
        }
        self.feed(piece, at, piece.len(), watch, &mut stops);
        stops
    }

    fn feed(&mut self, piece: &[u8], from: usize, to: usize, watch: bool, stops: &mut Vec<(usize, Turn)>) {
        let mut at = from;
        while at < to {
            let (taken, turn) = self.state.term.advance_to_turn(&piece[at..to], watch);
            at += taken;
            stops.push((at, turn));
        }
    }

    /// As `io::collect` collects what the terminal reported.
    fn collect(&mut self) -> Vec<Out> {
        self.state.term.take_events(&mut self.events);
        let mut out = Vec::new();
        for event in self.events.drain(..) {
            if let Some(outgoing) = translate(event, &self.state, self.size) {
                out.push(match outgoing {
                    Outgoing::Write(bytes) => Out::Write(bytes),
                    Outgoing::Title(title) => Out::Title(title),
                    Outgoing::Bell => Out::Bell,
                    Outgoing::Notify(_, _) | Outgoing::Progress(_) => continue,
                    Outgoing::ClipboardStore(text) => Out::ClipboardStore(text),
                    Outgoing::ClipboardLoad(format) => Out::ClipboardLoad(format(CLIPBOARD)),
                });
            }
        }
        out
    }

    fn modes(&self) -> Modes {
        modes::unpack(modes::pack(self.state.term.mode()))
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        self.size.num_cols = cols;
        self.size.num_lines = rows;
        self.state.term.resize(cols as usize, rows as usize);
    }
}

/// How the engine keeps its scrollback: the number of the newest lines
/// kept as cells, and the size of a page of the packed ones.
#[derive(Clone, Copy, Debug)]
struct Storage(usize, usize);

/// As a session keeps it.
const USUAL: Storage = Storage(usize::MAX, tern_vt::engine::PAGE_BYTES);
/// Small enough that a short scrollback goes through the ring and across
/// pages, and that whole pages go as lines leave the scrollback.
const TINY: Storage = Storage(1, 256);
/// A ring that grows a little past the few dozen lines it keeps, into rows
/// of its own, and then packs into small pages.
const MIDDLE: Storage = Storage(40, 256);

fn options(scrollback_lines: usize, shape: CursorShape, blinking: bool, storage: Storage) -> Options {
    Options {
        scrollback_lines,
        cursor: CursorStyle { shape: engine_shape(shape), blinking },
        ring_rows: storage.0,
        page_bytes: storage.1,
    }
}

/// A small, fast generator of random numbers, the same on every machine.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn pick<'a, T: ?Sized>(&mut self, items: &[&'a T]) -> &'a T {
        items[self.below(items.len())]
    }
}

/// Text in the scripts and forms terminals meet: wide, joined, emoji with
/// their modifiers, box drawing, and what takes no cell.
const SCRIPTS: [&str; 9] = [
    "\u{4e2d}\u{6587}\u{5b57}\u{6f22}\u{8a9e}\u{65e5}\u{672c}\u{3042}\u{30a2}\u{d55c}",
    "\u{e9}\u{e0}\u{fc}\u{df}\u{f1}\u{141}\u{17c}\u{3b1}\u{3b2}\u{3c9}\u{436}\u{44f}",
    "\u{1f600}\u{1f680}\u{1f44d}\u{1f3fd}\u{2764}\u{fe0f}\u{1f469}\u{200d}\u{1f52c}\u{2728}",
    "\u{2500}\u{2502}\u{250c}\u{2510}\u{2514}\u{2518}\u{2588}\u{2591}\u{2580}\u{28ff}",
    "e\u{301}a\u{308}\u{323}\u{4e2d}\u{301}\u{ff46}\u{ff55}\u{ad}\u{85}\u{a0}\u{200b}",
    "a\u{4e2d}b\u{e9}c \u{6587}d\u{1f600}e ",
    "\u{e9} \u{e0} \u{fc}a\u{f1} ",
    "\u{4e2d}",
    "\u{301}\u{302}\u{20d7}\u{fe0f}\u{200d}",
];

/// Output of every kind a program sends: text in long and short runs and
/// in every script, line ends and controls, colors and styles, cursor
/// moves, erasing and inserting, scrolling regions, modes, the other screen,
/// synchronized updates, links, titles, colors set and asked for, the
/// clipboard, queries, strings that are ignored, and broken sequences and
/// stray bytes.
fn stream(rng: &mut Rng, tokens: usize) -> Vec<u8> {
    const TEXT: &[u8] = b"abcdefghijklmnopqrstuvwxyz ABCDEFGHIJ 0123456789 _`{|}~!@#$%";
    let mut out = Vec::new();
    for _ in 0..tokens {
        match rng.below(64) {
            0..=13 => {
                let len = *rng.pick(&[&1usize, &3, &9, &15, &16, &17, &40, &79, &80, &81, &200]);
                for _ in 0..len + rng.below(3) {
                    out.push(TEXT[rng.below(TEXT.len())]);
                }
            },
            14..=19 => out.extend_from_slice(rng.pick::<[u8]>(&[b"\n", b"\r\n", b"\r", b"\n\n", b"\r\n\r\n\r\n"])),
            20 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\t", b"\x08", b"\x07", b"\x7f", b"\x0b", b"\x0c", b"\x0e", b"\x0f", b"\x18", b"\x1a",
                b"\x00", b"\x05",
            ])),
            21..=24 => {
                let script: Vec<char> = SCRIPTS[rng.below(SCRIPTS.len())].chars().collect();
                let len = *rng.pick(&[&1usize, &2, &5, &9, &10, &19, &20, &21, &45, &90]);
                let mut utf8 = [0; 4];
                for _ in 0..len + rng.below(3) {
                    let c = script[rng.below(script.len())];
                    out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
                }
            },
            25 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\xff", b"\xc3", b"\xe4\xb8", b"\x9b", b"\x9c", b"\x80", b"\xf0\x9f", b"\xed\xa0\x80",
            ])),
            26..=28 => out.extend_from_slice(
                format!(
                    "\x1b[{}m",
                    rng.pick::<str>(&[
                        "1", "0", "31", "38;5;196", "48;2;1;2;3", "4:3", "7", "", "2", "3", "9", "8",
                        "92;41", "38:2::10:20:30", "21", "53", "4:0", "4:2", "4:4", "4:5",
                    ])
                )
                .as_bytes(),
            ),
            // Colors and styles in every spelling, valid or not.
            29..=30 => {
                const VALUES: &[u32] = &[
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 33, 37,
                    38, 39, 40, 44, 47, 48, 49, 50, 58, 59, 60, 89, 90, 97, 98, 100, 107, 108, 255,
                    256, 300, 9999, 10000, 65535, 65536,
                ];
                let count = *rng.pick(&[&0usize, &1, &1, &2, &3, &5, &8, &16, &17, &33]);
                out.extend_from_slice(b"\x1b[");
                for at in 0..count {
                    if at > 0 {
                        out.push(*rng.pick(&[&b';', &b';', &b';', &b';', &b':']));
                    }
                    if rng.below(12) > 0 {
                        let value = VALUES[rng.below(VALUES.len())];
                        let zeros = if rng.chance(10) { "00" } else { "" };
                        out.extend_from_slice(format!("{zeros}{value}").as_bytes());
                    }
                }
                out.extend_from_slice(rng.pick::<[u8]>(&[b"m", b"m", b"m", b"m", b" m", b"", b"M"]));
            },
            31..=33 => out.extend_from_slice(
                format!("\x1b[{};{}H", rng.below(16) + 1, rng.below(45) + 1).as_bytes(),
            ),
            34..=35 => {
                let number = rng.pick::<str>(&["", "", "0", "1", "2", "3", "4", "7", "40", "999", "010"]);
                let last = rng.pick::<str>(&[
                    "A", "B", "C", "D", "E", "F", "G", "H", "J", "K", "L", "M", "P", "@", "X", "S", "T",
                    "a", "d", "e", "f", "`", "I", "Z", "g", "b", "r", "s", "u",
                ]);
                let second = rng.pick::<str>(&["", "", "", ";", ";3", ";0", ";25;7", ";999"]);
                out.extend_from_slice(format!("\x1b[{number}{second}{last}").as_bytes());
            },
            36 => out.extend_from_slice(format!("\x1b[{}b", rng.below(6)).as_bytes()),
            37 => out.extend_from_slice(rng.pick::<[u8]>(&[b"\x1b[4h", b"\x1b[4l", b"\x1b[4l"])),
            38 => out.extend_from_slice(rng.pick::<[u8]>(&[b"\x1b[?7l", b"\x1b[?7h", b"\x1b[?7h"])),
            39..=40 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b[?47h", b"\x1b[?47l", b"\x1b[?1047h",
                b"\x1b[?1047l", b"\x1b[?1048h", b"\x1b[?1048l", b"\x1b[?6h", b"\x1b[?6l", b"\x1b[20h",
                b"\x1b[20l",
            ])),
            41 => out.extend_from_slice(
                format!("\x1b[{};{}r", rng.below(8) + 1, rng.below(8) + 6).as_bytes(),
            ),
            42 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b(0", b"\x1b(B", b"\x1b)0", b"\x1b)B", b"\x1b*0", b"\x1b+0", b"\x1b(A", b"\x0e",
                b"\x0f", b"\x1bn", b"\x1bo",
            ])),
            43 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b7", b"\x1b8", b"\x1bM", b"\x1bD", b"\x1bE", b"\x1bH", b"\x1b=", b"\x1b>", b"\x1bc",
                b"\x1b#8", b"\x1b[s", b"\x1b[u", b"\x1b[!p",
            ])),
            44 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b]0;a title\x07",
                b"\x1b]2;another\x1b\\",
                b"\x1b]1;icon\x07",
                b"\x1b]2;\x07",
                b"\x1b[22;0t",
                b"\x1b[23;0t",
                b"\x1b[22t",
                b"\x1b[23t",
            ])),
            45..=46 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b]8;;http://example.com\x1b\\",
                b"\x1b]8;;\x1b\\",
                b"\x1b]8;id=x;http://example.com/x\x07",
                b"\x1b]8;id=y;file:///tmp/y\x1b\\",
                b"\x1b]8;;\x07",
                b"\x1b]8;;https://a.b/\xc3\xa9\x07",
            ])),
            47 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b[58:5:196m",
                b"\x1b[58;5;196m",
                b"\x1b[4;58;2;1;2;3m",
                b"\x1b[59m",
                b"\x1b[59m",
                b"\x1b[0m",
            ])),
            48..=49 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b[?2026h",
                b"\x1b[?2026l",
                b"\x1b[?2026l",
                b"\x1b[?2026;25h",
                b"\x1b[?25;2026l",
                b"\x1bP=1s\x1b\\",
                b"\x1bP=2s\x1b\\",
                b"\x1b[?2026$p",
            ])),
            50 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1bPq#0;2;0;0;0\x1b\\",
                b"\x1b_an application command\x1b\\",
                b"\x1b^private\x1b\\",
                b"\x1bXstring\x1b\\",
                b"\x1bP1$r\x9c",
                b"\x1bP$qm\x1b\\",
            ])),
            // Colors set, asked for and reset.
            51..=52 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b]4;1;rgb:ff/00/00\x07",
                b"\x1b]4;200;#123456\x1b\\",
                b"\x1b]4;1;?\x07",
                b"\x1b]4;17;?\x1b\\",
                b"\x1b]10;?\x07",
                b"\x1b]11;?\x1b\\",
                b"\x1b]12;?\x07",
                b"\x1b]10;rgb:12/34/56\x07",
                b"\x1b]11;#abcdef\x07",
                b"\x1b]12;rgb:fe/dc/ba\x07",
                b"\x1b]104;1\x07",
                b"\x1b]104\x07",
                b"\x1b]110\x07",
                b"\x1b]111\x07",
                b"\x1b]112\x07",
                b"\x1b]4;1;rgb:zz/00/00\x07",
            ])),
            // The clipboard, and what a program may ask of the terminal.
            53..=54 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b]52;c;aGVsbG8=\x07",
                b"\x1b]52;c;?\x07",
                b"\x1b]52;p;?\x1b\\",
                b"\x1b]52;s;d29ybGQ=\x1b\\",
                b"\x1b]52;c;!!\x07",
                b"\x1b[c",
                b"\x1b[0c",
                b"\x1b[>c",
                b"\x1b[5n",
                b"\x1b[6n",
                b"\x1b[?6n",
                b"\x1b[14t",
                b"\x1b[18t",
                b"\x1b[16t",
                b"\x1b[?1$p",
                b"\x1b[4$p",
                b"\x1b[?25$p",
                b"\x1b[?9999$p",
                b"\x1b[?u",
                b"\x1b[>1u",
                b"\x1b[>5u",
                b"\x1b[<u",
                b"\x1b[<2u",
                b"\x1b[=3;1u",
                b"\x1b[=1;2u",
            ])),
            // Modes that change what input or the cursor looks like.
            55..=56 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b[?1h", b"\x1b[?1l", b"\x1b[?25l", b"\x1b[?25h", b"\x1b[?12h", b"\x1b[?12l",
                b"\x1b[?1000h", b"\x1b[?1002h", b"\x1b[?1003h", b"\x1b[?1000l", b"\x1b[?1006h",
                b"\x1b[?1005h", b"\x1b[?1006l", b"\x1b[?1004h", b"\x1b[?1004l", b"\x1b[?2004h",
                b"\x1b[?2004l", b"\x1b[?1007l", b"\x1b[?1042h", b"\x1b[3 q", b"\x1b[0 q", b"\x1b[6 q",
                b"\x1b[2 q", b"\x1b[?12;25h", b"\x1b[?3h", b"\x1b[?5h",
            ])),
            // Tab stops, and sequences few programs send.
            57 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1bH", b"\x1b[g", b"\x1b[0g", b"\x1b[3g", b"\x1b[2I", b"\x1b[Z", b"\t\t", b"\x1b[W",
                b"\x1b[?5W", b"\x1b]50;CursorShape=1\x07", b"\x1b]50;CursorShape=2\x1b\\",
                b"\x1b[>4;1m", b"\x1b[>4;2m", b"\x1b[?4m", b"\x1b[0;1 k", b"\x1b[1;2 k",
            ])),
            // Sequences left unfinished, and stray bytes inside them.
            58 => out.extend_from_slice(rng.pick::<[u8]>(&[
                b"\x1b", b"\x1b[", b"\x1b[38;5", b"\x1b]0;unfinished", b"\x1bP", b"\x1b(", b"\x1b[1\x18",
                b"\x1b[3\n1m", b"\x1b[\xe4m", b"\x1b\x7f[1m", b"\x1b[ q", b"\x1b[1 q", b"\x1b]8;;",
                b"\x1b[?", b"\x1b[?20", b"\x1b]52;c;",
            ])),
            // Many lines at once, so that the scrollback fills.
            59..=60 => {
                for i in 0..rng.below(30) + 1 {
                    out.extend_from_slice(format!("line {i} ").as_bytes());
                    if rng.chance(3) {
                        out.extend_from_slice(b"\x1b[32mgreen\x1b[0m \xe4\xb8\xad e\xcc\x81");
                    }
                    out.extend_from_slice(b"\r\n");
                }
            },
            _ => {
                for _ in 0..rng.below(4) + 1 {
                    out.push(rng.below(256) as u8);
                }
            },
        }
    }
    out
}

/// The engine and what it was seen to do.
struct Harness {
    live: Live,
    /// What happened so far, for the message on a panic.
    log: Vec<String>,
    /// All the engine was seen to do, folded together (FNV-1a), to compare
    /// with what the engine it replaced did, as recorded.
    digest: u64,
}

impl Harness {
    fn new(cols: u16, rows: u16, scrollback: usize, storage: Storage) -> Harness {
        Harness {
            live: Live::new(cols, rows, scrollback, storage),
            log: Vec::new(),
            digest: 0xcbf2_9ce4_8422_2325,
        }
    }

    fn interpret(&mut self, piece: &[u8], watch: bool) {
        self.log.push(format!("interpret \"{}\" watch {watch}", piece.escape_ascii()));
        let live = self.live.interpret(piece, watch);
        self.note(&format!("{live:?}"));
    }

    /// Everything that can be seen without changing what is seen.
    fn check(&mut self) {
        let mut seen = String::new();
        seen += &format!("{:?}", self.live.collect());
        seen += &format!("{:?}", self.live.modes());
        seen += &format!("{:?}", self.live.state.term.sync_update());
        seen += &format!("{:?}", self.live.state.term.take_ended());
        seen += &format!("{:?}", self.live.state.term.take_wake());
        // The frame is brought up to date a change at a time, as the
        // session does: what was not marked as changed shows too.
        snapshot::fill(&mut self.live.state, &mut self.live.frame);
        seen += &frame_digest(&self.live.frame);
        seen += &format!("{:?}", self.live.state.term.screen_text());
        seen += &format!("{:?}", self.live.state.term.selection_text());
        seen += &format!("{:?}", self.live.state.term.has_selection());
        let (cols, rows) = (self.live.frame.cols as usize, self.live.frame.rows as usize);
        for row in 0..rows + 1 {
            for col in 0..cols + 1 {
                seen += &format!("{:?}", self.live.state.term.hyperlink_at(row, col));
            }
        }
        self.note(&seen);
    }

    /// Folds `what` into the digest.
    fn note(&mut self, what: &str) {
        for &byte in what.as_bytes() {
            self.digest = (self.digest ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.digest = (self.digest ^ 0xff).wrapping_mul(0x0000_0100_0000_01b3);
    }

    /// Output that shows state nothing else may have shown: modes asked
    /// for, where the cursor is and where it was saved, the tab stops, the
    /// stacks of titles and keyboard modes, and the character sets.
    fn probe(&mut self) {
        const PROBE: &[u8] = b"\x1b[?1$p\x1b[?6$p\x1b[?7$p\x1b[4$p\x1b[20$p\x1b[?25$p\x1b[?1049$p\x1b[?2004$p\
            \x1b[6n\x1b8\x1b[6n\r\tA\tB\tC\tD\tE\x1b[6n\x1b[23t\x1b[23t\x1b[?u\x1b[<u\x1b[?u\
            \x1bn\x1bo\x0ejq\x0fjq\x1b[r\x1b[6n";
        self.interpret(PROBE, false);
        self.check();
    }

    /// One step of what the app or the program might do.
    fn step(&mut self, rng: &mut Rng, output: &[u8], at: &mut usize) {
        match rng.below(40) {
            0..=23 => {
                if *at < output.len() {
                    let len = *rng.pick(&[&1usize, &2, &3, &7, &16, &64, &200, &1024, &4096]);
                    let end = whole_characters(output, output.len().min(*at + len));
                    let piece = output[*at..end].to_vec();
                    *at = end;
                    self.interpret(&piece, rng.chance(2));
                }
            },
            24..=25 => {
                let cols = *rng.pick(&[&2u16, &3, &5, &10, &17, &20, &33, &40, &80]) + rng.below(3) as u16;
                let rows = *rng.pick(&[&1u16, &2, &3, &5, &8, &12, &15, &24]) + rng.below(2) as u16;
                self.log.push(format!("resize {cols}x{rows}"));
                self.live.resize(cols, rows);
            },
            26..=28 => {
                let scroll = match rng.below(6) {
                    0 => ViewScroll::Top,
                    1 => ViewScroll::Bottom,
                    2 => ViewScroll::Pages(rng.below(7) as i32 - 3),
                    _ => ViewScroll::Lines(*rng.pick(&[
                        &1i32, &-1, &3, &-3, &10, &-10, &1000, &-1000, &i32::MAX, &i32::MIN,
                    ])),
                };
                self.log.push(format!("scroll {scroll:?}"));
                self.live.state.term.scroll(scroll);
            },
            29..=32 => {
                let row = rng.below(20) as i32 - 2;
                let col = rng.below(45);
                let right = rng.chance(2);
                let side = if right { Side::Right } else { Side::Left };
                match rng.below(7) {
                    0..=1 => {
                        let kind = match rng.below(4) {
                            0 => SelectionKind::Simple,
                            1 => SelectionKind::Block,
                            2 => SelectionKind::Word,
                            _ => SelectionKind::Line,
                        };
                        self.log.push(format!("select from {row},{col} {side:?} {kind:?}"));
                        self.live.state.term.selection_start(row, col, side, kind);
                    },
                    2..=4 => {
                        self.log.push(format!("select to {row},{col} {side:?}"));
                        self.live.state.term.selection_update(row, col, side);
                    },
                    5 => {
                        self.log.push("select all".into());
                        self.live.state.term.select_all();
                    },
                    _ => {
                        self.log.push("selection cleared".into());
                        self.live.state.term.selection_clear();
                    },
                }
            },
            33 => {
                self.log.push("update ended for lasting too long".into());
                self.live.state.term.stop_sync();
            },
            34 => {
                let focused = rng.chance(2);
                self.log.push(format!("focused {focused}"));
                let live = self.live.state.term.set_focused(focused).map(<[u8]>::to_vec);
                self.note(&format!("{live:?}"));
            },
            35 => {
                let shape = *rng.pick(&[
                    &CursorShape::Block,
                    &CursorShape::Beam,
                    &CursorShape::Underline,
                    &CursorShape::HollowBlock,
                ]);
                let blinking = rng.chance(2);
                self.log.push(format!("cursor {shape:?} blinking {blinking}"));
                let (scrollback, storage) = (self.live.scrollback, self.live.storage);
                self.live.state.term.set_options(&options(scrollback, shape, blinking, storage));
            },
            36 => {
                let text: &str = rng.pick(&["\r\n[exited]\r\n", "message \u{e9}\u{4e2d}", "\x1b[31mred\x1b[0m"]);
                self.log.push(format!("message {text:?}"));
                self.live.state.term.show_message(text.as_bytes());
            },
            37 => {
                self.log.push("clear".into());
                let live = self.live.state.term.clear_all();
                self.note(&format!("{live:?}"));
            },
            38 => {
                let theme = theme(rng);
                self.log.push("theme".into());
                self.live.state.theme = theme;
            },
            _ => {
                // Packs at rest what output left as cells.
                let lines = if rng.chance(2) { usize::MAX } else { 1 + rng.below(20) };
                self.log.push(format!("history settled, {lines} lines"));
                self.live.state.term.settle(lines);
            },
        }
    }
}

impl Drop for Harness {
    /// A panic says what led to it as well.
    fn drop(&mut self) {
        if std::thread::panicking() {
            let keep = if std::env::var_os("TERN_DIFF_LOG").is_some() { usize::MAX } else { 12 };
            let start = self.log.len().saturating_sub(keep).max(1);
            eprintln!("{}\nlast steps:\n  {}", self.log[0], self.log[start..].join("\n  "));
        }
    }
}

/// Advances `end` to the next UTF-8 character boundary.
fn whole_characters(bytes: &[u8], mut end: usize) -> usize {
    while end < bytes.len() && (0x80..=0xbf).contains(&bytes[end]) {
        end += 1;
    }
    end
}

/// What of a frame is compared, as text.
fn frame_digest(frame: &Frame) -> String {
    format!(
        "{}x{} {:?} {:?} {:?} {:?} {:?} {:?} {} {}",
        frame.cols,
        frame.rows,
        frame.cells,
        frame.extra_refs,
        frame.extra_chars,
        (frame.fg, frame.bg),
        frame.cursor,
        frame.cursor_point,
        frame.display_offset,
        frame.history,
    )
}

/// Golden FNV-1a digests across 790 recorded seeds (`test seed digest`).
const RECORDED: &str = include_str!("engine.digests");

/// Verifies `digest` against `engine.digests` when `seed` is recorded.
fn held_to_record(test: &str, seed: u64, digest: u64) {
    let key = format!("{test} {seed} ");
    // Seeds past those recorded are run for what they can show alone: a
    // panic.
    if let Some(line) = RECORDED.lines().find(|line| line.starts_with(&key)) {
        assert_eq!(
            &line[key.len()..],
            format!("{digest:016x}"),
            "{test}, seed {seed}: engine digest mismatch"
        );
    }
}

/// A theme unlike the default one in the ways that change how cells look.
fn theme(rng: &mut Rng) -> Theme {
    let mut theme = Theme {
        bold_is_bright: rng.chance(2),
        ..Theme::default()
    };
    if rng.chance(2) {
        theme.selection_bg = None;
    }
    if rng.chance(2) {
        theme.selection_fg = Some(0x00ff_00ff);
    }
    if rng.chance(2) {
        theme.cursor = Some(0x0012_3456);
    }
    theme.palette[1] = rng.next() as u32 & 0xff_ffff;
    theme
}

fn seeds(usual: u64) -> std::ops::RangeInclusive<u64> {
    if let Some(seed) = std::env::var("TERN_DIFF_SEED").ok().and_then(|s| s.parse().ok()) {
        return seed..=seed;
    }
    let count = std::env::var("TERN_DIFF_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(usual);
    1..=count
}

/// Runs one seed: a terminal of a random size and history, random output,
/// and random steps of the app's among the pieces of it.
fn run(seed: u64, tokens: usize, steps: usize, storage: Storage) {
    let mut rng = Rng::new(seed);
    let cols = *rng.pick(&[&2u16, &5, &10, &20, &33, &40, &80]);
    let rows = *rng.pick(&[&1u16, &2, &5, &8, &12, &24]);
    let scrollback = *rng.pick(&[&0usize, &1, &10, &50, &200, &1000]);
    let mut harness = Harness::new(cols, rows, scrollback, storage);
    harness.log.push(format!("seed {seed}: {cols}x{rows}, {scrollback} lines of history, {storage:?}"));
    let output = stream(&mut rng, tokens);
    let mut at = 0;
    harness.check();
    for _ in 0..steps {
        harness.step(&mut rng, &output, &mut at);
        harness.check();
    }
    // The rest of the output, then every way of looking back at it.
    while at < output.len() {
        harness.step(&mut rng, &output, &mut at);
    }
    harness.check();
    for scroll in [ViewScroll::Top, ViewScroll::Pages(-1), ViewScroll::Lines(-3), ViewScroll::Bottom] {
        harness.live.state.term.scroll(scroll);
        harness.check();
    }
    harness.live.state.term.select_all();
    harness.check();
    harness.probe();
    held_to_record(&format!("run {storage:?}"), seed, harness.digest);
}

#[test]
fn engine_matches_recorded_digests() {
    for seed in seeds(250) {
        run(seed, 150, 80, USUAL);
    }
}

/// As above, with the scrollback kept in a ring of one line and pages of a
/// few lines each.
#[test]
fn engine_matches_recorded_digests_across_pages() {
    for seed in seeds(250) {
        run(seed, 150, 80, TINY);
    }
}

/// As above, with a ring that grows into rows of its own.
#[test]
fn engine_matches_recorded_digests_with_the_ring_grown() {
    for seed in seeds(250) {
        run(seed, 150, 80, MIDDLE);
    }
}

/// Long output into a small terminal with a long history, changing size
/// and scrolling back all the while: the scrollback and the rewrapping of
/// its lines.
#[test]
fn scrollback_and_reflow_match_recorded_digests() {
    for seed in seeds(40) {
        let mut rng = Rng::new(seed ^ 0x5eed);
        let storage = [USUAL, TINY, MIDDLE][seed as usize % 3];
        let mut harness = Harness::new(*rng.pick(&[&7u16, &20, &41]), *rng.pick(&[&3u16, &6]), 300, storage);
        harness.log.push(format!("seed {seed}"));
        for _ in 0..12 {
            let output = stream(&mut rng, 60);
            let len = rng.below(300) + 1;
            let mut at = 0;
            while at < output.len() {
                let end = whole_characters(&output, output.len().min(at + len));
                harness.interpret(&output[at..end], false);
                at = end;
            }
            harness.check();
            let cols = rng.below(50) as u16 + 2;
            let rows = rng.below(12) as u16 + 1;
            harness.log.push(format!("resize {cols}x{rows}"));
            harness.live.resize(cols, rows);
            harness.check();
            let lines = rng.below(400) as i32 - 100;
            harness.log.push(format!("scroll {lines} lines"));
            harness.live.state.term.scroll(ViewScroll::Lines(lines));
            harness.check();
            if rng.chance(3) {
                let row = rng.below(8) as i32;
                harness.live.state.term.selection_start(row, 1, Side::Left, SelectionKind::Simple);
                harness.live.state.term.scroll(ViewScroll::Lines(-5));
                harness.live.state.term.selection_update(row + 3, 4, Side::Right);
                harness.check();
            }
        }
        held_to_record(&format!("scrollback {storage:?}"), seed, harness.digest);
    }
}
