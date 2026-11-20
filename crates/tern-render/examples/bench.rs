//! Measures renderer startup and per-frame CPU cost, offscreen.
//!
//!     cargo run --release -p tern-render --example bench

#[path = "support/mod.rs"]
mod support;

use std::time::{Duration, Instant};

use tern_render::{FontConfig, PaneDraw, Renderer, Scene};
use tern_types::{flags, Frame, Point, Rect};

const COLS: u16 = 200;
const ROWS: u16 = 60;

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn us(d: Duration) -> String {
    format!("{:8.1} us", d.as_secs_f64() * 1e6)
}

fn main() {
    let family = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Menlo".to_string());
    let scale = 2.0;

    let t = Instant::now();
    let mut renderer = match Renderer::new(&FontConfig {
        family: family.clone(),
        size_pt: 13.0,
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let new_time = t.elapsed();
    let startup = renderer.startup_times();
    let t = Instant::now();
    let m = renderer.metrics(scale);
    let font_time = t.elapsed();

    let (w, h) = (COLS as u32 * m.cell_w, ROWS as u32 * m.cell_h);
    let pages: Vec<Frame> = (0..4)
        .map(|i| support::ascii_page(COLS, ROWS, i * 5))
        .collect();
    let scene_for = |frame: &Frame| -> (u32, u32, Frame) { (w, h, frame.clone()) };

    // First frame: cold glyph cache.
    let (_, _, first) = scene_for(&pages[0]);
    let draw = |r: &mut Renderer, frame: &Frame| -> Duration {
        let panes = [PaneDraw {
            id: 1,
            rect: Rect::new(0, 0, w as i32, h as i32),
            origin: Point { x: 0, y: 0 },
            frame,
            focused: true,
            cursor_on: true,
            dim: 0.0,
        }];
        let scene = Scene {
            width: w,
            height: h,
            scale,
            clear: 0,
            panes: &panes,
            quads: &[],
        };
        let t = Instant::now();
        if let Err(e) = r.render_offscreen(&scene) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        t.elapsed()
    };
    let first_wall = draw(&mut renderer, &first);
    let first_stats = renderer.stats();

    // Warm every page's glyphs, then alternate pages so every row changes each frame.
    let mut frame = pages[0].clone();
    for p in &pages {
        draw(&mut renderer, p);
    }
    let mut full_cpu = Vec::new();
    let mut full_wall = Vec::new();
    let mut instances = 0;
    for i in 0..200 {
        let next = &pages[(i + 1) % pages.len()];
        frame.cells.copy_from_slice(&next.cells);
        frame.seq += 1;
        let seq = frame.seq;
        frame.row_seq.iter_mut().for_each(|s| *s = seq);
        full_wall.push(draw(&mut renderer, &frame));
        let s = renderer.stats();
        assert_eq!(s.rows_rebuilt, ROWS as usize);
        instances = s.instances;
        full_cpu.push(s.prepare + s.encode);
    }

    let mut same_cpu = Vec::new();
    let mut same_prepare = Vec::new();
    for _ in 0..200 {
        draw(&mut renderer, &frame);
        let s = renderer.stats();
        assert_eq!(s.rows_rebuilt, 0);
        same_cpu.push(s.prepare + s.encode);
        same_prepare.push(s.prepare);
    }

    // Worst case: every cell has its own background, a glyph and an
    // underline, and every row changes every frame.
    let dense: Vec<Frame> = (0..2u32)
        .map(|k| {
            let mut f = pages[0].clone();
            for (i, cell) in f.cells.iter_mut().enumerate() {
                let i = i as u32 + k;
                cell.ch = b'!' as u32 + i % 94;
                cell.bg = 0x202020 + (i % 7) * 0x010203;
                cell.flags |= flags::UNDERLINE;
            }
            f
        })
        .collect();
    for d in &dense {
        draw(&mut renderer, d);
    }
    let mut dense_cpu = Vec::new();
    let mut dense_instances = 0;
    for i in 0..200 {
        frame.cells.copy_from_slice(&dense[i % 2].cells);
        frame.seq += 1;
        let seq = frame.seq;
        frame.row_seq.iter_mut().for_each(|s| *s = seq);
        draw(&mut renderer, &frame);
        let s = renderer.stats();
        assert_eq!(s.rows_rebuilt, ROWS as usize);
        dense_instances = s.instances;
        dense_cpu.push(s.prepare + s.encode);
    }

    let mut one_row_cpu = Vec::new();
    for i in 0..200 {
        let row = (i % ROWS as usize) as u16;
        frame.row_mut(row)[0].ch = b'a' as u32 + (i % 26) as u32;
        frame.seq += 1;
        frame.row_seq[row as usize] = frame.seq;
        draw(&mut renderer, &frame);
        let s = renderer.stats();
        assert_eq!(s.rows_rebuilt, 1);
        one_row_cpu.push(s.prepare + s.encode);
    }

    // Four unchanged panes side by side: the marginal cost of a pane.
    let quarter = |i: i32| {
        Rect::new(
            i * w as i32 / 2 % w as i32,
            (i / 2) * h as i32 / 2,
            w as i32 / 2,
            h as i32 / 2,
        )
    };
    let small: Vec<Frame> = (0..4)
        .map(|i| support::ascii_page(COLS / 2, ROWS / 2, i))
        .collect();
    let mut four_cpu = Vec::new();
    for _ in 0..200 {
        let panes: Vec<PaneDraw> = (0..4)
            .map(|i| PaneDraw {
                id: 10 + i as u64,
                rect: quarter(i),
                origin: Point {
                    x: quarter(i).x,
                    y: quarter(i).y,
                },
                frame: &small[i as usize],
                focused: i == 0,
                cursor_on: true,
                dim: if i == 0 { 0.0 } else { 0.3 },
            })
            .collect();
        let scene = Scene {
            width: w,
            height: h,
            scale,
            clear: 0,
            panes: &panes,
            quads: &[],
        };
        if let Err(e) = renderer.render_offscreen(&scene) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        let s = renderer.stats();
        four_cpu.push(s.prepare + s.encode);
    }
    // The first frames built the panes; the rest were unchanged.
    let four_same = median(four_cpu[10..].to_vec());

    println!("font {family} 13pt at scale {scale}: cell {}x{} px, pane {COLS}x{ROWS} = {w}x{h} px, {instances} instances", m.cell_w, m.cell_h);
    println!("Renderer::new total            {}", us(new_time));
    println!("  Metal device + queue         {}", us(startup.device));
    println!("  shader compile + pipeline    {}", us(startup.pipeline));
    println!("  font family lookup           {}", us(startup.font));
    println!("font load at scale (metrics)   {}", us(font_time));
    println!(
        "first frame, cold glyph cache  {} CPU ({} prepare), {} wall incl. GPU + readback",
        us(first_stats.prepare + first_stats.encode),
        us(first_stats.prepare),
        us(first_wall)
    );
    println!(
        "full rebuild, warm glyphs      {} CPU median, {} wall median",
        us(median(full_cpu)),
        us(median(full_wall))
    );
    println!(
        "full rebuild, dense            {} CPU median ({dense_instances} instances: every cell its own bg, glyph, underline)",
        us(median(dense_cpu))
    );
    println!(
        "one row changed                {} CPU median",
        us(median(one_row_cpu))
    );
    println!(
        "4 unchanged {}x{} panes         {} CPU median",
        COLS / 2,
        ROWS / 2,
        us(four_same)
    );
    println!(
        "unchanged frame                {} CPU median ({} prepare)",
        us(median(same_cpu)),
        us(median(same_prepare))
    );
}
