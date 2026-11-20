//! Renders sample scenes offscreen and writes PNGs, at scale 1 and 2, in
//! Berkeley Mono Trial (when installed) and Menlo.
//!
//!     cargo run -p tern-render --example demo -- <out-dir>

#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use support::{Term, BG, FG, PALETTE};
use tern_render::{FontConfig, PaneDraw, Quad, Renderer, Scene};
use tern_types::{flags, xterm_256_palette, CursorShape, Frame, Point, Rect};

fn ascii_page() -> Frame {
    let mut t = Term::new(78, 20, FG, BG);
    t.at(0, 0)
        .print("The quick brown fox jumps over the lazy dog. 0123456789 !@#$%^&*()[]{}<>");
    t.at(1, 0)
        .print("fn main() { let x: Vec<u8> = (0..=255).collect(); println!(\"{x:?}\"); }");
    for (i, c) in PALETTE.iter().enumerate() {
        t.at(3 + (i / 8) as u16, (i % 8) as u16 * 9)
            .style(*c, BG, 0)
            .print(&format!("color{i:02}"));
    }
    t.at(6, 0).style(FG, BG, flags::BOLD).print("Bold text");
    t.reset().print("  ");
    t.style(FG, BG, flags::ITALIC).print("Italic text");
    t.reset().print("  ");
    t.style(FG, BG, flags::BOLD | flags::ITALIC)
        .print("Bold italic");
    t.reset().print("  ");
    t.style(0x8a8f98, BG, flags::DIM).print("dim (pre-applied)");
    t.at(7, 0).style(BG, FG, 0).print(" inverse ");
    t.reset().print(" ");
    t.style(0x000000, 0xe5c07b, 0).print(" on yellow ");
    t.reset().print(" ");
    t.style(0xffffff, 0x3e4451, flags::SELECTED)
        .print(" selected ");
    t.reset().print(" ");
    t.style(FG, BG, flags::HIDDEN).print("hidden!");
    t.reset().print(" <- hidden");
    let styles = [
        ("underline", flags::UNDERLINE),
        ("double", flags::DOUBLE_UNDERLINE),
        ("curly", flags::UNDERCURL),
        ("dotted", flags::DOTTED_UNDERLINE),
        ("dashed", flags::DASHED_UNDERLINE),
        ("strikeout", flags::STRIKEOUT),
    ];
    for (i, (name, f)) in styles.iter().enumerate() {
        t.at(9, i as u16 * 13).style(FG, BG, *f);
        if *f == flags::UNDERCURL {
            t.underline_color(0xe06c75);
        }
        t.print(name);
    }
    t.at(10, 0)
        .style(0x61afef, BG, flags::UNDERLINE | flags::BOLD)
        .print("bold underlined link");
    t.reset().print("  ");
    t.style(0x98c379, BG, flags::ITALIC | flags::UNDERCURL)
        .underline_color(0xe5c07b)
        .print("italic curly gjpqy");
    t.at(12, 0)
        .reset()
        .print("Descenders gjpqy|  Ascenders bdfhklt  Caps ÀÉÎÕÜ  Accents àéîõü ñ ç ß");
    t.at(13, 0)
        .print("Symbols: → ← ↑ ↓ ⇒ ≠ ≤ ≥ ± × ÷ ∞ √ ∑ ∫ π λ Ω µ ° • … © ® ™ € £ ¥");
    t.at(14, 0)
        .print("Ligature check (no ligatures across cells): -> => != === <= >= :: ... www");
    t.at(16, 0).style(0x000000, 0x98c379, 0).print(" NORMAL ");
    t.style(0x98c379, 0x3e4451, 0).print("\u{e0b0}");
    t.style(FG, 0x3e4451, 0).print(" main ");
    t.style(0x3e4451, BG, 0).print("\u{e0b0}");
    t.at(18, 0).reset().print("$ ");
    t.cursor(18, 2, CursorShape::Block, FG, BG);
    t.frame()
}

