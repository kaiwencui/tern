//! The packed form of a line of the scrollback: a record that holds every
//! cell of the line, extras and all, in a fraction of the room its cells take.
//!
//! A line of text is mostly characters in one style, and a cell spends twelve
//! of its sixteen bytes on the style. A record keeps each character once, in
//! UTF-8, a style once for every run of cells that share it, and what few
//! cells have besides (characters joined to theirs, an underline color, a
//! link) in a table of its own at the end. The blank cells at the end of a
//! line take nothing. The characters are one stretch of UTF-8, so text can be
//! searched where it lies.
//!
//! A record, in bytes, little-endian:
//!
//! - 0..4: the length of the record.
//! - 4..6: how many cells are kept, from the first: the rest of the line is
//!   cells as [`Cell::default`] makes them.
//! - 6..8: how many runs of style.
//! - 8..10: how many extras.
//! - 10: [`WRAPPED`]: the last kept cell has the flag `WRAPLINE` besides those
//!   of its run. Only the last cell of a line has it, and it would otherwise
//!   cost every wrapped line a run of one cell.
//! - 11: unused.
//! - 12..16: the length of the text.
//! - The text: the kept cells' characters, one each.
//! - The runs: for each, the number of cells (2 bytes), their flags (2),
//!   foreground (4) and background (4), the colors as a cell keeps them.
//! - The extras, by column: the column (2), what the extra has (1, the bits
//!   below), and then in this order: with [`JOINED`] the length (2) and the
//!   UTF-8 of the characters joined to the cell's own; with [`UNDERLINE`] the
//!   underline color (4); with [`LINK`] the link's id and target, each as a
//!   length (4) and UTF-8. [`SAME_LINK`] stands for the link of the extra
//!   before that had one, which is how a link over many cells is kept once.
//!   An extra with none of these is an extra entry that holds nothing, which
//!   a cell can have, and is kept as such.

use crate::term::cell::{Cell, CellExtra, Flags, Hyperlink, color_word, word_color};
use crate::ansi::Color;

pub(crate) const HEADER: usize = 16;

/// The flag of a record whose last kept cell is marked `WRAPLINE`.
const WRAPPED: u8 = 1;

/// What an extra has.
const JOINED: u8 = 1;
const UNDERLINE: u8 = 2;
const LINK: u8 = 4;
const SAME_LINK: u8 = 8;

/// The bytes of a run: cells, flags, foreground, background.
const RUN: usize = 12;

#[inline]
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

#[inline]
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Packs lines, keeping its scratch space from one line to the next so that
/// packing asks for no memory once it has packed a few.
#[derive(Clone, Default)]
pub(crate) struct Packer {
    runs: Vec<u8>,
    extras: Vec<u8>,
}

impl Packer {
    /// Appends the record of the line `cells` to `out`. The cells from
    /// `occupied` on are known to be default ones. With `extras` false, the
    /// caller knows that no cell has an extra.
    #[cfg(test)]
    pub(crate) fn pack(&mut self, cells: &[Cell], occupied: usize, extras: bool, out: &mut Vec<u8>) {
        self.pack_marked(cells, occupied, extras, 0, out);
    }

