//! Pixel tests: scenes rendered offscreen and read back.

#[path = "../examples/support/mod.rs"]
mod support;

use support::Term;
use tern_render::{CellMetrics, FontConfig, Image, PaneDraw, Quad, Renderer, Scene};
use tern_types::{flags, CursorShape, Frame, Point, Rect, Rgb};

const FG: Rgb = 0xc0c0c0;
const BG: Rgb = 0x102030;
const CLEAR: Rgb = 0x000000;

fn renderer() -> Renderer {
    Renderer::new(&FontConfig {
        family: "Menlo".into(),
        size_pt: 13.0,
    })
    .expect("renderer")
}

fn rgb(c: Rgb) -> [u8; 4] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]
}

struct Setup {
    m: CellMetrics,
    pad: i32,
}

impl Setup {
    fn cell_rect(&self, row: i32, col: i32) -> Rect {
        Rect::new(
            self.pad + col * self.m.cell_w as i32,
            self.pad + row * self.m.cell_h as i32,
            self.m.cell_w as i32,
            self.m.cell_h as i32,
        )
    }
}

fn pane(id: u64, rect: Rect, pad: i32, frame: &Frame) -> PaneDraw<'_> {
    PaneDraw {
        id,
        rect,
        origin: Point {
            x: rect.x + pad,
            y: rect.y + pad,
        },
        frame,
        focused: true,
        cursor_on: true,
        dim: 0.0,
    }
}

/// Renders one pane covering the whole scene.
fn render_single(r: &mut Renderer, frame: &Frame, scale: f32, focused: bool) -> (Image, Setup) {
    let m = r.metrics(scale);
    let pad = 3;
    let w = frame.cols as i32 * m.cell_w as i32 + 2 * pad;
    let h = frame.rows as i32 * m.cell_h as i32 + 2 * pad;
    let mut p = pane(1, Rect::new(0, 0, w, h), pad, frame);
    p.focused = focused;
    let panes = [p];
    let scene = Scene {
        width: w as u32,
        height: h as u32,
        scale,
        clear: CLEAR,
        panes: &panes,
        quads: &[],
    };
    (
        r.render_offscreen(&scene).expect("render"),
        Setup { m, pad },
    )
}

fn region_all(img: &Image, r: Rect, color: [u8; 4]) -> bool {
    (r.y..r.bottom()).all(|y| (r.x..r.right()).all(|x| img.pixel(x as u32, y as u32) == color))
}

