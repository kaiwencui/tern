//! Where things are in the grid: lines, columns and the points they make.
//!
//! Lines count from the top of the screen, at 0, down to its bottom; the
//! scrollback is above, its lines less than 0. Columns count from the left,
//! at 0. Each is its own type, so one is never passed for the other.

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, AddAssign, Deref, Sub, SubAssign};

use crate::grid::Dimensions;

/// Which side of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// What a point is kept within.
pub enum Boundary {
    /// Where the cursor can go: the screen.
    Cursor,
    /// The whole grid: from the top of the scrollback to the bottom of the
    /// screen.
    Grid,
}

/// A cell of the grid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Point {
    pub line: Line,
    pub column: Column,
}

impl Point {
    pub fn new(line: Line, column: Column) -> Point {
        Point { line, column }
    }

    /// The point `count` cells before this one in the order of the text,
    /// within `boundary`.
    #[inline]
    #[must_use]
    pub fn sub<D: Dimensions>(mut self, dimensions: &D, boundary: Boundary, count: usize) -> Point {
        let columns = dimensions.columns();
        // Lines gone back over: one for each time the count reaches past
        // the start of a line.
        self.line -= (count + columns - 1).saturating_sub(self.column.0) / columns;
        self.column = Column((columns + self.column.0 - count % columns) % columns);
        self.grid_clamp(dimensions, boundary)
    }

    /// The nearest point within `boundary`: the first cell of it for one
    /// above it, the last for one below.
    #[inline]
    #[must_use]
    pub fn grid_clamp<D: Dimensions>(mut self, dimensions: &D, boundary: Boundary) -> Point {
        let last_column = dimensions.last_column();
        self.column = self.column.min(last_column);
        let top = match boundary {
            Boundary::Cursor => Line(0),
            Boundary::Grid => dimensions.topmost_line(),
        };
        let bottom = dimensions.bottommost_line();
        if self.line < top {
            Point::new(top, Column(0))
        } else if self.line > bottom {
            Point::new(bottom, last_column)
        } else {
            self
        }
    }
}

/// Points are in the order of the text: by line, then by column.
impl Ord for Point {
    fn cmp(&self, other: &Point) -> Ordering {
        self.line.cmp(&other.line).then_with(|| self.column.cmp(&other.column))
    }
}

impl PartialOrd for Point {
    fn partial_cmp(&self, other: &Point) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A line of the grid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Line(pub i32);

impl From<usize> for Line {
    fn from(line: usize) -> Line {
        Line(line as i32)
    }
}

impl Add<usize> for Line {
    type Output = Line;

    #[inline]
    fn add(self, lines: usize) -> Line {
        Line(self.0 + lines as i32)
    }
}

impl AddAssign<usize> for Line {
    #[inline]
    fn add_assign(&mut self, lines: usize) {
        self.0 += lines as i32;
    }
}

impl Sub<usize> for Line {
    type Output = Line;

