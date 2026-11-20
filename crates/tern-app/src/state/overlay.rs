//! What is drawn over the panes: the dividers, the highlight on the pane a
//! drag from another app would drop on, and the marks of a pane being moved,
//! here or into this window from another tab, or of a tab being docked.
//! Built when one of them changes, never per frame.

use tern_layout::zone_rect;
use tern_render::Quad;
use tern_types::{PaneId, Point, Rect, Rgb};

use super::mouse::Gesture;
use super::WindowState;
use crate::app::app;

/// The highlight color: a blue that reads on light and dark themes.
const ACCENT: Rgb = 0x3d8af7;
/// How much a highlighted area is tinted.
const TINT: f32 = 0.22;
/// The tint on a pane that is being moved.
const SOURCE_TINT: f32 = 0.15;
/// The outline around a highlighted area, in points.
const OUTLINE_PT: f64 = 2.0;
/// Width and edge margin of the translucent scrollback thumb, in points.
const SCROLLBAR_WIDTH_PT: f64 = 3.0;
const SCROLLBAR_INSET_PT: f64 = 3.0;
const SCROLLBAR_MIN_HEIGHT_PT: f64 = 24.0;
const SCROLLBAR_ALPHA: f32 = 0.38;

/// Computes the translucent right-edge scrollback thumb for a scrolled pane.
/// Returns `None` when the pane is at the live bottom or has no scrollback.
pub(super) fn scrollbar_thumb(
    rect: Rect,
    rows: u16,
    history: u32,
    display_offset: u32,
    fg: Rgb,
    scale: f64,
) -> Option<Quad> {
    if display_offset == 0 || history == 0 || rows == 0 {
        return None;
    }
    let scale = scale.max(1.0);
    let w = (SCROLLBAR_WIDTH_PT * scale).round().max(2.0) as i32;
    let inset = (SCROLLBAR_INSET_PT * scale).round().max(2.0) as i32;
    let track_h = rect.h - 2 * inset;
    if rect.w < w + 2 * inset || track_h < w * 2 {
        return None;
    }
    let total = f64::from(history) + f64::from(rows);
    let min_h = ((SCROLLBAR_MIN_HEIGHT_PT * scale).round() as i32).min(track_h);
    let thumb_h = ((f64::from(rows) / total) * f64::from(track_h))
        .round()
        .clamp(f64::from(min_h), f64::from(track_h)) as i32;
    let offset = display_offset.min(history);
    let frac = f64::from(history - offset) / f64::from(history);
    let y = rect.y + inset + (frac * f64::from(track_h - thumb_h)).round() as i32;
    let x = rect.right() - inset - w;
    Some(Quad {
        rect: Rect::new(x, y, w, thumb_h),
        color: fg,
        alpha: SCROLLBAR_ALPHA,
    })
}

/// A tinted area with an outline just inside its edges.
fn highlight(quads: &mut Vec<Quad>, r: Rect, tint: f32, line: i32) {
    let quad = |rect| Quad {
        rect,
        color: ACCENT,
        alpha: 1.0,
    };
    quads.push(Quad {
        rect: r,
        color: ACCENT,
        alpha: tint,
    });
    let line = line.min(r.w / 2).min(r.h / 2).max(0);
    if line == 0 {
        return;
    }
    quads.push(quad(Rect::new(r.x, r.y, r.w, line)));
    quads.push(quad(Rect::new(r.x, r.bottom() - line, r.w, line)));
    quads.push(quad(Rect::new(r.x, r.y + line, line, r.h - 2 * line)));
    quads.push(quad(Rect::new(
        r.right() - line,
        r.y + line,
        line,
        r.h - 2 * line,
    )));
}

impl WindowState {
    /// Rebuilds the quads drawn over the panes.
    pub(super) fn update_quads(&self) {
        let color = self.divider_color();
        let line = (OUTLINE_PT * self.scale.get()).round() as i32;
        let rect_of = |id: PaneId| self.with_pane(id, |p| p.rect).filter(|_| self.is_shown(id));
        let moving = self.gesture.get();
        // A pane picked up in another tab, over one of this window's panes,
        // or a tab dragged off its tab bar.
        let arriving = match moving {
            Some(Gesture::Move { .. }) => None,
            _ => {
                let app = app();
                app.move_mark_elsewhere(self.id)
                    .or_else(|| app.tab_dock_mark(self.id))
            }
        };
        let mut quads = self.quads.borrow_mut();
        quads.clear();
        quads.extend(self.dividers.borrow().iter().map(|d| Quad {
            rect: d.rect,
            color,
            alpha: 1.0,
        }));
        if let Some(r) = self.drop_target.get().and_then(rect_of) {
            highlight(&mut quads, r, TINT, line);
        }
        let target = match moving {
            Some(Gesture::Move { source, target }) => {
                if let Some(r) = rect_of(source) {
                    highlight(&mut quads, r, SOURCE_TINT, 0);
                }
                target
            }
            _ => arriving,
        };
        if let Some((r, zone)) = target.and_then(|(t, zone)| rect_of(t).map(|r| (r, zone))) {
            highlight(&mut quads, zone_rect(r, zone), TINT, line);
        }
        if app().tab_merge_mark(self.id) {
            let area = self.area();
            let strip_h = ((26.0 * self.scale.get()).round() as i32).min(area.h);
            if strip_h > 0 {
                highlight(&mut quads, Rect::new(area.x, area.y, area.w, strip_h), TINT, line);
            }
        }
    }

    /// What a pane move in another tab points at in this window changed.
    pub(crate) fn marks_changed(&self) {
        self.update_quads();
        self.request_frame();
    }

    fn is_shown(&self, id: PaneId) -> bool {
        self.with_pane(id, |p| p.shown).unwrap_or(false)
    }

    /// A drag from another app moved over the view (None: it left). The
    /// pane it would drop on is highlighted.
    pub(crate) fn drag_moved(&self, at: Option<Point>) {
        let target = at.and_then(|p| self.pane_at(p));
        if self.drop_target.replace(target) != target {
            self.update_quads();
            self.request_frame();
        }
    }

    /// The quads drawn over the panes, for the automation socket.
    pub fn quads(&self) -> Vec<Quad> {
        self.quads.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollbar_thumb_tracks_history_position_and_hides_at_bottom() {
        let r = Rect::new(0, 0, 800, 600);
        assert_eq!(scrollbar_thumb(r, 40, 1000, 0, 0xffffff, 2.0), None);
        assert_eq!(scrollbar_thumb(r, 40, 0, 10, 0xffffff, 2.0), None);

        let top = scrollbar_thumb(r, 40, 1000, 1000, 0xffffff, 2.0).unwrap();
        let mid = scrollbar_thumb(r, 40, 1000, 500, 0xffffff, 2.0).unwrap();
        let near_bot = scrollbar_thumb(r, 40, 1000, 1, 0xffffff, 2.0).unwrap();

        assert_eq!(top.rect.y, 6);
        assert!(mid.rect.y > top.rect.y && near_bot.rect.y > mid.rect.y);
        assert!(near_bot.rect.bottom() <= r.bottom() - 6);
        assert_eq!(top.rect.w, 6);
    }
}
