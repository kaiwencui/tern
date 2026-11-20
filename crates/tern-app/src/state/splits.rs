//! The panes of a window: splitting, focus, closing and zoom, and the
//! dividers between panes, which the mouse drags.

use std::path::PathBuf;
use std::sync::Arc;

use tern_config::{Direction as Side, FocusTarget};
use tern_layout::{Axis, Closed, Direction, Divider, DividerHandle};
use tern_types::{PaneId, Point, Rect, Rgb};

use super::WindowState;
use crate::app::app;
use crate::pane::{self, new_pane_id, Launch, Pane};

/// How thick dividers are, in points: a hairline.
const DIVIDER_PT: f64 = 1.0;
/// How far beside a divider, in points, the pointer still grabs it: the
/// line is too thin to aim at, so this makes about 8 points of it.
const DIVIDER_SLOP_PT: f64 = 3.5;
/// The smallest a pane gets from a split, a divider drag or a resize, in
/// cells across the divider.
const MIN_PANE_COLS: i32 = 6;
const MIN_PANE_ROWS: i32 = 3;
/// How far a divider's color goes from the background toward the
/// foreground when the configuration names none.
const DIVIDER_MIX: f32 = 0.15;

/// A divider held by the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Grab {
    handle: DividerHandle,
    axis: Axis,
    /// The pointer's distance from the divider's center line at the click,
    /// kept through the drag so the line does not jump to the pointer.
    offset: i32,
}

/// The layout's direction for a configured one.
fn direction(side: Side) -> Direction {
    match side {
        Side::Left => Direction::Left,
        Side::Right => Direction::Right,
        Side::Up => Direction::Up,
        Side::Down => Direction::Down,
    }
}

/// The coordinate a divider of this axis moves along: a side-by-side
/// (horizontal) split has a vertical line that moves left and right.
pub(super) fn along(axis: Axis, p: Point) -> i32 {
    match axis {
        Axis::Horizontal => p.x,
        Axis::Vertical => p.y,
    }
}

/// a, moved toward b by t (0 to 1), channel by channel.
pub(super) fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let channel = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        ((x + (y - x) * t).round() as u32).min(0xff) << shift
    };
    channel(16) | channel(8) | channel(0)
}

impl WindowState {
    /// The area the layout fills: the whole drawable.
    pub(super) fn area(&self) -> Rect {
        let (w, h) = self.drawable_size();
        Rect::new(0, 0, w as i32, h as i32)
    }

    /// Divider thickness in pixels at the window's scale.
    pub(super) fn divider_px(&self) -> i32 {
        (DIVIDER_PT * self.scale.get()).round().max(1.0) as i32
    }

    fn slop_px(&self) -> i32 {
        (DIVIDER_SLOP_PT * self.scale.get()).round() as i32
    }

    /// The smallest length across a divider of this axis that a drag leaves
    /// a pane: a few cells inside the padding.
    pub(super) fn min_pane_px(&self, axis: Axis) -> i32 {
        let m = self.metrics.get();
        let (pad_x, pad_y) = self.padding_px();
        let cells = |n: i32, cell: u32| n.saturating_mul(i32::try_from(cell).unwrap_or(i32::MAX));
        match axis {
            Axis::Horizontal => pad_x
                .saturating_mul(2)
                .saturating_add(cells(MIN_PANE_COLS, m.cell_w)),
            Axis::Vertical => pad_y
                .saturating_mul(2)
                .saturating_add(cells(MIN_PANE_ROWS, m.cell_h)),
        }
    }

    pub(super) fn divider_color(&self) -> Rgb {
        let app = app();
        let config = app.config();
        config
            .split_divider_color
            .unwrap_or_else(|| mix(config.theme.bg, config.theme.fg, DIVIDER_MIX))
    }

    /// Gives every pane its rectangle and grid from the layout, and works
    /// out the dividers. Runs when the layout or the geometry changes, never
    /// per frame.
    pub(super) fn relayout(&self) {
        let area = self.area();
        let divider = self.divider_px();
        let layout = self.layout.borrow();
        let rects = layout.rects(area, divider);
        let order = layout.panes();
        let metrics = self.metrics.get();
        let padding = self.padding_px();
        // The only pane shown, alone or zoomed, is centered in the window.
        let alone = rects.len() == 1;
        {
            let mut panes = self.panes.borrow_mut();
            panes.sort_by_key(|p| order.iter().position(|&id| id == p.id));
            for pane in panes.iter_mut() {
                match rects.iter().find(|(id, _)| *id == pane.id) {
                    Some((_, rect)) => {
                        pane.place(*rect, &metrics, padding, alone);
                        pane.shown = true;
                    }
                    // Hidden behind a zoomed pane: it keeps its size and runs on.
                    None => pane.shown = false,
                }
            }
        }
        let dividers = layout.dividers(area, divider);
        drop(layout);
        *self.dividers.borrow_mut() = dividers;
        self.update_quads();
        // A divider may have come or gone under a pointer that is not moving.
        self.hover();
    }

