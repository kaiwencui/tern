//! Glyphs drawn in code instead of taken from the font: box drawing, block
//! elements, braille, Powerline separators and the text decorations. They are
//! generated at the exact cell size so lines join and blocks tile with no gaps.

use crate::CellMetrics;

/// An 8-bit coverage image, row by row from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mask {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Mask {
    pub(crate) fn new(w: u32, h: u32) -> Mask {
        Mask {
            w,
            h,
            data: vec![0; w as usize * h as usize],
        }
    }

    #[cfg(test)]
    pub(crate) fn at(&self, x: u32, y: u32) -> u8 {
        self.data[(y * self.w + x) as usize]
    }

    /// Fills [x0, x1) x [y0, y1), clipped to the mask.
    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, a: u8) {
        let (w, h) = (self.w as i32, self.h as i32);
        let (x0, x1) = (x0.clamp(0, w), x1.clamp(0, w));
        let (y0, y1) = (y0.clamp(0, h), y1.clamp(0, h));
        for y in y0..y1 {
            let row = (y * w) as usize;
            for v in &mut self.data[row + x0 as usize..row + x1.max(x0) as usize] {
                *v = (*v).max(a);
            }
        }
    }

    /// Antialiased shape: each pixel gets the fraction of a 4x4 grid of sample
    /// points for which inside(x, y) holds.
    fn shape(&mut self, inside: impl Fn(f32, f32) -> bool) {
        const N: u32 = 4;
        for y in 0..self.h {
            for x in 0..self.w {
                let mut hits = 0;
                for sy in 0..N {
                    for sx in 0..N {
                        let px = x as f32 + (sx as f32 + 0.5) / N as f32;
                        let py = y as f32 + (sy as f32 + 0.5) / N as f32;
                        hits += inside(px, py) as u32;
                    }
                }
                if hits > 0 {
                    let a = (hits * 255 / (N * N)) as u8;
                    let v = &mut self.data[(y * self.w + x) as usize];
                    *v = (*v).max(a);
                }
            }
        }
    }
}

/// Whether c is drawn by [builtin] rather than by the font.
pub(crate) fn is_builtin(c: u32) -> bool {
    matches!(c, 0x2500..=0x259f | 0x2800..=0x28ff | 0xe0b0..=0xe0b7)
}

/// The cell-sized coverage mask for a built-in character, or None when c is
/// not one.
pub(crate) fn builtin(c: u32, m: &CellMetrics) -> Option<Mask> {
    let mut mask = Mask::new(m.cell_w, m.cell_h);
    let g = Geometry::new(m);
    match c {
        0x2504..=0x250b | 0x254c..=0x254f => g.dashed(&mut mask, c),
        0x256d..=0x2570 => g.rounded(&mut mask, c),
        0x2571..=0x2573 => g.diagonal(&mut mask, c),
        0x2500..=0x257f => g.lines(&mut mask, arms(c)),
        0x2580..=0x259f => g.block(&mut mask, c),
        0x2800..=0x28ff => g.braille(&mut mask, c),
        0xe0b0..=0xe0b7 => g.powerline(&mut mask, c),
        _ => return None,
    }
    Some(mask)
}

/// Underline styles and strikeout, each a cell-sized mask that tiles across cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Decoration {
    Underline,
    Double,
    Curly,
    Dotted,
    Dashed,
    Strike,
}

