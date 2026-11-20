use std::cmp::Ordering;
use std::collections::VecDeque;
use std::mem;

use crate::grid::pages::Pages;
use crate::grid::{Dimensions, Grid, RING_ROWS};
use crate::index::{Boundary, Column, Line};
use crate::term::cell::{Cell, Flags};

struct ResizeDims {
    total_lines: usize,
    lines: usize,
    columns: usize,
}

impl Dimensions for ResizeDims {
    #[inline]
    fn total_lines(&self) -> usize {
        self.total_lines
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.lines
    }

    #[inline]
    fn columns(&self) -> usize {
        self.columns
    }
}

struct ResizeSink {
    pages: Pages,
    tail: VecDeque<(Vec<Cell>, u8)>,
    target_cols: usize,
    tail_keep: usize,
    page_cap: usize,
}

impl ResizeSink {
    fn new(page_bytes: usize, target_lines: usize, target_cols: usize, max_scroll_limit: usize) -> Self {
        let tail_keep = 2 * target_lines + RING_ROWS + 4;
        let page_cap = max_scroll_limit.saturating_add(target_lines);
        Self {
            pages: Pages::new(page_bytes),
            tail: VecDeque::with_capacity(tail_keep + 2),
            target_cols,
            tail_keep,
            page_cap,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.pages.len() + self.tail.len()
    }

    #[inline]
    fn last_mut(&mut self) -> Option<&mut Vec<Cell>> {
        self.tail.back_mut().map(|(cells, _)| cells)
    }

    fn seal_last(&mut self) {
        if let Some((back, _)) = self.tail.back_mut()
            && back.len() < self.target_cols
        {
            back.resize(self.target_cols, Cell::default());
        }
    }

    fn push_marked(&mut self, row: Vec<Cell>, mark: u8) {
        self.seal_last();
        self.tail.push_back((row, mark));
        while self.tail.len() > self.tail_keep {
            let (oldest, old_mark) = self.tail.pop_front().expect("tail row");
            debug_assert_eq!(oldest.len(), self.target_cols);
            let (occ, unclean) = row_meta(&oldest);
            self.pages.push_marked(&oldest, occ, unclean, old_mark);
            if self.pages.len() > self.page_cap {
                self.pages.drop_oldest(self.pages.len() - self.page_cap);
            }
        }
    }

    fn push_blank(&mut self) {
        let mut blank = Vec::with_capacity(self.target_cols);
        blank.resize(self.target_cols, Cell::default());
        self.push_marked(blank, 0);
    }

    fn pop_newest(&mut self) {
        let _ = self.tail.pop_back();
    }

    fn drop_oldest(&mut self, mut count: usize) {
        let from_pages = count.min(self.pages.len());
        if from_pages > 0 {
            self.pages.drop_oldest(from_pages);
            count -= from_pages;
        }
        if count > 0 {
            let from_tail = count.min(self.tail.len());
            self.tail.drain(..from_tail);
        }
    }

    fn screen_cell_has_wrapline(&self, target_lines: usize, line: Line, col: Column) -> bool {
        let screen_idx = line.0.max(0) as usize;
        if screen_idx >= target_lines || self.tail.len() < target_lines {
            return false;
        }
        let tail_idx = self.tail.len() - target_lines + screen_idx;
        self.tail
            .get(tail_idx)
            .and_then(|(row, _)| row.get(col.0))
            .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE))
    }
}

#[inline]
fn row_meta(cells: &[Cell]) -> (usize, bool) {
    let occ = cells.iter().rposition(|c| !c.is_default()).map_or(0, |i| i + 1);
    let unclean = cells[..occ].iter().any(Cell::has_extra);
    (occ, unclean)
}

#[inline]
fn slice_is_clear(cells: &[Cell]) -> bool {
    cells.iter().all(Cell::is_blank)
}