fn color_ramps() -> Frame {
    let mut t = Term::new(72, 16, FG, BG);
    let palette = xterm_256_palette();
    t.at(0, 0).print("256 colors:");
    for i in 0..16u16 {
        t.at(1, i * 4)
            .style(FG, palette[i as usize], 0)
            .print(&format!("{i:>3} "));
    }
    for i in 0..216u16 {
        let (row, col) = (2 + i / 36, (i % 36) * 2);
        t.at(row, col)
            .style(FG, palette[16 + i as usize], 0)
            .print("  ");
    }
    for i in 0..24u16 {
        t.at(8, i * 3)
            .style(FG, palette[232 + i as usize], 0)
            .print("   ");
    }
    t.at(10, 0).reset().print("Truecolor:");
    for col in 0..72u16 {
        let x = col as u32 * 255 / 71;
        let (r, g, b) = (x, 255 - x, (x * 2).min(255) ^ 0x80);
        t.at(11, col).style(FG, r << 16 | g << 8 | b, 0).print(" ");
        let hue = col as f32 / 72.0 * 6.0;
        let f = hue.fract();
        let (r, g, b) = match hue as u32 {
            0 => (1.0, f, 0.0),
            1 => (1.0 - f, 1.0, 0.0),
            2 => (0.0, 1.0, f),
            3 => (0.0, 1.0 - f, 1.0),
            4 => (f, 0.0, 1.0),
            _ => (1.0, 0.0, 1.0 - f),
        };
        let rgb = ((r * 255.0) as u32) << 16 | ((g * 255.0) as u32) << 8 | (b * 255.0) as u32;
        t.at(12, col).style(FG, rgb, 0).print(" ");
        t.at(13, col).style(rgb, BG, 0).print("█");
        let gray = col as u32 * 255 / 71;
        t.at(14, col)
            .style(FG, gray << 16 | gray << 8 | gray, 0)
            .print(" ");
    }
    t.frame()
}

fn unicode() -> Frame {
    let mut t = Term::new(64, 14, FG, BG);
    t.at(0, 0).print("CJK: 漢字かなカナ한국어 中文字体测试");
    t.at(1, 0).print("Fullwidth: ＡＢＣ１２３");
    t.at(3, 0).print("Emoji: 😀 🚀 🎉 👍 🐱 🍕");
    t.at(4, 0)
        .print("ZWJ family: 👨\u{200d}👩\u{200d}👧\u{200d}👦  flags: 🇯🇵 🇺🇸 🇫🇷");
    t.at(5, 0)
        .print("Skin tone: 👋🏽  heart: ❤\u{fe0f}  keycap: 1\u{fe0f}\u{20e3}");
    t.at(7, 0)
        .print("Combining: e\u{301} a\u{300} o\u{308} n\u{303} c\u{327} a\u{30a}\u{301} Z\u{335}");
    t.at(8, 0)
        .print("Greek: αβγδε ΑΒΓΔΕ  Cyrillic: Привет  Hebrew: שלום");
    t.at(9, 0).print("Math: ∀x∈ℝ: ∃y ⊂ ℕ, ⌈x⌉ ≅ ⌊y⌋ ∧ ¬z");
    t.at(11, 0)
        .style(0xe5c07b, BG, flags::BOLD)
        .print("Bold 漢字 ");
    t.style(0x61afef, BG, flags::ITALIC | flags::UNDERLINE)
        .print("italic 漢字 underlined");
    t.frame()
}

fn chart() -> Frame {
    let mut t = Term::new(70, 30, FG, BG);
    t.at(0, 0)
        .print("Box drawing U+2500..257F, blocks U+2580..259F, braille, Powerline");
    for i in 0..128u32 {
        let c = char::from_u32(0x2500 + i).unwrap_or(' ');
        let (row, col) = (2 + (i / 32) as u16, (i % 32) as u16 * 2);
        t.at(row, col).print(&c.to_string());
    }
    for i in 0..32u32 {
        let c = char::from_u32(0x2580 + i).unwrap_or(' ');
        t.at(7, i as u16 * 2).print(&c.to_string());
    }
    for i in 0..256u32 {
        let c = char::from_u32(0x2800 + i).unwrap_or(' ');
        let (row, col) = (9 + (i / 64) as u16, (i % 64) as u16);
        t.at(row, col).print(&c.to_string());
    }
    t.at(14, 0).style(0x000000, 0x61afef, 0).print(" pl ");
    for c in [
        '\u{e0b0}', '\u{e0b1}', '\u{e0b2}', '\u{e0b3}', '\u{e0b4}', '\u{e0b5}', '\u{e0b6}',
        '\u{e0b7}',
    ] {
        t.style(0x61afef, 0x3e4451, 0).print(&c.to_string());
        t.style(FG, 0x3e4451, 0).print(" ");
    }
    t.reset();
    // Joined structures: boxes, a table, a tree and shading.
    let art = [
        "┌──────┬──────┐  ╔══════╦══════╗  ┏━━━━━━┳━━━━━━┓  ╭──────╮",
        "│ cell │ cell │  ║ dbl  ║ dbl  ║  ┃ bold ┃ bold ┃  │round │",
        "├──────┼──────┤  ╠══════╬══════╣  ┣━━━━━━╋━━━━━━┫  ╰──────╯",
        "│ cell │ cell │  ║ dbl  ║ dbl  ║  ┃ bold ┃ bold ┃  ╱╲╱╲╱╲",
        "└──────┴──────┘  ╚══════╩══════╝  ┗━━━━━━┻━━━━━━┛  ╲╱╲╱╲╱",
        "╒═════╤═════╕  ╓─────╥─────╖  ┄┄┄┄ ┈┈┈┈ ╌╌╌╌  ┆┊╎",
        "╞═════╪═════╡  ╟─────╫─────╢  ┅┅┅┅ ┉┉┉┉ ╍╍╍╍  ┇┋╏",
        "╘═════╧═════╛  ╙─────╨─────╜  ░░░░▒▒▒▒▓▓▓▓████",
        "▀▀▀▀▄▄▄▄ ▌▐▌▐ ▖▗▘▝ ▙▛▜▟ ▚▞  █▉▊▋▌▍▎▏ ▁▂▃▄▅▆▇█",
        "████████████████  ▄▄▄▄▄▄▄▄  ▀▀▀▀▀▀▀▀",
        "████████████████  ████████  ████████",
    ];
    for (i, line) in art.iter().enumerate() {
        t.at(16 + i as u16, 0).print(line);
    }
    t.at(28, 0)
        .style(0x98c379, BG, 0)
        .print("⣿⣿⣿⣿⡇⢸⣿⣿ ⠁⠂⠄⡀⠈⠐⠠⢀ braille spinner ⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");
    t.frame()
}

