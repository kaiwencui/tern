//! Writing characters: in the style of the cursor, through the character
//! set in use, two cells for a wide one, and a character of no width onto
//! the one before it.
//!
//! Most output does not come through here: the feeder writes runs of plain
//! text straight into the rows. What it leaves comes here a character at a
//! time.

use unicode_width::UnicodeWidthChar;

use crate::event::EventListener;
use crate::grid::Dimensions;
use crate::index::{Column, Point};
use crate::term::cell::{Cell, Flags};
use crate::term::{Term, TermMode};

impl<T: EventListener> Term<T> {
    /// A character of the output.
    #[inline(never)]
    pub fn input(&mut self, c: char) {
        let Some(width) = c.width() else { return };
        if width == 0 {
            return self.join_previous(c);
        }
        if self.grid.cursor.input_needs_wrap {
            self.wrapline();
        }

        let columns = self.columns();
        let Point { line, column } = self.grid.cursor.point;
        if self.mode.contains(TermMode::INSERT) && column + width < columns {
            // IRM: the rest of the line moves right, off its end. Cell by
            // cell, so what is left at the cursor, which is written over,
            // is as it always was.
            super::erase::split_wide(&mut self.grid[line], column.0);
            super::erase::split_wide(&mut self.grid[line], columns - width);
            let row = &mut self.grid[line][..];
            for at in (column.0..columns - width).rev() {
                row.swap(at + width, at);
            }
        }

        if width == 1 {
            self.write_at_cursor(c);
        } else {
            if self.grid.cursor.point.column + 1 >= columns {
                if !self.mode.contains(TermMode::LINE_WRAP) {
                    // There is no room for it, and no next line to go to.
                    self.grid.cursor.input_needs_wrap = true;
                    return;
                }
                // It goes on the next line, and this one ends with a
                // spacer where it did not fit.
                self.write_flagged(' ', Flags::LEADING_WIDE_CHAR_SPACER);
                self.wrapline();
            }
            self.write_flagged(c, Flags::WIDE_CHAR);
            self.grid.cursor.point.column += 1;
            self.write_flagged(' ', Flags::WIDE_CHAR_SPACER);
        }

        if self.grid.cursor.point.column + 1 < columns {
            self.grid.cursor.point.column += 1;
        } else {
            self.grid.cursor.input_needs_wrap = true;
        }
    }

    /// `c` in the cell at the cursor, in the cursor's style.
    #[inline(always)]
    pub(super) fn write_at_cursor(&mut self, c: char) {
        let c = self.grid.cursor.charsets[self.active_charset].map(c);
        let style = Cell::styled(' ', &self.grid.cursor.template);
        let extra = self.grid.cursor.template.share_extra();
        let point = self.grid.cursor.point;

        let write = |cell: &mut Cell| {
            cell.set_c(c);
            cell.set_style(&style);
            cell.put_extra(extra);
        };

        // Nearly every cell is no half of a wide character, and is written
        // without the row being looked up again.
        let cell = &mut self.grid[point.line][point.column];
        if !cell.flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER) {
            return write(cell);
        }
        self.break_wide(point);
        write(self.grid.cursor_cell());
    }

    /// The line goes on in the next: the cursor to the start of that one,
    /// scrolling the region at its bottom. Without DECAWM, nothing.
    #[inline]
    pub(super) fn wrapline(&mut self) {
        if !self.mode.contains(TermMode::LINE_WRAP) {
            return;
        }
        let point = self.grid.cursor.point;
        if point.column + 1 == self.columns() {
            self.grid[point.line].set_wrapline();
        } else {
            self.grid.cursor_cell().flags.insert(Flags::WRAPLINE);
        }

        if point.line + 1 >= self.scroll_region.end {
            self.linefeed();
        } else {
            self.damage_cursor();
            self.grid.cursor.point.line += 1;
        }
        self.grid.cursor.point.column = Column(0);
        self.grid.cursor.input_needs_wrap = false;
        self.damage_cursor();
    }

    /// `c` at the cursor, with `flag` too.
    #[inline]
    fn write_flagged(&mut self, c: char, flag: Flags) {
        self.grid.cursor.template.flags.insert(flag);
        self.write_at_cursor(c);
        self.grid.cursor.template.flags.remove(flag);
    }

    /// A character of no width goes with the one before the cursor, or with
    /// the one at it when a wrap is pending. On the spacer of a wide
    /// character it goes with that character.
    fn join_previous(&mut self, c: char) {
        let Point { line, mut column } = self.grid.cursor.point;
        if !self.grid.cursor.input_needs_wrap {
            column = Column(column.saturating_sub(1));
        }
        if self.grid[line][column].flags.contains(Flags::WIDE_CHAR_SPACER) {
            column = Column(column.saturating_sub(1));
        }
        self.grid[line][column].push_zerowidth(c);
    }

    /// The cell at `point` is about to be written over, and is half of a
    /// wide character: the other half becomes an ordinary cell.
    #[cold]
    fn break_wide(&mut self, point: Point) {
        let Point { line, column } = point;
        let wide = self.grid[line][column].flags.contains(Flags::WIDE_CHAR);
        if wide {
            // A wide character in the last column has lost its spacer
            // already, and has no other half to clear.
            if column < self.last_column() {
                self.grid[line][column + 1].flags.remove(Flags::WIDE_CHAR_SPACER);
            }
        } else if column > 0 && self.grid[line][column - 1].flags.contains(Flags::WIDE_CHAR) {
            // Only a spacer with its wide character before it: a spacer left
            // on its own has nothing to the left that belongs to it.
            self.grid[line][column - 1].clear_wide();
        }
        // A wide character that did not fit at the end of the line before
        // left a spacer there.
        if column <= 1 && line != self.topmost_line() {
            let last = self.last_column();
            self.grid[line - 1i32][last].flags.remove(Flags::LEADING_WIDE_CHAR_SPACER);
        }
    }
}
