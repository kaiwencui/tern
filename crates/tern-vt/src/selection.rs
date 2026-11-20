//! What is selected: from where the mouse went down to where it is, in
//! one of four ways. The cells selected are found when they are asked for,
//! so that a word or a line is the one there is then.
//!
//! An end of a selection is a cell and a side of it: the cells covered from
//! one end to the other are those selected, so a selection that ends at the
//! left side of a cell leaves that cell out.

use std::mem;
use std::ops::{Bound, Range, RangeBounds};

use crate::grid::Dimensions;
use crate::index::{Boundary, Column, Line, Point, Side};
use crate::term::Term;

/// An end of a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    point: Point,
    side: Side,
}

/// The cells selected: those from `start` to `end`, both in it, in the
/// order of the text, or in the rectangle they are the corners of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRange {
    /// The top left.
    pub start: Point,
    /// The bottom right.
    pub end: Point,
    pub is_block: bool,
}

impl SelectionRange {
    pub fn new(start: Point, end: Point, is_block: bool) -> SelectionRange {
        assert!(start <= end);
        SelectionRange { start, end, is_block }
    }

    /// Whether the cell at `point` is selected.
    pub fn contains(&self, point: Point) -> bool {
        let Point { line, column } = point;
        if line < self.start.line || line > self.end.line {
            return false;
        }
        // Lines between the first and the last are selected whole, unless
        // it is a rectangle.
        let after_start = column >= self.start.column || (line != self.start.line && !self.is_block);
        let before_end = column <= self.end.column || (line != self.end.line && !self.is_block);
        after_start && before_end
    }
}

/// The ways of selecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionType {
    /// The cells between the ends, as text runs.
    Simple,
    /// The rectangle the ends are the corners of.
    Block,
    /// Whole words: from the start of the one under one end to the end of
    /// the one under the other, or a pair of brackets.
    Semantic,
    /// Whole lines, as they were written before they wrapped.
    Lines,
}

/// A selection in the grid, its ends where they were put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub ty: SelectionType,
    /// Where it began.
    start: Anchor,
    /// Where it is now.
    end: Anchor,
}

impl Selection {
    pub fn new(ty: SelectionType, point: Point, side: Side) -> Selection {
        let anchor = Anchor { point, side };
        Selection { ty, start: anchor, end: anchor }
    }

    /// The end that moves, to `point` and `side`.
    pub fn update(&mut self, point: Point, side: Side) {
        self.end = Anchor { point, side };
    }

    /// The selection after the lines of `region` move up by `delta`, down
    /// if it is less than 0. An end in the region moves with the lines and
    /// stops at its edge; the selection goes if what it covered leaves the
    /// region. A region from the top of the screen takes in the scrollback.
    pub fn rotate<D: Dimensions>(
        mut self,
        dimensions: &D,
        region: &Range<Line>,
        delta: i32,
    ) -> Option<Selection> {
        let (top, bottom) = (region.start, region.end);
        let bottommost = dimensions.bottommost_line();
        let block = self.ty == SelectionType::Block;
        let in_region = |line: Line| (line >= top || top == 0) && line < bottom;
        let (first, last) = self.ordered_mut();

        if in_region(first.point.line) {
            first.point.line = (first.point.line - delta).min(bottommost);
            if first.point.line >= bottom && last.point.line < bottom {
                return None;
            }
            if first.point.line < top && top != 0 {
                if !block {
                    first.point.column = Column(0);
                    first.side = Side::Left;
                }
                first.point.line = top;
            }
        }

        if in_region(last.point.line) {
            last.point.line = (last.point.line - delta).min(bottommost);
            if last.point.line < first.point.line {
                return None;
            }
            if last.point.line >= bottom {
                if !block {
                    last.point.column = dimensions.last_column();
                    last.side = Side::Right;
                }
                last.point.line = bottom - 1;
            }
        }

        Some(self)
    }