impl Decoration {
    #[cfg(test)]
    pub(crate) const ALL: [Decoration; 6] = [
        Decoration::Underline,
        Decoration::Double,
        Decoration::Curly,
        Decoration::Dotted,
        Decoration::Dashed,
        Decoration::Strike,
    ];

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

pub(crate) fn decoration(d: Decoration, m: &CellMetrics) -> Mask {
    let (w, h) = (m.cell_w as i32, m.cell_h as i32);
    let t = m.underline_thickness.max(1) as i32;
    let mut mask = Mask::new(m.cell_w, m.cell_h);
    // Keep the line inside the cell even for fonts with a deep underline.
    let top = (m.underline_top as i32).min(h - t).max(0);
    match d {
        Decoration::Underline => mask.rect(0, top, w, top + t, 255),
        Decoration::Double => {
            let top = top.min(h - 3 * t).max(0);
            mask.rect(0, top, w, top + t, 255);
            mask.rect(0, top + 2 * t, w, top + 3 * t, 255);
        }
        Decoration::Curly => {
            // One period per cell so the wave continues into the next cell.
            let half = t as f32 * 0.5 + 0.25;
            let amp = (h as f32 / 14.0).max(t as f32);
            // The crests touch the underline's top; the troughs hang below it.
            let center = (top as f32 + amp)
                .min(h as f32 - amp - half)
                .max(amp + half);
            let k = std::f32::consts::TAU / w as f32;
            mask.shape(|x, y| {
                let fy = center - amp * (k * x).cos();
                let slope = amp * k * (k * x).sin();
                (y - fy).abs() / (1.0 + slope * slope).sqrt() <= half
            });
        }
        Decoration::Dotted => {
            let n = (w / (2 * t)).max(1);
            for i in 0..n {
                let x = i * w / n + (w / n - t) / 2;
                mask.rect(x, top, x + t, top + t, 255);
            }
        }
        Decoration::Dashed => {
            let inset = w / 6;
            mask.rect(inset, top, w - inset, top + t, 255);
        }
        Decoration::Strike => {
            let top = (m.strike_top as i32).min(h - t).max(0);
            mask.rect(0, top, w, top + t, 255);
        }
    }
    mask
}

const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// Line weights of the four arms (up, right, down, left) of U+2500..U+257F,
/// written as four digits: 0 none, 1 light, 2 heavy, 3 double. Dashed lines,
/// arcs and diagonals are drawn separately and show as 0000 here.
const ARMS: [u16; 128] = [
    0x0101, 0x0202, 0x1010, 0x2020, 0, 0, 0, 0, 0, 0, 0, 0, // 2500-250b
    0x0110, 0x0210, 0x0120, 0x0220, // 250c ┌ ┍ ┎ ┏
    0x0011, 0x0012, 0x0021, 0x0022, // 2510 ┐ ┑ ┒ ┓
    0x1100, 0x1200, 0x2100, 0x2200, // 2514 └ ┕ ┖ ┗
    0x1001, 0x1002, 0x2001, 0x2002, // 2518 ┘ ┙ ┚ ┛
    0x1110, 0x1210, 0x2110, 0x1120, 0x2120, 0x2210, 0x1220, 0x2220, // 251c ├ .. ┣
    0x1011, 0x1012, 0x2011, 0x1021, 0x2021, 0x2012, 0x1022, 0x2022, // 2524 ┤ .. ┫
    0x0111, 0x0112, 0x0211, 0x0212, 0x0121, 0x0122, 0x0221, 0x0222, // 252c ┬ .. ┳
    0x1101, 0x1102, 0x1201, 0x1202, 0x2101, 0x2102, 0x2201, 0x2202, // 2534 ┴ .. ┻
    0x1111, 0x1112, 0x1211, 0x1212, 0x2111, 0x1121, 0x2121, 0x2112, // 253c ┼ .. ╃
    0x2211, 0x1122, 0x1221, 0x2212, 0x1222, 0x2122, 0x2221, 0x2222, // 2544 ╄ .. ╋
    0, 0, 0, 0, // 254c-254f dashed
    0x0303, 0x3030, // 2550 ═ ║
    0x0310, 0x0130, 0x0330, // 2552 ╒ ╓ ╔
    0x0013, 0x0031, 0x0033, // 2555 ╕ ╖ ╗
    0x1300, 0x3100, 0x3300, // 2558 ╘ ╙ ╚
    0x1003, 0x3001, 0x3003, // 255b ╛ ╜ ╝
    0x1310, 0x3130, 0x3330, // 255e ╞ ╟ ╠
    0x1013, 0x3031, 0x3033, // 2561 ╡ ╢ ╣
    0x0313, 0x0131, 0x0333, // 2564 ╤ ╥ ╦
    0x1303, 0x3101, 0x3303, // 2567 ╧ ╨ ╩
    0x1313, 0x3131, 0x3333, // 256a ╪ ╫ ╬
    0, 0, 0, 0, 0, 0, 0, // 256d-2573 arcs and diagonals
    0x0001, 0x1000, 0x0100, 0x0010, // 2574 ╴ ╵ ╶ ╷
    0x0002, 0x2000, 0x0200, 0x0020, // 2578 ╸ ╹ ╺ ╻
    0x0201, 0x1020, 0x0102, 0x2010, // 257c ╼ ╽ ╾ ╿
];

/// (up, right, down, left) weights of a line-drawing character.
fn arms(c: u32) -> [u8; 4] {
    let v = ARMS[(c - 0x2500) as usize];
    [
        (v >> 12) as u8 & 0xf,
        (v >> 8) as u8 & 0xf,
        (v >> 4) as u8 & 0xf,
        v as u8 & 0xf,
    ]
}

/// Stroke positions shared by every built-in character of one cell size, so
/// that the same line lands on the same pixels in every cell.
struct Geometry {
    w: i32,
    h: i32,
    light: i32,
    heavy: i32,
}

impl Geometry {
    fn new(m: &CellMetrics) -> Geometry {
        let light = m.stroke.max(1) as i32;
        Geometry {
            w: m.cell_w as i32,
            h: m.cell_h as i32,
            light,
            heavy: light * 2,
        }
    }

