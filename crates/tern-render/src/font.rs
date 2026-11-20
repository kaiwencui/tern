//! Fonts through Core Text: loading a family and its styles, cell metrics,
//! fallback, and rasterizing glyphs and clusters into bitmaps.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2_core_foundation::{
    kCFBooleanTrue, CFAttributedString, CFDictionary, CFRange, CFRetained, CFString, CFType,
    CGAffineTransform, CGFloat, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGGlyph, CGImageAlphaInfo, CGTextDrawingMode,
};
use objc2_core_text::{
    kCTFontAttributeName, kCTFontFamilyNameAttribute, kCTForegroundColorFromContextAttributeName,
    CTFont, CTFontDescriptor, CTFontOrientation, CTFontSymbolicTraits, CTFontUIFontType, CTLine,
    CTLineBoundsOptions,
};

use crate::instances::{COLOR, MASK, SOLID};
use crate::CellMetrics;
#[cfg(test)]
use crate::RenderError;

/// Style index: bit 0 bold, bit 1 italic.
pub(crate) const BOLD: usize = 1;
pub(crate) const ITALIC: usize = 2;

/// kCGBitmapByteOrder32Little: with premultiplied-first alpha the bytes in
/// memory are B, G, R, A, which is what a BGRA8Unorm texture expects.
const BYTE_ORDER_32_LITTLE: u32 = 2 << 12;

/// Slant of a synthesized italic: tan(12 degrees).
const SYNTHETIC_SLANT: CGFloat = 0.2126;

/// Bitmaps larger than this are not glyphs; a broken font cannot make us
/// allocate without bound.
const MAX_BITMAP: i32 = 2048;

/// Font sizes in pixels are kept within 1..=1024. Far beyond any real use;
/// it keeps a mistyped size from making every cell-sized sprite gigabytes.
const MAX_SIZE_PX: f32 = 1024.0;

fn sane_size(size_px: f32) -> CGFloat {
    if size_px.is_nan() {
        1.0
    } else {
        size_px.clamp(1.0, MAX_SIZE_PX) as CGFloat
    }
}

/// A rasterized glyph: an alpha mask or premultiplied BGRA pixels, with the
/// offset of its top-left corner from the top-left corner of its cell.
#[derive(Debug)]
pub(crate) struct Raster {
    /// MASK, COLOR, or SOLID when there is nothing to draw.
    pub kind: u32,
    pub w: u32,
    pub h: u32,
    pub left: i32,
    pub top: i32,
    pub data: Vec<u8>,
}

impl Raster {
    fn empty() -> Raster {
        Raster {
            kind: SOLID,
            w: 0,
            h: 0,
            left: 0,
            top: 0,
            data: Vec::new(),
        }
    }
}

struct Face {
    font: CFRetained<CTFont>,
    /// The family has no bold face for this style: draw with an outline stroke.
    synthetic_bold: bool,
}

/// One family at one pixel size: the four styles, the metrics and the
/// per-character fallback fonts.
pub(crate) struct Fonts {
    faces: [Face; 4],
    metrics: CellMetrics,
    size_px: CGFloat,
    /// Fallback font per (character, style); None when no font has it.
    fallback: HashMap<(char, usize), Option<CFRetained<CTFont>>>,
    /// The fallback fonts themselves, each once. Core Text makes a new font
    /// object every time it is asked for the font of a character, also
    /// when the font is one it has named before: a page of CJK text would
    /// leave a thousand objects for one font.
    fallbacks: Vec<CFRetained<CTFont>>,
}

/// The family name that stands for the system's monospaced font (SF Mono).
/// No descriptor matches that font by name, so it has no name here either.
pub(crate) const SYSTEM_FAMILY: &str = "";

/// The system's monospaced font at a pixel size: the system font with the
/// monospaced trait asked of it.
fn load_system(size_px: CGFloat) -> Option<CFRetained<CTFont>> {
    // SAFETY: no language means the user's own.
    let ui = unsafe { CTFont::new_ui_font_for_language(CTFontUIFontType::System, size_px, None) }?;
    let mono = CTFontSymbolicTraits::TraitMonoSpace;
    // SAFETY: null matrix keeps the font's own.
    let font = unsafe { ui.copy_with_symbolic_traits(0.0, ptr::null(), mono, mono) }?;
    traits(&font).contains(mono).then_some(font)
}

