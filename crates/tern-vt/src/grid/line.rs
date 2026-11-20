//! Reading a line wherever it is kept: on the screen, among the newest lines
//! of the scrollback that are still cells, or packed in the older
//! scrollback. Whatever reads the scrollback (the view, selections, the
//! search for the ends of a word, links) reads it through these, and none of
//! it makes cells to do so.

use crate::grid::pack::{Extra, PackedCell, PackedCells, Record};
use crate::grid::row::Row;
use crate::index::Column;
use crate::term::cell::{Cell, Flags, word_color};
use crate::ansi::Color;

/// One line of the grid, read-only.
#[derive(Clone, Copy)]
pub enum LineRef<'a> {
    /// Kept as cells.
    Cells(&'a Row),
    /// Packed, with the number of columns of the grid.
    #[doc(hidden)]
    Packed(Record<'a>, usize),
}

impl<'a> LineRef<'a> {
    /// How many cells the line has: the grid's columns.
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            LineRef::Cells(row) => row.len(),
            LineRef::Packed(_, columns) => *columns,
        }
    }

    /// Whether the line has no cells, which no line of a grid has.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The cell at `column`.
    #[inline]
    pub fn cell(&self, column: Column) -> CellRef<'a> {
        match self {
            LineRef::Cells(row) => CellRef::of(&row[column]),
            LineRef::Packed(..) => self.cells().nth(column.0).expect("a column of the line"),
        }
    }

    /// Whether the line goes on in the next one: its last cell is marked so.
    #[inline]
    pub fn wrapped(&self) -> bool {
        match self {
            LineRef::Cells(row) => row[Column(row.len() - 1)].flags.contains(Flags::WRAPLINE),
            LineRef::Packed(record, columns) => record.wrapped(*columns),
        }
    }

    /// Semantic prompt/output mark bits recorded on this line (`OSC 133`).
    #[inline]
    pub fn mark(&self) -> u8 {
        match self {
            LineRef::Cells(row) => row.mark(),
            LineRef::Packed(record, _) => record.mark(),
        }
    }

    /// The cells from the first column on.
    #[inline]
    pub fn cells(&self) -> Cells<'a> {
        match self {
            LineRef::Cells(row) => Cells::Cells(row.cells().iter()),
            LineRef::Packed(record, columns) => Cells::Packed(record.cells(*columns)),
        }
    }

    /// How many cells from the start are anything but blank: the line's
    /// length as text.
    pub fn line_length(&self) -> Column {
        if self.wrapped() {
            return Column(self.len());
        }
        let mut length = 0;
        for (at, cell) in self.cells().enumerate() {
            if cell.c() != ' ' || (cell.has_extra() && cell.zerowidth().next().is_some()) {
                length = at + 1;
            }
        }
        Column(length)
    }

    /// Writes the line's cells into `cells`, which has as many.
    pub(crate) fn copy_into(&self, cells: &mut [Cell]) {
        match self {
            LineRef::Cells(row) => {
                for (to, from) in cells.iter_mut().zip(row.cells()) {
                    *to = from.clone();
                }
            },
            LineRef::Packed(record, _) => record.unpack_into(cells),
        }
    }
}

/// The cells of a line, in order.
pub enum Cells<'a> {
    Cells(std::slice::Iter<'a, Cell>),
    Packed(PackedCells<'a>),
}

impl<'a> Iterator for Cells<'a> {
    type Item = CellRef<'a>;

    #[inline]
    fn next(&mut self) -> Option<CellRef<'a>> {
        match self {
            Cells::Cells(iter) => iter.next().map(CellRef::of),
            Cells::Packed(iter) => iter.next().map(CellRef::packed),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Cells::Cells(iter) => iter.size_hint(),
            Cells::Packed(iter) => iter.size_hint(),
        }
    }
}

impl ExactSizeIterator for Cells<'_> {}

/// One cell of a line, read-only, wherever the line is kept.
#[derive(Clone, Copy)]
pub struct CellRef<'a> {
    c: char,
    fg: u32,
    bg: u32,
    flags: Flags,
    extra: Extra<'a>,
}