    fn thickness(&self, weight: u8) -> i32 {
        match weight {
            LIGHT => self.light,
            HEAVY => self.heavy,
            _ => 0,
        }
    }

    /// Top row of a horizontal stroke of thickness t.
    fn hy(&self, t: i32) -> i32 {
        (self.h - t) / 2
    }

    /// Left column of a vertical stroke of thickness t.
    fn vx(&self, t: i32) -> i32 {
        (self.w - t) / 2
    }

    /// Top row of the band a double horizontal line occupies (stroke, gap, stroke).
    fn bh(&self) -> i32 {
        (self.h - 3 * self.light) / 2
    }

    /// Left column of the band a double vertical line occupies.
    fn bv(&self) -> i32 {
        (self.w - 3 * self.light) / 2
    }

    fn lines(&self, mask: &mut Mask, a: [u8; 4]) {
        let [up, right, down, left] = a;
        let (w, h, l) = (self.w, self.h, self.light);
        let (bh, bv) = (self.bh(), self.bv());
        let thick = |weight: u8| {
            if weight == DOUBLE {
                0
            } else {
                self.thickness(weight)
            }
        };
        // Thickest single stroke crossing the center in each direction.
        let tv = thick(up).max(thick(down));
        let th = thick(left).max(thick(right));
        let vdouble = up == DOUBLE || down == DOUBLE;
        let hdouble = left == DOUBLE || right == DOUBLE;

        // Horizontal arms: x extent of each, measured against the vertical strokes.
        for (side, weight) in [(1, right), (-1, left)] {
            if weight == NONE {
                continue;
            }
            let opposite = if side == 1 { left } else { right };
            if weight == DOUBLE {
                for (k, perpendicular) in [(0, up), (2, down)] {
                    let y = bh + k * l;
                    let (start, end) = if vdouble {
                        // Stop at the inner stroke of a double arm on this side,
                        // run to the outer stroke when that side is empty.
                        if perpendicular == DOUBLE {
                            (bv + 2 * l, bv + l)
                        } else {
                            (bv, bv + 3 * l)
                        }
                    } else if tv > 0 {
                        (self.vx(tv), self.vx(tv) + tv)
                    } else {
                        (bv, bv + 3 * l)
                    };
                    if side == 1 {
                        mask.rect(start, y, w, y + l, 255);
                    } else {
                        mask.rect(0, y, end, y + l, 255);
                    }
                }
            } else {
                let t = self.thickness(weight);
                let y = self.hy(t);
                let (start, end) = if vdouble {
                    // A single line ending at a double one that runs straight
                    // through stops at its near stroke; otherwise it caps it.
                    if up == DOUBLE && down == DOUBLE && opposite == NONE {
                        (bv + 2 * l, bv + l)
                    } else {
                        (bv, bv + 3 * l)
                    }
                } else {
                    let tt = if tv > 0 { tv } else { t };
                    (self.vx(tt), self.vx(tt) + tt)
                };
                if side == 1 {
                    mask.rect(start, y, w, y + t, 255);
                } else {
                    mask.rect(0, y, end, y + t, 255);
                }
            }
        }

        // Vertical arms, the same rules turned a quarter.
        for (side, weight) in [(1, down), (-1, up)] {
            if weight == NONE {
                continue;
            }
            let opposite = if side == 1 { up } else { down };
            if weight == DOUBLE {
                for (k, perpendicular) in [(0, left), (2, right)] {
                    let x = bv + k * l;
                    let (start, end) = if hdouble {
                        if perpendicular == DOUBLE {
                            (bh + 2 * l, bh + l)
                        } else {
                            (bh, bh + 3 * l)
                        }
                    } else if th > 0 {
                        (self.hy(th), self.hy(th) + th)
                    } else {
                        (bh, bh + 3 * l)
                    };
                    if side == 1 {
                        mask.rect(x, start, x + l, h, 255);
                    } else {
                        mask.rect(x, 0, x + l, end, 255);
                    }
                }
            } else {
                let t = self.thickness(weight);
                let x = self.vx(t);
                let (start, end) = if hdouble {
                    if left == DOUBLE && right == DOUBLE && opposite == NONE {
                        (bh + 2 * l, bh + l)
                    } else {
                        (bh, bh + 3 * l)
                    }
                } else {
                    let tt = if th > 0 { th } else { t };
                    (self.hy(tt), self.hy(tt) + tt)
                };
                if side == 1 {
                    mask.rect(x, start, x + t, h, 255);
                } else {
                    mask.rect(x, 0, x + t, end, 255);
                }
            }
        }
    }