fn cursors() -> Vec<Frame> {
    let shapes = [
        CursorShape::Block,
        CursorShape::Beam,
        CursorShape::Underline,
        CursorShape::HollowBlock,
    ];
    let mut frames = Vec::new();
    for shape in shapes {
        let mut t = Term::new(24, 3, FG, BG);
        t.at(0, 0).print(&format!("{shape:?}"));
        t.at(1, 0).print("$ echo hi 漢字");
        t.cursor(1, 7, shape, 0xe5c07b, BG);
        frames.push(t.frame());
    }
    let mut t = Term::new(24, 3, FG, BG);
    t.at(0, 0).print("Wide block cursor");
    t.at(1, 0).print("$ echo hi 漢字");
    t.cursor(1, 10, CursorShape::Block, 0xe5c07b, BG);
    frames.push(t.frame());
    frames
}

fn shell_pane(title: &str, color: u32) -> Frame {
    let mut t = Term::new(36, 12, FG, BG);
    t.at(0, 0).style(0x98c379, BG, flags::BOLD).print("kai@mac");
    t.reset().print(":");
    t.style(0x61afef, BG, 0).print("~/tern");
    t.reset().print("$ ls");
    t.at(1, 0).style(color, BG, 0).print(title);
    t.at(2, 0).reset().print("Cargo.toml  crates  target");
    t.at(3, 0).print("$ cargo build");
    t.at(4, 0)
        .style(0x98c379, BG, flags::BOLD)
        .print("   Compiling");
    t.reset().print(" tern-render v0.1.0");
    t.at(5, 0)
        .style(0x98c379, BG, flags::BOLD)
        .print("    Finished");
    t.reset().print(" dev in 1.2s");
    t.at(6, 0).print("$ ");
    t.cursor(6, 2, CursorShape::Block, FG, BG);
    t.frame()
}

struct Out<'a> {
    renderer: &'a mut Renderer,
    dir: &'a Path,
    tag: &'a str,
}