    pub(crate) fn pack_marked(
        &mut self,
        cells: &[Cell],
        occupied: usize,
        extras: bool,
        mark: u8,
        out: &mut Vec<u8>,
    ) {
        let start = out.len();
        // The cells kept: up to the last that is not a default one.
        let mut kept = occupied.min(cells.len());
        while kept > 0 && cells[kept - 1].is_default() {
            kept -= 1;
        }
        let cells = &cells[..kept];
        self.runs.clear();
        self.extras.clear();

        // The usual line is plain ASCII in one style, which is copied a byte
        // a cell for as far as it goes. Room for the text is made at once.
        out.reserve(HEADER + 4 * kept);
        out.extend_from_slice(&[0; HEADER]);
        let plain = match cells.first() {
            Some(first) => plain_prefix(cells, first, out),
            None => 0,
        };
        let mut run: Option<(u16, u16, u32, u32)> = cells
            .first()
            .filter(|_| plain > 0)
            .map(|first| (plain as u16, first.flags.bits(), first.fg_word(), first.bg_word()));

        let mut wrapped = false;
        let mut runs = 0u16;
        let mut extra_count = 0u16;
        let mut last_link: Option<Hyperlink> = None;
        for (at, cell) in cells.iter().enumerate().skip(plain) {
            push_utf8(out, cell.c());
            let mut flags = cell.flags;
            if at + 1 == kept && flags.contains(Flags::WRAPLINE) {
                wrapped = true;
                flags.remove(Flags::WRAPLINE);
            }
            let style = (flags.bits(), cell.fg_word(), cell.bg_word());
            match &mut run {
                Some((count, f, fg, bg)) if (*f, *fg, *bg) == style => *count += 1,
                _ => {
                    if let Some(done) = run.take() {
                        push_run(&mut self.runs, done);
                        runs += 1;
                    }
                    run = Some((1, style.0, style.1, style.2));
                },
            }
            if extras && cell.has_extra() {
                extra_count += 1;
                self.push_extra(at, cell, &mut last_link);
            }
        }
        let text = out.len() - start - HEADER;
        if runs == 0 && extra_count == 0 {
            // One run and nothing else, as most lines are: written as it is.
            if let Some(done) = run {
                push_run(out, done);
                runs = 1;
            }
        } else {
            if let Some(done) = run {
                push_run(&mut self.runs, done);
                runs += 1;
            }
            out.extend_from_slice(&self.runs);
            out.extend_from_slice(&self.extras);
        }
        let len = out.len() - start;
        let header = &mut out[start..start + HEADER];
        header[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        header[4..6].copy_from_slice(&(kept as u16).to_le_bytes());
        header[6..8].copy_from_slice(&runs.to_le_bytes());
        header[8..10].copy_from_slice(&extra_count.to_le_bytes());
        header[10] = if wrapped { WRAPPED } else { 0 };
        header[11] = mark;
        header[12..16].copy_from_slice(&(text as u32).to_le_bytes());
    }

    #[cold]
    fn push_extra(&mut self, at: usize, cell: &Cell, last_link: &mut Option<Hyperlink>) {
        let out = &mut self.extras;
        out.extend_from_slice(&(at as u16).to_le_bytes());
        let what_at = out.len();
        out.push(0);
        let Some(extra) = cell.extra_parts() else {
            return;
        };
        let mut what = 0;
        if !extra.zerowidth.is_empty() {
            what |= JOINED;
            let len_at = out.len();
            out.extend_from_slice(&[0, 0]);
            let mut utf8 = [0; 4];
            for c in &extra.zerowidth {
                // Whole characters only, and no more than the length can
                // count: the reader trusts these bytes to be UTF-8.
                let bytes = c.encode_utf8(&mut utf8).as_bytes();
                if out.len() - len_at - 2 + bytes.len() > usize::from(u16::MAX) {
                    break;
                }
                out.extend_from_slice(bytes);
            }
            let len = (out.len() - len_at - 2) as u16;
            out[len_at..len_at + 2].copy_from_slice(&len.to_le_bytes());
        }
        if let Some(color) = extra.underline_color {
            what |= UNDERLINE;
            out.extend_from_slice(&color_word(color).to_le_bytes());
        }
        if let Some(link) = &extra.hyperlink {
            if last_link.as_ref() == Some(link) {
                what |= SAME_LINK;
            } else {
                what |= LINK;
                for part in [link.id(), link.uri()] {
                    out.extend_from_slice(&(part.len() as u32).to_le_bytes());
                    out.extend_from_slice(part.as_bytes());
                }
                *last_link = Some(link.clone());
            }
        }
        out[what_at] = what;
    }
}

/// Copies the characters of the cells at the front of `cells` that are
/// ASCII in the style of `first`, with no extra and no mark of a wrapped
/// line, to `out`, which has room for them; returns how many.
///
/// A cell is two words here: the character (whose word also holds the upper
/// bits of the number of an extra) with the foreground, and the background
/// with the flags and the rest of the number. Such a cell is one of these
/// exactly when the first word less its lowest seven bits and the second
/// word are those of `first` with no character and no extra.
#[inline]
fn plain_prefix(cells: &[Cell], first: &Cell, out: &mut Vec<u8>) -> usize {
    const _: () = assert!(cfg!(target_endian = "little"));
    if first.flags.contains(Flags::WRAPLINE) {
        return 0;
    }
    let style = Cell::from_words('\0', first.fg_word(), first.bg_word(), first.flags);
    let words = |cell: &Cell| -> [u64; 2] {
        // SAFETY: a cell is 16 bytes of plain integers (see its layout).
        unsafe { std::ptr::read_unaligned(cell as *const Cell as *const [u64; 2]) }
    };
    let want = words(&style);
    let base = out.len();
    let room = out.spare_capacity_mut();
    let limit = cells.len().min(room.len());
    // Only the aarch64 loop below advances `n`.
    #[allow(unused_mut)]
    let mut n = 0;

    // Four cells at a time: their words loaded apart, so that the four first words are
    // compared at once, and the four second words, and so on.
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the loads read whole cells within `cells`, which are four words each with
    // nothing between them, and the stores write within the spare capacity of `out`.
    unsafe {
        use std::arch::aarch64::*;

        let fg = vdupq_n_u32(first.fg_word());
        let bg = vdupq_n_u32(first.bg_word());
        let rest = vdupq_n_u32((want[1] >> 32) as u32);
        let high = vdupq_n_u32(!0x7f);
        let from = cells.as_ptr().cast::<u32>();
        let to = room.as_mut_ptr().cast::<u8>();
        while n + 4 <= limit {
            let four = vld4q_u32(from.add(n * 4));
            let ascii = vceqzq_u32(vandq_u32(four.0, high));
            let style = vandq_u32(vandq_u32(vceqq_u32(four.1, fg), vceqq_u32(four.2, bg)), vceqq_u32(four.3, rest));
            if vminvq_u32(vandq_u32(ascii, style)) == 0 {
                break;
            }
            let bytes = vmovn_u16(vcombine_u16(vmovn_u32(four.0), vdup_n_u16(0)));
            std::ptr::write_unaligned(to.add(n).cast::<u32>(), vget_lane_u32::<0>(vreinterpret_u32_u8(bytes)));
            n += 4;
        }
    }

    for (cell, slot) in cells[n..limit].iter().zip(room[n..limit].iter_mut()) {
        let [low, high] = words(cell);
        if low & !0x7f != want[0] || high != want[1] {
            break;
        }
        slot.write(low as u8);
        n += 1;
    }
    // SAFETY: the first `n` bytes of the spare capacity were written.
    unsafe { out.set_len(base + n) };
    n
}

/// Appends `c` in UTF-8 to `out`, which has room for it.
#[inline(always)]
fn push_utf8(out: &mut Vec<u8>, c: char) {
    let code = c as u32;
    let len = out.len();
    debug_assert!(out.capacity() - len >= 4);
    // SAFETY: there is room for four bytes (see `Packer::pack`), and as many as are written
    // are counted.
    unsafe {
        let to = out.as_mut_ptr().add(len);
        let written = if code < 0x80 {
            *to = code as u8;
            1
        } else if code < 0x800 {
            *to = 0xc0 | (code >> 6) as u8;
            *to.add(1) = 0x80 | (code & 0x3f) as u8;
            2
        } else if code < 0x10000 {
            *to = 0xe0 | (code >> 12) as u8;
            *to.add(1) = 0x80 | ((code >> 6) & 0x3f) as u8;
            *to.add(2) = 0x80 | (code & 0x3f) as u8;
            3
        } else {
            *to = 0xf0 | (code >> 18) as u8;
            *to.add(1) = 0x80 | ((code >> 12) & 0x3f) as u8;
            *to.add(2) = 0x80 | ((code >> 6) & 0x3f) as u8;
            *to.add(3) = 0x80 | (code & 0x3f) as u8;
            4
        };
        out.set_len(len + written);
    }
}

/// The most bytes the record of `kept` cells none of which has an extra can
/// take: each character in four, and a run for each cell.
#[inline]
pub(crate) fn plain_bound(kept: usize) -> usize {
    HEADER + kept * (4 + RUN)
}

#[inline]
fn push_run(out: &mut Vec<u8>, (count, flags, fg, bg): (u16, u16, u32, u32)) {
    let mut bytes = [0; RUN];
    bytes[0..2].copy_from_slice(&count.to_le_bytes());
    bytes[2..4].copy_from_slice(&flags.to_le_bytes());
    bytes[4..8].copy_from_slice(&fg.to_le_bytes());
    bytes[8..12].copy_from_slice(&bg.to_le_bytes());
    out.extend_from_slice(&bytes);
}

/// The length of the record at the start of `bytes`.
#[inline]
pub(crate) fn record_len(bytes: &[u8]) -> usize {
    u32_at(bytes, 0) as usize
}

/// A packed line, to read.
#[derive(Clone, Copy)]
pub struct Record<'a> {
    bytes: &'a [u8],
}

