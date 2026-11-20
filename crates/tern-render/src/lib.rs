//! Draws terminal panes with Metal, with glyphs rasterized by Core Text.
//!
//! Input is plain data: a [Frame] per pane and the rectangles to put them in.
//! The renderer never opens a window; the app hands it a CAMetalLayer, and
//! [Renderer::render_offscreen] draws the same scene into memory for tests
//! and screenshots.
//!
//! All methods must be called from one thread (the app's main thread).

mod color;
mod font;
mod glyphs;
mod gpu;
mod instances;
mod packer;
mod png;
mod sprites;

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_graphics::{kCGColorSpaceSRGB, CGColorSpace};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder,
    MTLCommandQueue, MTLDevice, MTLDrawable, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLResourceOptions,
    MTLScissorRect, MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureUsage,
    MTLViewport,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use tern_types::{Frame, PaneId, Point, Rect, Rgb};

use crate::glyphs::ScaleState;
use crate::gpu::{Buffer, CommandBuffer, Gpu, Texture};
use crate::instances::{Inst, Layout, PaneCache};

/// The font family and size. Bold, italic and bold-italic come from the same
/// family; a missing face is synthesized (slanted or stroked).
#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    /// The family's name; empty for the system's monospaced font (SF Mono).
    pub family: String,
    /// Points; times the backing scale it is kept within 1 to 1024 pixels.
    pub size_pt: f32,
}

/// Cell geometry at one backing scale, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    pub cell_w: u32,
    pub cell_h: u32,
    /// Baseline, measured down from the top of the cell.
    pub baseline: u32,
    /// Top row of a single underline, from the top of the cell.
    pub underline_top: u32,
    pub underline_thickness: u32,
    /// Top row of the strikeout line.
    pub strike_top: u32,
    /// Thickness of light box-drawing lines and of beam, underline and hollow cursors.
    pub stroke: u32,
}

/// How [Renderer::draw] presents the drawable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Present {
    /// Schedule the present with the command buffer. The normal case.
    Async,
    /// Commit, wait until scheduled, then present inside the current Core
    /// Animation transaction, so the new frame appears together with a window
    /// resize. Use with layer.presentsWithTransaction set.
    WithTransaction,
}

/// Everything to draw in one window, in device pixels.
pub struct Scene<'a> {
    pub width: u32,
    pub height: u32,
    /// Backing scale factor (1.0 or 2.0 on current Macs); selects the glyph cache.
    pub scale: f32,
    /// Color of whatever no pane covers.
    pub clear: Rgb,
    pub panes: &'a [PaneDraw<'a>],
    /// Drawn over all panes, in order.
    pub quads: &'a [Quad],
}

/// One pane of the scene.
pub struct PaneDraw<'a> {
    pub id: PaneId,
    /// The pane's whole area; drawing is clipped to it and cleared to frame.bg.
    pub rect: Rect,
    /// Top-left corner of the cell grid (rect plus padding).
    pub origin: Point,
    pub frame: &'a Frame,
    /// Unfocused panes show a hollow cursor.
    pub focused: bool,
    /// Blink phase; false hides the cursor.
    pub cursor_on: bool,
    /// Alpha of a veil in the pane's background color (frame.bg) over the
    /// pane, which fades what it shows toward that background; 0 for none.
    pub dim: f32,
}

/// A blended solid rectangle: dividers, drop-zone highlights, borders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub rect: Rect,
    pub color: Rgb,
    pub alpha: f32,
}