    /// ┄ ┅ ┆ ┇ ┈ ┉ ┊ ┋ ╌ ╍ ╎ ╏: the cell split into equal parts, each with a
    /// dash centered in it, so dashes keep their rhythm across cells.
    fn dashed(&self, mask: &mut Mask, c: u32) {
        let (n, vertical, heavy) = match c {
            0x2504 => (3, false, false),
            0x2505 => (3, false, true),
            0x2506 => (3, true, false),
            0x2507 => (3, true, true),
            0x2508 => (4, false, false),
            0x2509 => (4, false, true),
            0x250a => (4, true, false),
            0x250b => (4, true, true),
            0x254c => (2, false, false),
            0x254d => (2, false, true),
            0x254e => (2, true, false),
            _ => (2, true, true),
        };
        let t = if heavy { self.heavy } else { self.light };
        let len = if vertical { self.h } else { self.w };
        for i in 0..n {
            let (a, b) = (i * len / n, (i + 1) * len / n);
            let seg = b - a;
            let gap = (seg / 3).max(1).min((seg - 1).max(0));
            let (s, e) = (a + gap / 2, b - (gap - gap / 2));
            if vertical {
                let x = self.vx(t);
                mask.rect(x, s, x + t, e, 255);
            } else {
                let y = self.hy(t);
                mask.rect(s, y, e, y + t, 255);
            }
        }
    }

    /// ╭ ╮ ╯ ╰: straight light lines from the edges into a quarter circle.
    fn rounded(&self, mask: &mut Mask, c: u32) {
        let t = self.light;
        let (x0, y0) = (self.vx(t), self.hy(t));
        // Centers of the straight strokes, in continuous coordinates.
        let (cx, cy) = (x0 as f32 + t as f32 / 2.0, y0 as f32 + t as f32 / 2.0);
        // (sx, sy): the direction from the corner to the arc's center.
        let (sx, sy) = match c {
            0x256d => (1.0, 1.0),   // ╭ down and right
            0x256e => (-1.0, 1.0),  // ╮ down and left
            0x256f => (-1.0, -1.0), // ╯ up and left
            _ => (1.0, -1.0),       // ╰ up and right
        };
        let room_x = if sx > 0.0 { self.w as f32 - cx } else { cx };
        let room_y = if sy > 0.0 { self.h as f32 - cy } else { cy };
        // One pixel short of the edge, so the last pixel row or column
        // toward each edge is straight and matches ─ and │ exactly.
        let r = (room_x.min(room_y) - 1.0).max(1.0);
        let (ax, ay) = (cx + sx * r, cy + sy * r);
        let half = t as f32 / 2.0;
        mask.shape(|x, y| {
            if (x - ax) * sx > 0.0 || (y - ay) * sy > 0.0 {
                return false;
            }
            let d = ((x - ax).powi(2) + (y - ay).powi(2)).sqrt();
            (d - r).abs() <= half
        });
        // The straight parts use the same pixels as ─ and │, starting in the
        // pixel where the arc ends.
        if sx > 0.0 {
            mask.rect(ax.floor() as i32, y0, self.w, y0 + t, 255);
        } else {
            mask.rect(0, y0, ax.ceil() as i32, y0 + t, 255);
        }
        if sy > 0.0 {
            mask.rect(x0, ay.floor() as i32, x0 + t, self.h, 255);
        } else {
            mask.rect(x0, 0, x0 + t, ay.ceil() as i32, 255);
        }
    }