impl PartialEq for CellRef<'_> {
    /// The same character, style and extras.
    fn eq(&self, other: &Self) -> bool {
        (self.c, self.fg, self.bg, self.flags) == (other.c, other.fg, other.bg, other.flags)
            && self.has_extra() == other.has_extra()
            && (!self.has_extra()
                || (self.zerowidth().eq(other.zerowidth())
                    && self.underline_color() == other.underline_color()
                    && self.hyperlink() == other.hyperlink()))
    }
}

impl std::fmt::Debug for CellRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cell")
            .field("c", &self.c)
            .field("fg", &self.fg())
            .field("bg", &self.bg())
            .field("flags", &self.flags)
            .field("zerowidth", &self.zerowidth().collect::<String>())
            .field("underline", &self.underline_color())
            .field("link", &Link(self.hyperlink()))
            .field("extra", &self.has_extra())
            .finish()
    }
}

impl<'a> CellRef<'a> {
    #[inline]
    pub(crate) fn of(cell: &'a Cell) -> CellRef<'a> {
        CellRef {
            c: cell.c(),
            fg: cell.fg_word(),
            bg: cell.bg_word(),
            flags: cell.flags,
            extra: if cell.has_extra() { Extra::Cell(cell) } else { Extra::None },
        }
    }

    #[inline]
    fn packed(cell: PackedCell<'a>) -> CellRef<'a> {
        CellRef { c: cell.c, fg: cell.fg, bg: cell.bg, flags: cell.flags, extra: cell.extra }
    }

    #[inline]
    pub fn c(&self) -> char {
        self.c
    }

    #[inline]
    pub fn fg(&self) -> Color {
        word_color(self.fg)
    }

    #[inline]
    pub fn bg(&self) -> Color {
        word_color(self.bg)
    }

    #[inline]
    pub fn flags(&self) -> Flags {
        self.flags
    }

    /// Whether the cell has any of: characters joined to its own, an
    /// underline color, a hyperlink.
    #[inline]
    pub fn has_extra(&self) -> bool {
        !matches!(self.extra, Extra::None)
    }

    /// The characters joined to the cell's own, in order.
    #[inline]
    pub fn zerowidth(&self) -> Zerowidth<'a> {
        match self.extra {
            Extra::None => Zerowidth::Cell([].iter()),
            Extra::Cell(cell) => Zerowidth::Cell(cell.zerowidth().unwrap_or(&[]).iter()),
            Extra::Packed(entry) => Zerowidth::Packed(entry.joined()),
        }
    }

    #[inline]
    pub fn underline_color(&self) -> Option<Color> {
        match self.extra {
            Extra::None => None,
            Extra::Cell(cell) => cell.underline_color(),
            Extra::Packed(entry) => entry.underline_color(),
        }
    }

    /// The id and the target of the cell's hyperlink.
    pub fn hyperlink(&self) -> Option<(String, String)> {
        match self.extra {
            Extra::None => None,
            Extra::Cell(cell) => cell.hyperlink().map(|link| (link.id().to_owned(), link.uri().to_owned())),
            Extra::Packed(entry) => entry.link().map(|(id, uri)| (id.to_owned(), uri.to_owned())),
        }
    }

    /// The target of the cell's hyperlink.
    #[inline]
    pub fn hyperlink_uri(&self) -> Option<String> {
        self.hyperlink().map(|(_, uri)| uri)
    }

    #[inline]
    pub fn has_hyperlink(&self) -> bool {
        match self.extra {
            Extra::None => false,
            Extra::Cell(cell) => cell.hyperlink().is_some(),
            Extra::Packed(entry) => entry.has_link(),
        }
    }
}

/// A cell's link, for [`CellRef`]'s debug form, as a cell's own debug form has it.
struct Link(Option<(String, String)>);

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some((id, uri)) => write!(f, "Some(Hyperlink {{ id: \"{id}\", uri: {uri:?} }})"),
            None => write!(f, "None"),
        }
    }
}

/// The characters joined to a cell's own.
pub enum Zerowidth<'a> {
    Cell(std::slice::Iter<'a, char>),
    Packed(std::str::Chars<'a>),
}

impl Iterator for Zerowidth<'_> {
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<char> {
        match self {
            Zerowidth::Cell(iter) => iter.next().copied(),
            Zerowidth::Packed(iter) => iter.next(),
        }
    }
}
