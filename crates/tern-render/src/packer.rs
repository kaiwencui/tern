//! Shelf packing for the glyph atlases. Pure bookkeeping; the textures live in gpu.rs.

/// Rows ("shelves") of rectangles, each shelf as tall as the tallest thing
/// first put on it. Glyphs of one font are close in height, so shelves fill
/// well. Growing keeps every placed rectangle where it is, so coordinates
/// already handed out stay valid.
#[derive(Debug)]
pub(crate) struct Packer {
    width: u32,
    height: u32,
    max: u32,
    shelves: Vec<Shelf>,
    /// Top of the unused area below the last shelf.
    next_y: u32,
}

#[derive(Debug, Clone, Copy)]
struct Shelf {
    y: u32,
    h: u32,
    /// Where the next rectangle on this shelf starts.
    x: u32,
}

impl Packer {
    pub(crate) fn new(width: u32, height: u32, max: u32) -> Packer {
        Packer {
            width,
            height,
            max,
            shelves: Vec::new(),
            next_y: 0,
        }
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Finds room for a w x h rectangle without growing.
    pub(crate) fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.width || h > self.height {
            return None;
        }
        // The shelf that wastes the least height, among those with room.
        let mut best: Option<usize> = None;
        for (i, s) in self.shelves.iter().enumerate() {
            // A shelf much taller than the glyph wastes space; open a new one instead.
            let fits = s.h >= h && s.h <= h + h / 2 + 2 && self.width - s.x >= w;
            if fits && best.is_none_or(|b| self.shelves[b].h > s.h) {
                best = Some(i);
            }
        }
        if let Some(i) = best {
            let s = &mut self.shelves[i];
            let at = (s.x, s.y);
            s.x += w;
            return Some(at);
        }
        if self.height - self.next_y >= h {
            let y = self.next_y;
            self.shelves.push(Shelf { y, h, x: w });
            self.next_y += h;
            return Some((0, y));
        }
        None
    }

    /// The size to grow to when full: the smaller side doubles, so the atlas
    /// stays roughly square. None at the maximum.
    pub(crate) fn grown_size(&self) -> Option<(u32, u32)> {
        if self.width >= self.max && self.height >= self.max {
            return None;
        }
        if self.height < self.width || self.width >= self.max {
            Some((self.width, (self.height * 2).min(self.max)))
        } else {
            Some(((self.width * 2).min(self.max), self.height))
        }
    }

    /// Takes the new size; existing shelves gain the extra width.
    pub(crate) fn grow_to(&mut self, width: u32, height: u32) {
        debug_assert!(width >= self.width && height >= self.height);
        self.width = width;
        self.height = height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> bool {
        a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
    }

    #[test]
    fn fills_a_shelf_left_to_right() {
        let mut p = Packer::new(100, 100, 100);
        assert_eq!(p.alloc(10, 20), Some((0, 0)));
        assert_eq!(p.alloc(10, 20), Some((10, 0)));
        assert_eq!(p.alloc(10, 18), Some((20, 0)));
        // Much shorter glyphs open their own shelf.
        assert_eq!(p.alloc(5, 5), Some((0, 20)));
    }

    #[test]
    fn rejects_what_does_not_fit_and_grows() {
        let mut p = Packer::new(16, 16, 64);
        assert_eq!(p.alloc(17, 1), None);
        assert_eq!(p.alloc(16, 16), Some((0, 0)));
        assert_eq!(p.alloc(1, 1), None);
        assert_eq!(p.grown_size(), Some((32, 16)));
        p.grow_to(32, 16);
        // The existing shelf gains the new width.
        assert_eq!(p.alloc(16, 16), Some((16, 0)));
        assert_eq!(p.grown_size(), Some((32, 32)));
        p.grow_to(32, 32);
        assert_eq!(p.alloc(8, 8), Some((0, 16)));
        p.grow_to(64, 64);
        assert_eq!(p.grown_size(), None);
    }

    #[test]
    fn many_glyphs_never_overlap_and_stay_inside() {
        let mut p = Packer::new(64, 64, 1024);
        let mut placed = Vec::new();
        let mut grows = 0;
        for i in 0..3000u32 {
            let (w, h) = (3 + i * 7 % 13, 8 + i * 5 % 11);
            let at = loop {
                if let Some(at) = p.alloc(w, h) {
                    break at;
                }
                let (nw, nh) = p.grown_size().expect("room up to the maximum");
                p.grow_to(nw, nh);
                grows += 1;
            };
            let r = (at.0, at.1, w, h);
            let (pw, ph) = p.size();
            assert!(r.0 + r.2 <= pw && r.1 + r.3 <= ph);
            placed.push(r);
        }
        assert!(grows > 0);
        for (i, a) in placed.iter().enumerate() {
            for b in &placed[i + 1..] {
                assert!(!overlaps(*a, *b), "{a:?} overlaps {b:?}");
            }
        }
    }
}