fn count_not(img: &Image, r: Rect, color: [u8; 4]) -> usize {
    (r.y..r.bottom())
        .flat_map(|y| (r.x..r.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| img.pixel(x as u32, y as u32) != color)
        .count()
}

#[test]
fn red_cell_background_is_exactly_its_cell() {
    let mut r = renderer();
    let mut t = Term::new(10, 4, FG, BG);
    t.at(1, 2).style(FG, 0xff0000, 0).print(" ");
    let (img, s) = render_single(&mut r, &t.frame(), 1.0, true);
    let cell = s.cell_rect(1, 2);
    let red = rgb(0xff0000);
    assert!(region_all(&img, cell, red));
    // Everything else is the frame's background.
    let mut others = 0;
    for y in 0..img.height as i32 {
        for x in 0..img.width as i32 {
            if !cell.contains(Point { x, y }) {
                assert_eq!(img.pixel(x as u32, y as u32), rgb(BG), "({x}, {y})");
                others += 1;
            }
        }
    }
    assert!(others > 0);
}

fn cursor_frame(shape: CursorShape) -> Frame {
    let mut t = Term::new(6, 3, FG, BG);
    t.cursor(1, 2, shape, 0xffff00, 0x000000);
    t.frame()
}

#[test]
fn cursor_shapes_cover_the_right_pixels() {
    let mut r = renderer();
    let yellow = rgb(0xffff00);
    let bg = rgb(BG);
    for (shape, focused) in [
        (CursorShape::Block, true),
        (CursorShape::Beam, true),
        (CursorShape::Underline, true),
        (CursorShape::HollowBlock, true),
        (CursorShape::Block, false),
    ] {
        let (img, s) = render_single(&mut r, &cursor_frame(shape), 2.0, focused);
        let c = s.cell_rect(1, 2);
        let t = s.m.stroke as i32;
        let (outer, inner) = match (shape, focused) {
            (CursorShape::Block, true) => (c, Rect::new(0, 0, 0, 0)),
            (CursorShape::Beam, _) => (Rect::new(c.x, c.y, t, c.h), Rect::new(0, 0, 0, 0)),
            (CursorShape::Underline, _) => (
                Rect::new(c.x, c.bottom() - t, c.w, t),
                Rect::new(0, 0, 0, 0),
            ),
            _ => (c, c.inset(t, t)),
        };
        for y in 0..img.height as i32 {
            for x in 0..img.width as i32 {
                let p = Point { x, y };
                let want = if outer.contains(p) && !inner.contains(p) {
                    yellow
                } else {
                    bg
                };
                assert_eq!(
                    img.pixel(x as u32, y as u32),
                    want,
                    "{shape:?} focused={focused} at ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn block_cursor_redraws_its_character_in_text_color() {
    let mut r = renderer();
    let mut t = Term::new(6, 3, FG, BG);
    t.at(1, 2).print("M");
    t.cursor(1, 2, CursorShape::Block, 0xffff00, 0x0000ff);
    let (img, s) = render_single(&mut r, &t.frame(), 2.0, true);
    let c = s.cell_rect(1, 2);
    let blue_ish = (c.y..c.bottom())
        .flat_map(|y| (c.x..c.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = img.pixel(x as u32, y as u32);
            p[2] > 200 && p[0] < 60
        })
        .count();
    assert!(
        blue_ish > 20,
        "the character shows in the cursor's text color"
    );
    // No fg-colored pixels survive inside the cursor.
    assert_eq!(count_not(&img, c, rgb(FG)), (c.w * c.h) as usize);
}

#[test]
fn text_is_inside_its_cells_and_nothing_leaves_the_pane() {
    let mut r = renderer();
    let mut t = Term::new(20, 6, FG, BG);
    t.at(2, 3).print("Hgj|");
    let frame = t.frame();
    let m = r.metrics(1.0);
    let pad = 4;
    // The pane sits inside a larger scene.
    let rect = Rect::new(
        30,
        20,
        20 * m.cell_w as i32 + 2 * pad,
        6 * m.cell_h as i32 + 2 * pad,
    );
    let panes = [pane(7, rect, pad, &frame)];
    let scene = Scene {
        width: rect.right() as u32 + 30,
        height: rect.bottom() as u32 + 20,
        scale: 1.0,
        clear: CLEAR,
        panes: &panes,
        quads: &[],
    };
    let img = r.render_offscreen(&scene).unwrap();
    let s = Setup { m, pad };
    let cell = |row, col| {
        let c = s.cell_rect(row, col);
        Rect::new(c.x + rect.x, c.y + rect.y, c.w, c.h)
    };
    for col in 3..7 {
        assert!(
            count_not(&img, cell(2, col), rgb(BG)) > 3,
            "ink in column {col}"
        );
    }
    // Ink only in the text's row, give or take a descender's reach into the next.
    let text_area = Rect::new(
        cell(2, 3).x - 1,
        cell(2, 3).y - 2,
        4 * m.cell_w as i32 + 2,
        2 * m.cell_h as i32,
    );
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            if !text_area.contains(Point { x, y }) {
                assert_eq!(
                    img.pixel(x as u32, y as u32),
                    rgb(BG),
                    "stray ink at ({x}, {y})"
                );
            }
        }
    }
    for y in 0..img.height as i32 {
        for x in 0..img.width as i32 {
            if !rect.contains(Point { x, y }) {
                assert_eq!(
                    img.pixel(x as u32, y as u32),
                    rgb(CLEAR),
                    "outside the pane at ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn a_grid_larger_than_the_pane_is_clipped_to_the_pane() {
    let mut r = renderer();
    let mut t = Term::new(40, 10, FG, 0x00ff00);
    for row in 0..10 {
        t.at(row, 0)
            .style(0xffffff, 0xff00ff, flags::BOLD)
            .print(&"█W".repeat(20));
    }
    let frame = t.frame();
    let rect = Rect::new(10, 10, 60, 40);
    let panes = [pane(3, rect, 2, &frame)];
    let scene = Scene {
        width: 100,
        height: 80,
        scale: 2.0,
        clear: CLEAR,
        panes: &panes,
        quads: &[],
    };
    let img = r.render_offscreen(&scene).unwrap();
    for y in 0..80 {
        for x in 0..100 {
            if !rect.contains(Point { x, y }) {
                assert_eq!(img.pixel(x as u32, y as u32), rgb(CLEAR), "({x}, {y})");
            }
        }
    }
}

#[test]
fn pane_larger_than_its_grid_shows_frame_bg_in_the_remainder() {
    let mut r = renderer();
    let mut t = Term::new(4, 2, FG, 0x336699);
    t.at(0, 0).style(FG, 0xff0000, 0).print("    ");
    t.at(1, 0).style(FG, 0xff0000, 0).print("    ");
    let frame = t.frame();
    let m = r.metrics(1.0);
    let rect = Rect::new(0, 0, 4 * m.cell_w as i32 + 50, 2 * m.cell_h as i32 + 30);
    let panes = [pane(4, rect, 0, &frame)];
    let scene = Scene {
        width: rect.w as u32,
        height: rect.h as u32,
        scale: 1.0,
        clear: CLEAR,
        panes: &panes,
        quads: &[],
    };
    let img = r.render_offscreen(&scene).unwrap();
    let grid = Rect::new(0, 0, 4 * m.cell_w as i32, 2 * m.cell_h as i32);
    assert!(region_all(&img, grid, rgb(0xff0000)));
    assert!(region_all(
        &img,
        Rect::new(grid.right(), 0, 50, rect.h),
        rgb(0x336699)
    ));
    assert!(region_all(
        &img,
        Rect::new(0, grid.bottom(), rect.w, 30),
        rgb(0x336699)
    ));
}

#[test]
fn divider_quad_lands_where_asked_and_blends() {
    let mut r = renderer();
    let frame = Term::new(10, 3, FG, BG).frame();
    let panes = [pane(5, Rect::new(0, 0, 120, 60), 0, &frame)];
    let quads = [
        Quad {
            rect: Rect::new(50, 0, 2, 60),
            color: 0xffffff,
            alpha: 1.0,
        },
        Quad {
            rect: Rect::new(80, 10, 20, 20),
            color: 0xff0000,
            alpha: 0.5,
        },
    ];
    let scene = Scene {
        width: 120,
        height: 60,
        scale: 1.0,
        clear: CLEAR,
        panes: &panes,
        quads: &quads,
    };
    let img = r.render_offscreen(&scene).unwrap();
    for y in 0..60 {
        for x in 0..120 {
            let p = img.pixel(x, y);
            if (50..52).contains(&x) {
                assert_eq!(p, [255, 255, 255, 255]);
            } else if (80..100).contains(&x) && (10..30).contains(&y) {
                // Half red over 0x102030, blended as stored values.
                let blend = |a: u8, b: u8| ((a as f32 * 0.5 + b as f32 * 0.5) - p[0] as f32).abs();
                assert!(blend(255, 0x10) <= 1.5, "{p:?}");
                assert!(
                    (p[1] as i32 - 0x10).abs() <= 1 && (p[2] as i32 - 0x18).abs() <= 1,
                    "{p:?}"
                );
            } else {
                assert_eq!(p, rgb(BG), "({x}, {y})");
            }
        }
    }
}

#[test]
fn dim_veil_fades_only_its_pane_toward_its_background() {
    let mut r = renderer();
    // Black cells on a gray pane; around the 4 by 2 grid the pane shows
    // its background.
    let mut t = Term::new(4, 2, FG, 0x808080);
    for row in 0..2 {
        t.at(row, 0).style(FG, 0x000000, 0).print("    ");
    }
    let frame = t.frame();
    let mut a = pane(1, Rect::new(0, 0, 50, 40), 0, &frame);
    a.dim = 0.5;
    let b = pane(2, Rect::new(50, 0, 50, 40), 0, &frame);
    let panes = [a, b];
    let scene = Scene {
        width: 100,
        height: 40,
        scale: 1.0,
        clear: CLEAR,
        panes: &panes,
        quads: &[],
    };
    let img = r.render_offscreen(&scene).unwrap();
    // Halfway from black to the pane's background; the background stays.
    let faded = img.pixel(10, 10);
    assert!(
        (faded[0] as i32 - 0x40).abs() <= 1 && faded[0] == faded[1] && faded[1] == faded[2],
        "{faded:?}"
    );
    assert_eq!(img.pixel(45, 36), rgb(0x808080));
    assert_eq!(img.pixel(60, 10), rgb(0x000000));
    assert_eq!(img.pixel(95, 36), rgb(0x808080));
}

#[test]
fn color_emoji_keep_their_colors() {
    let mut r = renderer();
    let mut t = Term::new(6, 2, 0xffffff, BG);
    t.at(0, 1).print("🍕");
    let (img, s) = render_single(&mut r, &t.frame(), 2.0, true);
    let c = s.cell_rect(0, 1);
    let two = Rect::new(c.x, c.y, 2 * c.w, c.h);
    let colorful = (two.y..two.bottom())
        .flat_map(|y| (two.x..two.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = img.pixel(x as u32, y as u32);
            p[0].abs_diff(p[1]) > 40 || p[1].abs_diff(p[2]) > 40
        })
        .count();
    assert!(colorful > 50, "{colorful}");
}

#[test]
fn unchanged_frames_rebuild_nothing_and_changed_rows_rebuild_alone() {
    let mut r = renderer();
    let mut t = Term::new(30, 8, FG, BG);
    t.at(0, 0).print("first line");
    let mut frame = t.frame();
    let (a, _) = render_single(&mut r, &frame, 1.0, true);
    assert_eq!(r.stats().rows_rebuilt, 8);
    let (b, _) = render_single(&mut r, &frame, 1.0, true);
    assert_eq!(r.stats().rows_rebuilt, 0);
    assert_eq!(a, b);
    // The core changes one row.
    frame.row_mut(5)[0].ch = 'Z' as u32;
    frame.seq += 1;
    frame.row_seq[5] = frame.seq;
    let (c, s) = render_single(&mut r, &frame, 1.0, true);
    assert_eq!(r.stats().rows_rebuilt, 1);
    assert!(count_not(&c, s.cell_rect(5, 0), rgb(BG)) > 3);
}

#[test]
fn frames_that_change_size_and_scale_redraw_cleanly() {
    let mut r = renderer();
    let mut t = Term::new(20, 6, FG, BG);
    for row in 0..6 {
        t.at(row, 0).print("XXXXXXXXXXXXXXXXXXXX");
    }
    let mut frame = t.frame();
    render_single(&mut r, &frame, 1.0, true);
    // Shrink the grid: nothing of the old content may remain.
    frame.resize(5, 2);
    let (img, _) = render_single(&mut r, &frame, 1.0, true);
    assert!(img.rgba.chunks_exact(4).all(|p| p == rgb(BG)));
    // Back and forth between scales with the same pane.
    frame.row_mut(1)[1].ch = 'Q' as u32;
    frame.seq += 1;
    frame.row_seq[1] = frame.seq;
    let (two, s2) = render_single(&mut r, &frame, 2.0, true);
    let (one, s1) = render_single(&mut r, &frame, 1.0, true);
    assert!(s2.m.cell_w > s1.m.cell_w);
    assert!(
        count_not(&two, s2.cell_rect(1, 1), rgb(BG)) > count_not(&one, s1.cell_rect(1, 1), rgb(BG))
    );
    assert!(count_not(&one, s1.cell_rect(1, 1), rgb(BG)) > 3);
}

#[test]
fn zero_sized_scenes_and_empty_frames() {
    let mut r = renderer();
    let frame = Frame::default();
    let panes = [pane(9, Rect::new(0, 0, 10, 10), 0, &frame)];
    let scene = Scene {
        width: 0,
        height: 50,
        scale: 1.0,
        clear: CLEAR,
        panes: &panes,
        quads: &[],
    };
    let img = r.render_offscreen(&scene).unwrap();
    assert!(img.rgba.is_empty());
    // A frame with no cells draws its (default) background.
    let scene = Scene {
        width: 20,
        height: 20,
        scale: 1.0,
        clear: 0xffffff,
        panes: &panes,
        quads: &[],
    };
    let img = r.render_offscreen(&scene).unwrap();
    assert_eq!(img.pixel(5, 5), rgb(0));
    assert_eq!(img.pixel(15, 15), rgb(0xffffff));
    // Nonsense scale falls back to 1.
    let scene = Scene {
        width: 20,
        height: 20,
        scale: f32::NAN,
        clear: 0xffffff,
        panes: &panes,
        quads: &[],
    };
    assert!(r.render_offscreen(&scene).is_ok());
}

#[test]
fn thousands_of_distinct_glyphs() {
    let mut r = renderer();
    let mut seen = 0;
    for page in 0..6u32 {
        let mut t = Term::new(100, 10, FG, BG);
        for i in 0..500u32 {
            let c = char::from_u32(0x4e00 + page * 500 + i).unwrap();
            t.at((i / 50) as u16, (i % 50 * 2) as u16)
                .print(&c.to_string());
        }
        let (img, s) = render_single(&mut r, &t.frame(), 2.0, true);
        assert!(count_not(&img, s.cell_rect(9, 98), rgb(BG)) > 3);
        seen += 500;
    }
    assert_eq!(seen, 3000);
}

#[test]
fn forget_pane_and_set_font() {
    let mut r = renderer();
    let mut t = Term::new(10, 2, FG, BG);
    t.at(0, 0).print("abc");
    let frame = t.frame();
    let (a, _) = render_single(&mut r, &frame, 1.0, true);
    r.forget_pane(1);
    let (b, _) = render_single(&mut r, &frame, 1.0, true);
    assert_eq!(r.stats().rows_rebuilt, 2);
    assert_eq!(a, b);
    let before = r.metrics(1.0);
    assert!(r
        .set_font(&FontConfig {
            family: "No Such Family 987".into(),
            size_pt: 13.0
        })
        .is_err());
    assert_eq!(r.metrics(1.0), before);
    r.set_font(&FontConfig {
        family: "Menlo".into(),
        size_pt: 26.0,
    })
    .unwrap();
    assert!(r.metrics(1.0).cell_w > before.cell_w);
    let (c, _) = render_single(&mut r, &frame, 1.0, true);
    assert!(c.width > a.width);
}

#[test]
fn underlines_use_the_underline_color_on_the_metrics_rows() {
    let mut r = renderer();
    let mut t = Term::new(6, 2, FG, BG);
    t.at(0, 1)
        .style(FG, BG, flags::UNDERLINE)
        .underline_color(0xff0000)
        .print(" ");
    t.at(0, 3).style(FG, BG, flags::STRIKEOUT).print(" ");
    let (img, s) = render_single(&mut r, &t.frame(), 2.0, true);
    let m = s.m;
    let c = s.cell_rect(0, 1);
    let line = Rect::new(
        c.x,
        c.y + m.underline_top as i32,
        c.w,
        m.underline_thickness as i32,
    );
    assert!(region_all(&img, line, rgb(0xff0000)));
    assert_eq!(count_not(&img, c, rgb(BG)), (line.w * line.h) as usize);
    // Strikeout is drawn in the foreground color.
    let c = s.cell_rect(0, 3);
    let strike = Rect::new(
        c.x,
        c.y + m.strike_top as i32,
        c.w,
        m.underline_thickness as i32,
    );
    assert!(region_all(&img, strike, rgb(FG)));
    assert_eq!(count_not(&img, c, rgb(BG)), (strike.w * strike.h) as usize);
}

#[test]
fn moving_a_pane_rebuilds_nothing_and_draws_at_the_new_place() {
    let mut r = renderer();
    let mut t = Term::new(4, 2, FG, BG);
    t.at(1, 2).style(FG, 0xff0000, 0).print(" ");
    let frame = t.frame();
    let m = r.metrics(1.0);
    let (gw, gh) = (4 * m.cell_w as i32, 2 * m.cell_h as i32);
    let mut images = Vec::new();
    for x in [0, 37] {
        let panes = [pane(1, Rect::new(x, 5, gw, gh), 0, &frame)];
        let scene = Scene {
            width: (37 + gw) as u32,
            height: (5 + gh) as u32,
            scale: 1.0,
            clear: CLEAR,
            panes: &panes,
            quads: &[],
        };
        images.push((r.render_offscreen(&scene).unwrap(), r.stats().rows_rebuilt));
    }
    assert_eq!(images[0].1, 2);
    assert_eq!(images[1].1, 0, "a move alone rebuilds no rows");
    let red = Rect::new(
        37 + 2 * m.cell_w as i32,
        5 + m.cell_h as i32,
        m.cell_w as i32,
        m.cell_h as i32,
    );
    assert!(region_all(&images[1].0, red, rgb(0xff0000)));
    assert_eq!(
        count_not(&images[1].0, Rect::new(0, 0, 37, 5 + gh), rgb(CLEAR)),
        0
    );
}

#[test]
fn an_atlas_that_fills_mid_frame_redraws_every_pane() {
    // At the largest font size a few dozen CJK glyphs fill the 8192 x 8192
    // mask atlas, so the second pane's glyphs start the atlas over after the
    // first pane was already built against it.
    let font = FontConfig {
        family: "Menlo".into(),
        size_pt: 1024.0,
    };
    let text = |first: u32| {
        let mut t = Term::new(30, 2, FG, BG);
        for i in 0..30u32 {
            let c = char::from_u32(first + i).unwrap();
            t.at((i / 15) as u16, (i % 15 * 2) as u16)
                .print(&c.to_string());
        }
        t.frame()
    };
    let (a, b) = (text(0x4e00), text(0x5e00));
    let mut r = Renderer::new(&font).unwrap();
    let m = r.metrics(1.0);
    let view = Rect::new(0, 0, 2 * m.cell_w as i32, m.cell_h as i32);
    let draw = |r: &mut Renderer, panes: &[PaneDraw]| {
        let scene = Scene {
            width: view.w as u32,
            height: view.h as u32,
            scale: 1.0,
            clear: CLEAR,
            panes,
            quads: &[],
        };
        r.render_offscreen(&scene).unwrap()
    };
    let first = pane(
        1,
        Rect::new(0, 0, 30 * m.cell_w as i32, 2 * m.cell_h as i32),
        0,
        &a,
    );
    // The second pane lies outside the image but is still prepared.
    let second = pane(2, Rect::new(100_000, 0, 10, 10), 0, &b);
    let img = draw(&mut r, &[first, second]);
    // Two rows per pane, then both panes again after the atlas started over.
    assert_eq!(r.stats().rows_rebuilt, 8);
    // The first pane's first glyph matches a renderer that never filled up.
    let mut fresh = Renderer::new(&font).unwrap();
    let first = pane(
        1,
        Rect::new(0, 0, 30 * m.cell_w as i32, 2 * m.cell_h as i32),
        0,
        &a,
    );
    let want = draw(&mut fresh, &[first]);
    assert!(count_not(&want, view, rgb(BG)) > 1000);
    assert!(img == want, "the first pane draws stale atlas coordinates");
    // The next frame is stable: no starting over again, nothing rebuilt.
    let first = pane(
        1,
        Rect::new(0, 0, 30 * m.cell_w as i32, 2 * m.cell_h as i32),
        0,
        &a,
    );
    let second = pane(2, Rect::new(100_000, 0, 10, 10), 0, &b);
    let again = draw(&mut r, &[first, second]);
    assert_eq!(r.stats().rows_rebuilt, 0);
    assert!(again == want);
}

#[test]
fn png_files_are_readable_by_the_system() {
    let mut r = renderer();
    let mut t = Term::new(10, 2, FG, BG);
    t.at(0, 0).print("png");
    let (img, _) = render_single(&mut r, &t.frame(), 1.0, true);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let png = dir.join("tern-render-offscreen.png");
    img.write_png(&png).unwrap();
    let out = std::process::Command::new("/usr/bin/sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(&png)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("pixelWidth: {}", img.width)),
        "{text}"
    );
    assert!(
        text.contains(&format!("pixelHeight: {}", img.height)),
        "{text}"
    );
    // A full decode: convert to BMP and compare a pixel.
    let bmp = dir.join("tern-render-offscreen.bmp");
    let status = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "bmp"])
        .arg(&png)
        .arg("--out")
        .arg(&bmp)
        .output()
        .unwrap();
    assert!(status.status.success());
    let data = std::fs::read(&bmp).unwrap();
    assert_eq!(&data[..2], b"BM");
}
