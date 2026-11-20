//! The split-pane tree of one tab, as pure logic with no drawing and no OS
//! calls.
//!
//! A [`Layout`] is a binary tree. Every leaf is a pane; every split divides
//! its rectangle between two children along an [`Axis`], giving the first
//! child a share (the ratio) of the space left beside the divider. The layout
//! also remembers which pane has focus and whether that pane is zoomed to fill
//! the whole area.
//!
//! Geometry is computed on request for whatever area the caller passes, in
//! integer device pixels, so one layout serves every window size. Pane and
//! divider rectangles always tile that area exactly. A negative width or
//! height counts as zero.
//!
//! ```
//! use tern_layout::{Direction, Layout};
//! use tern_types::Rect;
//!
//! let mut layout = Layout::new(1);
//! layout.split(1, Direction::Right, 2)?;
//! let area = Rect::new(0, 0, 101, 40);
//! assert_eq!(
//!     layout.rects(area, 1),
//!     vec![(1, Rect::new(0, 0, 50, 40)), (2, Rect::new(51, 0, 50, 40))],
//! );
//! assert_eq!(layout.to_string(), "h0.5(1,*2)");
//! assert_eq!("h0.5(1,*2)".parse::<Layout>()?, layout);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

mod layout;
mod text;
mod tree;
mod zone;

#[cfg(test)]
mod random_tests;
pub use layout::{Closed, Divider, DividerHandle, Layout, LayoutError, MAX_DEPTH};
pub use text::ParseLayoutError;
pub use zone::{drop_zone, zone_rect};

use tern_types::{Point, Rect};

/// How a split arranges its two children.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Axis {
    /// Side by side, the first child on the left. The divider between them is
    /// a vertical line that moves left and right.
    Horizontal,
    /// Stacked, the first child on top. The divider is a horizontal line that
    /// moves up and down.
    Vertical,
}

impl Axis {
    /// The rectangle's length along this axis, never negative.
    pub(crate) fn extent(self, r: Rect) -> i32 {
        match self {
            Axis::Horizontal => r.w.max(0),
            Axis::Vertical => r.h.max(0),
        }
    }

    /// Where the rectangle starts along this axis.
    pub(crate) fn start(self, r: Rect) -> i32 {
        match self {
            Axis::Horizontal => r.x,
            Axis::Vertical => r.y,
        }
    }

    /// The other axis.
    pub(crate) fn cross(self) -> Axis {
        match self {
            Axis::Horizontal => Axis::Vertical,
            Axis::Vertical => Axis::Horizontal,
        }
    }

    /// The point's coordinate along this axis.
    pub(crate) fn coord(self, p: Point) -> i32 {
        match self {
            Axis::Horizontal => p.x,
            Axis::Vertical => p.y,
        }
    }
}

/// A side of a pane, for splitting, resizing and moving focus.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// The axis along which this direction points.
    pub(crate) fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }

    /// Whether this direction points toward larger coordinates.
    pub(crate) fn is_forward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }
}

/// Where a pane dragged over another pane would land: beside it on one of
/// its sides, or in its place.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

impl Zone {
    /// The side an edge zone names; None for the center.
    pub(crate) fn side(self) -> Option<Direction> {
        match self {
            Zone::Left => Some(Direction::Left),
            Zone::Right => Some(Direction::Right),
            Zone::Top => Some(Direction::Up),
            Zone::Bottom => Some(Direction::Down),
            Zone::Center => None,
        }
    }
}

/// The area with negative sizes raised to zero.
pub(crate) fn normalized(area: Rect) -> Rect {
    Rect::new(area.x, area.y, area.w.max(0), area.h.max(0))
}
