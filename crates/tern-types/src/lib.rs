//! Plain data shared by the terminal core, the renderer, the input encoder,
//! the layout and the app. Nothing here depends on the operating system.

mod frame;
mod geometry;
mod keys;
mod modes;
mod theme;

pub use frame::{flags, Cell, CursorDraw, CursorShape, ExtraRef, Frame};
pub use geometry::{Point, Rect};
pub use keys::{Key, KeypadKey, Mods};
pub use modes::{Modes, MouseEncoding, MouseTracking};
pub use theme::{xterm_256_palette, Theme};

/// A color as 0x00RRGGBB.
pub type Rgb = u32;

/// Identifies one pane (one terminal session) for the life of the process.
pub type PaneId = u64;