/// Loads the regular face of a family at a pixel size, or None when the
/// system has no such family.
fn load_family(family: &str, size_px: CGFloat) -> Option<CFRetained<CTFont>> {
    if family == SYSTEM_FAMILY {
        return load_system(size_px);
    }
    let name = CFString::from_str(family);
    let key: &CFString = unsafe { kCTFontFamilyNameAttribute };
    let attrs = CFDictionary::<CFString, CFString>::from_slices(&[key], &[&name]);
    // SAFETY: the dictionary holds CFString keys and values as the descriptor expects.
    let desc = unsafe { CTFontDescriptor::with_attributes(attrs.as_opaque()) };
    // SAFETY: a null matrix means identity.
    let font = unsafe { CTFont::with_font_descriptor(&desc, size_px, ptr::null()) };
    // Core Text always returns some font; make sure it is the one asked for.
    let got = unsafe { font.family_name() }.to_string();
    if !got.eq_ignore_ascii_case(family) {
        return None;
    }
    // The descriptor may match any face of the family; ask for the upright regular one.
    let mask = CTFontSymbolicTraits::TraitBold | CTFontSymbolicTraits::TraitItalic;
    // SAFETY: null matrix keeps the font's own.
    let regular = unsafe {
        font.copy_with_symbolic_traits(0.0, ptr::null(), CTFontSymbolicTraits::empty(), mask)
    };
    Some(regular.unwrap_or(font))
}

/// Whether the system has this font family.
pub(crate) fn family_exists(family: &str) -> bool {
    load_family(family, 12.0).is_some()
}

fn traits(font: &CTFont) -> CTFontSymbolicTraits {
    // SAFETY: plain getter on a valid font.
    unsafe { font.symbolic_traits() }
}

/// The face with the given traits from the same family, if the family has it.
fn styled(base: &CTFont, want: CTFontSymbolicTraits) -> Option<CFRetained<CTFont>> {
    // SAFETY: null matrix keeps the font's own.
    let font = unsafe { base.copy_with_symbolic_traits(0.0, ptr::null(), want, want) }?;
    traits(&font).contains(want).then_some(font)
}

fn slanted(base: &CTFont) -> CFRetained<CTFont> {
    let skew = CGAffineTransform {
        a: 1.0,
        b: 0.0,
        c: SYNTHETIC_SLANT,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };
    // SAFETY: the matrix lives for the duration of the call.
    unsafe { base.copy_with_attributes(0.0, &skew, None) }
}

fn glyph_for(font: &CTFont, ch: char) -> Option<CGGlyph> {
    let mut units = [0u16; 2];
    let n = ch.encode_utf16(&mut units).len();
    let mut glyphs = [0 as CGGlyph; 2];
    // SAFETY: both arrays hold n entries.
    let ok = unsafe {
        font.glyphs_for_characters(
            NonNull::new_unchecked(units.as_mut_ptr()),
            NonNull::new_unchecked(glyphs.as_mut_ptr()),
            n as isize,
        )
    };
    (ok && glyphs[0] != 0).then_some(glyphs[0])
}

fn advance(font: &CTFont, glyph: CGGlyph) -> CGFloat {
    let mut g = glyph;
    // SAFETY: one glyph, null output array allowed.
    unsafe {
        font.advances_for_glyphs(
            CTFontOrientation::Horizontal,
            NonNull::from(&mut g),
            ptr::null_mut(),
            1,
        )
    }
}

fn glyph_bounds(font: &CTFont, glyph: CGGlyph) -> CGRect {
    let mut g = glyph;
    // SAFETY: one glyph, null output array allowed.
    unsafe {
        font.bounding_rects_for_glyphs(
            CTFontOrientation::Horizontal,
            NonNull::from(&mut g),
            ptr::null_mut(),
            1,
        )
    }
}

fn is_color(font: &CTFont) -> bool {
    traits(font).contains(CTFontSymbolicTraits::TraitColorGlyphs)
}

fn resized(font: &CTFont, size: CGFloat) -> CFRetained<CTFont> {
    // SAFETY: null matrix keeps the font's own (including a synthetic slant).
    unsafe { font.copy_with_attributes(size, ptr::null(), None) }
}

