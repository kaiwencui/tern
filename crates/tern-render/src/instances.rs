//! Turning a Frame into instanced quads, and remembering them per pane so
//! only rows that changed are built again.

use tern_types::{flags, Cell, CursorShape, Frame, Rect, Rgb};

use crate::color::pack;
use crate::sprites::Decoration;
use crate::PaneDraw;

/// Instance kinds, read by the fragment shader.
pub(crate) const SOLID: u32 = 0;
pub(crate) const MASK: u32 = 1;
pub(crate) const COLOR: u32 = 2;

/// One quad as the vertex shader reads it. Positions are in device pixels;
/// for the cached pane data they are relative to the pane's grid origin, so
/// moving a pane costs nothing. uv is in texels of the atlas named by kind.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Inst {
    pub pos: [i16; 2],
    pub size: [u16; 2],
    pub uv: [u16; 2],
    pub color: u32,
    pub kind: u32,
}

/// Where a rasterized glyph lives in an atlas and where it sits relative to
/// the top-left corner of its cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Glyph {
    /// MASK or COLOR; SOLID here means there is nothing to draw (a blank glyph).
    pub kind: u32,
    pub uv: [u16; 2],
    pub size: [u16; 2],
    pub offset: [i16; 2],
}

impl Glyph {
    pub(crate) const EMPTY: Glyph = Glyph {
        kind: SOLID,
        uv: [0, 0],
        size: [0, 0],
        offset: [0, 0],
    };

    pub(crate) fn is_empty(&self) -> bool {
        self.kind == SOLID || self.size[0] == 0 || self.size[1] == 0
    }
}

/// Supplies rasterized glyphs; the renderer's glyph cache in production and a
/// fake in tests.
pub(crate) trait GlyphSource {
    /// The glyph for the cell at frame.cells[index], including its
    /// zero-width extras, style and width.
    fn glyph(&mut self, frame: &Frame, index: usize) -> Glyph;
    fn decoration(&mut self, d: Decoration) -> Glyph;
}

/// Everything besides the cells that the cached instances depend on. When it
/// changes the whole pane is rebuilt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u32,
    pub cell_h: u32,
    pub bg: Rgb,
    /// Changes when the glyph atlases are rebuilt (font, scale or a full atlas).
    pub generation: u64,
}

#[derive(Default)]
struct Row {
    bg: Vec<Inst>,
    text: Vec<Inst>,
    deco: Vec<Inst>,
}

/// The instance data of one pane, kept between frames.
#[derive(Default)]
pub(crate) struct PaneCache {
    layout: Option<Layout>,
    /// Frame::seq of the frame last built.
    seq: u64,
    rows: Vec<Row>,
    len: usize,
}

/// Largest coordinate an instance can hold; cells past it cannot be on any
/// screen anyway.
const MAX_COORD: u32 = i16::MAX as u32;

impl PaneCache {
    /// Brings the instances up to date with frame. Returns how many rows were
    /// rebuilt (0 when nothing changed).
    pub(crate) fn update(
        &mut self,
        frame: &Frame,
        layout: Layout,
        src: &mut impl GlyphSource,
    ) -> usize {
        let cols = usable_cols(frame, &layout);
        let rows = usable_rows(frame, &layout);
        // The row count can change without the layout when the frame's cell
        // buffer does not cover its stated size.
        let full = self.layout != Some(layout) || frame.seq < self.seq || self.rows.len() != rows;
        if !full && frame.seq == self.seq {
            return 0;
        }
        if full {
            self.layout = Some(layout);
            self.rows.truncate(rows);
            self.rows.resize_with(rows, Row::default);
        }
        let mut rebuilt = 0;
        for r in 0..rows {
            // A row the core never stamped counts as changed with the frame.
            let row_seq = frame.row_seq.get(r).copied().unwrap_or(frame.seq);
            if full || row_seq > self.seq {
                build_row(&mut self.rows[r], frame, r, cols, &layout, src);
                rebuilt += 1;
            }
        }
        self.seq = frame.seq;
        self.len = self
            .rows
            .iter()
            .map(|r| r.bg.len() + r.text.len() + r.deco.len())
            .sum();
        rebuilt
    }