    /// Whether it covers no cell.
    pub fn is_empty(&self) -> bool {
        let (start, end) = (self.start, self.end);
        let right_left = |a: Anchor, b: Anchor| a.side == Side::Right && b.side == Side::Left;
        match self.ty {
            // From the right of a cell to the left of the next covers none.
            SelectionType::Simple => {
                let (first, last) = if start.point > end.point { (end, start) } else { (start, end) };
                first == last
                    || (right_left(first, last)
                        && first.point.line == last.point.line
                        && first.point.column + 1 == last.point.column)
            },
            // As for a simple one, in the columns, whatever the lines.
            SelectionType::Block => {
                (start.point.column == end.point.column && start.side == end.side)
                    || (start.point.column + 1 == end.point.column && right_left(start, end))
                    || (end.point.column + 1 == start.point.column && right_left(end, start))
            },
            SelectionType::Semantic | SelectionType::Lines => false,
        }
    }

    /// Whether it touches a line of `lines`.
    pub fn intersects_range<R: RangeBounds<Line>>(&self, lines: R) -> bool {
        let (mut first, mut last) = (self.start.point.line, self.end.point.line);
        if first > last {
            mem::swap(&mut first, &mut last);
        }
        let top = match lines.start_bound() {
            Bound::Included(&line) => line,
            Bound::Excluded(&line) => line + 1,
            Bound::Unbounded => Line(i32::MIN),
        };
        let bottom = match lines.end_bound() {
            Bound::Included(&line) => line,
            Bound::Excluded(&line) => line - 1,
            Bound::Unbounded => Line(i32::MAX),
        };
        bottom >= first && top <= last
    }

    /// The cells selected in `term` now, if there are any.
    pub fn to_range<T>(&self, term: &Term<T>) -> Option<SelectionRange> {
        let (mut first, mut last) =
            if self.start.point > self.end.point { (self.end, self.start) } else { (self.start, self.end) };
        // All of it has gone out of the scrollback.
        if last.point.line < term.topmost_line() {
            return None;
        }
        first.point = first.point.grid_clamp(term, Boundary::Grid);
        last.point = last.point.grid_clamp(term, Boundary::Grid);

        match self.ty {
            SelectionType::Simple => self.simple_range(first, last, term.columns()),
            SelectionType::Block => self.block_range(first, last),
            SelectionType::Semantic => Some(semantic_range(term, first.point, last.point)),
            SelectionType::Lines => Some(SelectionRange {
                start: term.line_search_left(first.point),
                end: term.line_search_right(last.point),
                is_block: false,
            }),
        }
    }

    /// The ends, the first in the text first.
    fn ordered_mut(&mut self) -> (&mut Anchor, &mut Anchor) {
        if self.start.point > self.end.point {
            (&mut self.end, &mut self.start)
        } else {
            (&mut self.start, &mut self.end)
        }
    }

    /// An end at the left of a cell leaves it out, and one at the right of
    /// a cell where it starts leaves that out, going to the next line from
    /// the last column.
    fn simple_range(&self, mut first: Anchor, mut last: Anchor, columns: usize) -> Option<SelectionRange> {
        if self.is_empty() {
            return None;
        }
        if last.side == Side::Left && first.point != last.point {
            if last.point.column == 0 {
                last.point.column = Column(columns - 1);
                last.point.line -= 1;
            } else {
                last.point.column -= 1;
            }
        }
        if first.side == Side::Right && first.point != last.point {
            first.point.column += 1;
            if first.point.column == columns {
                first.point.column = Column(0);
                first.point.line += 1;
            }
        }
        Some(SelectionRange { start: first.point, end: last.point, is_block: false })
    }

    /// As for a simple one, in the columns: from the left column to the
    /// right, the top line to the bottom.
    fn block_range(&self, mut first: Anchor, mut last: Anchor) -> Option<SelectionRange> {
        if self.is_empty() {
            return None;
        }
        if first.point.column > last.point.column {
            mem::swap(&mut first.side, &mut last.side);
            mem::swap(&mut first.point.column, &mut last.point.column);
        }
        if last.side == Side::Left && first.point != last.point && last.point.column.0 > 0 {
            last.point.column -= 1;
        }
        if first.side == Side::Right && first.point != last.point {
            first.point.column += 1;
        }
        Some(SelectionRange { start: first.point, end: last.point, is_block: true })
    }
}