fn line_for(text: &str, font: &CTFont) -> Option<CFRetained<CTLine>> {
    let s = CFString::from_str(text);
    // SAFETY: reading framework constants.
    let keys: [&CFString; 2] = unsafe {
        [
            kCTFontAttributeName,
            kCTForegroundColorFromContextAttributeName,
        ]
    };
    let yes: &CFType = unsafe { kCFBooleanTrue }?;
    let values: [&CFType; 2] = [font, yes];
    let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    // SAFETY: CFString keys with the value types Core Text documents for them.
    let attributed = unsafe { CFAttributedString::new(None, Some(&s), Some(attrs.as_opaque())) }?;
    // SAFETY: a valid attributed string.
    Some(unsafe { CTLine::with_attributed_string(&attributed) })
}

/// Typographic width, ascent and descent of a line.
fn line_extent(line: &CTLine) -> (CGFloat, CGFloat, CGFloat) {
    let (mut a, mut d) = (0.0, 0.0);
    // SAFETY: out-pointers to locals; leading may be null.
    let w = unsafe { line.typographic_bounds(&mut a, &mut d, ptr::null_mut()) };
    (w, a, d)
}

/// Where and how to draw: the pixel area relative to the cell's top-left
/// corner, and the pen position inside it.
struct Canvas {
    left: i32,
    top: i32,
    w: i32,
    h: i32,
}

impl Canvas {
    /// The area covering ink bounds (y up from the baseline) drawn with the pen
    /// at (pen_x, baseline) in cell coordinates, padded by pad pixels.
    fn around(ink: CGRect, pen_x: CGFloat, baseline: CGFloat, pad: i32) -> Option<Canvas> {
        if ink.size.width <= 0.0 || ink.size.height <= 0.0 {
            return None;
        }
        let left = (pen_x + ink.origin.x).floor() as i32 - pad;
        let right = (pen_x + ink.origin.x + ink.size.width).ceil() as i32 + pad;
        let top = (baseline - ink.origin.y - ink.size.height).floor() as i32 - pad;
        let bottom = (baseline - ink.origin.y).ceil() as i32 + pad;
        let (w, h) = (right - left, bottom - top);
        (w > 0 && h > 0 && w <= MAX_BITMAP && h <= MAX_BITMAP).then_some(Canvas { left, top, w, h })
    }

    /// The pen position in the bitmap's own coordinates (origin bottom-left).
    fn pen(&self, pen_x: CGFloat, baseline: CGFloat) -> CGPoint {
        CGPoint {
            x: pen_x - self.left as CGFloat,
            y: (self.top + self.h) as CGFloat - baseline,
        }
    }
}

/// A bitmap context over pixels we own. Color: premultiplied BGRA; otherwise
/// an alpha-only mask.
struct Bitmap {
    ctx: CFRetained<CGContext>,
    pixels: Vec<u8>,
    w: usize,
    h: usize,
    color: bool,
}

impl Bitmap {
    fn new(w: i32, h: i32, color: bool) -> Option<Bitmap> {
        let (w, h) = (w as usize, h as usize);
        let bpp = if color { 4 } else { 1 };
        let mut pixels = vec![0u8; w * h * bpp];
        let space = if color {
            CGColorSpace::new_device_rgb()
        } else {
            None
        };
        let info = if color {
            CGImageAlphaInfo::PremultipliedFirst.0 | BYTE_ORDER_32_LITTLE
        } else {
            CGImageAlphaInfo::Only.0
        };
        // SAFETY: pixels holds h rows of w * bpp bytes and outlives the context
        // (both live in this struct, and the context is dropped first).
        let ctx = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr() as *mut c_void,
                w,
                h,
                8,
                w * bpp,
                space.as_deref(),
                info,
            )
        }?;
        let c = Some(&*ctx);
        CGContext::set_should_antialias(c, true);
        CGContext::set_allows_font_smoothing(c, false);
        CGContext::set_should_smooth_fonts(c, false);
        CGContext::set_should_subpixel_position_fonts(c, false);
        CGContext::set_should_subpixel_quantize_fonts(c, false);
        CGContext::set_gray_fill_color(c, 1.0, 1.0);
        CGContext::set_gray_stroke_color(c, 1.0, 1.0);
        Some(Bitmap {
            ctx,
            pixels,
            w,
            h,
            color,
        })
    }

    fn synthetic_bold(&self, width: CGFloat) {
        let c = Some(&*self.ctx);
        CGContext::set_text_drawing_mode(c, CGTextDrawingMode::FillStroke);
        CGContext::set_line_width(c, width);
    }

    fn alpha(&self, x: usize, y: usize) -> u8 {
        if self.color {
            self.pixels[(y * self.w + x) * 4 + 3]
        } else {
            self.pixels[y * self.w + x]
        }
    }

    /// Whether any pixel is not a shade of white, i.e. the glyph has its own colors.
    fn has_color(&self) -> bool {
        self.color
            && self
                .pixels
                .chunks_exact(4)
                .any(|p| p[0] != p[3] || p[1] != p[3] || p[2] != p[3])
    }

    /// Crops to the drawn pixels and returns the raster, as a mask unless
    /// keep_color and the pixels carry color.
    fn finish(self, canvas: &Canvas, keep_color: bool) -> Raster {
        let (w, h) = (self.w, self.h);
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if self.alpha(x, y) != 0 {
                    x0 = x0.min(x);
                    x1 = x1.max(x + 1);
                    y0 = y0.min(y);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x0 >= x1 {
            return Raster::empty();
        }
        let color = keep_color && self.has_color();
        let bpp = if color { 4 } else { 1 };
        let mut data = Vec::with_capacity((x1 - x0) * (y1 - y0) * bpp);
        for y in y0..y1 {
            if color {
                data.extend_from_slice(&self.pixels[(y * w + x0) * 4..(y * w + x1) * 4]);
            } else {
                data.extend((x0..x1).map(|x| self.alpha(x, y)));
            }
        }
        Raster {
            kind: if color { COLOR } else { MASK },
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
            left: canvas.left + x0 as i32,
            top: canvas.top + y0 as i32,
            data,
        }
    }
}

