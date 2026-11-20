//! Where new windows go and how big they are.

use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSScreen, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::App;
use crate::window::window_style;

impl App {
    /// The content size, in points, of a window holding the configured
    /// number of cells at this backing scale.
    pub(super) fn content_size(&self, scale: f64) -> NSSize {
        let m = self.renderer().metrics(scale as f32);
        let c = self.config();
        let side = |cells: u16, cell: u32, pad_pt: u16| {
            let pad = (f64::from(pad_pt) * scale).round();
            (f64::from(cells) * f64::from(cell) + 2.0 * pad) / scale
        };
        NSSize::new(
            side(c.window_width, m.cell_w, c.window_padding_x).ceil(),
            side(c.window_height, m.cell_h, c.window_padding_y).ceil(),
        )
    }

    /// The screen a window placed by the configuration opens on: the one
    /// its top-left corner lands on, or the main screen.
    pub(super) fn first_window_screen(&self) -> Option<Retained<NSScreen>> {
        let main = NSScreen::mainScreen(self.mtm)?;
        let area = main.visibleFrame();
        // Where the window's top-left corner goes; the far-edge forms of the
        // position need the window's size, but its corner's screen is enough.
        let x = match self.config().window_position_x {
            Some(x) if x >= 0 => area.origin.x + f64::from(x),
            Some(x) => area.origin.x + area.size.width + f64::from(x),
            None => area.origin.x + area.size.width / 2.0,
        };
        let y = match self.config().window_position_y {
            Some(y) if y >= 0 => area.origin.y + area.size.height - f64::from(y),
            Some(y) => area.origin.y - f64::from(y),
            None => area.origin.y + area.size.height / 2.0,
        };
        let on = NSScreen::screens(self.mtm).iter().find(|s| {
            let f = s.frame();
            x >= f.origin.x
                && x < f.origin.x + f.size.width
                && y >= f.origin.y
                && y <= f.origin.y + f.size.height
        });
        Some(on.unwrap_or(main))
    }

    /// Cascades from another window, or centers the window on the main
    /// screen.
    pub(super) fn place(&self, window: &NSWindow, cascade_from: Option<&NSWindow>) {
        match cascade_from {
            Some(previous) => {
                let top_left = previous.cascadeTopLeftFromPoint(NSPoint::new(0.0, 0.0));
                window.cascadeTopLeftFromPoint(top_left);
            }
            None => window.center(),
        }
    }

    /// Where the configured window position puts the bottom-left corner of
    /// a window with this content size, in screen coordinates; None when no
    /// position is configured.
    pub(super) fn configured_origin(&self, content: NSSize) -> Option<NSPoint> {
        let (want_x, want_y) = (
            self.config().window_position_x,
            self.config().window_position_y,
        );
        if want_x.is_none() && want_y.is_none() {
            return None;
        }
        let area = NSScreen::mainScreen(self.mtm)?.visibleFrame();
        let frame = NSWindow::frameRectForContentRect_styleMask(
            NSRect::new(NSPoint::new(0.0, 0.0), content),
            window_style(),
            self.mtm,
        );
        let (w, h) = (frame.size.width, frame.size.height);
        let x = match want_x.map(f64::from) {
            Some(x) if x >= 0.0 => area.origin.x + x,
            Some(x) => area.origin.x + area.size.width + x - w,
            None => area.origin.x + (area.size.width - w) / 2.0,
        };
        // AppKit's y grows upward; the setting counts from the top.
        let y = match want_y.map(f64::from) {
            Some(y) if y >= 0.0 => area.origin.y + area.size.height - y - h,
            Some(y) => area.origin.y - y,
            None => area.origin.y + (area.size.height - h) / 2.0,
        };
        Some(NSPoint::new(x, y))
    }

    pub(super) fn show(&self, window: &NSWindow) {
        window.makeKeyAndOrderFront(None);
        if !NSApplication::sharedApplication(self.mtm).isActive() {
            // An inactive app's window would open behind the active app's
            // windows. This brings it up without taking keyboard focus.
            window.orderFrontRegardless();
        }
    }
}