/// Pixels read back from [Renderer::render_offscreen]: RGBA, row by row from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    /// The RGBA value at (x, y).
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// The image as a PNG file (uncompressed, so large but fast to write).
    /// Panics if rgba does not hold width * height * 4 bytes.
    pub fn png(&self) -> Vec<u8> {
        png::encode(self.width, self.height, &self.rgba)
    }

    /// Writes the image as a PNG file. PNG has no empty images, so a zero
    /// width or height is an InvalidInput error, as is a pixel buffer that
    /// does not hold width * height * 4 bytes.
    pub fn write_png(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let want = self.width as usize * self.height as usize * 4;
        if self.width == 0 || self.height == 0 || self.rgba.len() != want {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "cannot write a {}x{} PNG from {} bytes",
                    self.width,
                    self.height,
                    self.rgba.len()
                ),
            ));
        }
        std::fs::write(path, self.png())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The Mac has no Metal device.
    NoDevice,
    /// The shader failed to compile or link; holds Metal's message.
    Shader(String),
    /// No installed font family has this name.
    FontNotFound(String),
    /// A GPU resource could not be created.
    Gpu(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::NoDevice => write!(f, "no Metal device"),
            RenderError::Shader(m) => write!(f, "shader error: {m}"),
            RenderError::FontNotFound(name) => write!(f, "font family not found: {name}"),
            RenderError::Gpu(m) => write!(f, "GPU error: {m}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// What the last [Renderer::draw] or [Renderer::render_offscreen] cost on the CPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Updating pane instances (including rasterizing new glyphs) and uploading them.
    pub prepare: Duration,
    /// Encoding the GPU commands.
    pub encode: Duration,
    /// Grid rows whose instances were rebuilt.
    pub rows_rebuilt: usize,
    /// Instances drawn.
    pub instances: usize,
}

/// How long the parts of [Renderer::new] took; startup time is a headline number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartupTimes {
    /// MTLCreateSystemDefaultDevice and the command queue.
    pub device: Duration,
    /// Compiling the shader source and building the pipeline.
    pub pipeline: Duration,
    /// Finding the font family (the first Core Text call also loads Core
    /// Text's font registry). Runs alongside the device and pipeline.
    pub font: Duration,
}

/// Glyph caches kept for different (size, scale) pairs at once; more are
/// evicted, least recently used first.
const MAX_SCALES: usize = 8;

/// Metal limits textures to 16384 pixels per side.
const MAX_TARGET: u32 = 16384;

#[repr(C)]
#[derive(Clone, Copy)]
struct Uniforms {
    viewport: [f32; 2],
    origin: [f32; 2],
}

#[derive(Default)]
struct PaneState {
    cache: PaneCache,
    buffer: Option<Retained<Buffer>>,
    count: usize,
}

/// Ranges into the frame's dynamic instances for one pane.
#[derive(Clone, Copy, Default)]
struct PaneRanges {
    fill: (usize, usize),
    overlay: (usize, usize),
}

/// One per app: the Metal device and pipeline, fonts, glyph atlases, and the
/// cached instance data of every pane.
pub struct Renderer {
    gpu: Gpu,
    font: FontConfig,
    scales: Vec<ScaleState>,
    panes: HashMap<PaneId, PaneState>,
    pool: gpu::BufferPool,
    /// The newest committed command buffer: anything released now may be in
    /// use until it completes.
    last_commit: Option<Retained<CommandBuffer>>,
    frame_no: u64,
    dynamic: Vec<Inst>,
    ranges: Vec<PaneRanges>,
    quads: (usize, usize),
    offscreen: Option<Offscreen>,
    stats: FrameStats,
    startup: StartupTimes,
}

struct Offscreen {
    texture: Retained<Texture>,
    readback: Retained<Buffer>,
    width: u32,
    height: u32,
}

/// The slow half of creating a [Renderer]: the Metal device, the compiled
/// pipeline and the font check. It can be made on any thread and sent to
/// the one the renderer will live on, so an app can do this work while its
/// UI starts up.
pub struct Prepared {
    gpu: Gpu,
    font: FontConfig,
    startup: StartupTimes,
}

/// Called on a Metal thread when a drawable from [Renderer::draw_notify]
/// has been presented (or dropped: its presentedTime is then 0).
pub type PresentedHandler = RcBlock<dyn Fn(NonNull<ProtocolObject<dyn MTLDrawable>>)>;

impl Renderer {
    /// Creates the device, compiles the shaders and checks that the font family exists.
    pub fn new(font: &FontConfig) -> Result<Renderer, RenderError> {
        Ok(Renderer::from_prepared(Renderer::prepare(font)?))
    }