impl Out<'_> {
    /// One pane filling the image with padding around the grid.
    fn single(&mut self, name: &str, frame: &Frame, scale: f32) {
        let m = self.renderer.metrics(scale);
        let pad = (6.0 * scale) as i32;
        let w = frame.cols as i32 * m.cell_w as i32 + 2 * pad;
        let h = frame.rows as i32 * m.cell_h as i32 + 2 * pad;
        let panes = [PaneDraw {
            id: 1,
            rect: Rect::new(0, 0, w, h),
            origin: Point { x: pad, y: pad },
            frame,
            focused: true,
            cursor_on: true,
            dim: 0.0,
        }];
        let scene = Scene {
            width: w as u32,
            height: h as u32,
            scale,
            clear: BG,
            panes: &panes,
            quads: &[],
        };
        self.write(name, &scene);
        self.renderer.forget_pane(1);
    }

    fn write(&mut self, name: &str, scene: &Scene) {
        let image = match self.renderer.render_offscreen(scene) {
            Ok(image) => image,
            Err(e) => {
                eprintln!("{name}: {e}");
                return;
            }
        };
        let path = self
            .dir
            .join(format!("{}-{name}@{}x.png", self.tag, scene.scale));
        if let Err(e) = image.write_png(&path) {
            eprintln!("{}: {e}", path.display());
        } else {
            println!("{}", path.display());
        }
    }

    fn cursors(&mut self, scale: f32) {
        let frames = cursors();
        let m = self.renderer.metrics(scale);
        let pad = (6.0 * scale) as i32;
        let pw = 24 * m.cell_w as i32 + 2 * pad;
        let ph = 3 * m.cell_h as i32 + 2 * pad;
        let mut panes = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            panes.push(PaneDraw {
                id: 10 + i as u64,
                rect: Rect::new(0, i as i32 * ph, pw, ph),
                origin: Point {
                    x: pad,
                    y: i as i32 * ph + pad,
                },
                frame: f,
                focused: true,
                cursor_on: true,
                dim: 0.0,
            });
        }
        // The same block cursor, unfocused: hollow.
        panes.push(PaneDraw {
            id: 20,
            rect: Rect::new(pw, 0, pw, ph),
            origin: Point {
                x: pw + pad,
                y: pad,
            },
            frame: &frames[0],
            focused: false,
            cursor_on: true,
            dim: 0.0,
        });
        // Blink phase off: no cursor.
        panes.push(PaneDraw {
            id: 21,
            rect: Rect::new(pw, ph, pw, ph),
            origin: Point {
                x: pw + pad,
                y: ph + pad,
            },
            frame: &frames[0],
            focused: true,
            cursor_on: false,
            dim: 0.0,
        });
        let h = frames.len() as i32 * ph;
        let quads = [Quad {
            rect: Rect::new(pw, 0, (scale as i32).max(1), h),
            color: 0x3e4451,
            alpha: 1.0,
        }];
        let scene = Scene {
            width: (2 * pw) as u32,
            height: h as u32,
            scale,
            clear: BG,
            panes: &panes,
            quads: &quads,
        };
        self.write("cursors", &scene);
        for p in &panes {
            self.renderer.forget_pane(p.id);
        }
    }

    fn split(&mut self, scale: f32) {
        let left = shell_pane("focused pane", 0xe5c07b);
        let right = shell_pane("dimmed pane", 0xc678dd);
        let m = self.renderer.metrics(scale);
        let pad = (6.0 * scale) as i32;
        let pw = 36 * m.cell_w as i32 + 2 * pad;
        let ph = 12 * m.cell_h as i32 + 2 * pad;
        let divider = (scale as i32).max(1);
        let panes = [
            PaneDraw {
                id: 30,
                rect: Rect::new(0, 0, pw, ph),
                origin: Point { x: pad, y: pad },
                frame: &left,
                focused: true,
                cursor_on: true,
                dim: 0.0,
            },
            PaneDraw {
                id: 31,
                // Wider than its grid: the remainder shows the frame's bg.
                rect: Rect::new(pw + divider, 0, pw + 40, ph),
                origin: Point {
                    x: pw + divider + pad,
                    y: pad,
                },
                frame: &right,
                focused: false,
                cursor_on: true,
                dim: 0.35,
            },
        ];
        let w = 2 * pw + divider + 40;
        let quads = [
            Quad {
                rect: Rect::new(pw, 0, divider, ph),
                color: 0x5a6374,
                alpha: 1.0,
            },
            // A drop-zone highlight over the right half of the right pane.
            Quad {
                rect: Rect::new(pw + divider + pw / 2, 0, pw / 2 + 40, ph),
                color: 0x61afef,
                alpha: 0.25,
            },
        ];
        let scene = Scene {
            width: w as u32,
            height: ph as u32,
            scale,
            clear: 0x000000,
            panes: &panes,
            quads: &quads,
        };
        self.write("split", &scene);
        self.renderer.forget_pane(30);
        self.renderer.forget_pane(31);
    }
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo".to_string());
    let dir = Path::new(&dir);
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("{}: {e}", dir.display());
        std::process::exit(1);
    }
    let fonts = [("berkeley", "Berkeley Mono Trial"), ("menlo", "Menlo")];
    for (tag, family) in fonts {
        let config = FontConfig {
            family: family.to_string(),
            size_pt: 13.0,
        };
        let mut renderer = match Renderer::new(&config) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{family}: {e}; skipped");
                continue;
            }
        };
        let mut out = Out {
            renderer: &mut renderer,
            dir,
            tag,
        };
        for scale in [1.0, 2.0] {
            out.single("ascii", &ascii_page(), scale);
            out.single("colors", &color_ramps(), scale);
            out.single("unicode", &unicode(), scale);
            out.single("chart", &chart(), scale);
            out.cursors(scale);
            out.split(scale);
        }
    }
}