impl Grid {
    pub fn resize(&mut self, reflow: bool, target_lines: usize, target_cols: usize) {
        if target_lines == 0 || target_cols == 0 {
            return;
        }
        if self.lines == target_lines
            && self.columns == target_cols
            && self.slab.base_rows() == target_lines + self.base_ring
        {
            return;
        }

        let saved_template = mem::take(&mut self.cursor.template);

        let old_lines = self.lines;
        let old_cols = self.columns;
        let history = self.history_size();
        let max_scroll_limit = self.max_scroll_limit;
        let page_bytes = self.pages.page_bytes();

        let mut cursor = self.cursor.clone();
        let mut saved_cursor = self.saved_cursor.clone();
        let mut display_offset = self.display_offset;

        let (src_start_line, src_end_line, blank_bottom) = match target_lines.cmp(&old_lines) {
            Ordering::Equal => (-(history as i32), old_lines as i32, 0usize),
            Ordering::Greater => {
                let added = target_lines - old_lines;
                let pulled = added.min(history);
                let blank_bottom = added - pulled;

                cursor.point.line += pulled;
                saved_cursor.point.line += pulled;
                display_offset = display_offset.saturating_sub(added);

                (-(history as i32), old_lines as i32, blank_bottom)
            },
            Ordering::Less => {
                let scroll_needed = (cursor.point.line.0 as usize + 1).saturating_sub(target_lines);
                if scroll_needed > 0 {
                    if display_offset != 0 {
                        display_offset = (display_offset + scroll_needed).min(max_scroll_limit);
                    }
                    cursor.point.line -= scroll_needed;
                }
                let max_line = Line(target_lines as i32 - 1);
                cursor.point.line = cursor.point.line.min(max_line);
                saved_cursor.point.line = saved_cursor.point.line.min(max_line);

                let new_history_raw = history + scroll_needed;
                let excess_oldest = new_history_raw.saturating_sub(max_scroll_limit);
                let start = -(history as i32) + excess_oldest as i32;
                let end = (scroll_needed + target_lines) as i32;
                let kept_history = new_history_raw - excess_oldest;
                display_offset = display_offset.min(kept_history);

                (start, end, 0usize)
            },
        };

        let existing_count = (src_end_line - src_start_line) as usize;
        let total_source_lines = existing_count + blank_bottom;

        let mut sink = ResizeSink::new(page_bytes, target_lines, target_cols, max_scroll_limit);

        let read_source_row = |src_idx: usize| -> (Vec<Cell>, u8) {
            let mut buf = Vec::with_capacity(old_cols.max(target_cols));
            buf.resize(old_cols, Cell::default());
            let mut mark = 0;
            if src_idx < existing_count {
                let line = Line(src_start_line + src_idx as i32);
                let l = self.line(line);
                mark = l.mark();
                l.copy_into(&mut buf);
            }
            (buf, mark)
        };

        match target_cols.cmp(&old_cols) {
            Ordering::Equal => {
                for src_idx in 0..total_source_lines {
                    let (row, mark) = read_source_row(src_idx);
                    sink.push_marked(row, mark);
                }
            },
            Ordering::Greater => {
                if reflow && cursor.input_needs_wrap {
                    cursor.input_needs_wrap = false;
                    cursor.point.column += 1;
                }

                if reflow {
                    let dims = ResizeDims {
                        total_lines: total_source_lines,
                        lines: target_lines,
                        columns: target_cols,
                    };
                    let cursor_rev =
                        (target_lines - 1).saturating_sub(cursor.point.line.0.max(0) as usize);
                    let mut cursor_line_delta = 0usize;

                    for src_idx in 0..total_source_lines {
                        let rev_idx = total_source_lines - 1 - src_idx;
                        let (mut current_row, current_mark) = read_source_row(src_idx);

                        let can_unwrap = sink.last_mut().is_some_and(|prev| {
                            prev.len() < target_cols
                                && prev.last().is_some_and(|c| c.flags.contains(Flags::WRAPLINE))
                        });

                        if can_unwrap {
                            let prev_row = sink.last_mut().expect("previous row");
                            if let Some(last) = prev_row.last_mut() {
                                last.flags.remove(Flags::WRAPLINE);
                            }
                            if prev_row
                                .last()
                                .is_some_and(|c| c.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER))
                            {
                                prev_row.pop();
                            }

                            let prev_len = prev_row.len();
                            let room = target_cols - prev_len;
                            let take = current_row.len().min(room);

                            let sub_cols = if take > 0
                                && current_row[take - 1].flags.contains(Flags::WIDE_CHAR)
                            {
                                prev_row.extend(current_row.drain(..take - 1));
                                let mut spacer = Cell::default();
                                spacer.flags.insert(Flags::LEADING_WIDE_CHAR_SPACER);
                                prev_row.push(spacer);
                                room - 1
                            } else {
                                prev_row.extend(current_row.drain(..take));
                                room
                            };

                            let rem_clear = slice_is_clear(&current_row);
                            let mut skip_current = false;

                            if rev_idx == cursor_rev {
                                let mut target =
                                    cursor.point.sub(&dims, Boundary::Cursor, sub_cols);
                                if target.column == 0 && rem_clear {
                                    cursor.input_needs_wrap = true;
                                    target = target.sub(&dims, Boundary::Cursor, 1);
                                }
                                cursor.point.column = target.column;
                                let line_delta = (cursor.point.line - target.line).0 as usize;
                                if line_delta != 0 && rem_clear {
                                    skip_current = true;
                                } else {
                                    cursor_line_delta += line_delta;
                                }
                            } else if rem_clear {
                                if rev_idx < display_offset {
                                    display_offset -= 1;
                                }
                                if rev_idx < cursor_rev {
                                    cursor.point.line += 1;
                                }
                                skip_current = true;
                            }

                            if skip_current {
                                if let Some((_, m)) = sink.tail.back_mut() {
                                    *m |= current_mark;
                                }
                            } else {
                                if let Some(prev) = sink.last_mut()
                                    && let Some(last) = prev.last_mut()
                                {
                                    last.flags.insert(Flags::WRAPLINE);
                                }
                                sink.push_marked(current_row, current_mark);
                            }
                        } else {
                            sink.push_marked(current_row, current_mark);
                        }
                    }

                    sink.seal_last();

                    if sink.len() < target_lines {
                        let shortfall = target_lines - sink.len();
                        for _ in 0..shortfall {
                            sink.push_blank();
                        }
                        cursor.point.line = (cursor.point.line - shortfall).max(Line(0));
                    }

                    if cursor_line_delta > 0 {
                        let below_cursor =
                            (target_lines - 1).saturating_sub(cursor.point.line.0.max(0) as usize);
                        let can_drop = below_cursor.min(sink.len() - target_lines);
                        let rem = cursor_line_delta.saturating_sub(can_drop);
                        let to_pop = cursor_line_delta - rem;
                        for _ in 0..to_pop {
                            sink.pop_newest();
                        }
                        cursor.point.line = (cursor.point.line - rem).max(Line(0));
                    }

                    display_offset = display_offset.min(sink.len() - target_lines);
                    cursor.point.column = cursor.point.column.min(Column(target_cols - 1));
                    saved_cursor.point.column =
                        saved_cursor.point.column.min(Column(target_cols - 1));
                } else {
                    for src_idx in 0..total_source_lines {
                        let (mut row, mark) = read_source_row(src_idx);
                        row.resize(target_cols, Cell::default());
                        sink.push_marked(row, mark);
                    }
                    cursor.point.column = cursor.point.column.min(Column(target_cols - 1));
                    saved_cursor.point.column =
                        saved_cursor.point.column.min(Column(target_cols - 1));
                }
            },
            Ordering::Less => {
                if reflow && cursor.input_needs_wrap {
                    cursor.input_needs_wrap = false;
                    cursor.point.column += 1;
                }

                let mut carry: Option<(Vec<Cell>, u8)> = None;

                for src_idx in 0..total_source_lines {
                    let rev_idx = total_source_lines - 1 - src_idx;
                    let (mut row, mut cur_mark) = read_source_row(src_idx);

                    if let Some((mut prefix, pmark)) = carry.take() {
                        let cursor_rev =
                            (target_lines - 1).saturating_sub(cursor.point.line.0.max(0) as usize);
                        if rev_idx == cursor_rev {
                            cursor.point.column += prefix.len();
                        }
                        prefix.append(&mut row);
                        row = prefix;
                        cur_mark |= pmark;
                    }

                    loop {
                        if row.len() < target_cols {
                            row.resize(target_cols, Cell::default());
                        }
                        let mut wrapped = row.split_off(target_cols);

                        if !reflow {
                            // A wide character cut from its spacer at the
                            // new last column is cleared, as writing over
                            // half of one would.
                            if let Some(last) = row.last_mut().filter(|c| c.flags.contains(Flags::WIDE_CHAR)) {
                                last.clear_wide();
                            }
                            sink.push_marked(row, cur_mark);
                            break;
                        }

                        while wrapped.last().is_some_and(Cell::is_blank) {
                            wrapped.pop();
                        }

                        let cursor_rev =
                            (target_lines - 1).saturating_sub(cursor.point.line.0.max(0) as usize);
                        if wrapped.is_empty()
                            && !(rev_idx == cursor_rev && cursor.point.column > target_cols)
                        {
                            sink.push_marked(row, cur_mark);
                            break;
                        }

                        if target_cols >= 2 && row[target_cols - 1].flags.contains(Flags::WIDE_CHAR)
                        {
                            let mut spacer = Cell::default();
                            spacer.flags.insert(Flags::LEADING_WIDE_CHAR_SPACER);
                            let wide = mem::replace(&mut row[target_cols - 1], spacer);
                            wrapped.insert(0, wide);
                        }

                        if wrapped
                            .last()
                            .is_some_and(|c| c.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER))
                        {
                            if wrapped.len() == 1 {
                                row[target_cols - 1].flags.insert(Flags::WRAPLINE);
                                sink.push_marked(row, cur_mark);
                                break;
                            }
                            let wlen = wrapped.len();
                            wrapped[wlen - 2].flags.insert(Flags::WRAPLINE);
                            wrapped.pop();
                        }

                        row[target_cols - 1].flags.insert(Flags::WRAPLINE);
                        sink.push_marked(row, cur_mark);
                        cur_mark = 0;

                        if wrapped.last().is_some_and(|c| c.flags.contains(Flags::WRAPLINE))
                            && rev_idx >= 1
                            && wrapped.len() < target_cols
                        {
                            if let Some(last) = wrapped.last_mut() {
                                last.flags.remove(Flags::WRAPLINE);
                            }
                            carry = Some((wrapped, 0));
                            break;
                        }

                        let cursor_rev_now =
                            (target_lines - 1).saturating_sub(cursor.point.line.0.max(0) as usize);
                        if (rev_idx == cursor_rev_now && cursor.point.column < target_cols)
                            || rev_idx < cursor_rev_now
                        {
                            cursor.point.line = (cursor.point.line - 1i32).max(Line(0));
                        }
                        if rev_idx == cursor_rev_now && cursor.point.column >= target_cols {
                            cursor.point.column -= target_cols;
                        }

                        if wrapped.len() < target_cols {
                            wrapped.resize(target_cols, Cell::default());
                        }
                        row = wrapped;
                        if rev_idx < display_offset {
                            display_offset += 1;
                        }
                    }
                }

                sink.seal_last();

                let max_total = max_scroll_limit + target_lines;
                if sink.len() > max_total {
                    sink.drop_oldest(sink.len() - max_total);
                }

                display_offset = display_offset.min(sink.len() - target_lines);

                if reflow {
                    if cursor.point.column == target_cols
                        && !sink.screen_cell_has_wrapline(
                            target_lines,
                            cursor.point.line,
                            Column(target_cols - 1),
                        )
                    {
                        cursor.input_needs_wrap = true;
                        cursor.point.column = Column(target_cols - 1);
                    } else {
                        let dims = ResizeDims {
                            total_lines: sink.len(),
                            lines: target_lines,
                            columns: target_cols,
                        };
                        cursor.point = cursor.point.grid_clamp(&dims, Boundary::Cursor);
                    }
                } else {
                    cursor.point.column = cursor.point.column.min(Column(target_cols - 1));
                }
                saved_cursor.point.column = saved_cursor.point.column.min(Column(target_cols - 1));
            },
        }

        sink.seal_last();
        let max_total = max_scroll_limit + target_lines;
        if sink.len() > max_total {
            sink.drop_oldest(sink.len() - max_total);
        }
        while sink.len() < target_lines {
            sink.push_blank();
        }

        let mut new_grid = Grid::with_storage(
            target_lines,
            target_cols,
            max_scroll_limit,
            self.ring_cap,
            page_bytes,
        );

        let new_history = sink.len() - target_lines;
        let in_ring = new_history.min(new_grid.base_ring);
        let keep_in_tail = in_ring + target_lines;

        while sink.tail.len() > keep_in_tail {
            let (oldest, mark) = sink.tail.pop_front().expect("tail row");
            let (occ, unclean) = row_meta(&oldest);
            sink.pages.push_marked(&oldest, occ, unclean, mark);
        }
        sink.pages.shrink_scratch();
        new_grid.pages = sink.pages;

        for _ in 0..in_ring {
            let (cells, mark) = sink.tail.pop_front().expect("ring row");
            let (occ, unclean) = row_meta(&cells);
            let row_idx = new_grid.alloc_row();
            let dst = new_grid.slab.row_mut(row_idx as usize);
            for (dst_cell, src_cell) in dst[..].iter_mut().zip(cells) {
                *dst_cell = src_cell;
            }
            dst.meta.occ = occ as u32;
            dst.meta.unclean = u32::from(unclean);
            dst.set_mark(mark);
            new_grid.ring.push_back(row_idx);
        }

        for line_idx in 0..target_lines {
            let (cells, mark) = sink.tail.pop_front().expect("screen row");
            let (occ, unclean) = row_meta(&cells);
            let row_idx = new_grid.map[line_idx] as usize;
            let dst = new_grid.slab.row_mut(row_idx);
            for (dst_cell, src_cell) in dst[..].iter_mut().zip(cells) {
                *dst_cell = src_cell;
            }
            dst.meta.occ = occ as u32;
            dst.meta.unclean = u32::from(unclean);
            dst.set_mark(mark);
        }

        cursor.template = saved_template;
        new_grid.cursor = cursor;
        new_grid.saved_cursor = saved_cursor;
        new_grid.display_offset = display_offset.min(new_grid.history_size());

        *self = new_grid;
    }
}

