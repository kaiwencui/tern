//! The glyph cache of one backing scale: fonts, atlases and the lookup
//! tables from characters to atlas entries.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2::Message;
use objc2_metal::MTLPixelFormat;
use tern_types::{flags, Frame};

use crate::font::{Fonts, Raster, BOLD, ITALIC};
use crate::gpu::{Atlas, Device};
use crate::instances::{Glyph, GlyphSource, COLOR, MASK};
use crate::sprites::{self, Decoration, Mask};
use crate::CellMetrics;

/// FxHash: the keys are small integers and strings we produce ourselves, so
/// a fast non-cryptographic hash is enough and much cheaper than SipHash.
#[derive(Default, Clone, Copy)]
pub(crate) struct FxHasher(u64);

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, i: u8) {
        self.write_u64(i as u64);
    }

    fn write_u32(&mut self, i: u32) {
        self.write_u64(i as u64);
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type FxBuild = BuildHasherDefault<FxHasher>;

/// Every glyph cache gets a distinct generation, so cached pane instances
/// built against another cache (another scale, font, or a cleared atlas) are
/// never mistaken for current.
static GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// Key bits for the character map: the character, style and width.
fn char_key(ch: u32, style: usize, wide: bool) -> u32 {
    ch | (style as u32) << 21 | (wide as u32) << 23
}

/// Built-in glyphs ignore style and width.
const BUILTIN_KEY: u32 = 1 << 24;

pub(crate) struct ScaleState {
    pub size_pt: f32,
    pub scale: f32,
    pub fonts: Fonts,
    /// Created on first use; None also when the GPU could not allocate one,
    /// in which case glyphs come out empty instead of failing the frame.
    pub mask: Option<Atlas>,
    pub color: Option<Atlas>,
    pub generation: u64,
    /// Whether a full atlas may start over. The renderer allows it once per
    /// frame: a second reset would strand instances built earlier in the
    /// same frame, so glyphs that still do not fit draw blank instead.
    pub may_reset: bool,
    /// Renderer frame counter when last used, for evicting unused scales.
    pub last_used: u64,
    device: Retained<Device>,
    ascii: Box<[[Option<Glyph>; 128]; 4]>,
    chars: HashMap<u32, Glyph, FxBuild>,
    /// Keyed by style digit, width digit and the cluster's text.
    clusters: HashMap<Box<str>, Glyph, FxBuild>,
    decorations: [Option<Glyph>; 6],
    scratch: String,
}

impl ScaleState {
    pub(crate) fn new(device: &Device, family: &str, size_pt: f32, scale: f32) -> ScaleState {
        ScaleState {
            size_pt,
            scale,
            fonts: Fonts::with_fallback(family, size_pt * scale),
            mask: None,
            color: None,
            generation: next_generation(),
            may_reset: true,
            last_used: 0,
            device: device.retain(),
            ascii: Box::new([[None; 128]; 4]),
            chars: HashMap::default(),
            clusters: HashMap::default(),
            decorations: [None; 6],
            scratch: String::new(),
        }
    }

    pub(crate) fn metrics(&self) -> CellMetrics {
        self.fonts.metrics()
    }

    /// Number of glyphs cached.
    #[cfg(test)]
    pub(crate) fn cached(&self) -> usize {
        self.ascii.iter().flatten().filter(|g| g.is_some()).count()
            + self.chars.len()
            + self.clusters.len()
            + self.decorations.iter().filter(|g| g.is_some()).count()
    }

    /// Drops every glyph and starts the atlases over, under a new generation.
    /// The atlases get fresh textures: a frame still in flight reads the old
    /// ones (its command buffer retains them), so new glyphs must not be
    /// written over what it draws.
    fn reset(&mut self) {
        self.mask = None;
        self.color = None;
        *self.ascii = [[None; 128]; 4];
        self.chars.clear();
        self.clusters.clear();
        self.decorations = [None; 6];
        self.generation = next_generation();
    }

    /// Puts a raster into its atlas. When the atlas is full at its largest
    /// size it starts over, if allowed; otherwise the glyph draws blank.
    fn store(&mut self, r: &Raster) -> Glyph {
        if r.w == 0 || r.h == 0 || (r.kind != MASK && r.kind != COLOR) {
            return Glyph::EMPTY;
        }
        if let Some(g) = self.insert(r) {
            return g;
        }
        if !self.may_reset {
            return Glyph::EMPTY;
        }
        self.reset();
        self.may_reset = false;
        self.insert(r).unwrap_or(Glyph::EMPTY)
    }

    /// Puts a raster into its atlas (created on first use), or None when the
    /// atlas is full or the GPU cannot allocate it.
    fn insert(&mut self, r: &Raster) -> Option<Glyph> {
        let (slot, format, size) = if r.kind == COLOR {
            (&mut self.color, MTLPixelFormat::BGRA8Unorm, 512)
        } else {
            (&mut self.mask, MTLPixelFormat::R8Unorm, 1024)
        };
        if slot.is_none() {
            *slot = Atlas::new(&self.device, format, size);
        }
        let uv = slot.as_mut()?.insert(&self.device, r.w, r.h, &r.data)?;
        Some(Glyph {
            kind: r.kind,
            uv: [uv.0, uv.1],
            size: [r.w as u16, r.h as u16],
            offset: [
                r.left.clamp(-32768, 32767) as i16,
                r.top.clamp(-32768, 32767) as i16,
            ],
        })
    }

    fn store_mask(&mut self, m: Mask) -> Glyph {
        self.store(&Raster {
            kind: MASK,
            w: m.w,
            h: m.h,
            left: 0,
            top: 0,
            data: m.data,
        })
    }

    fn char_glyph(&mut self, ch: u32, style: usize, wide: bool) -> Glyph {
        if ch < 128 && !wide
            && let Some(g) = self.ascii[style][ch as usize] {
                return g;
            }
        let key = if sprites::is_builtin(ch) {
            ch | BUILTIN_KEY
        } else {
            char_key(ch, style, wide)
        };
        if (ch >= 128 || wide)
            && let Some(g) = self.chars.get(&key) {
                return *g;
            }
        let g = match sprites::builtin(ch, &self.fonts.metrics()) {
            Some(mask) => self.store_mask(mask),
            None => {
                let c = char::from_u32(ch).unwrap_or(' ');
                let r = self.fonts.rasterize(c, style, if wide { 2 } else { 1 });
                self.store(&r)
            }
        };
        if ch < 128 && !wide {
            self.ascii[style][ch as usize] = Some(g);
        } else {
            self.chars.insert(key, g);
        }
        g
    }

    fn cluster_glyph(&mut self, base: char, extras: &[char], style: usize, wide: bool) -> Glyph {
        let mut key = std::mem::take(&mut self.scratch);
        key.clear();
        key.push(char::from(b'0' + style as u8));
        key.push(if wide { '2' } else { '1' });
        key.push(base);
        key.extend(extras);
        let g = match self.clusters.get(key.as_str()) {
            Some(g) => *g,
            None => {
                let r = self
                    .fonts
                    .rasterize_cluster(&key[2..], style, if wide { 2 } else { 1 });
                let g = self.store(&r);
                self.clusters.insert(key.as_str().into(), g);
                g
            }
        };
        self.scratch = key;
        g
    }
}

/// The zero-width characters attached to the cell at index. A reference
/// outside extra_chars (a bug in whatever built the frame) counts as none
/// rather than taking the renderer down.
fn extras_of(frame: &Frame, index: usize) -> &[char] {
    let Ok(i) = frame
        .extra_refs
        .binary_search_by_key(&index, |e| e.cell as usize)
    else {
        return &[];
    };
    let e = frame.extra_refs[i];
    let start = e.start as usize;
    start
        .checked_add(e.len as usize)
        .and_then(|end| frame.extra_chars.get(start..end))
        .unwrap_or(&[])
}

impl GlyphSource for ScaleState {
    fn glyph(&mut self, frame: &Frame, index: usize) -> Glyph {
        let cell = &frame.cells[index];
        let style =
            (cell.has(flags::BOLD) as usize * BOLD) | (cell.has(flags::ITALIC) as usize * ITALIC);
        let wide = cell.has(flags::WIDE);
        if cell.has(flags::HAS_EXTRA) {
            let extras = extras_of(frame, index);
            if !extras.is_empty() {
                return self.cluster_glyph(cell.char(), extras, style, wide);
            }
        }
        if char::from_u32(cell.ch).is_none() {
            return Glyph::EMPTY;
        }
        self.char_glyph(cell.ch, style, wide)
    }

    fn decoration(&mut self, d: Decoration) -> Glyph {
        if let Some(g) = self.decorations[d.index()] {
            return g;
        }
        let g = self.store_mask(sprites::decoration(d, &self.fonts.metrics()));
        self.decorations[d.index()] = Some(g);
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_metal::MTLTexture;
    use std::hash::BuildHasher;
    use tern_types::{Cell, ExtraRef};

    fn state() -> ScaleState {
        let device = objc2_metal::MTLCreateSystemDefaultDevice().expect("Metal device");
        ScaleState::new(&device, "Menlo", 13.0, 2.0)
    }

    fn frame_of(cells: Vec<Cell>) -> Frame {
        let mut f = Frame {
            cols: cells.len() as u16,
            rows: 1,
            cells,
            ..Frame::default()
        };
        f.row_seq = vec![1];
        f
    }

    #[test]
    fn fx_hash_spreads_small_keys() {
        let b = FxBuild::default();
        let hashes: std::collections::HashSet<u64> = (0..1000u32).map(|k| b.hash_one(k)).collect();
        assert_eq!(hashes.len(), 1000);
    }

    #[test]
    fn glyphs_are_cached_by_char_style_and_width() {
        let mut st = state();
        let mut cells = vec![Cell::blank(0xffffff, 0); 6];
        cells[0].ch = 'a' as u32;
        cells[1].ch = 'a' as u32;
        cells[1].flags = flags::BOLD;
        cells[2].ch = '─' as u32;
        cells[3].ch = '漢' as u32;
        cells[3].flags = flags::WIDE;
        cells[4].ch = 'a' as u32;
        cells[5].ch = 0xd800; // not a scalar value
        let f = frame_of(cells);
        let a = st.glyph(&f, 0);
        let bold = st.glyph(&f, 1);
        assert!(!a.is_empty() && !bold.is_empty());
        assert_ne!(a.uv, bold.uv);
        assert_eq!(st.glyph(&f, 4), a);
        let line = st.glyph(&f, 2);
        let m = st.metrics();
        assert_eq!(
            (line.size, line.offset),
            ([m.cell_w as u16, m.cell_h as u16], [0, 0])
        );
        assert_eq!(st.glyph(&f, 3).kind, MASK);
        assert!(st.glyph(&f, 5).is_empty());
        assert_eq!(st.cached(), 4);
    }

    #[test]
    fn clusters_are_cached_by_text() {
        let mut st = state();
        let mut cells = vec![Cell::blank(0xffffff, 0); 2];
        for c in &mut cells {
            c.ch = 'e' as u32;
            c.flags = flags::HAS_EXTRA;
        }
        let mut f = frame_of(cells);
        f.extra_chars = vec!['\u{301}', '\u{300}'];
        f.extra_refs = vec![
            ExtraRef {
                cell: 0,
                start: 0,
                len: 1,
            },
            ExtraRef {
                cell: 1,
                start: 1,
                len: 1,
            },
        ];
        let acute = st.glyph(&f, 0);
        let grave = st.glyph(&f, 1);
        assert_ne!(acute.uv, grave.uv);
        assert_eq!(st.glyph(&f, 0), acute);
        assert_eq!(st.cached(), 2);
    }

    #[test]
    fn thousands_of_glyphs_grow_the_atlas() {
        let mut st = state();
        let mut cells = vec![Cell::blank(0xffffff, 0); 1];
        let mut seen = std::collections::HashSet::new();
        // CJK ideographs: wide, distinct, all from one fallback font.
        for ch in 0x4e00u32..0x4e00 + 3000 {
            cells[0].ch = ch;
            cells[0].flags = flags::WIDE;
            let f = frame_of(cells.clone());
            let g = st.glyph(&f, 0);
            assert!(!g.is_empty());
            assert!(
                seen.insert((g.uv, g.kind)),
                "two glyphs share an atlas slot"
            );
        }
        let atlas = st.mask.as_ref().unwrap();
        assert!(atlas.texture.height() > 1024 || atlas.texture.width() > 1024);
    }

    #[test]
    fn decorations_are_cell_sized() {
        let mut st = state();
        let m = st.metrics();
        for d in Decoration::ALL {
            let g = st.decoration(d);
            assert_eq!(g.size, [m.cell_w as u16, m.cell_h as u16]);
            assert_eq!(st.decoration(d), g);
        }
    }

    #[test]
    fn a_full_atlas_starts_over_with_a_new_generation() {
        let mut st = state();
        let gen0 = st.generation;
        let raster = |n: u8| Raster {
            kind: MASK,
            w: 4096,
            h: 4096,
            left: 0,
            top: 0,
            data: vec![n; 4096 * 4096],
        };
        // 8192 x 8192 holds four of these; the fifth forces a reset.
        for n in 0..4 {
            assert!(!st.store(&raster(n)).is_empty());
            assert_eq!(st.generation, gen0);
        }
        // Held as a command buffer in flight would hold it.
        let before = st.mask.as_ref().map(|a| a.texture.clone()).unwrap();
        let g = st.store(&raster(9));
        assert!(!g.is_empty());
        assert_ne!(st.generation, gen0);
        assert_eq!(g.uv, [0, 0]);
        // Once per frame: filling up again draws blank instead of starting
        // over a second time.
        let gen1 = st.generation;
        for n in 0..3 {
            assert!(!st.store(&raster(n)).is_empty());
        }
        assert!(st.store(&raster(7)).is_empty());
        assert_eq!(st.generation, gen1);
        // A frame still in flight may read the old texture, so the new glyphs
        // must go into a different one.
        let after = st.mask.as_ref().map(|a| a.texture.clone()).unwrap();
        assert_ne!(Retained::as_ptr(&before), Retained::as_ptr(&after));
    }

    #[test]
    fn malformed_extras_do_not_panic() {
        let mut st = state();
        let mut cells = vec![Cell::blank(0xffffff, 0); 3];
        for c in &mut cells {
            c.ch = 'e' as u32;
            c.flags = flags::HAS_EXTRA;
        }
        let mut f = frame_of(cells);
        f.extra_chars = vec!['\u{301}'];
        f.extra_refs = vec![
            // Past the end of extra_chars.
            ExtraRef {
                cell: 0,
                start: 0,
                len: 5,
            },
            // start + len overflows u32.
            ExtraRef {
                cell: 1,
                start: u32::MAX,
                len: 2,
            },
        ];
        // Falls back to the plain character.
        assert_eq!(st.glyph(&f, 0), st.glyph(&f, 2));
        assert_eq!(st.glyph(&f, 1), st.glyph(&f, 2));
        assert!(!st.glyph(&f, 0).is_empty());
    }
}