/// Used when neither the configured family (uninstalled while running, say)
/// nor the system's monospaced font can be had.
const FALLBACK_FAMILY: &str = "Menlo";

impl Fonts {
    #[cfg(test)]
    pub(crate) fn new(family: &str, size_px: f32) -> Result<Fonts, RenderError> {
        let size = sane_size(size_px);
        let regular = load_family(family, size)
            .ok_or_else(|| RenderError::FontNotFound(family.to_string()))?;
        Ok(Fonts::from_regular(regular, size))
    }

    /// The family, else the system's monospaced font, else whatever Core
    /// Text matches for Menlo's name; never fails, so a font removed at run
    /// time cannot stop drawing.
    pub(crate) fn with_fallback(family: &str, size_px: f32) -> Fonts {
        let size = sane_size(size_px);
        let regular = load_family(family, size)
            .or_else(|| load_system(size))
            .unwrap_or_else(|| {
                // SAFETY: a null matrix means identity.
                unsafe {
                    CTFont::with_name(
                        &CFString::from_static_str(FALLBACK_FAMILY),
                        size,
                        ptr::null(),
                    )
                }
            });
        Fonts::from_regular(regular, size)
    }

    fn from_regular(regular: CFRetained<CTFont>, size_px: CGFloat) -> Fonts {
        let bold = styled(&regular, CTFontSymbolicTraits::TraitBold);
        let italic = styled(&regular, CTFontSymbolicTraits::TraitItalic);
        let bold_italic = styled(
            &regular,
            CTFontSymbolicTraits::TraitBold | CTFontSymbolicTraits::TraitItalic,
        );

        let regular_face = Face {
            font: regular.clone(),
            synthetic_bold: false,
        };
        let bold_face = match &bold {
            Some(f) => Face {
                font: f.clone(),
                synthetic_bold: false,
            },
            None => Face {
                font: regular.clone(),
                synthetic_bold: true,
            },
        };
        let italic_face = Face {
            font: italic.clone().unwrap_or_else(|| slanted(&regular)),
            synthetic_bold: false,
        };
        let bold_italic_face = match (bold_italic, &italic, &bold) {
            (Some(f), _, _) => Face {
                font: f,
                synthetic_bold: false,
            },
            (None, Some(i), _) => Face {
                font: i.clone(),
                synthetic_bold: true,
            },
            (None, None, Some(b)) => Face {
                font: slanted(b),
                synthetic_bold: false,
            },
            (None, None, None) => Face {
                font: slanted(&regular),
                synthetic_bold: true,
            },
        };
        let metrics = cell_metrics(&regular, size_px);
        Fonts {
            faces: [regular_face, bold_face, italic_face, bold_italic_face],
            metrics,
            size_px,
            fallback: HashMap::new(),
            fallbacks: Vec::new(),
        }
    }

    pub(crate) fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    fn bold_stroke(&self) -> CGFloat {
        (self.size_px / 24.0).max(0.6)
    }