#[cfg(test)]
mod tests {
    use crate::ansi::{Color, NamedColor};
    use crate::grid::{Dimensions, Grid, Scroll};
    use crate::index::{Column, Line, Point};
    use crate::term::cell::{Cell, Flags};

    fn row_str(grid: &Grid, line: i32) -> String {
        grid.line(Line(line)).cells().map(|c| c.c()).collect::<String>()
    }

    #[test]
    fn shrink_and_grow_columns_reflow_wrapped_lines_and_cursor() {
        let mut grid = Grid::with_storage(3, 10, 50, 2, 64);
        grid[Line(0)].write_ascii(0, b"0123456789", &Cell::default());
        grid.cursor.point = Point::new(Line(0), Column(9));
        grid.cursor.input_needs_wrap = true;

        grid.resize(true, 3, 5);
        assert_eq!(row_str(&grid, -1), "01234");
        assert!(grid.line(Line(-1)).wrapped());
        assert_eq!(row_str(&grid, 0), "56789");
        assert!(!grid.line(Line(0)).wrapped());
        assert_eq!(grid.cursor.point, Point::new(Line(0), Column(4)));
        assert!(grid.cursor.input_needs_wrap);

        grid.resize(true, 3, 10);
        assert_eq!(row_str(&grid, -1), "0123456789");
        assert!(grid.line(Line(-1)).wrapped());
        assert_eq!(row_str(&grid, 0).trim_end(), "");
        assert!(!grid.line(Line(0)).wrapped());
        assert_eq!(grid.cursor.point, Point::new(Line(0), Column(0)));
        assert!(grid.cursor.input_needs_wrap);
    }

