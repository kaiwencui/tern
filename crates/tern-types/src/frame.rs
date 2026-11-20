use crate::Rgb;

/// Bits of [Cell::flags].
pub mod flags {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    /// Dim has already been applied to [super::Cell::fg]; the bit is informational.
    pub const DIM: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 4;
    pub const UNDERCURL: u16 = 1 << 5;
    pub const DOTTED_UNDERLINE: u16 = 1 << 6;
    pub const DASHED_UNDERLINE: u16 = 1 << 7;
    pub const STRIKEOUT: u16 = 1 << 8;
    /// First half of a double-width character; its glyph covers two cells.
    pub const WIDE: u16 = 1 << 9;
    /// Second half of a double-width character; draw the background only.
    pub const WIDE_SPACER: u16 = 1 << 10;
    /// Concealed text: draw the background only.
    pub const HIDDEN: u16 = 1 << 11;
    /// Zero-width characters follow this cell's character; see [super::Frame::extra_refs].
    pub const HAS_EXTRA: u16 = 1 << 12;
    /// The cell is inside the selection. Selection colors are already in fg and bg.
    pub const SELECTED: u16 = 1 << 13;
    /// The cell carries a hyperlink (OSC 8).
    pub const HYPERLINK: u16 = 1 << 14;

    pub const ANY_UNDERLINE: u16 =
        UNDERLINE | DOUBLE_UNDERLINE | UNDERCURL | DOTTED_UNDERLINE | DASHED_UNDERLINE;
}

/// One visible cell with every color already resolved: palette lookups,
/// inverse video, dim and selection have been applied by the core, so the
/// renderer draws exactly these colors.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    /// Unicode scalar value. A blank cell holds a space.
    pub ch: u32,
    pub fg: Rgb,
    pub bg: Rgb,
    /// Underline color. Equal to fg unless the program set one.
    pub ul: Rgb,
    pub flags: u16,
    pub _reserved: u16,
}

impl Cell {
    pub const fn blank(fg: Rgb, bg: Rgb) -> Cell {
        Cell {
            ch: 0x20,
            fg,
            bg,
            ul: fg,
            flags: 0,
            _reserved: 0,
        }
    }

    #[inline]
    pub fn has(&self, bits: u16) -> bool {
        self.flags & bits != 0
    }

    #[inline]
    pub fn char(&self) -> char {
        char::from_u32(self.ch).unwrap_or(' ')
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Beam,
    Underline,
    HollowBlock,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CursorDraw {
    pub row: u16,
    pub col: u16,
    pub shape: CursorShape,
    pub color: Rgb,
    /// Color of the character under a block cursor.
    pub text_color: Rgb,
    /// The cursor sits on a double-width character.
    pub wide: bool,
    /// The program asked for a blinking cursor.
    pub blinking: bool,
}

/// Zero-width characters attached to one cell: a slice of [Frame::extra_chars].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExtraRef {
    /// Index into [Frame::cells].
    pub cell: u32,
    pub start: u32,
    pub len: u32,
}

/// What one pane shows right now. The core fills the same Frame again on every
/// snapshot and only rewrites rows that changed, so the buffers are reused.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    /// rows * cols cells, row by row from the top.
    pub cells: Vec<Cell>,
    /// For each row, the value [Frame::seq] had when the row last changed. A
    /// consumer that remembers the seq it last drew redraws rows with a larger
    /// value, and is correct however many snapshots happened in between.
    pub row_seq: Vec<u64>,
    /// Raised by the core whenever anything in the frame changed.
    pub seq: u64,
    /// None when the cursor is hidden or scrolled out of view.
    pub cursor: Option<CursorDraw>,
    /// Where the cursor is, as (row, col), whether it is shown or not: where
    /// typed text goes, and so where an input method composes. None when it
    /// is scrolled out of view.
    pub cursor_point: Option<(u16, u16)>,
    /// Default colors; the padding around the grid uses bg.
    pub fg: Rgb,
    pub bg: Rgb,
    /// Sorted by cell index.
    pub extra_refs: Vec<ExtraRef>,
    pub extra_chars: Vec<char>,
    /// Lines the view is scrolled back from the bottom.
    pub display_offset: u32,
    /// Lines of scrollback that exist.
    pub history: u32,
}

impl Frame {
    /// Sets the grid size. Every row counts as changed afterwards.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let blank = Cell::blank(self.fg, self.bg);
        self.cols = cols;
        self.rows = rows;
        self.cells.clear();
        self.cells.resize(cols as usize * rows as usize, blank);
        self.seq += 1;
        self.row_seq.clear();
        self.row_seq.resize(rows as usize, self.seq);
        self.extra_refs.clear();
        self.extra_chars.clear();
    }

    #[inline]
    pub fn row(&self, row: u16) -> &[Cell] {
        let c = self.cols as usize;
        let start = row as usize * c;
        &self.cells[start..start + c]
    }

    #[inline]
    pub fn row_mut(&mut self, row: u16) -> &mut [Cell] {
        let c = self.cols as usize;
        let start = row as usize * c;
        &mut self.cells[start..start + c]
    }

    #[inline]
    pub fn cell(&self, row: u16, col: u16) -> &Cell {
        &self.cells[row as usize * self.cols as usize + col as usize]
    }

    /// The zero-width characters attached to the cell at this index.
    pub fn extras(&self, cell_index: u32) -> &[char] {
        match self
            .extra_refs
            .binary_search_by_key(&cell_index, |e| e.cell)
        {
            Ok(i) => {
                let e = self.extra_refs[i];
                &self.extra_chars[e.start as usize..(e.start + e.len) as usize]
            }
            Err(_) => &[],
        }
    }

    /// One row as text, with trailing blanks removed.
    pub fn row_text(&self, row: u16) -> String {
        let mut s = String::with_capacity(self.cols as usize);
        let base = row as u32 * self.cols as u32;
        for (i, c) in self.row(row).iter().enumerate() {
            if c.has(flags::WIDE_SPACER) {
                continue;
            }
            s.push(c.char());
            if c.has(flags::HAS_EXTRA) {
                s.extend(self.extras(base + i as u32));
            }
        }
        let end = s.trim_end_matches(' ').len();
        s.truncate(end);
        s
    }

    /// The whole frame as text, one line per row.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for r in 0..self.rows {
            s.push_str(&self.row_text(r));
            s.push('\n');
        }
        s
    }
}
