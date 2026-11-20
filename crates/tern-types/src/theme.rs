use crate::Rgb;

/// The colors a terminal starts with. Programs can override entries at run
/// time (OSC 4, 10, 11, 12); the core tracks those overrides itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    /// None: the cursor takes the foreground color of the cell it is on.
    pub cursor: Option<Rgb>,
    /// None: the character under a block cursor takes the cell's background.
    pub cursor_text: Option<Rgb>,
    /// None: selected cells swap foreground and background.
    pub selection_bg: Option<Rgb>,
    pub selection_fg: Option<Rgb>,
    pub palette: [Rgb; 256],
    /// Draw bold text in the bright variant of the eight basic colors.
    pub bold_is_bright: bool,
}

impl Default for Theme {
    fn default() -> Theme {
        let mut palette = xterm_256_palette();
        const BASE: [Rgb; 16] = [
            0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
            0x5a6374, 0xef7d86, 0xa9d48a, 0xf0d08c, 0x74bdf7, 0xd38aea, 0x67c7d3, 0xffffff,
        ];
        palette[..16].copy_from_slice(&BASE);
        Theme {
            fg: 0xdcdfe4,
            bg: 0x181a1f,
            cursor: Some(0xdcdfe4),
            cursor_text: Some(0x181a1f),
            selection_bg: Some(0x3e4451),
            selection_fg: None,
            palette,
            bold_is_bright: false,
        }
    }
}

/// The standard xterm palette: 16 basic colors, a 6x6x6 cube, 24 grays.
pub fn xterm_256_palette() -> [Rgb; 256] {
    let mut p = [0u32; 256];
    const BASE: [Rgb; 16] = [
        0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
        0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    p[..16].copy_from_slice(&BASE);
    const LEVELS: [u32; 6] = [0, 95, 135, 175, 215, 255];
    for i in 0..216 {
        let (r, g, b) = (LEVELS[i / 36], LEVELS[(i / 6) % 6], LEVELS[i % 6]);
        p[16 + i] = r << 16 | g << 8 | b;
    }
    for i in 0..24u32 {
        let v = 8 + 10 * i;
        p[232 + i as usize] = v << 16 | v << 8 | v;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_palette_landmarks() {
        let p = xterm_256_palette();
        assert_eq!(p[16], 0x000000);
        assert_eq!(p[21], 0x0000ff);
        assert_eq!(p[196], 0xff0000);
        assert_eq!(p[231], 0xffffff);
        assert_eq!(p[232], 0x080808);
        assert_eq!(p[255], 0xeeeeee);
    }
}