/// Words from `start` to `end`. A click on a bracket, with the ends at the
/// same cell, selects to the bracket that matches it.
fn semantic_range<T>(term: &Term<T>, start: Point, end: Point) -> SelectionRange {
    if start == end
        && let Some(matching) = term.bracket_search(start)
    {
        let (start, end) = if matching < start { (matching, start) } else { (start, matching) };
        return SelectionRange { start, end, is_block: false };
    }
    SelectionRange {
        start: term.semantic_search_left(start),
        end: term.semantic_search_right(end),
        is_block: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VoidListener;
    use crate::term::test::TermSize;
    use crate::engine::Options;
    use crate::term::Term;

    fn empty_term(rows: usize, cols: usize) -> Term<VoidListener> {
        Term::new(Options::default(), &TermSize::new(cols, rows), VoidListener)
    }

    #[test]
    fn single_and_adjacent_simple_anchors() {
        let t = empty_term(2, 6);
        let p0 = Point::new(Line(0), Column(2));

        let mut s = Selection::new(SelectionType::Simple, p0, Side::Left);
        assert!(s.is_empty());
        assert_eq!(s.to_range(&t), None);

        s.update(p0, Side::Right);
        assert!(!s.is_empty());
        assert_eq!(s.to_range(&t), Some(SelectionRange::new(p0, p0, false)));

        let mut rev = Selection::new(SelectionType::Simple, p0, Side::Right);
        rev.update(p0, Side::Left);
        assert_eq!(rev.to_range(&t), Some(SelectionRange::new(p0, p0, false)));

        let mut seam = Selection::new(SelectionType::Simple, p0, Side::Right);
        seam.update(Point::new(Line(0), Column(3)), Side::Left);
        assert!(seam.is_empty());
        assert_eq!(seam.to_range(&t), None);
    }

    #[test]
    fn multiline_and_block_side_exclusion() {
        let t = empty_term(4, 5);
        let mut simple = Selection::new(SelectionType::Simple, Point::new(Line(0), Column(4)), Side::Right);
        simple.update(Point::new(Line(2), Column(0)), Side::Left);
        assert_eq!(
            simple.to_range(&t),
            Some(SelectionRange::new(Point::new(Line(1), Column(0)), Point::new(Line(1), Column(4)), false))
        );

        let mut block = Selection::new(SelectionType::Block, Point::new(Line(3), Column(3)), Side::Right);
        block.update(Point::new(Line(1), Column(1)), Side::Right);
        let r = block.to_range(&t).unwrap();
        assert_eq!(r, SelectionRange::new(Point::new(Line(1), Column(2)), Point::new(Line(3), Column(3)), true));
        assert!(r.contains(Point::new(Line(2), Column(2))));
        assert!(!r.contains(Point::new(Line(2), Column(0))));
    }

    #[test]
    fn rotation_clips_to_region_edges_and_drops_scrolled_out() {
        let dims = (8usize, 6usize);
        let region = Line(1)..Line(7);

        let mut up = Selection::new(SelectionType::Simple, Point::new(Line(2), Column(3)), Side::Left);
        up.update(Point::new(Line(5), Column(2)), Side::Right);
        let up = up.rotate(&dims, &region, 3).unwrap();
        assert_eq!(
            up.to_range(&empty_term(8, 6)),
            Some(SelectionRange::new(Point::new(Line(1), Column(0)), Point::new(Line(2), Column(2)), false))
        );

        let mut gone = Selection::new(SelectionType::Simple, Point::new(Line(1), Column(0)), Side::Left);
        gone.update(Point::new(Line(2), Column(2)), Side::Right);
        assert_eq!(gone.rotate(&dims, &region, 4), None);

        assert!(up.intersects_range(Line(2)..=Line(4)));
        assert!(!up.intersects_range(Line(3)..=Line(6)));
    }
}