impl<'a> Record<'a> {
    /// The record at the start of `bytes`.
    #[inline]
    pub(crate) fn at(bytes: &'a [u8]) -> Record<'a> {
        Record { bytes: &bytes[..record_len(bytes)] }
    }

    /// How many cells are kept; the rest are default ones.
    #[inline]
    pub(crate) fn kept(&self) -> usize {
        u16_at(self.bytes, 4) as usize
    }

    #[inline]
    fn runs_len(&self) -> usize {
        u16_at(self.bytes, 6) as usize * RUN
    }

    #[inline]
    fn text_len(&self) -> usize {
        u32_at(self.bytes, 12) as usize
    }

    #[inline]
    fn marked_wrapped(&self) -> bool {
        self.bytes[10] & WRAPPED != 0
    }

    /// Whether the line of `columns` cells goes on in the next: its last
    /// cell is marked so.
    #[inline]
    pub(crate) fn wrapped(&self, columns: usize) -> bool {
        self.marked_wrapped() && self.kept() == columns
    }

    #[inline]
    pub(crate) fn mark(&self) -> u8 {
        self.bytes[11]
    }

    /// The kept cells' characters, in UTF-8.
    #[inline]
    pub(crate) fn text(&self) -> &'a [u8] {
        &self.bytes[HEADER..HEADER + self.text_len()]
    }