    #[test]
    fn wide_characters_wrap_and_unwrap_cleanly_at_column_boundaries() {
        let mut grid = Grid::with_storage(3, 6, 20, 2, 64);
        grid[Line(0)].write_ascii(0, b"abcd", &Cell::default());
        grid[Line(0)][Column(4)] = Cell::new(
            '\u{4e2d}',
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            Flags::WIDE_CHAR,
        );
        grid[Line(0)][Column(5)] = Cell::new(
            ' ',
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            Flags::WIDE_CHAR_SPACER,
        );

        // The prefix moves into history and the wide character stays on screen.
        grid.resize(true, 3, 5);
        let prefix = grid.line(Line(-1));
        assert!(prefix.cell(Column(4)).flags().contains(Flags::LEADING_WIDE_CHAR_SPACER));
        assert!(prefix.wrapped());
        assert_eq!(grid[Line(0)][Column(0)].c(), '\u{4e2d}');
        assert!(grid[Line(0)][Column(0)].flags.contains(Flags::WIDE_CHAR));
        assert!(grid[Line(0)][Column(1)].flags.contains(Flags::WIDE_CHAR_SPACER));

        // The wider history line now has room for both cells of the character.
        grid.resize(true, 3, 6);
        let combined = grid.line(Line(-1));
        assert_eq!(combined.cell(Column(4)).c(), '\u{4e2d}');
        assert!(combined.cell(Column(4)).flags().contains(Flags::WIDE_CHAR));
        assert!(combined.cell(Column(5)).flags().contains(Flags::WIDE_CHAR_SPACER));
        assert!(combined.wrapped());
    }