    /// The part of [Renderer::new] that may run on another thread.
    pub fn prepare(font: &FontConfig) -> Result<Prepared, RenderError> {
        // The first Metal call and the first Core Text call each load a
        // framework's state (tens of milliseconds together); they are
        // independent, so overlap them.
        let (found, font_time, gpu) = std::thread::scope(|s| {
            let lookup = s.spawn(|| {
                let start = Instant::now();
                (font::family_exists(&font.family), start.elapsed())
            });
            let gpu = Gpu::new();
            // A panic in the lookup thread is a bug; surface it here.
            let (found, time) = lookup
                .join()
                .unwrap_or_else(|e| std::panic::resume_unwind(e));
            (found, time, gpu)
        });
        if !found {
            return Err(RenderError::FontNotFound(font.family.clone()));
        }
        let (gpu, device, pipeline) = gpu?;
        Ok(Prepared {
            gpu,
            font: font.clone(),
            startup: StartupTimes {
                device,
                pipeline,
                font: font_time,
            },
        })
    }

    /// Finishes a renderer from [Renderer::prepare]'s result. Cheap.
    pub fn from_prepared(prepared: Prepared) -> Renderer {
        let Prepared { gpu, font, startup } = prepared;
        Renderer {
            gpu,
            font,
            scales: Vec::new(),
            panes: HashMap::new(),
            pool: gpu::BufferPool::default(),
            last_commit: None,
            frame_no: 0,
            dynamic: Vec::new(),
            ranges: Vec::new(),
            quads: (0, 0),
            offscreen: None,
            stats: FrameStats::default(),
            startup,
        }
    }

    /// Prepares a layer for this renderer: device, BGRA8Unorm, framebuffer
    /// only, opaque, sRGB.
    pub fn configure_layer(&self, layer: &CAMetalLayer) {
        layer.setDevice(Some(&self.gpu.device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);
        layer.setOpaque(true);
        // SAFETY: reading a framework constant.
        let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        layer.setColorspace(srgb.as_deref());
    }

    /// Cell size in device pixels at a backing scale. Loads the font at that
    /// scale the first time.
    pub fn metrics(&mut self, scale: f32) -> CellMetrics {
        self.metrics_at(self.font.size_pt, scale)
    }

    /// Cell size in device pixels at an explicit point size and backing scale.
    pub fn metrics_at(&mut self, size_pt: f32, scale: f32) -> CellMetrics {
        let i = self.scale_index_at(size_pt, scale);
        self.scales[i].metrics()
    }

    /// Changes the font family or size. Drops every glyph cache; panes are
    /// rebuilt on their next draw. On error the old font stays.
    pub fn set_font(&mut self, font: &FontConfig) -> Result<(), RenderError> {
        if !font::family_exists(&font.family) {
            return Err(RenderError::FontNotFound(font.family.clone()));
        }
        self.font = font.clone();
        self.scales.clear();
        Ok(())
    }

    pub fn font(&self) -> &FontConfig {
        &self.font
    }

    /// Forgets a pane's cached instances (call when a pane closes).
    pub fn forget_pane(&mut self, id: PaneId) {
        if let Some(state) = self.panes.remove(&id)
            && let Some(buffer) = state.buffer {
                self.pool.release(buffer, self.last_commit.clone());
            }
    }

    /// How long creating this renderer took, by part.
    pub fn startup_times(&self) -> StartupTimes {
        self.startup
    }

    /// CPU cost of the last frame.
    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// Draws the scene into the layer's next drawable and presents it.
    /// Returns false when no drawable was available (draw again later). A
    /// scene with no area draws nothing and returns true.
    pub fn draw(&mut self, layer: &CAMetalLayer, scene: &Scene, present: Present) -> bool {
        self.draw_notify(layer, scene, present, None)
    }

    /// [Renderer::draw], also adding a presented handler to the drawable.
    /// The handler is retained, not copied, so one can serve every frame.
    pub fn draw_notify(
        &mut self,
        layer: &CAMetalLayer,
        scene: &Scene,
        present: Present,
        presented: Option<&PresentedHandler>,
    ) -> bool {
        self.draw_sized(layer, scene, self.font.size_pt, present, presented)
    }

    /// [Renderer::draw_notify] using an explicit font size in points.
    pub fn draw_sized(
        &mut self,
        layer: &CAMetalLayer,
        scene: &Scene,
        size_pt: f32,
        present: Present,
        presented: Option<&PresentedHandler>,
    ) -> bool {
        if scene.width == 0 || scene.height == 0 {
            return true;
        }
        let si = self.prepare_scene(scene, size_pt);
        let Some(drawable) = layer.nextDrawable() else {
            return false;
        };
        let Some(cb) = self.gpu.queue.commandBuffer() else {
            return false;
        };
        let target = drawable.texture();
        let dynamic = self.encode(&cb, &target, scene, si);
        if let Some(handler) = presented {
            let drawable: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*drawable);
            // SAFETY: the handler has the signature Metal calls it with, and
            // Metal keeps its own reference to the block.
            unsafe { drawable.addPresentedHandler(RcBlock::as_ptr(handler)) };
        }
        match present {
            Present::Async => {
                cb.presentDrawable(ProtocolObject::from_ref(&*drawable));
                cb.commit();
            }
            Present::WithTransaction => {
                cb.commit();
                cb.waitUntilScheduled();
                drawable.present();
            }
        }
        self.committed(cb, dynamic);
        true
    }