    /// A font that has ch in the given style: the face itself, or a fallback
    /// found once per character and remembered.
    fn font_for(&mut self, ch: char, style: usize) -> Option<(CFRetained<CTFont>, CGGlyph, bool)> {
        let face = &self.faces[style];
        if let Some(g) = glyph_for(&face.font, ch) {
            return Some((face.font.clone(), g, false));
        }
        let fallbacks = &mut self.fallbacks;
        let entry = self.fallback.entry((ch, style)).or_insert_with(|| {
            let mut buf = [0u8; 4];
            let s = CFString::from_str(ch.encode_utf8(&mut buf));
            let range = CFRange {
                location: 0,
                length: ch.len_utf16() as isize,
            };
            // SAFETY: the range covers the string.
            let font = unsafe { face.font.for_string(&s, range) };
            glyph_for(&font, ch)?;
            // The same font as one found before is that one.
            match fallbacks.iter().find(|known| ***known == *font) {
                Some(known) => Some(known.clone()),
                None => {
                    fallbacks.push(font.clone());
                    Some(font)
                }
            }
        });
        let font = entry.clone()?;
        let g = glyph_for(&font, ch)?;
        Some((font, g, true))
    }

    /// Padding for outlines that reach past the bounds Core Text reports:
    /// synthetic bold strokes and slanted faces.
    fn pad(&self, style: usize, synthetic_bold: bool, ascent: CGFloat) -> i32 {
        let bold = if synthetic_bold {
            self.bold_stroke().ceil() as i32
        } else {
            0
        };
        let slant = if style & ITALIC != 0 {
            (ascent * SYNTHETIC_SLANT).ceil() as i32
        } else {
            0
        };
        1 + bold + slant
    }

    /// Rasterizes one character in a style, centered in span cells.
    pub(crate) fn rasterize(&mut self, ch: char, style: usize, span: u32) -> Raster {
        let style = style & 3;
        let Some((font, glyph, fallback)) = self.font_for(ch, style) else {
            return Raster::empty();
        };
        let target_w = (span * self.metrics.cell_w) as CGFloat;
        if is_color(&font) {
            let first = self.draw_glyph(&font, glyph, style, target_w, true, false);
            return self.fit_color(first, target_w, |s| {
                // SAFETY: plain getter on a valid font.
                let smaller = resized(&font, unsafe { font.size() } * s);
                let g = glyph_for(&smaller, ch).unwrap_or(glyph);
                self.draw_glyph(&smaller, g, style, target_w, true, false)
            });
        }
        // A fallback glyph too wide for the grid is shrunk to fit its cells.
        let adv = advance(&font, glyph);
        if fallback && adv > target_w + 0.5 {
            // SAFETY: plain getter on a valid font.
            let smaller = resized(&font, unsafe { font.size() } * target_w / adv);
            let g = glyph_for(&smaller, ch).unwrap_or(glyph);
            return self.draw_glyph(&smaller, g, style, target_w, false, false);
        }
        let synthetic_bold = self.faces[style].synthetic_bold && !fallback;
        self.draw_glyph(&font, glyph, style, target_w, false, synthetic_bold)
    }

    /// Draws a glyph centered in target_w on the cell's baseline.
    fn draw_glyph(
        &self,
        font: &CTFont,
        glyph: CGGlyph,
        style: usize,
        target_w: CGFloat,
        color: bool,
        synthetic_bold: bool,
    ) -> Raster {
        let adv = advance(font, glyph);
        // SAFETY: plain getters on a valid font.
        let (asc, desc) = unsafe { (font.ascent(), font.descent()) };
        let pen_x = ((target_w - adv) / 2.0).round();
        let baseline = self.metrics.baseline as CGFloat;
        let mut ink = glyph_bounds(font, glyph);
        if color {
            // Bitmap fonts report loose bounds; take the whole em box too.
            ink = union(ink, em_box(adv, asc, desc));
        }
        let Some(canvas) =
            Canvas::around(ink, pen_x, baseline, self.pad(style, synthetic_bold, asc))
        else {
            return Raster::empty();
        };
        let Some(bitmap) = Bitmap::new(canvas.w, canvas.h, color) else {
            return Raster::empty();
        };
        if synthetic_bold {
            bitmap.synthetic_bold(self.bold_stroke());
        }
        let mut pos = canvas.pen(pen_x, baseline);
        let mut g = glyph;
        // SAFETY: one glyph and one position, valid for the call.
        unsafe {
            font.draw_glyphs(
                NonNull::from(&mut g),
                NonNull::from(&mut pos),
                1,
                &bitmap.ctx,
            )
        };
        bitmap.finish(&canvas, color)
    }