    #[inline]
    fn sub(self, lines: usize) -> Line {
        Line(self.0 - lines as i32)
    }
}

impl SubAssign<usize> for Line {
    #[inline]
    fn sub_assign(&mut self, lines: usize) {
        self.0 -= lines as i32;
    }
}

impl PartialEq<usize> for Line {
    #[inline]
    fn eq(&self, other: &usize) -> bool {
        self.0 == *other as i32
    }
}

impl PartialOrd<usize> for Line {
    #[inline]
    fn partial_cmp(&self, other: &usize) -> Option<Ordering> {
        self.0.partial_cmp(&(*other as i32))
    }
}

/// A column of the grid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Column(pub usize);

/// What a line and a column have in common with the number they wrap.
macro_rules! numeric {
    ($name:ident, $number:ty) => {
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl Deref for $name {
            type Target = $number;

            #[inline]
            fn deref(&self) -> &$number {
                &self.0
            }
        }

        impl From<$number> for $name {
            #[inline]
            fn from(number: $number) -> $name {
                $name(number)
            }
        }

        impl Add for $name {
            type Output = $name;

            #[inline]
            fn add(self, other: $name) -> $name {
                $name(self.0 + other.0)
            }
        }

        impl Add<$number> for $name {
            type Output = $name;

            #[inline]
            fn add(self, other: $number) -> $name {
                $name(self.0 + other)
            }
        }

        impl AddAssign for $name {
            #[inline]
            fn add_assign(&mut self, other: $name) {
                self.0 += other.0;
            }
        }

        impl AddAssign<$number> for $name {
            #[inline]
            fn add_assign(&mut self, other: $number) {
                self.0 += other;
            }
        }

        impl Sub for $name {
            type Output = $name;

            #[inline]
            fn sub(self, other: $name) -> $name {
                $name(self.0 - other.0)
            }
        }

        impl Sub<$number> for $name {
            type Output = $name;

            #[inline]
            fn sub(self, other: $number) -> $name {
                $name(self.0 - other)
            }
        }

        impl SubAssign for $name {
            #[inline]
            fn sub_assign(&mut self, other: $name) {
                self.0 -= other.0;
            }
        }

        impl SubAssign<$number> for $name {
            #[inline]
            fn sub_assign(&mut self, other: $number) {
                self.0 -= other;
            }
        }

        impl PartialEq<$number> for $name {
            #[inline]
            fn eq(&self, other: &$number) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<$name> for $number {
            #[inline]
            fn eq(&self, other: &$name) -> bool {
                *self == other.0
            }
        }

        impl PartialOrd<$number> for $name {
            #[inline]
            fn partial_cmp(&self, other: &$number) -> Option<Ordering> {
                self.0.partial_cmp(other)
            }
        }

        impl PartialOrd<$name> for $number {
            #[inline]
            fn partial_cmp(&self, other: &$name) -> Option<Ordering> {
                self.partial_cmp(&other.0)
            }
        }
    };
}

numeric!(Line, i32);
numeric!(Column, usize);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_are_in_the_order_of_the_text() {
        let point = |line, column| Point::new(Line(line), Column(column));
        assert!(point(0, 0) == point(0, 0));
        assert!(point(1, 0) > point(0, 0));
        assert!(point(0, 1) > point(0, 0));
        assert!(point(1, 0) > point(0, 9));
        assert!(point(1, 1) > point(1, 0));
        assert!(point(0, 0) > point(-1, 5));
    }

    #[test]
    fn back_along_a_line() {
        let point = Point::new(Line(0), Column(13));
        assert_eq!(point.sub(&(10, 42), Boundary::Cursor, 1), Point::new(Line(0), Column(12)));
        assert_eq!(point.sub(&(10, 42), Boundary::Cursor, 13 + 42), Point::new(Line(0), Column(0)));
    }

    #[test]
    fn back_to_the_line_before() {
        let point = Point::new(Line(1), Column(0));
        assert_eq!(point.sub(&(10, 42), Boundary::Cursor, 1), Point::new(Line(0), Column(41)));
        assert_eq!(point.sub(&(10, 42), Boundary::Cursor, 43), Point::new(Line(0), Column(0)));
    }

    #[test]
    fn back_stops_at_the_boundary() {
        let origin = Point::new(Line(0), Column(0));
        assert_eq!(origin.sub(&(10, 42), Boundary::Cursor, 1), origin);
        assert_eq!(origin.sub(&(0, 42), Boundary::Grid, 1), origin);
    }

    #[test]
    fn a_point_past_the_bottom_is_kept_at_the_last_cell() {
        let below = Point::new(Line(12), Column(50));
        assert_eq!(below.grid_clamp(&(10, 42), Boundary::Grid), Point::new(Line(9), Column(41)));
        let above = Point::new(Line(-1), Column(5));
        assert_eq!(above.grid_clamp(&(10, 42), Boundary::Cursor), Point::new(Line(0), Column(0)));
    }
}