    /// Draws the scene into memory and reads it back. Sizes above 16384 are clipped.
    pub fn render_offscreen(&mut self, scene: &Scene) -> Result<Image, RenderError> {
        self.render_offscreen_sized(scene, self.font.size_pt)
    }

    /// [Renderer::render_offscreen] using an explicit font size in points.
    pub fn render_offscreen_sized(
        &mut self,
        scene: &Scene,
        size_pt: f32,
    ) -> Result<Image, RenderError> {
        let (w, h) = (scene.width.min(MAX_TARGET), scene.height.min(MAX_TARGET));
        if w == 0 || h == 0 {
            return Ok(Image {
                width: w,
                height: h,
                rgba: Vec::new(),
            });
        }
        let si = self.prepare_scene(scene, size_pt);
        let (texture, readback) = self.offscreen_target(w, h)?;
        let cb = self
            .gpu
            .queue
            .commandBuffer()
            .ok_or_else(|| RenderError::Gpu("no command buffer".into()))?;
        let dynamic = self.encode(&cb, &texture, scene, si);
        let blit = cb
            .blitCommandEncoder()
            .ok_or_else(|| RenderError::Gpu("no blit encoder".into()))?;
        // SAFETY: the readback buffer holds w * h * 4 bytes; the region is the whole texture.
        unsafe {
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                &texture,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize { width: w as usize, height: h as usize, depth: 1 },
                &readback,
                0,
                w as usize * 4,
                w as usize * h as usize * 4,
            );
        }
        blit.endEncoding();
        cb.commit();
        cb.waitUntilCompleted();
        self.committed(cb, dynamic);
        let len = w as usize * h as usize * 4;
        // SAFETY: the GPU has finished writing; the buffer is shared memory of at least len bytes.
        let bgra =
            unsafe { std::slice::from_raw_parts(readback.contents().as_ptr() as *const u8, len) };
        let mut rgba = Vec::with_capacity(len);
        for p in bgra.chunks_exact(4) {
            rgba.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        Ok(Image {
            width: w,
            height: h,
            rgba,
        })
    }

