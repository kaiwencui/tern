//! Metal objects: device, pipeline, atlas textures and a pool of instance buffers.

use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLOrigin, MTLPixelFormat, MTLRegion,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLResourceOptions, MTLSize,
    MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};

use crate::packer::Packer;
use crate::RenderError;

pub(crate) type Device = ProtocolObject<dyn MTLDevice>;
pub(crate) type Texture = ProtocolObject<dyn MTLTexture>;
pub(crate) type Buffer = ProtocolObject<dyn MTLBuffer>;
pub(crate) type CommandBuffer = ProtocolObject<dyn MTLCommandBuffer>;

/// One pipeline draws everything: solid quads, mask glyphs tinted by the
/// instance color, and color glyphs. Positions are whole device pixels and
/// glyphs are drawn at their rasterized size, so atlases are read texel for
/// texel without filtering. Colors are blended as stored (non-linear sRGB),
/// with premultiplied alpha.
const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Inst {
    short2 pos;
    ushort2 size;
    ushort2 uv;
    uint color;
    uint kind;
};

struct Uniforms {
    float2 viewport;
    float2 origin;
};

struct VOut {
    float4 position [[position]];
    float2 uv;
    float4 color [[flat]];
    uint kind [[flat]];
};

vertex VOut vs(uint vid [[vertex_id]], uint iid [[instance_id]],
               const device Inst *insts [[buffer(0)]],
               constant Uniforms &u [[buffer(1)]]) {
    Inst i = insts[iid];
    float2 corner = float2(vid & 1, vid >> 1);
    float2 size = float2(i.size);
    float2 p = u.origin + float2(i.pos) + corner * size;
    VOut o;
    o.position = float4(p.x / u.viewport.x * 2.0 - 1.0, 1.0 - p.y / u.viewport.y * 2.0, 0.0, 1.0);
    o.uv = float2(i.uv) + corner * size;
    float4 c = unpack_unorm4x8_to_float(i.color);
    o.color = float4(c.rgb * c.a, c.a);
    o.kind = i.kind;
    return o;
}

fragment float4 fs(VOut in [[stage_in]],
                   texture2d<float, access::read> mask [[texture(0)]],
                   texture2d<float, access::read> color [[texture(1)]]) {
    uint2 t = uint2(in.uv);
    if (in.kind == 1) {
        return in.color * mask.read(t).r;
    }
    if (in.kind == 2) {
        return color.read(t) * in.color.a;
    }
    return in.color;
}
"#;

pub(crate) struct Gpu {
    pub device: Retained<Device>,
    pub queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pub pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
}

impl Gpu {
    /// The GPU objects, with the time taken by the device and by the pipeline.
    pub(crate) fn new() -> Result<(Gpu, Duration, Duration), RenderError> {
        let start = Instant::now();
        let device = MTLCreateSystemDefaultDevice().ok_or(RenderError::NoDevice)?;
        let queue = device
            .newCommandQueue()
            .ok_or_else(|| RenderError::Gpu("no command queue".into()))?;
        let device_time = start.elapsed();
        let start = Instant::now();
        let library = device
            .newLibraryWithSource_options_error(&NSString::from_str(SHADER), None)
            .map_err(|e| RenderError::Shader(e.localizedDescription().to_string()))?;
        let function = |name: &str| {
            library
                .newFunctionWithName(&NSString::from_str(name))
                .ok_or_else(|| RenderError::Shader(format!("shader function {name} missing")))
        };
        let desc = MTLRenderPipelineDescriptor::new();
        let (vs, fs) = (function("vs")?, function("fs")?);
        desc.setVertexFunction(Some(&vs));
        desc.setFragmentFunction(Some(&fs));
        // SAFETY: index 0 always exists.
        let att = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        att.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        att.setBlendingEnabled(true);
        att.setSourceRGBBlendFactor(MTLBlendFactor::One);
        att.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        att.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        att.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&desc)
            .map_err(|e| RenderError::Shader(e.localizedDescription().to_string()))?;
        Ok((
            Gpu {
                device,
                queue,
                pipeline,
            },
            device_time,
            start.elapsed(),
        ))
    }
}

pub(crate) fn new_texture(
    device: &Device,
    format: MTLPixelFormat,
    w: u32,
    h: u32,
    usage: MTLTextureUsage,
    storage: MTLStorageMode,
) -> Option<Retained<Texture>> {
    // SAFETY: plain descriptor constructor with valid sizes.
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            format, w as usize, h as usize, false,
        )
    };
    desc.setUsage(usage);
    desc.setStorageMode(storage);
    device.newTextureWithDescriptor(&desc)
}

fn region(x: u32, y: u32, w: u32, h: u32) -> MTLRegion {
    MTLRegion {
        origin: MTLOrigin {
            x: x as usize,
            y: y as usize,
            z: 0,
        },
        size: MTLSize {
            width: w as usize,
            height: h as usize,
            depth: 1,
        },
    }
}

/// Largest atlas side. 8192 x 8192 holds well over a hundred thousand glyphs
/// at Retina sizes; beyond that the cache starts over rather than failing.
const ATLAS_MAX: u32 = 8192;

/// A texture the CPU writes glyph bitmaps into, with its shelf packer.
pub(crate) struct Atlas {
    pub texture: Retained<Texture>,
    packer: Packer,
    format: MTLPixelFormat,
    bytes_per_pixel: u32,
}

