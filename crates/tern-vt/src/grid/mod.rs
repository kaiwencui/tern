use std::collections::VecDeque;
use std::fmt;
use std::ops::{Bound, Index, IndexMut, Range, RangeBounds};

use crate::ansi::{CharsetIndex, StandardCharset};
use crate::index::{Column, Line, Point};
use crate::term::cell::Cell;

pub mod line;
pub(crate) mod pack;
pub(crate) mod pages;
mod resize;
pub mod row;
pub(crate) mod slab;
#[cfg(test)]
mod tests;

pub use self::line::{CellRef, Cells, LineRef, Zerowidth};
pub use self::row::Row;

use self::pages::Pages;
use self::slab::Slab;

pub const RING_ROWS: usize = 32;
pub const PAGE_BYTES: usize = pages::PAGE_BYTES;
pub(crate) const RING_BYTES: usize = 40 << 20;
const GROW_ROWS: usize = 512;

pub trait Dimensions {
    fn total_lines(&self) -> usize;
    fn screen_lines(&self) -> usize;
    fn columns(&self) -> usize;

    #[inline]
    fn last_column(&self) -> Column {
        Column(self.columns() - 1)
    }

    #[inline]
    fn bottommost_line(&self) -> Line {
        Line(self.screen_lines() as i32 - 1)
    }

    #[inline]
    fn topmost_line(&self) -> Line {
        Line(-(self.history_size() as i32))
    }

    #[inline]
    fn history_size(&self) -> usize {
        self.total_lines().saturating_sub(self.screen_lines())
    }
}

#[cfg(test)]
impl Dimensions for (usize, usize) {
    fn total_lines(&self) -> usize {
        self.0
    }

    fn screen_lines(&self) -> usize {
        self.0
    }