    /// The offscreen texture and its readback buffer, reused while the size stays.
    fn offscreen_target(
        &mut self,
        w: u32,
        h: u32,
    ) -> Result<(Retained<Texture>, Retained<Buffer>), RenderError> {
        if !self
            .offscreen
            .as_ref()
            .is_some_and(|o| o.width == w && o.height == h)
        {
            let device = &self.gpu.device;
            let texture = gpu::new_texture(
                device,
                MTLPixelFormat::BGRA8Unorm,
                w,
                h,
                MTLTextureUsage::RenderTarget,
                MTLStorageMode::Private,
            )
            .ok_or_else(|| RenderError::Gpu("cannot allocate offscreen texture".into()))?;
            let readback = device
                .newBufferWithLength_options(
                    w as usize * h as usize * 4,
                    MTLResourceOptions::StorageModeShared,
                )
                .ok_or_else(|| RenderError::Gpu("cannot allocate readback buffer".into()))?;
            self.offscreen = Some(Offscreen {
                texture,
                readback,
                width: w,
                height: h,
            });
        }
        match &self.offscreen {
            Some(o) => Ok((o.texture.clone(), o.readback.clone())),
            None => Err(RenderError::Gpu("offscreen target missing".into())),
        }
    }

    /// The glyph cache for a `(size_pt, scale)` pair, created on first use.
    fn scale_index_at(&mut self, size_pt: f32, scale: f32) -> usize {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let size_pt = if size_pt.is_finite() && size_pt > 0.0 {
            size_pt
        } else {
            self.font.size_pt
        };
        self.frame_no += 1;
        let i = match self
            .scales
            .iter()
            .position(|s| s.scale == scale && (s.size_pt - size_pt).abs() < 0.01)
        {
            Some(i) => i,
            None => {
                if self.scales.len() >= MAX_SCALES
                    && let Some(lru) =
                        (0..self.scales.len()).min_by_key(|&i| self.scales[i].last_used)
                    {
                        self.scales.swap_remove(lru);
                    }
                let st = ScaleState::new(&self.gpu.device, &self.font.family, size_pt, scale);
                self.scales.push(st);
                self.scales.len() - 1
            }
        };
        self.scales[i].last_used = self.frame_no;
        i
    }

    /// Brings every pane's instances up to date and builds this frame's
    /// dynamic instances (pane fills, cursors, veils, quads).
    fn prepare_scene(&mut self, scene: &Scene, size_pt: f32) -> usize {
        let start = Instant::now();
        let si = self.scale_index_at(size_pt, scene.scale);
        self.scales[si].may_reset = true;
        let mut rows_rebuilt = 0;
        // A glyph cache that fills up during the pass starts over under a new
        // generation, invalidating instances built earlier in the pass; the
        // second pass rebuilds them against the fresh atlas. It may not start
        // over again (see ScaleState::may_reset), so two passes are enough.
        for _ in 0..2 {
            let st = &mut self.scales[si];
            let generation = st.generation;
            let m = st.metrics();
            for pane in scene.panes {
                let layout = layout_of(pane.frame, &m, st.generation);
                let ps = self.panes.entry(pane.id).or_default();
                let rebuilt = ps.cache.update(pane.frame, layout, st);
                rows_rebuilt += rebuilt;
                if rebuilt > 0 || (ps.buffer.is_none() && ps.cache.len() > 0) {
                    upload(&mut self.pool, &self.gpu.device, ps, &self.last_commit);
                }
            }

            self.dynamic.clear();
            self.ranges.clear();
            for pane in scene.panes {
                let layout = layout_of(pane.frame, &m, st.generation);
                let fill_at = self.dynamic.len();
                self.dynamic
                    .extend(instances::rect_quad(pane.rect, pane.frame.bg, 1.0));
                let overlay_at = self.dynamic.len();
                instances::overlay(&mut self.dynamic, pane, &layout, m.stroke, st);
                self.ranges.push(PaneRanges {
                    fill: (fill_at, overlay_at - fill_at),
                    overlay: (overlay_at, self.dynamic.len() - overlay_at),
                });
            }
            let quads_at = self.dynamic.len();
            for q in scene.quads {
                self.dynamic
                    .extend(instances::rect_quad(q.rect, q.color, q.alpha));
            }
            self.quads = (quads_at, self.dynamic.len() - quads_at);
            if self.scales[si].generation == generation {
                break;
            }
        }
        self.stats.prepare = start.elapsed();
        self.stats.rows_rebuilt = rows_rebuilt;
        si
    }

