//! Erasing, inserting and deleting, and scrolling the lines of a region.
//!
//! What is erased takes the background the cursor writes with, as xterm
//! does (bce). Erasing a line, or part of the screen, drops a selection it
//! touches; scrolling a region moves the selection with the lines.

use crate::ansi::{ClearMode, LineClearMode};
use crate::event::EventListener;
use crate::grid::{Dimensions, Row};
use crate::index::{Column, Line};
use crate::term::cell::{Cell, Flags};
use crate::term::{Term, TermMode};

/// A wide character with one half each side of the left edge of `column`
/// is about to be cut in two: both halves become blanks of its style, as in
/// xterm, so neither is left without the other.
///
/// The row is only written to when there is one, so how far it has been
/// written is left as it was.
pub(super) fn split_wide(row: &mut Row, column: usize) {
    if column == 0 || column >= row.len() {
        return;
    }
    let (left, right) = (Column(column - 1), Column(column));
    if row[right].flags.contains(Flags::WIDE_CHAR_SPACER) && row[left].flags.contains(Flags::WIDE_CHAR) {
        row[left].clear_wide();
        row[right].flags.remove(Flags::WIDE_CHAR_SPACER);
    }
}

impl<T: EventListener> Term<T> {
    /// ICH: blanks at the cursor, pushing the rest of the line right and
    /// off its end.
    #[inline]
    pub fn insert_blank(&mut self, count: usize) {
        let blank = self.blank();
        let (line, column) = self.cursor_at();
        let count = count.min(self.columns() - column);
        self.damage.damage_line(line.0 as usize);
        let columns = self.columns();
        split_wide(&mut self.grid[line], column);
        split_wide(&mut self.grid[line], columns - count);
        let row = &mut self.grid[line][..];
        row[column..].rotate_right(count);
        row[column..column + count].fill(blank);
    }

    /// DCH: the cells at the cursor go, the rest of the line moves left and
    /// blanks fill its end.
    #[inline]
    pub fn delete_chars(&mut self, count: usize) {
        let blank = self.blank();
        let (line, column) = self.cursor_at();
        let columns = self.columns();
        // No more than there are from the cursor on.
        let count = count.min(columns - column);
        self.damage.damage_line(line.0 as usize);
        split_wide(&mut self.grid[line], column);
        split_wide(&mut self.grid[line], column + count);
        let row = &mut self.grid[line][..];
        row[column..].rotate_left(count);
        row[columns - count..].fill(blank);
    }

    /// ECH: blanks over the cells at the cursor, none of them moving.
    #[inline]
    pub fn erase_chars(&mut self, count: usize) {
        let blank = self.blank();
        let (line, column) = self.cursor_at();
        let end = (column + count).min(self.columns());
        self.damage.damage_line(line.0 as usize);
        split_wide(&mut self.grid[line], column);
        split_wide(&mut self.grid[line], end);
        self.grid[line][Column(column)..Column(end)].fill(blank);
    }

    /// EL.
    #[inline]
    pub fn clear_line(&mut self, mode: LineClearMode) {
        let blank = self.blank();
        let cursor = &self.grid.cursor;
        let (line, column) = (cursor.point.line, cursor.point.column.0);
        let columns = self.columns();
        let (left, right) = match mode {
            // The cursor is past the end of the line, so there is nothing
            // from it on to erase.
            LineClearMode::Right if cursor.input_needs_wrap => return,
            LineClearMode::Right => (column, columns),
            LineClearMode::Left => (0, column + 1),
            LineClearMode::All => (0, columns),
        };
        self.damage.damage_line(line.0 as usize);
        let row = &mut self.grid[line];
        split_wide(row, left);
        split_wide(row, right);
        if right == columns {
            // Erasing to the end also forgets how far the row was written.
            row.erase_from(left, &blank);
        } else {
            row[Column(left)..Column(right)].fill(blank);
        }
        self.drop_selection_over(line..=line);
    }

