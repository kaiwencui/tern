//! Builds synthetic frames for the demo, the benchmark and the pixel tests.
//! Shared by path; not part of the crate.

#![allow(dead_code)]

use std::collections::BTreeMap;

use tern_types::{flags, Cell, CursorDraw, CursorShape, ExtraRef, Frame, Rgb};

/// Characters that occupy two cells. A small table covering what the demo
/// prints: Hangul, CJK, fullwidth forms and emoji.
pub fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115f | 0x2e80..=0x303e | 0x3041..=0x33ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe30..=0xfe4f | 0xff00..=0xff60
        | 0xffe0..=0xffe6 | 0x1f1e6..=0x1f1ff | 0x1f300..=0x1f64f | 0x1f680..=0x1f6ff
        | 0x1f900..=0x1f9ff | 0x2600..=0x26ff | 0x2700..=0x27bf)
}

/// Characters that attach to the previous cell.
pub fn is_zero_width(c: char) -> bool {
    matches!(c as u32, 0x300..=0x36f | 0x200d | 0xfe0e | 0xfe0f | 0x1f3fb..=0x1f3ff | 0x20e3)
}

/// A tiny terminal: prints styled text into a Frame.
pub struct Term {
    frame: Frame,
    row: u16,
    col: u16,
    pub fg: Rgb,
    pub bg: Rgb,
    pub ul: Option<Rgb>,
    pub flags: u16,
    extras: BTreeMap<u32, Vec<char>>,
    /// The previous character was a regional indicator starting a flag.
    flag_open: bool,
    /// The previous character was a zero-width joiner.
    joined: bool,
}

impl Term {
    pub fn new(cols: u16, rows: u16, fg: Rgb, bg: Rgb) -> Term {
        let mut frame = Frame {
            fg,
            bg,
            ..Frame::default()
        };
        frame.resize(cols, rows);
        Term {
            frame,
            row: 0,
            col: 0,
            fg,
            bg,
            ul: None,
            flags: 0,
            extras: BTreeMap::new(),
            flag_open: false,
            joined: false,
        }
    }

    pub fn at(&mut self, row: u16, col: u16) -> &mut Term {
        self.row = row;
        self.col = col;
        self.flag_open = false;
        self.joined = false;
        self
    }

    pub fn style(&mut self, fg: Rgb, bg: Rgb, flags: u16) -> &mut Term {
        self.fg = fg;
        self.bg = bg;
        self.flags = flags;
        self.ul = None;
        self
    }

    pub fn reset(&mut self) -> &mut Term {
        let (fg, bg) = (self.frame.fg, self.frame.bg);
        self.style(fg, bg, 0)
    }

    pub fn underline_color(&mut self, ul: Rgb) -> &mut Term {
        self.ul = Some(ul);
        self
    }

    fn cell(&self, wide_part: u16) -> Cell {
        Cell {
            ch: ' ' as u32,
            fg: self.fg,
            bg: self.bg,
            ul: self.ul.unwrap_or(self.fg),
            flags: self.flags | wide_part,
            _reserved: 0,
        }
    }

    pub fn print(&mut self, s: &str) -> &mut Term {
        let (cols, rows) = (self.frame.cols, self.frame.rows);
        for c in s.chars() {
            if c == '\n' {
                self.row += 1;
                self.col = 0;
                continue;
            }
            let attach = is_zero_width(c)
                || self.joined
                || (self.flag_open && (0x1f1e6..=0x1f1ff).contains(&(c as u32)));
            if attach && self.col > 0 {
                // Attach to the last printed base cell (skip back over a spacer).
                let mut col = self.col - 1;
                if col > 0 && self.frame.cell(self.row, col).has(flags::WIDE_SPACER) {
                    col -= 1;
                }
                let index = self.row as u32 * cols as u32 + col as u32;
                self.extras.entry(index).or_default().push(c);
                self.frame.cells[index as usize].flags |= flags::HAS_EXTRA;
                self.flag_open = false;
                self.joined = c == '\u{200d}';
                continue;
            }
            self.joined = false;
            self.flag_open = (0x1f1e6..=0x1f1ff).contains(&(c as u32));
            let wide = is_wide(c);
            let need = if wide { 2 } else { 1 };
            if self.col + need > cols {
                self.row += 1;
                self.col = 0;
            }
            if self.row >= rows {
                break;
            }
            let mut cell = self.cell(if wide { flags::WIDE } else { 0 });
            cell.ch = c as u32;
            *self.frame_cell(self.row, self.col) = cell;
            if wide {
                let mut spacer = self.cell(flags::WIDE_SPACER);
                spacer.flags &= !flags::WIDE;
                *self.frame_cell(self.row, self.col + 1) = spacer;
            }
            self.col += need;
        }
        self
    }

    fn frame_cell(&mut self, row: u16, col: u16) -> &mut Cell {
        &mut self.frame.row_mut(row)[col as usize]
    }

    pub fn cursor(
        &mut self,
        row: u16,
        col: u16,
        shape: CursorShape,
        color: Rgb,
        text_color: Rgb,
    ) -> &mut Term {
        let wide = self.frame.cell(row, col).has(flags::WIDE);
        self.frame.cursor = Some(CursorDraw {
            row,
            col,
            shape,
            color,
            text_color,
            wide,
            blinking: false,
        });
        self
    }

    /// The finished frame; every row counts as changed.
    pub fn frame(&mut self) -> Frame {
        let mut frame = self.frame.clone();
        frame.extra_refs.clear();
        frame.extra_chars.clear();
        for (cell, chars) in &self.extras {
            let start = frame.extra_chars.len() as u32;
            frame.extra_chars.extend(chars);
            frame.extra_refs.push(ExtraRef {
                cell: *cell,
                start,
                len: chars.len() as u32,
            });
        }
        frame.seq += 1;
        let seq = frame.seq;
        frame.row_seq.iter_mut().for_each(|s| *s = seq);
        frame
    }
}

pub const FG: Rgb = 0xdcdfe4;
pub const BG: Rgb = 0x181a1f;
pub const PALETTE: [Rgb; 16] = [
    0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4, 0x5a6374,
    0xef7d86, 0xa9d48a, 0xf0d08c, 0x74bdf7, 0xd38aea, 0x67c7d3, 0xffffff,
];

/// A page of source-like ASCII text, for benchmarks: cols x rows, varied
/// characters and a few colors, offset by seed so successive pages differ.
pub fn ascii_page(cols: u16, rows: u16, seed: usize) -> Frame {
    let mut t = Term::new(cols, rows, FG, BG);
    let words = [
        "fn", "let", "mut", "struct", "impl", "return", "match", "Some(x)", "None", "=>", "{", "}",
        "0x1f", "&self", "// note", "vec![]", "i32", "+=", "Frame", "cells",
    ];
    for r in 0..rows {
        t.at(r, 0);
        let mut col = 0usize;
        let mut k = seed + r as usize * 7;
        while col + 8 < cols as usize {
            let w = words[k % words.len()];
            let fg = PALETTE[1 + k % 14];
            t.style(fg, BG, if k.is_multiple_of(11) { flags::BOLD } else { 0 });
            t.print(w);
            t.reset().print(" ");
            col += w.len() + 1;
            k += 3;
        }
    }
    t.frame()
}
