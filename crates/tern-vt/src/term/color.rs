//! The colors programs have set, by slot. A slot no program has set has
//! None, and the app's color is used for it.

use std::ops::{Index, IndexMut};

use crate::ansi::{NamedColor, Rgb};

/// How many slots there are.
///
/// | Slots    |                                       |
/// | -------- | ------------------------------------- |
/// | 0..16    | the sixteen colors of the palette     |
/// | 16..232  | the 6×6×6 cube                        |
/// | 232..256 | the ramp of grays                     |
/// | 256..259 | the foreground, background and cursor |
pub const COUNT: usize = 259;

/// The colors programs have set.
#[derive(Clone, Copy)]
pub struct Colors([Option<Rgb>; COUNT]);

impl Default for Colors {
    fn default() -> Colors {
        Colors([None; COUNT])
    }
}

impl Index<usize> for Colors {
    type Output = Option<Rgb>;

    #[inline]
    fn index(&self, slot: usize) -> &Option<Rgb> {
        &self.0[slot]
    }
}

impl IndexMut<usize> for Colors {
    #[inline]
    fn index_mut(&mut self, slot: usize) -> &mut Option<Rgb> {
        &mut self.0[slot]
    }
}

impl Index<NamedColor> for Colors {
    type Output = Option<Rgb>;

    #[inline]
    fn index(&self, color: NamedColor) -> &Option<Rgb> {
        &self.0[color as usize]
    }
}

impl IndexMut<NamedColor> for Colors {
    #[inline]
    fn index_mut(&mut self, color: NamedColor) -> &mut Option<Rgb> {
        &mut self.0[color as usize]
    }
}