    /// Scales a color glyph so its ink fills target_w by the cell height
    /// (keeping its aspect), and centers it there. Emoji fonts carry generous
    /// margins, so fitting their metrics leaves them looking small.
    fn fit_color(
        &self,
        first: Raster,
        target_w: CGFloat,
        redraw: impl Fn(CGFloat) -> Raster,
    ) -> Raster {
        if first.kind != COLOR {
            return first;
        }
        let cell_h = self.metrics.cell_h as CGFloat;
        let mut r = first;
        let mut scale = 1.0;
        // Ink does not scale exactly with the font size (bitmap strikes,
        // rounding), so correct once more if the first guess is off.
        for _ in 0..2 {
            let fit = (target_w / r.w as CGFloat).min(cell_h / r.h as CGFloat);
            if (fit - 1.0).abs() < 0.03 && fit <= 1.0 {
                break;
            }
            scale *= fit;
            let next = redraw(scale);
            if next.kind != COLOR {
                break;
            }
            r = next;
        }
        r.left = ((target_w - r.w as CGFloat) / 2.0).round() as i32;
        r.top = ((cell_h - r.h as CGFloat) / 2.0).round() as i32;
        r
    }

    /// Rasterizes a base character with its combining marks, variation
    /// selectors or joined emoji as one shaped line.
    pub(crate) fn rasterize_cluster(&mut self, text: &str, style: usize, span: u32) -> Raster {
        let style = style & 3;
        let target_w = (span * self.metrics.cell_w) as CGFloat;
        let font = self.faces[style].font.clone();
        let Some(line) = line_for(text, &font) else {
            return Raster::empty();
        };
        // Text wider than its cells (a fallback font's wide accent) is shrunk to fit.
        let (w, _, _) = line_extent(&line);
        let first = if w > target_w + 0.5 {
            // SAFETY: plain getter on a valid font.
            let smaller = resized(&font, unsafe { font.size() } * target_w / w);
            match line_for(text, &smaller) {
                Some(l) => self.draw_line(&l, style, target_w),
                None => return Raster::empty(),
            }
        } else {
            self.draw_line(&line, style, target_w)
        };
        self.fit_color(first, target_w, |s| {
            // SAFETY: plain getter on a valid font.
            let smaller = resized(&font, unsafe { font.size() } * s);
            line_for(text, &smaller)
                .map(|l| self.draw_line(&l, style, target_w))
                .unwrap_or_else(Raster::empty)
        })
    }

    /// Draws a line centered in target_w on the cell's baseline. Drawn in
    /// color with a white fill; the raster keeps the color only if the run
    /// turned out to have its own (emoji).
    fn draw_line(&self, line: &CTLine, style: usize, target_w: CGFloat) -> Raster {
        let (w, asc, desc) = line_extent(line);
        let pen_x = ((target_w - w) / 2.0).round();
        let baseline = self.metrics.baseline as CGFloat;
        // SAFETY: a valid line.
        let ink = unsafe { line.bounds_with_options(CTLineBoundsOptions::UseGlyphPathBounds) };
        let ink = union(ink, em_box(w, asc, desc));
        let synthetic_bold = self.faces[style].synthetic_bold;
        let Some(canvas) =
            Canvas::around(ink, pen_x, baseline, self.pad(style, synthetic_bold, asc))
        else {
            return Raster::empty();
        };
        let Some(bitmap) = Bitmap::new(canvas.w, canvas.h, true) else {
            return Raster::empty();
        };
        if synthetic_bold {
            bitmap.synthetic_bold(self.bold_stroke());
        }
        let pos = canvas.pen(pen_x, baseline);
        CGContext::set_text_position(Some(&bitmap.ctx), pos.x, pos.y);
        // SAFETY: a valid line and context.
        unsafe { line.draw(&bitmap.ctx) };
        bitmap.finish(&canvas, true)
    }
}

/// The typographic box of a glyph or line: advance wide, from descent to ascent.
fn em_box(advance: CGFloat, ascent: CGFloat, descent: CGFloat) -> CGRect {
    CGRect {
        origin: CGPoint {
            x: 0.0,
            y: -descent,
        },
        size: CGSize {
            width: advance,
            height: ascent + descent,
        },
    }
}