    /// ╱ ╲ ╳: corner to corner, so diagonals continue into the next cell.
    fn diagonal(&self, mask: &mut Mask, c: u32) {
        let (w, h) = (self.w as f32, self.h as f32);
        let half = self.light as f32 / 2.0 + 0.15;
        let len = (w * w + h * h).sqrt();
        // Distance from (x, y) to the line through (0, 0) and (w, h), and to
        // the line through (w, 0) and (0, h).
        let down = move |x: f32, y: f32| (h * x - w * y).abs() / len <= half;
        let up = move |x: f32, y: f32| (h * x + w * y - w * h).abs() / len <= half;
        match c {
            0x2571 => mask.shape(up),
            0x2572 => mask.shape(down),
            _ => mask.shape(|x, y| up(x, y) || down(x, y)),
        }
    }

    fn block(&self, mask: &mut Mask, c: u32) {
        let (w, h) = (self.w, self.h);
        // Eighths are measured from fixed split points so complementary
        // blocks (▀ and ▄, ▌ and ▐) cover every pixel exactly once.
        let ys = |k: i32| (h * k + 4) / 8;
        let xs = |k: i32| (w * k + 4) / 8;
        let (mx, my) = (xs(4), ys(4));
        match c {
            0x2580 => mask.rect(0, 0, w, my, 255),
            0x2581..=0x2587 => mask.rect(0, ys(8 - (c - 0x2580) as i32), w, h, 255),
            0x2588 => mask.rect(0, 0, w, h, 255),
            0x2589..=0x258f => mask.rect(0, 0, xs(8 - (c - 0x2588) as i32), h, 255),
            0x2590 => mask.rect(mx, 0, w, h, 255),
            0x2591 => mask.rect(0, 0, w, h, 64),
            0x2592 => mask.rect(0, 0, w, h, 128),
            0x2593 => mask.rect(0, 0, w, h, 191),
            0x2594 => mask.rect(0, 0, w, ys(1), 255),
            0x2595 => mask.rect(xs(7), 0, w, h, 255),
            _ => {
                // Quadrants: bits for upper left, upper right, lower left, lower right.
                let q = match c {
                    0x2596 => 0b0010,
                    0x2597 => 0b0001,
                    0x2598 => 0b1000,
                    0x2599 => 0b1011,
                    0x259a => 0b1001,
                    0x259b => 0b1110,
                    0x259c => 0b1101,
                    0x259d => 0b0100,
                    0x259e => 0b0110,
                    _ => 0b0111,
                };
                if q & 0b1000 != 0 {
                    mask.rect(0, 0, mx, my, 255);
                }
                if q & 0b0100 != 0 {
                    mask.rect(mx, 0, w, my, 255);
                }
                if q & 0b0010 != 0 {
                    mask.rect(0, my, mx, h, 255);
                }
                if q & 0b0001 != 0 {
                    mask.rect(mx, my, w, h, 255);
                }
            }
        }
    }

    /// Braille: eight dots in two columns of four. Bit k of (c - U+2800) is dot k+1.
    fn braille(&self, mask: &mut Mask, c: u32) {
        let bits = c - 0x2800;
        let (w, h) = (self.w, self.h);
        let col = |i: i32| i * w / 2;
        let row = |i: i32| (i * h + 2) / 4;
        let dot = ((w / 2).min(h / 4) * 3 / 5).max(1);
        // (bit, column, row) in the Unicode dot numbering.
        const DOTS: [(u32, i32, i32); 8] = [
            (0, 0, 0),
            (1, 0, 1),
            (2, 0, 2),
            (3, 1, 0),
            (4, 1, 1),
            (5, 1, 2),
            (6, 0, 3),
            (7, 1, 3),
        ];
        for (bit, cx, cy) in DOTS {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let (x0, x1) = (col(cx), col(cx + 1));
            let (y0, y1) = (row(cy), row(cy + 1));
            let x = x0 + (x1 - x0 - dot) / 2;
            let y = y0 + (y1 - y0 - dot) / 2;
            mask.rect(x, y, x + dot, y + dot, 255);
        }
    }