    /// The cells of the line, `columns` of them.
    #[inline]
    pub(crate) fn cells(&self, columns: usize) -> PackedCells<'a> {
        let runs_at = HEADER + self.text_len();
        let extras_at = runs_at + self.runs_len();
        let extras = &self.bytes[extras_at..];
        PackedCells {
            text: self.text(),
            runs: &self.bytes[runs_at..extras_at],
            extras,
            next_extra: if extras.is_empty() { usize::MAX } else { u16_at(extras, 0) as usize },
            link: None,
            at: 0,
            kept: self.kept(),
            columns,
            wrapped: self.marked_wrapped(),
            run_left: 0,
            fg: 0,
            bg: 0,
            flags: Flags::empty(),
        }
    }

    /// Writes the line into `cells`, all of its columns: the cells it was
    /// packed from, extras and all. What `cells` held is dropped.
    pub(crate) fn unpack_into(&self, cells: &mut [Cell]) {
        let mut iter = self.cells(cells.len());
        for cell in cells.iter_mut() {
            let packed = iter.next().expect("as many cells as columns");
            let mut new = Cell::from_words(packed.c, packed.fg, packed.bg, packed.flags);
            if let Extra::Packed(entry) = packed.extra {
                new.set_extra(entry.to_extra());
            }
            *cell = new;
        }
    }
}

/// The cells of a packed line, in order.
pub struct PackedCells<'a> {
    text: &'a [u8],
    runs: &'a [u8],
    extras: &'a [u8],
    /// The column of the next extra, or `usize::MAX`.
    next_extra: usize,
    /// The link of the last extra that had one.
    link: Option<&'a [u8]>,
    at: usize,
    kept: usize,
    columns: usize,
    wrapped: bool,
    run_left: usize,
    fg: u32,
    bg: u32,
    flags: Flags,
}

impl<'a> Iterator for PackedCells<'a> {
    type Item = PackedCell<'a>;