    /// Encodes the scene into cb, drawing into target. Returns the buffer
    /// holding this frame's dynamic instances, to hand back to the pool once
    /// the command buffer is committed.
    fn encode(
        &mut self,
        cb: &CommandBuffer,
        target: &Texture,
        scene: &Scene,
        si: usize,
    ) -> Option<Retained<Buffer>> {
        let start = Instant::now();
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: index 0 always exists.
        let att = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        att.setTexture(Some(target));
        att.setLoadAction(MTLLoadAction::Clear);
        att.setStoreAction(MTLStoreAction::Store);
        let [r, g, b] = color::unit(scene.clear);
        att.setClearColor(MTLClearColor {
            red: r,
            green: g,
            blue: b,
            alpha: 1.0,
        });
        let enc = cb.renderCommandEncoderWithDescriptor(&pass)?;

        // Clip everything to both the scene and the texture.
        let bound_w = scene.width.min(target.width() as u32) as i32;
        let bound_h = scene.height.min(target.height() as u32) as i32;
        let dynamic = if self.dynamic.is_empty() {
            None
        } else {
            let bytes = std::mem::size_of_val(self.dynamic.as_slice());
            let buffer = self.pool.acquire(&self.gpu.device, bytes);
            if let Some(buffer) = &buffer {
                gpu::fill(buffer, &self.dynamic);
            }
            buffer
        };

        enc.setRenderPipelineState(&self.gpu.pipeline);
        enc.setViewport(MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: scene.width as f64,
            height: scene.height as f64,
            znear: 0.0,
            zfar: 1.0,
        });
        let st = &self.scales[si];
        // SAFETY: binding live textures to the indices the shader declares.
        unsafe {
            enc.setFragmentTexture_atIndex(st.mask.as_ref().map(|a| &*a.texture), 0);
            enc.setFragmentTexture_atIndex(st.color.as_ref().map(|a| &*a.texture), 1);
        }
        let viewport = [scene.width as f32, scene.height as f32];
        let mut drawn = 0;
        let draw = |buffer: &Buffer, first: usize, count: usize, origin: Point| {
            if count == 0 {
                return;
            }
            let u = Uniforms {
                viewport,
                origin: [origin.x as f32, origin.y as f32],
            };
            // SAFETY: the buffer holds first + count instances, the uniforms
            // match the shader's struct, and both are copied or retained by
            // the encoder.
            unsafe {
                enc.setVertexBuffer_offset_atIndex(
                    Some(buffer),
                    first * std::mem::size_of::<Inst>(),
                    0,
                );
                enc.setVertexBytes_length_atIndex(
                    NonNull::from(&u).cast(),
                    std::mem::size_of::<Uniforms>(),
                    1,
                );
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::TriangleStrip,
                    0,
                    4,
                    count,
                );
            }
        };
        let zero = Point { x: 0, y: 0 };
        for (pane, ranges) in scene.panes.iter().zip(&self.ranges) {
            let Some(clip) = clip(pane.rect, bound_w, bound_h) else {
                continue;
            };
            enc.setScissorRect(clip);
            if let Some(dynamic) = &dynamic {
                draw(dynamic, ranges.fill.0, ranges.fill.1, zero);
            }
            if let Some(ps) = self.panes.get(&pane.id)
                && let Some(buffer) = &ps.buffer {
                    draw(buffer, 0, ps.count, pane.origin);
                    drawn += ps.count;
                }
            if let Some(dynamic) = &dynamic {
                draw(dynamic, ranges.overlay.0, ranges.overlay.1, zero);
            }
        }
        if let (Some(dynamic), Some(full)) = (
            &dynamic,
            clip(Rect::new(0, 0, bound_w, bound_h), bound_w, bound_h),
        ) {
            enc.setScissorRect(full);
            draw(dynamic, self.quads.0, self.quads.1, zero);
        }
        enc.endEncoding();
        self.stats.encode = start.elapsed();
        self.stats.instances = drawn + self.dynamic.len();
        dynamic
    }

    fn committed(&mut self, cb: Retained<CommandBuffer>, dynamic: Option<Retained<Buffer>>) {
        if let Some(buffer) = dynamic {
            self.pool.release(buffer, Some(cb.clone()));
        }
        self.last_commit = Some(cb);
    }
}