fn union(a: CGRect, b: CGRect) -> CGRect {
    if a.size.width <= 0.0 || a.size.height <= 0.0 {
        return b;
    }
    let x0 = a.origin.x.min(b.origin.x);
    let y0 = a.origin.y.min(b.origin.y);
    let x1 = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    CGRect {
        origin: CGPoint { x: x0, y: y0 },
        size: CGSize {
            width: x1 - x0,
            height: y1 - y0,
        },
    }
}

/// Cell size and line positions from the regular face.
fn cell_metrics(font: &CTFont, size_px: CGFloat) -> CellMetrics {
    // SAFETY: plain getters on a valid font.
    let (asc, desc, leading, ul_pos, ul_thick, x_height) = unsafe {
        (
            font.ascent(),
            font.descent(),
            font.leading(),
            font.underline_position(),
            font.underline_thickness(),
            font.x_height(),
        )
    };
    let adv = glyph_for(font, 'M')
        .map(|g| advance(font, g))
        .filter(|a| *a > 0.0)
        .unwrap_or(size_px * 0.6);
    let cell_w = (adv.round() as u32).max(1);
    // A hair of tolerance so 15.0000001 is 15, not 16.
    let cell_h = ((asc + desc + leading.max(0.0) - 1e-3).ceil() as u32).max(1);
    let baseline = ((cell_h as CGFloat - (asc + desc)) / 2.0 + asc).round();
    // Fonts specify hairline underlines that vanish on Retina screens; keep
    // underlines and box lines at least 1/16 of the font size.
    let thickness = (ul_thick.round() as u32)
        .max((size_px / 16.0).round() as u32)
        .max(1);
    let underline_top = (baseline - ul_pos - thickness as CGFloat / 2.0)
        .round()
        .max(0.0) as u32;
    let strike_top = (baseline - x_height / 2.0 - thickness as CGFloat / 2.0)
        .round()
        .max(0.0) as u32;
    CellMetrics {
        cell_w,
        cell_h,
        baseline: baseline.max(0.0) as u32,
        underline_top,
        underline_thickness: thickness,
        strike_top,
        stroke: thickness,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_family_is_an_error() {
        assert!(!family_exists("No Such Font Family 12345"));
        assert!(matches!(
            Fonts::new("No Such Font Family 12345", 12.0),
            Err(RenderError::FontNotFound(_))
        ));
    }

    #[test]
    fn the_system_font_is_monospaced_with_real_bold_and_italic() {
        assert!(family_exists(SYSTEM_FAMILY));
        let f = Fonts::new(SYSTEM_FAMILY, 26.0).unwrap();
        let m = f.metrics();
        assert!(m.cell_w >= 12 && m.cell_w <= 20, "{m:?}");
        assert!(m.cell_h >= 26 && m.cell_h <= 40, "{m:?}");
        let (bold, italic) = (
            CTFontSymbolicTraits::TraitBold,
            CTFontSymbolicTraits::TraitItalic,
        );
        let styles = [CTFontSymbolicTraits::empty(), bold, italic, bold | italic];
        for (face, want) in f.faces.iter().zip(styles) {
            assert!(!face.synthetic_bold);
            let has = traits(&face.font);
            assert!(
                has.contains(CTFontSymbolicTraits::TraitMonoSpace),
                "{has:?}"
            );
            assert_eq!(has & (bold | italic), want);
        }
    }

    #[test]
    fn menlo_metrics_are_sane_and_scale() {
        let one = Fonts::new("Menlo", 13.0).unwrap().metrics();
        let two = Fonts::new("Menlo", 26.0).unwrap().metrics();
        assert!(one.cell_w >= 6 && one.cell_w <= 10, "{one:?}");
        assert!(one.cell_h >= 13 && one.cell_h <= 20, "{one:?}");
        assert!((two.cell_w as i32 - 2 * one.cell_w as i32).abs() <= 1);
        assert!((two.cell_h as i32 - 2 * one.cell_h as i32).abs() <= 2);
        for m in [one, two] {
            assert!(m.baseline < m.cell_h && m.baseline > m.cell_h / 2);
            assert!(m.underline_top >= m.baseline && m.underline_top < m.cell_h + 2);
            assert!(m.strike_top < m.baseline);
        }
    }

    #[test]
    fn ascii_glyph_sits_in_its_cell() {
        let mut f = Fonts::new("Menlo", 26.0).unwrap();
        let m = f.metrics();
        let r = f.rasterize('H', 0, 1);
        assert_eq!(r.kind, MASK);
        assert_eq!(r.data.len(), (r.w * r.h) as usize);
        // H stands on the baseline and stays within the cell horizontally.
        assert!(
            r.left >= 0 && r.left + r.w as i32 <= m.cell_w as i32,
            "{r:?} {m:?}"
        );
        assert!((r.top + r.h as i32 - m.baseline as i32).abs() <= 1);
        assert!(r.data.iter().filter(|&&a| a == 255).count() > 20);
        // A space has no ink.
        assert_eq!(f.rasterize(' ', 0, 1).kind, SOLID);
    }

    #[test]
    fn styles_differ() {
        let mut f = Fonts::new("Menlo", 26.0).unwrap();
        let plain = f.rasterize('a', 0, 1);
        let bold = f.rasterize('a', BOLD, 1);
        let italic = f.rasterize('a', ITALIC, 1);
        let sum = |r: &Raster| r.data.iter().map(|&a| a as u32).sum::<u32>();
        assert!(sum(&bold) > sum(&plain));
        assert_ne!(plain.data, italic.data);
    }

    #[test]
    fn a_fallback_font_is_kept_once_however_many_characters_use_it() {
        let mut f = Fonts::with_fallback("Menlo", 26.0);
        // Ideographs Menlo lacks, which one system font has.
        let text = "\u{4e16}\u{754c}\u{4e2d}\u{6587}\u{5b57}\u{6f22}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}";
        let mut used = Vec::new();
        for ch in text.chars() {
            let (font, _, fallback) = f.font_for(ch, 0).expect("a font with the ideograph");
            assert!(fallback, "{ch} is not in Menlo");
            used.push(font);
        }
        assert!(!f.fallbacks.is_empty() && f.fallbacks.len() <= 2, "{} fonts", f.fallbacks.len());
        // Each character got one of the fonts kept, the object itself.
        for font in &used {
            let kept = f.fallbacks.iter().any(|k| std::ptr::eq::<CTFont>(&**k, &**font));
            assert!(kept, "a font object of its own");
        }
        // And asking again makes nothing new.
        let before = f.fallbacks.len();
        for ch in text.chars() {
            f.font_for(ch, 0);
        }
        assert_eq!(f.fallbacks.len(), before);
    }

    #[test]
    fn fallback_and_emoji() {
        let mut f = Fonts::new("Menlo", 26.0).unwrap();
        let m = f.metrics();
        // CJK comes from a fallback font, centered over two cells.
        let cjk = f.rasterize('漢', 0, 2);
        assert_eq!(cjk.kind, MASK);
        assert!(
            cjk.left >= 0 && cjk.left + cjk.w as i32 <= 2 * m.cell_w as i32 + 1,
            "{cjk:?}"
        );
        let emoji = f.rasterize('😀', 0, 2);
        assert_eq!(emoji.kind, COLOR);
        assert_eq!(emoji.data.len(), (emoji.w * emoji.h * 4) as usize);
        assert!(
            emoji.w <= 2 * m.cell_w + 1 && emoji.h <= m.cell_h + 1,
            "{emoji:?} {m:?}"
        );
    }

    #[test]
    fn absurd_sizes_are_clamped() {
        // A config typo must not make every built-in glyph a gigabyte.
        let huge = Fonts::with_fallback("Menlo", 1.0e9).metrics();
        assert!(huge.cell_w <= 1024 && huge.cell_h <= 2048, "{huge:?}");
        for size in [0.0, -5.0, f32::NAN, f32::INFINITY] {
            let m = Fonts::with_fallback("Menlo", size).metrics();
            assert!(
                m.cell_w >= 1 && m.cell_h >= 1 && m.cell_h <= 2048,
                "{size}: {m:?}"
            );
        }
    }

    #[test]
    fn clusters_shape_as_one_image() {
        let mut f = Fonts::new("Menlo", 26.0).unwrap();
        let m = f.metrics();
        let accent = f.rasterize_cluster("e\u{301}", 0, 1);
        assert_eq!(accent.kind, MASK);
        let plain = f.rasterize('e', 0, 1);
        // The accent adds ink above the letter.
        assert!(accent.top < plain.top);
        let family = f.rasterize_cluster("👨\u{200d}👩\u{200d}👧", 0, 2);
        assert_eq!(family.kind, COLOR);
        assert!(
            family.w <= 2 * m.cell_w + 1 && family.h <= m.cell_h + 1,
            "{family:?}"
        );
        let flag = f.rasterize_cluster("🇯🇵", 0, 2);
        assert_eq!(flag.kind, COLOR);
    }
}