    /// ED.
    #[inline]
    pub fn clear_screen(&mut self, mode: ClearMode) {
        let blank = self.blank();
        let point = self.grid.cursor.point;
        match mode {
            ClearMode::Above => {
                // Every line above the cursor's.
                if point.line > 0 {
                    self.grid.reset_region(..point.line);
                }
                let end = (point.column + 1).min(Column(self.columns()));
                split_wide(&mut self.grid[point.line], end.0);
                self.grid[point.line][..end].fill(blank);
                self.drop_selection_over(Line(0)..=point.line);
            },
            ClearMode::Below => {
                split_wide(&mut self.grid[point.line], point.column.0);
                self.grid[point.line][point.column..].fill(blank);
                let screen_lines = self.screen_lines();
                if (point.line.0 as usize) < screen_lines - 1 {
                    self.grid.reset_region((point.line + 1)..);
                }
                self.drop_selection_over(point.line..Line(screen_lines as i32));
            },
            ClearMode::All => {
                if self.mode.contains(TermMode::ALT_SCREEN) {
                    self.grid.reset_region(..);
                } else {
                    self.grid.clear_viewport();
                }
                self.selection = None;
            },
            ClearMode::Saved => {
                if self.history_size() > 0 {
                    self.grid.clear_history();
                    self.drop_selection_over(..Line(0));
                }
            },
        }
        self.mark_fully_damaged();
    }

    /// IL: blank lines at the cursor, pushing the region's lines below it
    /// down. Outside the region it does nothing.
    #[inline]
    pub fn insert_blank_lines(&mut self, lines: usize) {
        let origin = self.grid.cursor.point.line;
        if self.scroll_region.contains(&origin) {
            self.scroll_down_relative(origin, lines);
        }
    }

    /// DL: the cursor's line and those below it go, the region's lines
    /// below them moving up. Outside the region it does nothing.
    #[inline]
    pub fn delete_lines(&mut self, lines: usize) {
        let origin = self.grid.cursor.point.line;
        let lines = lines.min(self.screen_lines() - origin.0 as usize);
        if lines > 0 && self.scroll_region.contains(&origin) {
            self.scroll_up_relative(origin, lines);
        }
    }

    /// SU.
    #[inline]
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_up_relative(self.scroll_region.start, lines);
    }

    /// SD.
    #[inline]
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_down_relative(self.scroll_region.start, lines);
    }

    /// DECSTBM: the region from line `top` to line `bottom`, both counted
    /// from 1 and both in it; the bottom of the screen if no bottom is
    /// given. A region of fewer than two lines is ignored.
    #[inline]
    pub fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        // A bottom past the screen is the bottom of the screen, as in xterm;
        // the check for two lines comes after, so a region that is left with
        // fewer is ignored rather than made empty.
        let screen_lines = self.screen_lines();
        let top = top.max(1);
        let bottom = bottom.unwrap_or(screen_lines).min(screen_lines);
        if top >= bottom {
            return;
        }
        self.scroll_region = Line(top as i32 - 1)..Line(bottom as i32);
        self.goto(0, 0);
    }

    /// DECALN: the screen filled with E, in the default style.
    #[inline]
    pub fn decaln(&mut self) {
        let mut e = Cell::default();
        e.set_c('E');
        for line in 0..self.screen_lines() {
            self.grid[Line(line as i32)][..].fill(e.clone());
        }
        self.mark_fully_damaged();
    }

    /// The lines from `origin` to the bottom of the region down by `lines`,
    /// blanks coming in at `origin`.
    #[inline]
    pub(super) fn scroll_down_relative(&mut self, origin: Line, lines: usize) {
        let region = origin..self.scroll_region.end;
        let lines = lines.min(self.region_height()).min((region.end - origin).0 as usize);
        self.selection = self.selection.take().and_then(|s| s.rotate(self, &region, -(lines as i32)));
        self.grid.scroll_down(&region, lines);
        self.mark_fully_damaged();
    }

    /// The lines from `origin` to the bottom of the region up by `lines`,
    /// blanks coming in at the bottom. From the top of the screen, the
    /// lines that go are kept in the scrollback.
    #[inline]
    pub(super) fn scroll_up_relative(&mut self, origin: Line, lines: usize) {
        let region = origin..self.scroll_region.end;
        let lines = lines.min(self.region_height());
        self.selection = self.selection.take().and_then(|s| s.rotate(self, &region, lines as i32));
        self.grid.scroll_up(&region, lines);
        self.mark_fully_damaged();
    }

    /// A cell erased now: blank, with the background the cursor writes with.
    #[inline]
    fn blank(&self) -> Cell {
        self.grid.cursor.template.bg().into()
    }

    /// The cursor's line, and its column as a number.
    #[inline]
    fn cursor_at(&self) -> (Line, usize) {
        let point = self.grid.cursor.point;
        (point.line, point.column.0)
    }

    #[inline]
    fn region_height(&self) -> usize {
        (self.scroll_region.end - self.scroll_region.start).0 as usize
    }

    /// No selection if it touches `lines`.
    #[inline]
    fn drop_selection_over(&mut self, lines: impl std::ops::RangeBounds<Line>) {
        self.selection = self.selection.take().filter(|s| !s.intersects_range(lines));
    }
}