/// What a pane's cached instances depend on besides its cells.
fn layout_of(frame: &Frame, m: &CellMetrics, generation: u64) -> Layout {
    Layout {
        cols: frame.cols,
        rows: frame.rows,
        cell_w: m.cell_w,
        cell_h: m.cell_h,
        bg: frame.bg,
        generation,
    }
}

/// Copies a pane's instances into a fresh buffer and returns the old one to
/// the pool.
fn upload(
    pool: &mut gpu::BufferPool,
    device: &gpu::Device,
    ps: &mut PaneState,
    last_commit: &Option<Retained<CommandBuffer>>,
) {
    if let Some(old) = ps.buffer.take() {
        pool.release(old, last_commit.clone());
    }
    ps.count = 0;
    let len = ps.cache.len();
    if len == 0 {
        return;
    }
    // Without a buffer the pane shows only its background this frame and the
    // upload is retried on the next one.
    if let Some(buffer) = pool.acquire(device, len * std::mem::size_of::<Inst>()) {
        gpu::fill_with(&buffer, len, |out| ps.cache.write(out));
        ps.buffer = Some(buffer);
        ps.count = len;
    }
}

/// The part of r inside [0, w) x [0, h) as a scissor rectangle, or None if
/// empty. Computed in i64 so rectangles near the ends of i32 cannot overflow.
fn clip(r: Rect, w: i32, h: i32) -> Option<MTLScissorRect> {
    let (w, h) = (w as i64, h as i64);
    let x0 = (r.x as i64).clamp(0, w);
    let y0 = (r.y as i64).clamp(0, h);
    let x1 = (r.x as i64 + r.w as i64).clamp(0, w);
    let y1 = (r.y as i64 + r.h as i64).clamp(0, h);
    (x1 > x0 && y1 > y0).then(|| MTLScissorRect {
        x: x0 as usize,
        y: y0 as usize,
        width: (x1 - x0) as usize,
        height: (y1 - y0) as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_to_bounds() {
        let c = clip(Rect::new(-10, 5, 50, 100), 30, 40).unwrap();
        assert_eq!((c.x, c.y, c.width, c.height), (0, 5, 30, 35));
        assert!(clip(Rect::new(40, 0, 10, 10), 30, 40).is_none());
        assert!(clip(Rect::new(0, 0, 0, 10), 30, 40).is_none());
    }

    #[test]
    fn clip_survives_extreme_rects() {
        let c = clip(Rect::new(i32::MAX - 5, 0, i32::MAX, 10), 30, 40);
        assert!(c.is_none());
        let c = clip(Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX), 30, 40);
        assert!(c.is_none());
        let c = clip(Rect::new(-10, -10, i32::MAX, i32::MAX), 30, 40).unwrap();
        assert_eq!((c.x, c.y, c.width, c.height), (0, 0, 30, 40));
    }

    #[test]
    fn write_png_rejects_images_png_cannot_hold() {
        let dir = std::env::temp_dir();
        let empty = Image {
            width: 0,
            height: 4,
            rgba: Vec::new(),
        };
        let err = empty
            .write_png(dir.join("tern-render-empty.png"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let short = Image {
            width: 2,
            height: 2,
            rgba: vec![0; 4],
        };
        let err = short
            .write_png(dir.join("tern-render-short.png"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn image_pixel_and_png() {
        let img = Image {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        assert_eq!(img.pixel(1, 0), [5, 6, 7, 8]);
        assert_eq!(png::tests::decode(&img.png()), (2, 1, img.rgba.clone()));
    }
}
