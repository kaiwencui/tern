/// A point in pixels, origin at the top left, y growing downward.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// A rectangle in pixels, origin at the top left, y growing downward.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[inline]
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    #[inline]
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    #[inline]
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn center(&self) -> Point {
        Point {
            x: self.x + self.w / 2,
            y: self.y + self.h / 2,
        }
    }

    /// The rectangle shrunk by dx on the left and right and dy on the top and bottom.
    pub fn inset(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            w: (self.w - 2 * dx).max(0),
            h: (self.h - 2 * dy).max(0),
        }
    }
}