    #[inline]
    fn next(&mut self) -> Option<PackedCell<'a>> {
        let at = self.at;
        if at >= self.columns {
            return None;
        }
        self.at += 1;
        if at >= self.kept {
            return Some(PackedCell::BLANK);
        }
        // The character.
        let first = self.text[0];
        let c = if first < 0x80 {
            self.text = &self.text[1..];
            first as char
        } else {
            let len = match first {
                0xf0.. => 4,
                0xe0.. => 3,
                _ => 2,
            };
            // SAFETY: the text was written from chars, so it is UTF-8.
            let s = unsafe { std::str::from_utf8_unchecked(&self.text[..len]) };
            self.text = &self.text[len..];
            s.chars().next().unwrap_or(' ')
        };
        // The style.
        if self.run_left == 0 {
            let run = self.runs;
            self.run_left = u16_at(run, 0) as usize;
            self.flags = Flags::from_bits_retain(u16_at(run, 2));
            self.fg = u32_at(run, 4);
            self.bg = u32_at(run, 8);
            self.runs = &run[RUN..];
        }
        self.run_left -= 1;
        let mut flags = self.flags;
        if self.wrapped && at + 1 == self.kept {
            flags.insert(Flags::WRAPLINE);
        }
        let extra = if at == self.next_extra { self.take_extra() } else { Extra::None };
        Some(PackedCell { c, fg: self.fg, bg: self.bg, flags, extra })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.columns - self.at;
        (left, Some(left))
    }
}

impl ExactSizeIterator for PackedCells<'_> {}

impl<'a> PackedCells<'a> {
    #[cold]
    fn take_extra(&mut self) -> Extra<'a> {
        let bytes = self.extras;
        let what = bytes[2];
        let mut at = 3;
        let mut entry = PackedExtra { joined: &[], underline: None, link: None, empty: what == 0 };
        if what & JOINED != 0 {
            let len = u16_at(bytes, at) as usize;
            entry.joined = &bytes[at + 2..at + 2 + len];
            at += 2 + len;
        }
        if what & UNDERLINE != 0 {
            entry.underline = Some(u32_at(bytes, at));
            at += 4;
        }
        if what & LINK != 0 {
            let link_at = at;
            for _ in 0..2 {
                at += 4 + u32_at(bytes, at) as usize;
            }
            self.link = Some(&bytes[link_at..at]);
        }
        if what & (LINK | SAME_LINK) != 0 {
            entry.link = self.link;
        }
        self.extras = &bytes[at..];
        self.next_extra = if self.extras.is_empty() { usize::MAX } else { u16_at(self.extras, 0) as usize };
        Extra::Packed(entry)
    }
}

/// A cell of a packed line.
#[derive(Clone, Copy)]
pub struct PackedCell<'a> {
    pub(crate) c: char,
    pub(crate) fg: u32,
    pub(crate) bg: u32,
    pub(crate) flags: Flags,
    pub(crate) extra: Extra<'a>,
}

impl PackedCell<'_> {
    const BLANK: PackedCell<'static> = PackedCell {
        c: ' ',
        fg: color_word(Color::Named(crate::ansi::NamedColor::Foreground)),
        bg: color_word(Color::Named(crate::ansi::NamedColor::Background)),
        flags: Flags::empty(),
        extra: Extra::None,
    };
}

/// What a cell has besides its character and style, wherever it is kept.
#[derive(Clone, Copy)]
pub enum Extra<'a> {
    None,
    /// In the table of a cell on the screen.
    Cell(&'a Cell),
    /// In a packed line.
    Packed(PackedExtra<'a>),
}

/// An extra of a packed line.
#[derive(Clone, Copy)]
pub struct PackedExtra<'a> {
    /// The joined characters, in UTF-8.
    pub(crate) joined: &'a [u8],
    pub(crate) underline: Option<u32>,
    /// The link: its id and target, each a length and UTF-8.
    link: Option<&'a [u8]>,
    /// An entry that holds nothing.
    empty: bool,
}