    /// Number of instances [PaneCache::write] produces.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Copies every instance into out in drawing order: all backgrounds, then
    /// all glyphs, then all decorations, so a glyph that overhangs its row is
    /// never covered by the next row's background.
    pub(crate) fn write(&self, out: &mut [Inst]) {
        let mut at = 0;
        for pick in
            [|r: &Row| &r.bg, |r: &Row| &r.text, |r: &Row| &r.deco] as [fn(&Row) -> &Vec<Inst>; 3]
        {
            for row in &self.rows {
                let v = pick(row);
                out[at..at + v.len()].copy_from_slice(v);
                at += v.len();
            }
        }
    }
}

fn usable_cols(frame: &Frame, layout: &Layout) -> usize {
    let fit = (MAX_COORD / layout.cell_w.max(1)).saturating_sub(1) as usize;
    (frame.cols as usize).min(fit)
}

fn usable_rows(frame: &Frame, layout: &Layout) -> usize {
    let fit = (MAX_COORD / layout.cell_h.max(1)).saturating_sub(1) as usize;
    // A frame whose cells do not cover its stated size draws what is there.
    let present = frame
        .cells
        .len()
        .checked_div(frame.cols as usize)
        .unwrap_or(0);
    (frame.rows as usize).min(present).min(fit)
}