impl Atlas {
    /// None when the GPU cannot allocate the texture.
    pub(crate) fn new(device: &Device, format: MTLPixelFormat, size: u32) -> Option<Atlas> {
        let bytes_per_pixel = if format == MTLPixelFormat::R8Unorm {
            1
        } else {
            4
        };
        let texture = new_texture(
            device,
            format,
            size,
            size,
            MTLTextureUsage::ShaderRead,
            MTLStorageMode::Shared,
        )?;
        Some(Atlas {
            texture,
            packer: Packer::new(size, size, ATLAS_MAX),
            format,
            bytes_per_pixel,
        })
    }

    /// Stores a w x h bitmap and returns where it went, growing the texture
    /// when it is full. None when the atlas is at its maximum size and full
    /// (the caller clears it and starts over) or the GPU is out of memory.
    pub(crate) fn insert(
        &mut self,
        device: &Device,
        w: u32,
        h: u32,
        data: &[u8],
    ) -> Option<(u16, u16)> {
        debug_assert_eq!(data.len(), (w * h * self.bytes_per_pixel) as usize);
        let at = loop {
            if let Some(at) = self.packer.alloc(w, h) {
                break at;
            }
            let (nw, nh) = self.packer.grown_size()?;
            self.grow(device, nw, nh)?;
        };
        // SAFETY: the region lies inside the texture and data holds h rows of
        // w pixels. The region is fresh, so no command in flight reads it.
        unsafe {
            self.texture
                .replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    region(at.0, at.1, w, h),
                    0,
                    NonNull::new_unchecked(data.as_ptr() as *mut _),
                    (w * self.bytes_per_pixel) as usize,
                );
        }
        Some((at.0 as u16, at.1 as u16))
    }

    /// Moves the contents into a larger texture; coordinates stay the same.
    fn grow(&mut self, device: &Device, w: u32, h: u32) -> Option<()> {
        let (ow, oh) = self.packer.size();
        let texture = new_texture(
            device,
            self.format,
            w,
            h,
            MTLTextureUsage::ShaderRead,
            MTLStorageMode::Shared,
        )?;
        let row = (ow * self.bytes_per_pixel) as usize;
        let mut pixels = vec![0u8; row * oh as usize];
        // SAFETY: both regions are inside their textures; pixels holds the whole old texture.
        unsafe {
            self.texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                NonNull::new_unchecked(pixels.as_mut_ptr() as *mut _),
                row,
                region(0, 0, ow, oh),
                0,
            );
            texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region(0, 0, ow, oh),
                0,
                NonNull::new_unchecked(pixels.as_mut_ptr() as *mut _),
                row,
            );
        }
        self.texture = texture;
        self.packer.grow_to(w, h);
        Some(())
    }
}

/// Shared-memory buffers handed out for instance data and returned when a
/// pane's data is replaced. A returned buffer is reused only once the last
/// command buffer that could read it has completed.
#[derive(Default)]
pub(crate) struct BufferPool {
    free: Vec<(Retained<Buffer>, Option<Retained<CommandBuffer>>)>,
}

/// Buffers kept for reuse; more than this are released.
const POOL_KEEP: usize = 16;

impl BufferPool {
    pub(crate) fn acquire(&mut self, device: &Device, bytes: usize) -> Option<Retained<Buffer>> {
        let ready = |cb: &Option<Retained<CommandBuffer>>| {
            cb.as_ref().is_none_or(|cb| {
                let s = cb.status();
                s == MTLCommandBufferStatus::Completed || s == MTLCommandBufferStatus::Error
            })
        };
        // The smallest free buffer that fits.
        let pick = self
            .free
            .iter()
            .enumerate()
            .filter(|(_, (b, cb))| b.length() >= bytes && ready(cb))
            .min_by_key(|(_, (b, _))| b.length())
            .map(|(i, _)| i);
        if let Some(i) = pick {
            return Some(self.free.swap_remove(i).0);
        }
        let size = bytes.next_power_of_two().max(4096);
        device.newBufferWithLength_options(size, MTLResourceOptions::StorageModeShared)
    }

    /// Returns a buffer; last_use is the latest command buffer that may read it.
    pub(crate) fn release(
        &mut self,
        buffer: Retained<Buffer>,
        last_use: Option<Retained<CommandBuffer>>,
    ) {
        self.free.push((buffer, last_use));
        if self.free.len() > POOL_KEEP {
            // Dropping is safe even if the GPU still reads it: command buffers
            // retain the buffers they use.
            self.free.remove(0);
        }
    }
}

/// Copies plain-old-data values into the start of a shared buffer.
pub(crate) fn fill<T: Copy>(buffer: &Buffer, values: &[T]) {
    let bytes = std::mem::size_of_val(values);
    assert!(buffer.length() >= bytes);
    // SAFETY: the buffer is shared memory at least bytes long and not read by
    // the GPU (callers only fill buffers fresh from the pool).
    unsafe {
        std::ptr::copy_nonoverlapping(
            values.as_ptr() as *const u8,
            buffer.contents().as_ptr() as *mut u8,
            bytes,
        )
    };
}

/// Lets f write len values of T straight into a shared buffer.
pub(crate) fn fill_with<T: Copy>(buffer: &Buffer, len: usize, f: impl FnOnce(&mut [T])) {
    assert!(buffer.length() >= len * std::mem::size_of::<T>());
    let ptr = buffer.contents().as_ptr() as *mut T;
    assert!(ptr.is_aligned());
    // SAFETY: the buffer is shared memory long enough for len values, page
    // aligned, and not read by the GPU (callers only fill buffers fresh from
    // the pool). The slice does not outlive this call.
    f(unsafe { std::slice::from_raw_parts_mut(ptr, len) });
}