    fn columns(&self) -> usize {
        self.1
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Charsets([StandardCharset; 4]);

impl Index<CharsetIndex> for Charsets {
    type Output = StandardCharset;

    #[inline]
    fn index(&self, index: CharsetIndex) -> &StandardCharset {
        &self.0[index as usize]
    }
}

impl IndexMut<CharsetIndex> for Charsets {
    #[inline]
    fn index_mut(&mut self, index: CharsetIndex) -> &mut StandardCharset {
        &mut self.0[index as usize]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub point: Point,
    pub template: Cell,
    pub charsets: Charsets,
    pub input_needs_wrap: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Scroll {
    Delta(i32),
    Top,
    Bottom,
}

#[inline]
pub(crate) fn ring_limit(
    lines: usize,
    columns: usize,
    max_scroll_limit: usize,
    cap: usize,
) -> usize {
    let row_bytes = (columns + 1) * std::mem::size_of::<Cell>();
    let worth = (RING_BYTES / row_bytes).max(RING_ROWS);
    let max_rows = (u16::MAX as usize + 1).saturating_sub(lines);
    cap.min(worth).max(1).min(max_scroll_limit).min(max_rows)
}

#[derive(Clone)]
pub struct Grid {
    pub cursor: Cursor,
    pub saved_cursor: Cursor,
    pub(crate) slab: Slab,
    pub(crate) map: Vec<u16>,
    pub(crate) ring: VecDeque<u16>,
    pub(crate) ring_rows: usize,
    pub(crate) ring_cap: usize,
    pub(crate) base_ring: usize,
    pub(crate) free: Vec<u16>,
    pub(crate) pages: Pages,
    pub(crate) columns: usize,
    pub(crate) lines: usize,
    pub(crate) display_offset: usize,
    pub(crate) max_scroll_limit: usize,
    pub(crate) spare: usize,
    pub(crate) spared: usize,
}

impl Grid {
    pub fn new(lines: usize, columns: usize, max_scroll_limit: usize) -> Self {
        Self::with_storage(lines, columns, max_scroll_limit, usize::MAX, PAGE_BYTES)
    }

    pub fn with_storage(
        lines: usize,
        columns: usize,
        max_scroll_limit: usize,
        ring_cap: usize,
        page_bytes: usize,
    ) -> Self {
        assert!(lines >= 1 && columns >= 1);
        let ring_rows = ring_limit(lines, columns, max_scroll_limit, ring_cap);
        let base_ring = ring_rows.min(RING_ROWS);
        let base_rows = lines + base_ring;
        let slab = Slab::new(base_rows, columns);
        let map = (0..lines as u16).collect();
        let free = (lines as u16..base_rows as u16).rev().collect();
        Self {
            cursor: Cursor::default(),
            saved_cursor: Cursor::default(),
            slab,
            map,
            ring: VecDeque::with_capacity(base_ring),
            ring_rows,
            ring_cap,
            base_ring,
            free,
            pages: Pages::new(page_bytes),
            columns,
            lines,
            display_offset: 0,
            max_scroll_limit,
            spare: 0,
            spared: 0,
        }
    }

    #[inline]
    pub fn line(&self, line: Line) -> LineRef<'_> {
        if line.0 >= 0 {
            let row_idx = self.map[line.0 as usize] as usize;
            LineRef::Cells(self.slab.row(row_idx))
        } else {
            let back = (-line.0 - 1) as usize;
            if back < self.ring.len() {
                let ring_idx = self.ring.len() - 1 - back;
                let row_idx = self.ring[ring_idx] as usize;
                LineRef::Cells(self.slab.row(row_idx))
            } else {
                LineRef::Packed(self.pages.get(back - self.ring.len()), self.columns)
            }
        }
    }

    #[inline]
    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    #[inline]
    pub fn cursor_cell(&mut self) -> &mut Cell {
        let point = self.cursor.point;
        &mut self[point]
    }

    pub fn history_stats(&self) -> (usize, usize, usize) {
        let packed_bytes = if self.pages.is_empty() { 0 } else { self.pages.bytes() };
        (self.history_size(), self.pages.len(), packed_bytes)
    }

    #[inline]
    pub fn prefetch_scroll(&self, ahead: usize) {
        if ahead == 0 {
            return;
        }
        let row = if self.ring.len() < self.ring_rows {
            self.free.len().checked_sub(ahead).map(|i| self.free[i])
        } else {
            self.ring.get(ahead - 1).copied()
        };
        if let Some(row_idx) = row {
            self.slab.row(row_idx as usize).prefetch();
        }
    }

    #[inline]
    pub fn unsettled(&self) -> bool {
        self.ring.len() > self.base_ring || self.slab.rows() > self.slab.base_rows()
    }

    pub fn settle(&mut self, lines: usize) -> bool {
        let excess = self.ring.len().saturating_sub(self.base_ring);
        let to_pack = excess.min(lines);
        for _ in 0..to_pack {
            let row_idx = self.ring.pop_front().expect("ring entry");
            {
                let row = self.slab.row(row_idx as usize);
                self.pages.push_marked(row.cells(), row.occupied(), row.unclean(), row.mark());
            }
            self.free.push(row_idx);
        }
        if self.ring.len() > self.base_ring {
            return true;
        }
        self.compact_to_base();
        self.pages.shrink_scratch();
        false
    }

    fn compact_to_base(&mut self) {
        let base_rows = self.slab.base_rows();
        debug_assert!(self.map.len() + self.ring.len() <= base_rows);
        if self.slab.rows() <= base_rows {
            return;
        }
        let mut used = vec![false; base_rows];
        for &row_idx in self.map.iter().chain(self.ring.iter()) {
            let idx = row_idx as usize;
            if idx < base_rows {
                used[idx] = true;
            }
        }
        let mut next_free = 0;
        for slot in self.map.iter_mut().chain(self.ring.iter_mut()) {
            let idx = *slot as usize;
            if idx >= base_rows {
                while used[next_free] {
                    next_free += 1;
                }
                used[next_free] = true;
                self.slab.swap(idx, next_free);
                *slot = next_free as u16;
            }
        }
        self.free.clear();
        let default_cell = Cell::default();
        for (b, &is_used) in used.iter().enumerate().rev() {
            if !is_used {
                if self.slab.row(b).unclean() {
                    self.slab.row_mut(b).reset(&default_cell);
                }
                self.free.push(b as u16);
            }
        }
        self.slab.drop_chunks();
    }

    #[inline]
    pub(crate) fn spare_reset(&mut self, spare: usize) {
        self.spare = spare;
        self.spared = 0;
    }

    #[inline]
    pub(crate) fn spared_reset(&mut self) -> usize {
        self.spare = 0;
        std::mem::take(&mut self.spared)
    }

    pub fn update_history(&mut self, history_size: usize) {
        let old_base_ring = self.base_ring;
        self.max_scroll_limit = history_size;
        self.ring_rows = ring_limit(self.lines, self.columns, history_size, self.ring_cap);
        self.base_ring = self.ring_rows.min(RING_ROWS);

        let total_history = self.ring.len() + self.pages.len();
        if total_history > history_size {
            let mut drop_count = total_history - history_size;
            let from_pages = drop_count.min(self.pages.len());
            if from_pages > 0 {
                self.pages.drop_oldest(from_pages);
                drop_count -= from_pages;
            }
            for _ in 0..drop_count {
                if let Some(row_idx) = self.ring.pop_front() {
                    self.free.push(row_idx);
                }
            }
        }

        while self.ring.len() > self.ring_rows {
            let row_idx = self.ring.pop_front().expect("ring entry");
            if history_size > 0 {
                let row = self.slab.row(row_idx as usize);
                self.pages.push_marked(row.cells(), row.occupied(), row.unclean(), row.mark());
            }
            self.free.push(row_idx);
        }

        self.display_offset = self.display_offset.min(self.history_size());

        if self.base_ring > old_base_ring {
            self.resize(false, self.lines, self.columns);
            if self.slab.base_rows() < self.lines + self.base_ring {
                self.rebuild_base_slab();
            }
        } else if history_size == 0 && self.slab.rows() > self.slab.base_rows() {
            self.compact_to_base();
        }
    }

    fn rebuild_base_slab(&mut self) {
        while self.ring.len() > self.base_ring {
            let row_idx = self.ring.pop_front().expect("ring entry");
            {
                let row = self.slab.row(row_idx as usize);
                self.pages.push_marked(row.cells(), row.occupied(), row.unclean(), row.mark());
            }
            self.free.push(row_idx);
        }
        let base_rows = self.lines + self.base_ring;
        let mut new_slab = Slab::new(base_rows, self.columns);
        for (dst_idx, slot) in self.map.iter_mut().enumerate() {
            let src = self.slab.row(*slot as usize);
            let dst = new_slab.row_mut(dst_idx);
            dst[..].clone_from_slice(src.cells());
            dst.meta = src.meta;
            *slot = dst_idx as u16;
        }
        for (i, slot) in self.ring.iter_mut().enumerate() {
            let dst_idx = self.lines + i;
            let src = self.slab.row(*slot as usize);
            let dst = new_slab.row_mut(dst_idx);
            dst[..].clone_from_slice(src.cells());
            dst.meta = src.meta;
            *slot = dst_idx as u16;
        }
        let used_rows = self.lines + self.ring.len();
        self.free = (used_rows as u16..base_rows as u16).rev().collect();
        self.slab = new_slab;
    }

    pub fn scroll_display(&mut self, scroll: Scroll) {
        let history = self.history_size();
        self.display_offset = match scroll {
            Scroll::Delta(delta) => {
                ((self.display_offset as i32).saturating_add(delta).max(0) as usize).min(history)
            },
            Scroll::Top => history,
            Scroll::Bottom => 0,
        };
    }

    pub fn scroll_down(&mut self, region: &Range<Line>, positions: usize) {
        self.spare = 0;
        self.spared = 0;
        if positions == 0 {
            return;
        }
        let start = region.start.0.max(0) as usize;
        let end = (region.end.0.max(0) as usize).min(self.lines);
        if start >= end {
            return;
        }
        let positions = positions.min(end - start);
        self.map[start..end].rotate_right(positions);
        let template = self.cursor.template.erased();
        for &row_idx in &self.map[start..start + positions] {
            self.slab.row_mut(row_idx as usize).reset(&template);
        }
    }

    pub fn scroll_up(&mut self, region: &Range<Line>, positions: usize) {
        let spare = std::mem::take(&mut self.spare);
        self.spared = 0;
        if positions == 0 {
            return;
        }
        let start = region.start.0.max(0) as usize;
        let end = (region.end.0.max(0) as usize).min(self.lines);
        if start >= end {
            return;
        }
        let positions = positions.min(end - start);
        let template = self.cursor.template.erased();

        if start != 0 || self.max_scroll_limit == 0 {
            self.map[start..end].rotate_left(positions);
            if positions == 1 && spare > 0 {
                let row_idx = self.map[end - 1] as usize;
                self.spared = self.slab.row_mut(row_idx).reset_after(&template, spare);
            } else {
                for &row_idx in &self.map[end - positions..end] {
                    self.slab.row_mut(row_idx as usize).reset(&template);
                }
            }
            // No line went into the history, so the view of it stays put.
            return;
        }

        if positions == 1 {
            let retired = self.map[0];
            let fresh = self.push_history_and_take_row(retired);
            self.map.copy_within(1..end, 0);
            self.map[end - 1] = fresh;
            if spare > 0 {
                self.spared = self.slab.row_mut(fresh as usize).reset_after(&template, spare);
            } else {
                self.slab.row_mut(fresh as usize).reset(&template);
            }
        } else {
            for i in 0..positions {
                let retired = self.map[i];
                let fresh = self.push_history_and_take_row(retired);
                self.slab.row_mut(fresh as usize).reset(&template);
                self.map[i] = fresh;
            }
            self.map[0..end].rotate_left(positions);
        }

        if self.display_offset != 0 {
            self.display_offset = (self.display_offset + positions).min(self.history_size());
        }
    }

    #[inline]
    fn alloc_row(&mut self) -> u16 {
        if let Some(row_idx) = self.free.pop() {
            return row_idx;
        }
        let limit = u16::MAX as usize + 1;
        let add = GROW_ROWS.min(limit - self.slab.rows());
        assert!(add > 0, "slab row index capacity exhausted");
        let first = self.slab.grow(add);
        for r in (first + 1..first + add).rev() {
            self.free.push(r as u16);
        }
        first as u16
    }

    #[inline]
    fn push_history_and_take_row(&mut self, retired: u16) -> u16 {
        if self.ring.len() < self.ring_rows {
            self.ring.push_back(retired);
            if self.ring.len() + self.pages.len() > self.max_scroll_limit {
                self.pages.drop_oldest(1);
            }
            return self.alloc_row();
        }
        if self.ring_rows == self.max_scroll_limit {
            let recycled = self.ring.pop_front().expect("non-empty ring");
            self.ring.push_back(retired);
            return recycled;
        }
        if self.ring_rows == 0 {
            let row = self.slab.row(retired as usize);
            self.pages.push_marked(row.cells(), row.occupied(), row.unclean(), row.mark());
            if self.pages.len() > self.max_scroll_limit {
                self.pages.drop_oldest(1);
            }
            return retired;
        }
        let oldest = self.ring.pop_front().expect("non-empty ring");
        {
            let row = self.slab.row(oldest as usize);
            self.pages.push_marked(row.cells(), row.occupied(), row.unclean(), row.mark());
        }
        self.ring.push_back(retired);
        if self.ring.len() + self.pages.len() > self.max_scroll_limit {
            self.pages.drop_oldest(1);
        }
        oldest
    }

    pub fn clear_viewport(&mut self) {
        let positions = (0..self.lines)
            .rposition(|line| !self.slab.row(self.map[line] as usize).is_clear())
            .map_or(0, |line| line + 1);
        let region = Line(0)..Line(self.lines as i32);
        self.scroll_up(&region, positions);
        let template = self.cursor.template.erased();
        for &row_idx in &self.map[0..(self.lines - positions)] {
            self.slab.row_mut(row_idx as usize).reset(&template);
        }
    }

    pub fn reset(&mut self) {
        self.clear_history();
        self.cursor = Cursor::default();
        self.saved_cursor = Cursor::default();
        self.display_offset = 0;
        self.spare = 0;
        self.spared = 0;
        let default_cell = Cell::default();
        for &row_idx in &self.map {
            self.slab.row_mut(row_idx as usize).reset(&default_cell);
        }
    }

    pub fn reset_region<R: RangeBounds<Line>>(&mut self, bounds: R) {
        let start = match bounds.start_bound() {
            Bound::Included(&line) => line.0.max(0) as usize,
            Bound::Excluded(&line) => (line.0 + 1).max(0) as usize,
            Bound::Unbounded => 0,
        };
        let end = match bounds.end_bound() {
            Bound::Included(&line) => ((line.0 + 1).max(0) as usize).min(self.lines),
            Bound::Excluded(&line) => (line.0.max(0) as usize).min(self.lines),
            Bound::Unbounded => self.lines,
        };
        if start >= end {
            return;
        }
        let template = self.cursor.template.erased();
        for &row_idx in &self.map[start..end] {
            self.slab.row_mut(row_idx as usize).reset(&template);
        }
    }

    pub fn clear_history(&mut self) {
        self.pages.clear();
        while let Some(row_idx) = self.ring.pop_front() {
            self.free.push(row_idx);
        }
        if self.slab.rows() > self.slab.base_rows() {
            self.compact_to_base();
        } else {
            let default_cell = Cell::default();
            for &row_idx in &self.free {
                let idx = row_idx as usize;
                if self.slab.row(idx).unclean() {
                    self.slab.row_mut(idx).reset(&default_cell);
                }
            }
        }
        self.display_offset = 0;
    }
}

impl Dimensions for Grid {
    #[inline]
    fn total_lines(&self) -> usize {
        self.lines + self.ring.len() + self.pages.len()
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

impl Index<Line> for Grid {
    type Output = Row;

    #[inline]
    fn index(&self, line: Line) -> &Row {
        let row_idx = if line.0 >= 0 {
            self.map[line.0 as usize] as usize
        } else {
            let back = (-line.0 - 1) as usize;
            let ring_idx = self.ring.len() - 1 - back;
            self.ring[ring_idx] as usize
        };
        self.slab.row(row_idx)
    }
}

impl IndexMut<Line> for Grid {
    #[inline]
    fn index_mut(&mut self, line: Line) -> &mut Row {
        let row_idx = if line.0 >= 0 {
            self.map[line.0 as usize] as usize
        } else {
            let back = (-line.0 - 1) as usize;
            let ring_idx = self.ring.len() - 1 - back;
            self.ring[ring_idx] as usize
        };
        self.slab.row_mut(row_idx)
    }
}

impl Index<Point> for Grid {
    type Output = Cell;

    #[inline]
    fn index(&self, point: Point) -> &Cell {
        &self[point.line][point.column]
    }
}

impl IndexMut<Point> for Grid {
    #[inline]
    fn index_mut(&mut self, point: Point) -> &mut Cell {
        &mut self[point.line][point.column]
    }
}

impl PartialEq for Grid {
    fn eq(&self, other: &Self) -> bool {
        if self.lines != other.lines
            || self.columns != other.columns
            || self.display_offset != other.display_offset
            || self.cursor != other.cursor
            || self.total_lines() != other.total_lines()
        {
            return false;
        }
        for raw_line in self.topmost_line().0..=self.bottommost_line().0 {
            let a = self.line(Line(raw_line));
            let b = other.line(Line(raw_line));
            if a.wrapped() != b.wrapped() || !a.cells().eq(b.cells()) {
                return false;
            }
        }
        true
    }
}

impl Eq for Grid {}

impl fmt::Debug for Grid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grid")
            .field("lines", &self.lines)
            .field("columns", &self.columns)
            .field("history", &self.history_size())
            .field("display_offset", &self.display_offset)
            .field("cursor", &self.cursor)
            .field("saved_cursor", &self.saved_cursor)
            .finish()
    }
}