fn clamp16(v: i64) -> i16 {
    v.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

/// The part of [start, start + len) an instance can hold (a start that fits
/// in i16, a length that fits in u16), or None when nothing is left. Clipping
/// rather than clamping keeps the visible edges where they belong.
fn span(start: i64, len: i64) -> Option<(i16, u16)> {
    let lo = start.max(i16::MIN as i64);
    let hi = (start + len).min(lo + u16::MAX as i64);
    (lo <= i16::MAX as i64 && hi > lo).then(|| (lo as i16, (hi - lo) as u16))
}

/// A filled rectangle, or None when it is empty. Coordinates are i64 so that
/// sums of extreme pane positions cannot overflow.
pub(crate) fn solid(x: i64, y: i64, w: i64, h: i64, rgb: Rgb, alpha: f32) -> Option<Inst> {
    if alpha.is_nan() || alpha <= 0.0 {
        return None;
    }
    let ((px, sw), (py, sh)) = (span(x, w)?, span(y, h)?);
    Some(Inst {
        pos: [px, py],
        size: [sw, sh],
        uv: [0, 0],
        color: pack(rgb, alpha),
        kind: SOLID,
    })
}

/// A glyph placed in the cell whose top-left corner is (x, y).
pub(crate) fn place(g: &Glyph, x: i64, y: i64, rgb: Rgb, alpha: f32) -> Inst {
    Inst {
        pos: [
            clamp16(x + g.offset[0] as i64),
            clamp16(y + g.offset[1] as i64),
        ],
        size: g.size,
        uv: g.uv,
        color: pack(rgb, alpha),
        kind: g.kind,
    }
}

/// Whether the cell shows a glyph at all.
pub(crate) fn draws_glyph(cell: &Cell) -> bool {
    !cell.has(flags::WIDE_SPACER | flags::HIDDEN)
        && (cell.has(flags::HAS_EXTRA) || (cell.ch != ' ' as u32 && cell.ch != 0))
}

fn underline_style(cell: &Cell) -> Option<Decoration> {
    let f = cell.flags;
    if f & flags::ANY_UNDERLINE == 0 {
        None
    } else if f & flags::DOUBLE_UNDERLINE != 0 {
        Some(Decoration::Double)
    } else if f & flags::UNDERCURL != 0 {
        Some(Decoration::Curly)
    } else if f & flags::DOTTED_UNDERLINE != 0 {
        Some(Decoration::Dotted)
    } else if f & flags::DASHED_UNDERLINE != 0 {
        Some(Decoration::Dashed)
    } else {
        Some(Decoration::Underline)
    }
}

fn build_row(
    row: &mut Row,
    frame: &Frame,
    r: usize,
    cols: usize,
    layout: &Layout,
    src: &mut impl GlyphSource,
) {
    row.bg.clear();
    row.text.clear();
    row.deco.clear();
    let (cw, ch) = (layout.cell_w as i64, layout.cell_h as i64);
    let y = r as i64 * ch;
    let base = r * frame.cols as usize;
    let cells = &frame.cells[base..base + cols];

    // Runs of one background color become one quad; the pane is already
    // cleared to the frame's background.
    let mut c = 0;
    while c < cols {
        let bg = cells[c].bg;
        if bg == layout.bg {
            c += 1;
            continue;
        }
        let start = c;
        while c < cols && cells[c].bg == bg {
            c += 1;
        }
        row.bg.extend(solid(
            start as i64 * cw,
            y,
            (c - start) as i64 * cw,
            ch,
            bg,
            1.0,
        ));
    }

    for (c, cell) in cells.iter().enumerate() {
        let x = c as i64 * cw;
        if draws_glyph(cell) {
            let g = src.glyph(frame, base + c);
            if !g.is_empty() {
                row.text.push(place(&g, x, y, cell.fg, 1.0));
            }
        }
        if cell.has(flags::WIDE_SPACER | flags::HIDDEN) {
            continue;
        }
        let span = if cell.has(flags::WIDE) { 2 } else { 1 };
        if let Some(d) = underline_style(cell) {
            let g = src.decoration(d);
            if !g.is_empty() {
                for k in 0..span {
                    row.deco.push(place(&g, x + k * cw, y, cell.ul, 1.0));
                }
            }
        }
        if cell.has(flags::STRIKEOUT) {
            let g = src.decoration(Decoration::Strike);
            if !g.is_empty() {
                for k in 0..span {
                    row.deco.push(place(&g, x + k * cw, y, cell.fg, 1.0));
                }
            }
        }
    }
}

/// The quads drawn after the cached instances: the cursor and the dim veil,
/// in scene coordinates.
pub(crate) fn overlay(
    out: &mut Vec<Inst>,
    pane: &PaneDraw,
    layout: &Layout,
    stroke: u32,
    src: &mut impl GlyphSource,
) {
    let frame = pane.frame;
    if let Some(cur) = frame.cursor.filter(|_| pane.cursor_on) {
        let (cols, rows) = (usable_cols(frame, layout), usable_rows(frame, layout));
        if (cur.col as usize) < cols && (cur.row as usize) < rows {
            let (cw, ch) = (layout.cell_w as i64, layout.cell_h as i64);
            let x = pane.origin.x as i64 + cur.col as i64 * cw;
            let y = pane.origin.y as i64 + cur.row as i64 * ch;
            let w = if cur.wide { 2 * cw } else { cw };
            let t = stroke.max(1) as i64;
            let shape = if pane.focused {
                cur.shape
            } else {
                CursorShape::HollowBlock
            };
            match shape {
                CursorShape::Block => {
                    out.extend(solid(x, y, w, ch, cur.color, 1.0));
                    let index = cur.row as usize * frame.cols as usize + cur.col as usize;
                    if draws_glyph(&frame.cells[index]) {
                        let g = src.glyph(frame, index);
                        if !g.is_empty() {
                            out.push(place(&g, x, y, cur.text_color, 1.0));
                        }
                    }
                }
                CursorShape::Beam => out.extend(solid(x, y, t, ch, cur.color, 1.0)),
                CursorShape::Underline => out.extend(solid(x, y + ch - t, w, t, cur.color, 1.0)),
                CursorShape::HollowBlock => {
                    out.extend(solid(x, y, w, t, cur.color, 1.0));
                    out.extend(solid(x, y + ch - t, w, t, cur.color, 1.0));
                    out.extend(solid(x, y + t, t, ch - 2 * t, cur.color, 1.0));
                    out.extend(solid(x + w - t, y + t, t, ch - 2 * t, cur.color, 1.0));
                }
            }
        }
    }
    if pane.dim > 0.0 {
        // In the pane's own background, so its text fades and its
        // background stays, on dark themes and light ones alike.
        out.extend(rect_quad(pane.rect, frame.bg, pane.dim.min(1.0)));
    }
}

/// A filled rectangle in scene coordinates: pane fills, veils and quads.
pub(crate) fn rect_quad(r: Rect, rgb: Rgb, alpha: f32) -> Option<Inst> {
    solid(r.x.into(), r.y.into(), r.w.into(), r.h.into(), rgb, alpha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_types::{CursorDraw, Point};

    /// Hands out a 1x1 mask glyph per cell, recording which cells it was asked for.
    #[derive(Default)]
    struct Fake {
        asked: Vec<usize>,
    }

    impl GlyphSource for Fake {
        fn glyph(&mut self, frame: &Frame, index: usize) -> Glyph {
            self.asked.push(index);
            let c = frame.cells[index].ch as u16;
            Glyph {
                kind: MASK,
                uv: [c, 0],
                size: [1, 1],
                offset: [1, 2],
            }
        }

        fn decoration(&mut self, d: Decoration) -> Glyph {
            Glyph {
                kind: MASK,
                uv: [d.index() as u16, 99],
                size: [10, 20],
                offset: [0, 0],
            }
        }
    }

    const FG: Rgb = 0xdddddd;
    const BG: Rgb = 0x101010;

    fn frame(cols: u16, rows: u16) -> Frame {
        let mut f = Frame {
            fg: FG,
            bg: BG,
            ..Frame::default()
        };
        f.resize(cols, rows);
        f
    }

    fn layout(f: &Frame) -> Layout {
        Layout {
            cols: f.cols,
            rows: f.rows,
            cell_w: 10,
            cell_h: 20,
            bg: f.bg,
            generation: 1,
        }
    }

    fn put(f: &mut Frame, row: u16, col: u16, text: &str) {
        for (i, ch) in text.chars().enumerate() {
            let cell = &mut f.row_mut(row)[col as usize + i];
            cell.ch = ch as u32;
        }
        f.seq += 1;
        f.row_seq[row as usize] = f.seq;
    }

    fn all(cache: &PaneCache) -> Vec<Inst> {
        let mut v = vec![Inst::default(); cache.len()];
        cache.write(&mut v);
        v
    }

    #[test]
    fn blank_frame_has_no_instances() {
        let f = frame(80, 24);
        let mut cache = PaneCache::default();
        assert_eq!(cache.update(&f, layout(&f), &mut Fake::default()), 24);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn glyphs_are_placed_in_their_cells_with_offsets() {
        let mut f = frame(10, 3);
        put(&mut f, 1, 2, "ab");
        let mut cache = PaneCache::default();
        let mut src = Fake::default();
        cache.update(&f, layout(&f), &mut src);
        let v = all(&cache);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].pos, [2 * 10 + 1, 20 + 2]);
        assert_eq!(v[1].pos, [3 * 10 + 1, 20 + 2]);
        assert_eq!(v[0].uv, ['a' as u16, 0]);
        assert_eq!(v[0].color, pack(FG, 1.0));
        assert_eq!(src.asked, vec![12, 13]);
    }

    #[test]
    fn only_rows_with_newer_seq_are_rebuilt() {
        let mut f = frame(10, 5);
        put(&mut f, 0, 0, "x");
        let mut cache = PaneCache::default();
        let mut src = Fake::default();
        assert_eq!(cache.update(&f, layout(&f), &mut src), 5);
        // Nothing changed: nothing rebuilt, no glyph lookups.
        src.asked.clear();
        assert_eq!(cache.update(&f, layout(&f), &mut src), 0);
        assert!(src.asked.is_empty());
        // Two snapshots in between; both changed rows are caught.
        put(&mut f, 2, 0, "y");
        put(&mut f, 4, 0, "z");
        assert_eq!(cache.update(&f, layout(&f), &mut src), 2);
        assert_eq!(src.asked, vec![20, 40]);
        assert_eq!(all(&cache).len(), 3);
        // A change of seq without row changes rebuilds nothing.
        f.seq += 1;
        assert_eq!(cache.update(&f, layout(&f), &mut src), 0);
    }

    #[test]
    fn layout_change_or_older_frame_rebuilds_everything() {
        let mut f = frame(10, 4);
        put(&mut f, 0, 0, "x");
        put(&mut f, 1, 0, "x");
        let mut cache = PaneCache::default();
        let mut src = Fake::default();
        cache.update(&f, layout(&f), &mut src);
        let mut l = layout(&f);
        l.generation = 2;
        assert_eq!(cache.update(&f, l, &mut src), 4);
        l.bg = 0x123456;
        assert_eq!(cache.update(&f, l, &mut src), 4);
        // A replaced frame whose seq went backwards.
        let mut g = frame(10, 4);
        put(&mut g, 3, 0, "q");
        assert!(g.seq < f.seq);
        assert_eq!(cache.update(&g, l, &mut src), 4);
        assert_eq!(all(&cache).iter().filter(|i| i.kind == MASK).count(), 1);
    }

    #[test]
    fn resized_frame_rebuilds_at_the_new_size() {
        let mut f = frame(10, 4);
        put(&mut f, 3, 0, "abc");
        let mut cache = PaneCache::default();
        cache.update(&f, layout(&f), &mut Fake::default());
        f.resize(5, 2);
        put(&mut f, 1, 4, "z");
        assert_eq!(cache.update(&f, layout(&f), &mut Fake::default()), 2);
        let v = all(&cache);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pos, [4 * 10 + 1, 20 + 2]);
    }

    #[test]
    fn background_runs_merge_and_default_bg_is_skipped() {
        let mut f = frame(10, 2);
        for c in 2..6 {
            f.row_mut(0)[c].bg = 0xff0000;
        }
        f.row_mut(0)[7].bg = 0x00ff00;
        f.row_mut(1)[9].bg = 0xff0000;
        let mut cache = PaneCache::default();
        cache.update(&f, layout(&f), &mut Fake::default());
        let v = all(&cache);
        assert_eq!(v.len(), 3);
        assert_eq!(
            (v[0].pos, v[0].size, v[0].color),
            ([20, 0], [40, 20], pack(0xff0000, 1.0))
        );
        assert_eq!((v[1].pos, v[1].size), ([70, 0], [10, 20]));
        assert_eq!((v[2].pos, v[2].size), ([90, 20], [10, 20]));
        assert!(v.iter().all(|i| i.kind == SOLID));
    }

    #[test]
    fn drawing_order_is_backgrounds_then_glyphs_then_decorations() {
        let mut f = frame(4, 2);
        put(&mut f, 0, 0, "ab");
        put(&mut f, 1, 0, "c");
        f.row_mut(1)[0].bg = 0x0000ff;
        f.row_mut(0)[1].flags |= flags::UNDERCURL;
        f.row_mut(0)[1].ul = 0x00ff00;
        let mut cache = PaneCache::default();
        cache.update(&f, layout(&f), &mut Fake::default());
        let v = all(&cache);
        let kinds: Vec<_> = v.iter().map(|i| (i.kind, i.uv[1])).collect();
        assert_eq!(
            kinds,
            vec![(SOLID, 0), (MASK, 0), (MASK, 0), (MASK, 0), (MASK, 99)]
        );
        let deco = v[4];
        assert_eq!(deco.uv[0] as usize, Decoration::Curly.index());
        assert_eq!(deco.pos, [10, 0]);
        assert_eq!(deco.color, pack(0x00ff00, 1.0));
    }

    #[test]
    fn spacers_hidden_and_blank_cells_draw_no_glyph() {
        let mut f = frame(6, 1);
        put(&mut f, 0, 0, "W ab");
        f.row_mut(0)[0].flags |= flags::WIDE | flags::UNDERLINE | flags::STRIKEOUT;
        f.row_mut(0)[1].flags |= flags::WIDE_SPACER | flags::UNDERLINE;
        f.row_mut(0)[2].ch = 'q' as u32;
        f.row_mut(0)[2].flags |= flags::HIDDEN | flags::UNDERLINE;
        f.row_mut(0)[4].ch = 0;
        let mut cache = PaneCache::default();
        let mut src = Fake::default();
        cache.update(&f, layout(&f), &mut src);
        assert_eq!(src.asked, vec![0, 3]);
        let v = all(&cache);
        let deco: Vec<_> = v.iter().filter(|i| i.uv[1] == 99).collect();
        // The wide cell's underline and strikeout each cover both of its cells.
        assert_eq!(deco.len(), 4);
        assert_eq!(deco.iter().filter(|d| d.pos == [10, 0]).count(), 2);
    }

    #[test]
    fn extras_force_a_glyph_on_a_blank_base() {
        let mut f = frame(3, 1);
        f.row_mut(0)[1].flags |= flags::HAS_EXTRA;
        let mut cache = PaneCache::default();
        let mut src = Fake::default();
        cache.update(&f, layout(&f), &mut src);
        assert_eq!(src.asked, vec![1]);
    }

    #[test]
    fn short_cell_buffer_does_not_panic() {
        let mut f = frame(10, 4);
        f.cells.truncate(25);
        f.row_seq.truncate(1);
        let mut cache = PaneCache::default();
        assert_eq!(cache.update(&f, layout(&f), &mut Fake::default()), 2);
    }

    fn pane(f: &Frame, focused: bool, cursor_on: bool, dim: f32) -> PaneDraw<'_> {
        PaneDraw {
            id: 1,
            rect: Rect::new(100, 50, 200, 100),
            origin: Point { x: 104, y: 54 },
            frame: f,
            focused,
            cursor_on,
            dim,
        }
    }

    fn with_cursor(shape: CursorShape, wide: bool) -> Frame {
        let mut f = frame(10, 4);
        put(&mut f, 1, 2, "k");
        f.cursor = Some(CursorDraw {
            row: 1,
            col: 2,
            shape,
            color: 0xffffff,
            text_color: 0x000000,
            wide,
            blinking: false,
        });
        f
    }

    #[test]
    fn block_cursor_redraws_the_character_in_text_color() {
        let f = with_cursor(CursorShape::Block, false);
        let mut out = Vec::new();
        overlay(
            &mut out,
            &pane(&f, true, true, 0.0),
            &layout(&f),
            2,
            &mut Fake::default(),
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            (out[0].pos, out[0].size, out[0].kind),
            ([124, 74], [10, 20], SOLID)
        );
        assert_eq!((out[1].pos, out[1].color), ([125, 76], pack(0x000000, 1.0)));
    }

    #[test]
    fn cursor_shapes_and_focus() {
        let lay = |f: &Frame| layout(f);
        let f = with_cursor(CursorShape::Beam, false);
        let mut out = Vec::new();
        overlay(
            &mut out,
            &pane(&f, true, true, 0.0),
            &lay(&f),
            2,
            &mut Fake::default(),
        );
        assert_eq!((out[0].pos, out[0].size), ([124, 74], [2, 20]));

        let f = with_cursor(CursorShape::Underline, true);
        out.clear();
        overlay(
            &mut out,
            &pane(&f, true, true, 0.0),
            &lay(&f),
            2,
            &mut Fake::default(),
        );
        assert_eq!((out[0].pos, out[0].size), ([124, 92], [20, 2]));

        // Unfocused: hollow whatever the frame asks for; four edges.
        let f = with_cursor(CursorShape::Block, false);
        out.clear();
        overlay(
            &mut out,
            &pane(&f, false, true, 0.0),
            &lay(&f),
            2,
            &mut Fake::default(),
        );
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|i| i.kind == SOLID));

        // Blink phase off or no cursor: nothing.
        out.clear();
        overlay(
            &mut out,
            &pane(&f, true, false, 0.0),
            &lay(&f),
            2,
            &mut Fake::default(),
        );
        assert!(out.is_empty());
        let mut g = with_cursor(CursorShape::Block, false);
        g.cursor = None;
        overlay(
            &mut out,
            &pane(&g, true, true, 0.0),
            &lay(&g),
            2,
            &mut Fake::default(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn cursor_outside_the_grid_is_ignored() {
        let mut f = with_cursor(CursorShape::Block, false);
        if let Some(c) = f.cursor.as_mut() {
            c.row = 40;
        }
        let mut out = Vec::new();
        overlay(
            &mut out,
            &pane(&f, true, true, 0.0),
            &layout(&f),
            2,
            &mut Fake::default(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn dim_veil_covers_the_pane_rect_in_its_background() {
        let mut f = frame(4, 4);
        f.bg = 0x102030;
        let mut out = Vec::new();
        overlay(
            &mut out,
            &pane(&f, false, true, 0.4),
            &layout(&f),
            1,
            &mut Fake::default(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].pos, out[0].size), ([100, 50], [200, 100]));
        assert_eq!(out[0].color, pack(0x102030, 0.4));
    }

    #[test]
    fn solid_skips_empty_and_clamps() {
        assert!(solid(0, 0, 0, 5, 0, 1.0).is_none());
        assert!(solid(0, 0, 5, 5, 0, 0.0).is_none());
        assert!(solid(0, 0, 5, 5, 0, f32::NAN).is_none());
        let big = solid(-40000, 30000, 100000, 3, 0, 1.0).unwrap();
        assert_eq!(big.pos, [i16::MIN, 30000]);
        assert_eq!(big.size, [u16::MAX, 3]);
    }

    #[test]
    fn rows_that_appear_or_vanish_without_a_layout_change() {
        let mut f = frame(10, 4);
        for r in 0..4 {
            put(&mut f, r, 0, "x");
        }
        let full = f.cells.clone();
        let mut cache = PaneCache::default();
        let mut src = Fake::default();
        assert_eq!(cache.update(&f, layout(&f), &mut src), 4);
        // The core hands over a frame whose cells cover only two rows.
        f.cells.truncate(20);
        f.seq += 1;
        assert_eq!(cache.update(&f, layout(&f), &mut src), 2);
        assert_eq!(all(&cache).len(), 2, "rows that are gone must not be drawn");
        // The cells come back, with the row stamps left as they were.
        f.cells = full;
        f.seq += 1;
        assert_eq!(cache.update(&f, layout(&f), &mut src), 4);
        assert_eq!(all(&cache).len(), 4);
    }

    #[test]
    fn solid_clips_to_the_representable_range_instead_of_moving() {
        // Covers x from -40000 to 10: after clipping it must still end at 10.
        let q = solid(-40000, 0, 40010, 5, 0, 1.0).unwrap();
        assert_eq!(q.pos[0] as i32 + q.size[0] as i32, 10);
        // Starts on screen and runs far past the range: the start stays put.
        let q = rect_quad(Rect::new(100, 0, i32::MAX, 5), 0, 1.0).unwrap();
        assert_eq!(q.pos[0], 100);
        assert!(q.pos[0] as i32 + q.size[0] as i32 >= i16::MAX as i32);
        // Entirely outside the range: nothing to draw.
        assert!(solid(-60000, 0, 100, 5, 0, 1.0).is_none());
        assert!(rect_quad(Rect::new(i32::MAX - 1, 0, i32::MAX, 5), 0, 1.0).is_none());
    }

    #[test]
    fn extreme_pane_positions_do_not_overflow() {
        let f = with_cursor(CursorShape::HollowBlock, true);
        let mut p = pane(&f, true, true, 0.5);
        p.origin = Point {
            x: i32::MAX - 3,
            y: i32::MAX - 3,
        };
        p.rect = Rect::new(i32::MAX - 5, i32::MIN, i32::MAX, i32::MAX);
        let mut out = Vec::new();
        overlay(&mut out, &p, &layout(&f), 2, &mut Fake::default());
        assert!(out.iter().all(|i| i.kind == SOLID));
    }

    #[test]
    fn instance_layout_matches_the_shader() {
        // short2 pos, ushort2 size, ushort2 uv, uint color, uint kind.
        assert_eq!(std::mem::size_of::<Inst>(), 20);
        assert_eq!(std::mem::align_of::<Inst>(), 4);
    }
}