impl<'a> PackedExtra<'a> {
    #[inline]
    pub(crate) fn joined(&self) -> std::str::Chars<'a> {
        // SAFETY: written from chars, so it is UTF-8.
        unsafe { std::str::from_utf8_unchecked(self.joined) }.chars()
    }

    #[inline]
    pub(crate) fn underline_color(&self) -> Option<Color> {
        self.underline.map(word_color)
    }

    #[inline]
    pub(crate) fn has_link(&self) -> bool {
        self.link.is_some()
    }

    /// The link's id and target.
    pub(crate) fn link(&self) -> Option<(&'a str, &'a str)> {
        let bytes = self.link?;
        let id_len = u32_at(bytes, 0) as usize;
        let id = &bytes[4..4 + id_len];
        let uri = &bytes[8 + id_len..];
        // SAFETY: written from strs.
        unsafe { Some((std::str::from_utf8_unchecked(id), std::str::from_utf8_unchecked(uri))) }
    }

    /// The extra as a cell on the screen keeps it.
    pub(crate) fn to_extra(self) -> CellExtra {
        debug_assert!(!self.empty || (self.joined.is_empty() && self.underline.is_none()));
        CellExtra {
            zerowidth: self.joined().collect(),
            underline_color: self.underline_color(),
            hyperlink: self.link().map(|(id, uri)| Hyperlink::new(Some(id), uri.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::{NamedColor, Rgb};

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
    }

    fn color(rng: &mut Rng) -> Color {
        match rng.below(6) {
            0 => Color::Named(NamedColor::Foreground),
            1 => Color::Named(NamedColor::Background),
            2 => Color::Named(NamedColor::Red),
            3 => Color::Indexed(rng.below(256) as u8),
            4 => Color::Spec(Rgb { r: rng.next() as u8, g: 7, b: 200 }),
            _ => Color::Named(NamedColor::Cursor),
        }
    }

    /// A line of cells of every kind: plain and colored, wide and joined,
    /// with underline colors, links and empty extras, blank or not at the
    /// end, wrapped or not.
    fn line(rng: &mut Rng, columns: usize) -> Vec<Cell> {
        const CHARS: [char; 8] = ['a', ' ', 'Z', '\u{e9}', '\u{4e2d}', '\u{1f600}', '\t', '\u{2500}'];
        let mut cells: Vec<Cell> = (0..columns).map(|_| Cell::default()).collect();
        let filled = match rng.below(4) {
            0 => 0,
            1 => columns,
            _ => rng.below(columns + 1),
        };
        let link = Hyperlink::new(Some(format!("id{}", rng.below(3))), "https://e.com/\u{e9}".into());
        let other = Hyperlink::new(None::<String>, "file:///x".into());
        for (at, cell) in cells.iter_mut().enumerate().take(filled) {
            let plain = rng.below(3) == 0;
            let fg = if plain { Color::Named(NamedColor::Foreground) } else { color(rng) };
            let bg = if plain { Color::Named(NamedColor::Background) } else { color(rng) };
            let flags = Flags::from_bits_retain(if plain { 0 } else { rng.next() as u16 & 0x7fff });
            *cell = Cell::new(CHARS[rng.below(CHARS.len())], fg, bg, flags);
            match rng.below(12) {
                0 => cell.push_zerowidth('\u{301}'),
                1 => {
                    cell.push_zerowidth('\u{301}');
                    cell.push_zerowidth('\u{fe0f}');
                    cell.set_underline_color(Some(color(rng)));
                },
                2 => cell.set_hyperlink(Some(link.clone())),
                3 => cell.set_hyperlink(Some(other.clone())),
                4 => {
                    // An entry emptied again.
                    cell.push_zerowidth('x');
                    cell.clear_wide();
                    cell.set_c(CHARS[at % CHARS.len()]);
                },
                _ => {},
            }
        }
        if rng.below(3) == 0 {
            cells[columns - 1].flags.insert(Flags::WRAPLINE);
        }
        cells
    }

    #[test]
    fn joined_characters_past_what_the_length_counts_are_cut_whole() {
        // More than the cell would take from output: written straight in.
        let mut cell = Cell::default();
        cell.set_c('a');
        for _ in 0..30_000 {
            cell.extra_mut().zerowidth.push('\u{20d0}');
        }
        let mut out = Vec::new();
        Packer::default().pack(std::slice::from_ref(&cell), 1, true, &mut out);
        let record = Record::at(&out);
        let read: Vec<PackedCell<'_>> = record.cells(1).collect();
        let Extra::Packed(entry) = read[0].extra else { panic!("the extra was not kept") };
        assert_eq!(entry.joined.len(), 65_535 / 3 * 3);
        assert!(std::str::from_utf8(entry.joined).is_ok());
        assert!(entry.joined().all(|c| c == '\u{20d0}'));
    }

    #[test]
    fn a_line_packs_and_reads_back_as_it_was() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut packer = Packer::default();
        let mut out = Vec::new();
        for round in 0..3000 {
            let columns = [1, 2, 3, 7, 40, 80, 300][round % 7];
            let cells = line(&mut rng, columns);
            out.clear();
            // Some junk before, as in a page.
            out.extend_from_slice(&[7; 5][..round % 5]);
            let start = out.len();
            packer.pack(&cells, columns, true, &mut out);
            let record = Record::at(&out[start..]);
            assert_eq!(record_len(&out[start..]), out.len() - start);
            assert_eq!(record.wrapped(columns), cells[columns - 1].flags.contains(Flags::WRAPLINE));

            // Read cell by cell.
            let read: Vec<PackedCell<'_>> = record.cells(columns).collect();
            assert_eq!(read.len(), columns);
            for (at, (cell, packed)) in cells.iter().zip(&read).enumerate() {
                assert_eq!(cell.c(), packed.c, "round {round} cell {at}");
                assert_eq!(cell.fg(), word_color(packed.fg), "round {round} cell {at}");
                assert_eq!(cell.bg(), word_color(packed.bg), "round {round} cell {at}");
                assert_eq!(cell.flags, packed.flags, "round {round} cell {at}");
                match (cell.extra_parts(), packed.extra) {
                    (None, Extra::None) => {},
                    (Some(extra), Extra::Packed(entry)) => assert_eq!(*extra, entry.to_extra()),
                    _ => panic!("round {round} cell {at}: the extra was not kept"),
                }
            }

            // Unpacked into cells.
            let mut back: Vec<Cell> = (0..columns).map(|_| Cell::new('q', Color::Indexed(3), Color::Indexed(4), Flags::BOLD)).collect();
            record.unpack_into(&mut back);
            assert_eq!(back, cells, "round {round}");

            // Packed again from what was unpacked, the same record.
            let mut again = Vec::new();
            packer.pack(&back, columns, true, &mut again);
            assert_eq!(again, out[start..], "round {round}");
        }
    }

    #[test]
    fn plain_text_takes_a_byte_a_column() {
        let text = "fn main() { println!(\"hello\"); }";
        let mut cells: Vec<Cell> = (0..120).map(|_| Cell::default()).collect();
        for (at, c) in text.chars().enumerate() {
            cells[at].set_c(c);
        }
        let mut out = Vec::new();
        Packer::default().pack(&cells, 120, false, &mut out);
        // The header, a byte a character and one run.
        assert_eq!(out.len(), HEADER + text.len() + RUN);
        let mut blank = Vec::new();
        let cells: Vec<Cell> = (0..120).map(|_| Cell::default()).collect();
        Packer::default().pack(&cells, 120, false, &mut blank);
        assert_eq!(blank.len(), HEADER);
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    /// How long packing takes: a line of source code, 43 characters in a
    /// row of 200 columns. `cargo test --release -p tern-vt packing_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn packing_speed() {
        let text = "        let mut packer = Packer::default();";
        let mut cells: Vec<Cell> = (0..200).map(|_| Cell::default()).collect();
        for (at, c) in text.chars().enumerate() {
            cells[at].set_c(c);
        }
        let mut packer = Packer::default();
        let mut out = Vec::with_capacity(1 << 20);
        let rounds = 2_000_000;
        let start = std::time::Instant::now();
        for _ in 0..rounds {
            out.clear();
            packer.pack(std::hint::black_box(&cells), text.len(), false, &mut out);
        }
        let each = start.elapsed().as_secs_f64() / rounds as f64 * 1e9;
        eprintln!("packing a line of {} characters: {each:.1} ns", text.len());
        let record = Record::at(&out);
        let start = std::time::Instant::now();
        let mut sum = 0u32;
        for _ in 0..rounds {
            for cell in std::hint::black_box(record).cells(200) {
                sum = sum.wrapping_add(cell.c as u32 ^ cell.fg);
            }
        }
        let each = start.elapsed().as_secs_f64() / rounds as f64 * 1e9;
        eprintln!("reading it back, 200 cells: {each:.1} ns ({sum})");
    }
}
