//! Color packing between the frame's 0xRRGGBB values and what the GPU reads.

use tern_types::Rgb;

/// Packs a color and an alpha into the byte order the shader unpacks with
/// unpack_unorm4x8_to_float: red in the lowest byte, alpha in the highest.
#[inline]
pub(crate) fn pack(rgb: Rgb, alpha: f32) -> u32 {
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    r | g << 8 | b << 16 | a << 24
}

/// The color as three channels from 0 to 1, for the render pass clear color.
pub(crate) fn unit(rgb: Rgb) -> [f64; 3] {
    [
        ((rgb >> 16) & 0xff) as f64 / 255.0,
        ((rgb >> 8) & 0xff) as f64 / 255.0,
        (rgb & 0xff) as f64 / 255.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_puts_red_in_the_low_byte() {
        assert_eq!(pack(0x112233, 1.0), 0xff33_2211);
        assert_eq!(pack(0xff0000, 0.0), 0x0000_00ff);
        assert_eq!(pack(0x00ff00, 0.5), 0x8000_ff00);
    }

    #[test]
    fn alpha_is_clamped() {
        assert_eq!(pack(0, 2.0) >> 24, 255);
        assert_eq!(pack(0, -1.0) >> 24, 0);
    }

    #[test]
    fn unit_channels() {
        assert_eq!(unit(0xff8000), [1.0, 128.0 / 255.0, 0.0]);
    }
}