    #[test]
    fn vertical_resize_pulls_from_packed_scrollback_and_tracks_offset() {
        let mut grid = Grid::with_storage(2, 6, 50, 1, 64);
        for i in 0..8 {
            grid[Line(1)].reset(&Cell::default());
            grid[Line(1)].write_ascii(0, format!("row{i:02}").as_bytes(), &Cell::default());
            if i < 7 {
                grid.scroll_up(&(Line(0)..Line(2)), 1);
            }
        }
        grid.cursor.point = Point::new(Line(1), Column(5));
        while grid.settle(16) {}
        assert_eq!(grid.history_size(), 7);

        grid.scroll_display(Scroll::Delta(3));
        assert_eq!(grid.display_offset(), 3);

        // Grow screen height from 2 to 5: pulls 3 lines from scrollback onto top of screen.
        grid.resize(true, 5, 6);
        assert_eq!(grid.history_size(), 4);
        assert_eq!(grid.display_offset(), 0);
        assert_eq!(grid.cursor.point.line, Line(4));
        assert_eq!(row_str(&grid, 0).trim_end(), "row03");
        assert_eq!(row_str(&grid, 4).trim_end(), "row07");

        // Shrink back to 2: cursor at Line(4) scrolls 3 top screen lines back to scrollback.
        grid.resize(true, 2, 6);
        assert_eq!(grid.history_size(), 7);
        assert_eq!(grid.cursor.point.line, Line(1));
        assert_eq!(row_str(&grid, 0).trim_end(), "row06");
        assert_eq!(row_str(&grid, 1).trim_end(), "row07");
    }

    #[test]
    fn no_reflow_preserves_custom_background_occupancy() {
        let mut grid = Grid::with_storage(2, 4, 0, 1, 64);
        let blue = Cell::from(Color::Named(NamedColor::Blue));
        grid[Line(0)].reset(&blue);
        grid.resize(false, 2, 8);

        assert_eq!(grid[Line(0)].occupied(), 4);
        assert_eq!(grid[Line(0)][Column(3)].bg(), Color::Named(NamedColor::Blue));
        assert!(grid[Line(0)][Column(4)].is_default());

        grid[Line(0)].reset(&Cell::default());
        assert!(grid[Line(0)][Column(3)].is_default());
        assert_eq!(grid[Line(0)].occupied(), 0);
    }
}