    /// Powerline separators: solid and outlined triangles and half circles
    /// that fill the full cell height so they meet the neighbouring backgrounds.
    fn powerline(&self, mask: &mut Mask, c: u32) {
        let (w, h) = (self.w as f32, self.h as f32);
        let half = self.light as f32 / 2.0 + 0.15;
        let mid = h / 2.0;
        // Mirror for the left-pointing shapes.
        let flip = matches!(c, 0xe0b2 | 0xe0b3 | 0xe0b6 | 0xe0b7);
        let fx = move |x: f32| if flip { w - x } else { x };
        match c {
            0xe0b0 | 0xe0b2 => mask.shape(|x, y| fx(x) <= w * (1.0 - (y - mid).abs() / mid)),
            0xe0b1 | 0xe0b3 => {
                // Two segments from the tall edge's corners to the middle of the far edge.
                let len = (w * w + mid * mid).sqrt();
                mask.shape(|x, y| {
                    let x = fx(x);
                    let d = if y <= mid {
                        (mid * x - w * y).abs() / len
                    } else {
                        (mid * x + w * (y - h)).abs() / len
                    };
                    d <= half && x <= w
                });
            }
            0xe0b4 | 0xe0b6 => mask.shape(|x, y| {
                let (nx, ny) = (fx(x) / w, (y - mid) / mid);
                nx * nx + ny * ny <= 1.0
            }),
            _ => {
                let t = self.light as f32;
                mask.shape(|x, y| {
                    let x = fx(x);
                    let (nx, ny) = (x / w, (y - mid) / mid);
                    let (ix, iy) = (x / (w - t).max(1.0), (y - mid) / (mid - t).max(1.0));
                    nx * nx + ny * ny <= 1.0 && ix * ix + iy * iy >= 1.0
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(w: u32, h: u32, stroke: u32) -> CellMetrics {
        CellMetrics {
            cell_w: w,
            cell_h: h,
            baseline: h * 4 / 5,
            underline_top: h * 4 / 5 + 1,
            underline_thickness: stroke,
            strike_top: h / 2,
            stroke,
        }
    }

    fn sizes() -> Vec<CellMetrics> {
        vec![
            metrics(7, 15, 1),
            metrics(8, 17, 1),
            metrics(14, 30, 2),
            metrics(17, 36, 2),
            metrics(9, 20, 3),
        ]
    }

    fn sprite(c: char, m: &CellMetrics) -> Mask {
        builtin(c as u32, m).expect("built-in")
    }

    fn covered(mask: &Mask) -> Vec<(u32, u32)> {
        let mut v = Vec::new();
        for y in 0..mask.h {
            for x in 0..mask.w {
                if mask.at(x, y) > 0 {
                    v.push((x, y));
                }
            }
        }
        v
    }

    #[test]
    fn every_builtin_is_cell_sized_and_drawn() {
        let m = metrics(9, 19, 1);
        for c in (0x2500..=0x259f)
            .chain(0x2800..=0x28ff)
            .chain(0xe0b0..=0xe0b7)
        {
            let mask = builtin(c, &m).unwrap();
            assert_eq!((mask.w, mask.h), (9, 19));
            // Only the empty braille pattern draws nothing.
            if c != 0x2800 {
                assert!(mask.data.iter().any(|&a| a > 0), "U+{c:04X} is empty");
            }
        }
        assert!(builtin('a' as u32, &m).is_none());
        assert!(!is_builtin('a' as u32));
        assert!(is_builtin(0x2500) && is_builtin(0x28ff) && is_builtin(0xe0b7));
        assert!(!is_builtin(0xe0b8));
    }

    #[test]
    fn light_lines_cross_at_the_center_pixel() {
        for m in sizes() {
            let hz = sprite('─', &m);
            let vt = sprite('│', &m);
            let both: Vec<_> = covered(&hz)
                .into_iter()
                .filter(|&(x, y)| vt.at(x, y) == 255)
                .collect();
            let t = m.stroke;
            assert_eq!(both.len() as u32, t * t, "{m:?}");
            let (cx, cy) = ((m.cell_w - t) / 2, (m.cell_h - t) / 2);
            assert!(both.contains(&(cx, cy)));
            // Cross equals the union of the two lines.
            let cross = sprite('┼', &m);
            for y in 0..m.cell_h {
                for x in 0..m.cell_w {
                    assert_eq!(cross.at(x, y), hz.at(x, y).max(vt.at(x, y)));
                }
            }
        }
    }

    #[test]
    fn lines_reach_the_edges_so_neighbours_join() {
        for m in sizes() {
            let (w, h) = (m.cell_w, m.cell_h);
            for c in ['─', '━', '═', '┼', '╋', '╬', '╪', '╫', '├', '┤'] {
                let s = sprite(c, &m);
                // The rows touching the left edge are exactly those touching the right edge.
                let left: Vec<u8> = (0..h).map(|y| s.at(0, y)).collect();
                let right: Vec<u8> = (0..h).map(|y| s.at(w - 1, y)).collect();
                if c != '├' && c != '┤' {
                    assert_eq!(left, right, "{c} {m:?}");
                }
            }
            for c in ['│', '┃', '║', '┼', '╬', '╪', '╫', '┬', '┴'] {
                let s = sprite(c, &m);
                let top: Vec<u8> = (0..w).map(|x| s.at(x, 0)).collect();
                let bottom: Vec<u8> = (0..w).map(|x| s.at(x, h - 1)).collect();
                if c != '┬' && c != '┴' {
                    assert_eq!(top, bottom, "{c} {m:?}");
                }
            }
            // A corner's arm lines up with the straight line it continues.
            let corner = sprite('┌', &m);
            let hz = sprite('─', &m);
            let vt = sprite('│', &m);
            for y in 0..h {
                assert_eq!(corner.at(w - 1, y), hz.at(w - 1, y));
            }
            for x in 0..w {
                assert_eq!(corner.at(x, h - 1), vt.at(x, h - 1));
            }
            let round = sprite('╭', &m);
            for y in 0..h {
                assert_eq!(round.at(w - 1, y), hz.at(w - 1, y), "╭ right edge {m:?}");
            }
            for x in 0..w {
                assert_eq!(round.at(x, h - 1), vt.at(x, h - 1), "╭ bottom edge {m:?}");
            }
        }
    }

    #[test]
    fn heavy_is_thicker_than_light() {
        let m = metrics(10, 20, 1);
        let light = covered(&sprite('─', &m)).len();
        let heavy = covered(&sprite('━', &m)).len();
        assert_eq!(light, 10);
        assert_eq!(heavy, 20);
    }

    #[test]
    fn double_lines_have_two_strokes_and_corners_close() {
        // Cells wide enough that the double band does not reach the edges.
        for m in sizes().into_iter().filter(|m| 3 * m.stroke + 2 <= m.cell_w) {
            let s = sprite('═', &m);
            let col: Vec<bool> = (0..m.cell_h).map(|y| s.at(0, y) > 0).collect();
            let runs = col.windows(2).filter(|p| !p[0] && p[1]).count() + col[0] as usize;
            assert_eq!(runs, 2, "{m:?}");
            // ╔ joined to ═ on its right and ║ below matches both exactly at the edges.
            let corner = sprite('╔', &m);
            let v = sprite('║', &m);
            for y in 0..m.cell_h {
                assert_eq!(corner.at(m.cell_w - 1, y), s.at(m.cell_w - 1, y));
            }
            for x in 0..m.cell_w {
                assert_eq!(corner.at(x, m.cell_h - 1), v.at(x, m.cell_h - 1));
            }
            // The top-left of the corner's band is drawn (outer corner closed),
            // and the inside of the band right of the inner corner stays open.
            let l = m.stroke as i32;
            let (bv, bh) = ((m.cell_w as i32 - 3 * l) / 2, (m.cell_h as i32 - 3 * l) / 2);
            assert_eq!(corner.at(bv as u32, bh as u32), 255);
            assert_eq!(corner.at((bv + l) as u32, (bh + l) as u32), 0);
        }
    }

    #[test]
    fn full_block_covers_every_pixel_and_halves_tile() {
        for m in sizes() {
            assert!(sprite('█', &m).data.iter().all(|&a| a == 255));
            // Complementary pairs cover every pixel exactly once.
            let pairs = [
                ('▀', '▄'),
                ('▌', '▐'),
                ('▘', '▟'),
                ('▚', '▞'),
                ('▔', '▇'),
                ('▕', '▉'),
            ];
            for (a, b) in pairs {
                let (a_m, b_m) = (sprite(a, &m), sprite(b, &m));
                for i in 0..a_m.data.len() {
                    let sum = a_m.data[i] as u32 + b_m.data[i] as u32;
                    assert_eq!(sum, 255, "{a}{b} pixel {i} {m:?}");
                }
            }
            let shade = sprite('▒', &m);
            assert!(shade.data.iter().all(|&a| a == 128));
        }
    }

    #[test]
    fn braille_dots_land_in_their_quadrants() {
        let m = metrics(8, 16, 1);
        assert!(covered(&sprite('\u{2800}', &m)).is_empty());
        let all = covered(&sprite('\u{28ff}', &m));
        // Dot 1 alone is in the top-left eighth, dot 8 alone in the bottom-right.
        let d1 = covered(&sprite('\u{2801}', &m));
        assert!(!d1.is_empty() && d1.iter().all(|&(x, y)| x < 4 && y < 4));
        let d8 = covered(&sprite('\u{2880}', &m));
        assert!(!d8.is_empty() && d8.iter().all(|&(x, y)| x >= 4 && y >= 12));
        assert_eq!(all.len(), d1.len() * 8);
    }

    #[test]
    fn powerline_triangle_spans_full_height_and_points_right() {
        for m in sizes() {
            let s = sprite('\u{e0b0}', &m);
            // Left column is solid from top to bottom.
            assert!((0..m.cell_h).all(|y| s.at(0, y) > 0));
            // Right column only touched near the middle.
            for y in 0..m.cell_h {
                if s.at(m.cell_w - 1, y) > 0 {
                    assert!((y as i32 - m.cell_h as i32 / 2).abs() <= 2);
                }
            }
            let mirrored = sprite('\u{e0b2}', &m);
            assert!((0..m.cell_h).all(|y| mirrored.at(m.cell_w - 1, y) > 0));
        }
    }

    #[test]
    fn decorations_stay_inside_the_cell_and_tile() {
        for m in sizes() {
            for d in Decoration::ALL {
                let s = decoration(d, &m);
                assert_eq!((s.w, s.h), (m.cell_w, m.cell_h));
                assert!(s.data.iter().any(|&a| a > 0), "{d:?}");
            }
            let ul = decoration(Decoration::Underline, &m);
            let rows: Vec<u32> = (0..m.cell_h).filter(|&y| ul.at(0, y) == 255).collect();
            assert_eq!(rows.len() as u32, m.underline_thickness);
            assert!((0..m.cell_w).all(|x| ul.at(x, rows[0]) == 255));
            // The curl starts and ends at the same height so it continues across cells.
            let curl = decoration(Decoration::Curly, &m);
            let first: Vec<u8> = (0..m.cell_h).map(|y| curl.at(0, y)).collect();
            let last: Vec<u8> = (0..m.cell_h).map(|y| curl.at(m.cell_w - 1, y)).collect();
            let peak = |v: &Vec<u8>| {
                v.iter()
                    .enumerate()
                    .max_by_key(|p| p.1)
                    .map(|p| p.0)
                    .unwrap() as i32
            };
            assert!((peak(&first) - peak(&last)).abs() <= 1, "{m:?}");
        }
    }

    #[test]
    fn tiny_cells_and_thick_strokes_do_not_panic() {
        for (w, h, stroke) in [
            (1, 1, 1),
            (1, 2, 1),
            (2, 1, 1),
            (2, 3, 3),
            (3, 2, 5),
            (4, 9, 4),
        ] {
            let m = metrics(w, h, stroke);
            for c in (0x2500..=0x259f)
                .chain(0x2800..=0x28ff)
                .chain(0xe0b0..=0xe0b7)
            {
                let s = builtin(c, &m).unwrap();
                assert_eq!(s.data.len(), (w * h) as usize);
            }
            for d in Decoration::ALL {
                assert_eq!(decoration(d, &m).data.len(), (w * h) as usize);
            }
        }
    }

    #[test]
    fn deep_underline_is_pulled_into_the_cell() {
        let mut m = metrics(8, 16, 2);
        m.underline_top = 30;
        let s = decoration(Decoration::Double, &m);
        assert!((0..16).any(|y| s.at(0, y) == 255));
        assert_eq!(s.at(0, 15), 255);
    }
}