    /// The dividers as the layout last placed them, in view pixels.
    pub fn dividers(&self) -> Vec<Divider> {
        self.dividers.borrow().clone()
    }

    pub fn is_zoomed(&self) -> bool {
        self.layout.borrow().is_zoomed()
    }

    /// The layout in its text form, for the automation socket.
    pub fn layout_text(&self) -> String {
        self.layout.borrow().to_string()
    }

    // ---- splitting and closing ----

    /// Splits the focused pane, starting a shell in working_dir beside it on
    /// the given side, and focuses the new pane.
    pub fn split(&self, side: Side, working_dir: Option<PathBuf>) {
        let Some(target) = self.focused_pane_id() else {
            return;
        };
        let id = new_pane_id();
        let mut layout = self.layout.borrow().clone();
        if let Err(e) = layout.split(target, direction(side), id) {
            eprintln!("tern: cannot split: {e}");
            return;
        }
        let area = self.area();
        let rects = layout.rects(area, self.divider_px());
        let rect_of = |pane| rects.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r);
        // Both halves must still hold a usable grid, or the user would type
        // into a pane too small to see.
        let axis = match side {
            Side::Left | Side::Right => Axis::Horizontal,
            Side::Up | Side::Down => Axis::Vertical,
        };
        let min = self.min_pane_px(axis);
        let fits = |r: Rect| match axis {
            Axis::Horizontal => r.w >= min,
            Axis::Vertical => r.h >= min,
        };
        if ![id, target]
            .into_iter()
            .all(|p| rect_of(p).is_some_and(fits))
        {
            eprintln!("tern: no room to split this pane");
            return;
        }
        // The shell starts with the grid it will have, so it gets no
        // SIGWINCH while it starts up.
        let rect = rect_of(id).unwrap_or(area);
        let metrics = self.metrics.get();
        let (cols, rows) = pane::grid(rect, &metrics, self.padding_px());
        let launch = Launch {
            id,
            command: None,
            working_dir,
            cols,
            rows,
            cell: pane::cell_size(&metrics),
        };
        let spawned = Pane::spawn(&app().config(), launch, Arc::clone(&self.frames));
        let pane = match spawned {
            Ok(pane) => pane,
            Err(e) => {
                eprintln!("tern: cannot start the shell: {e}");
                return;
            }
        };
        self.panes.borrow_mut().push(pane);
        *self.layout.borrow_mut() = layout;
        self.relayout();
        self.focus_moved(Some(target));
    }

    /// Removes a pane: its session hangs up, and its neighbor takes the
    /// space (and the focus, if it had it). True when it was the last pane,
    /// so the window should close.
    pub fn remove_pane(&self, id: PaneId) -> bool {
        let old = self.focused_pane_id();
        let closed = self.layout.borrow_mut().close(id);
        let removed = {
            let mut panes = self.panes.borrow_mut();
            let i = panes.iter().position(|p| p.id == id);
            i.map(|i| panes.remove(i))
        };
        if removed.is_some() {
            app().renderer().forget_pane(id);
        }
        // The session hangs up here, outside any borrow.
        drop(removed);
        self.pane_closed(id);
        match closed {
            Ok(Closed::Empty) => true,
            Ok(Closed::Focus(_)) => {
                self.relayout();
                self.focus_moved(old);
                self.mark_dirty();
                false
            }
            Err(_) => self.panes.borrow().is_empty(),
        }
    }

    // ---- focus ----

    /// Gives a pane of this window the keyboard focus.
    pub fn focus_pane(&self, id: PaneId) {
        let old = self.focused_pane_id();
        if old != Some(id) && self.layout.borrow_mut().set_focus(id) {
            self.focus_moved(old);
        }
    }

    /// Moves the focus as focus_split asks.
    pub fn focus_split(&self, target: FocusTarget) {
        let old = self.focused_pane_id();
        let (area, divider) = (self.area(), self.divider_px());
        let moved = {
            let mut layout = self.layout.borrow_mut();
            match target {
                FocusTarget::Next => layout.focus_next(),
                FocusTarget::Previous => layout.focus_previous(),
                FocusTarget::Left => layout.focus_direction(Direction::Left, area, divider),
                FocusTarget::Right => layout.focus_direction(Direction::Right, area, divider),
                FocusTarget::Up => layout.focus_direction(Direction::Up, area, divider),
                FocusTarget::Down => layout.focus_direction(Direction::Down, area, divider),
            }
        };
        if moved.is_some() {
            self.focus_moved(old);
        }
    }

    /// The layout's focus moved from old: the sessions, the zoom, the title
    /// and the cursor follow.
    pub(super) fn focus_moved(&self, old: Option<PaneId>) {
        let new = self.focused_pane_id();
        if old == new {
            return;
        }
        if !self.preedit.borrow().is_empty()
            && let Some(view) = self.window().and_then(|w| app().view_of(&w)) {
                view.discard_composition();
            }
        if self.focused.get() {
            for p in self.panes.borrow().iter() {
                if Some(p.id) == old {
                    p.session.focus(false);
                } else if Some(p.id) == new {
                    p.session.focus(true);
                }
            }
        }
        if self.is_zoomed() {
            // The zoom went with the focus.
            self.relayout();
        }
        self.update_title();
        self.restart_blink();
        self.mark_dirty();
    }

    // ---- sizes ----

    /// Moves the divider on that side of the focused pane by points.
    pub fn resize_split(&self, side: Side, points: u16) {
        let Some(id) = self.focused_pane_id() else {
            return;
        };
        let axis = match side {
            Side::Left | Side::Right => Axis::Horizontal,
            Side::Up | Side::Down => Axis::Vertical,
        };
        let amount = (f64::from(points) * self.scale.get()).round() as i32;
        let moved = self.layout.borrow_mut().resize(
            id,
            direction(side),
            amount,
            self.area(),
            self.divider_px(),
            self.min_pane_px(axis),
        );
        if moved {
            self.relayout();
            self.mark_dirty();
        }
    }

    pub fn equalize(&self) {
        self.layout
            .borrow_mut()
            .equalize(self.area(), self.divider_px());
        self.relayout();
        self.mark_dirty();
    }

    pub fn toggle_zoom(&self) {
        self.layout.borrow_mut().toggle_zoom();
        self.relayout();
        self.mark_dirty();
    }

    // ---- the mouse ----

    /// The pane under a point in view pixels.
    pub fn pane_at(&self, at: Point) -> Option<PaneId> {
        self.layout
            .borrow()
            .pane_at(self.area(), self.divider_px(), at)
            .map(|(id, _)| id)
    }

    pub(super) fn divider_at(&self, at: Point) -> Option<Divider> {
        self.layout
            .borrow()
            .divider_at(self.area(), self.divider_px(), at, self.slop_px())
    }

    /// Takes hold of a divider at the pointer.
    pub(super) fn grab_divider(&self, d: &Divider, at: Point) -> Grab {
        Grab {
            handle: d.handle,
            axis: d.axis,
            offset: along(d.axis, at) - along(d.axis, d.rect.center()),
        }
    }

    pub(super) fn drag_divider(&self, grab: Grab, at: Point) {
        let pointer = match grab.axis {
            Axis::Horizontal => Point {
                x: at.x.saturating_sub(grab.offset),
                y: at.y,
            },
            Axis::Vertical => Point {
                x: at.x,
                y: at.y.saturating_sub(grab.offset),
            },
        };
        // False when the line is already at its limit, or the split is gone
        // (closed during the drag); either way nothing changes.
        let moved = self.layout.borrow_mut().drag_divider(
            grab.handle,
            self.area(),
            self.divider_px(),
            pointer,
            self.min_pane_px(grab.axis),
        );
        if moved {
            self.relayout();
            self.mark_dirty();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_mix_channel_by_channel() {
        assert_eq!(mix(0x000000, 0xffffff, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xffffff, 1.0), 0xffffff);
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
        assert_eq!(mix(0x181a1f, 0xdcdfe4, 0.3), 0x53555a);
        assert_eq!(mix(0xff0000, 0x0000ff, 0.25), 0xbf0040);
    }

    #[test]
    fn configured_directions_map_to_the_layout() {
        assert_eq!(direction(Side::Left), Direction::Left);
        assert_eq!(direction(Side::Right), Direction::Right);
        assert_eq!(direction(Side::Up), Direction::Up);
        assert_eq!(direction(Side::Down), Direction::Down);
        let p = Point { x: 3, y: 9 };
        assert_eq!(along(Axis::Horizontal, p), 3);
        assert_eq!(along(Axis::Vertical, p), 9);
    }
}
