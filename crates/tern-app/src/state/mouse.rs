//! The mouse in a window's panes: selecting text, reporting to programs
//! that asked for the mouse, the wheel, Command-click on links, picking up
//! a pane with Command-Option to move it (within the window, or by way of
//! the tab bar to another tab), and the pointer's shape.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use tern_core::{Scroll, SelectionKind, Side};
use tern_input::{
    alternate_scroll_bytes, encode_mouse, wants_mouse, MouseButton, MouseEvent, MouseKind as Report,
};
use tern_layout::{drop_zone, Axis, Zone};
use tern_render::CellMetrics;
use tern_types::{Modes, Mods, MouseTracking, PaneId, Point, Rect};

use super::splits::Grab;
use super::{CursorStyle, WindowState};
use crate::app::app;
use crate::input::{MouseInput, MouseKind, ScrollInput};
use crate::links;

/// How often a selection dragged past the top or bottom scrolls.
const AUTOSCROLL_INTERVAL: f64 = 0.05;
/// The most lines one autoscroll step moves, however far past the edge the
/// pointer is.
const AUTOSCROLL_MAX_LINES: i32 = 8;
/// Lines one notch of a mouse wheel scrolls.
const WHEEL_NOTCH_LINES: f64 = 3.0;
/// The most wheel reports one scroll event sends a program.
const MAX_WHEEL_REPORTS: i32 = 100;
/// NSEventPhaseBegan: a trackpad gesture starts, so leftover movement from
/// the last one is forgotten.
const PHASE_BEGAN: usize = 1;

/// What a held mouse button is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Gesture {
    /// Dragging a divider.
    Divider(Grab),
    /// Selecting text in a pane.
    Select(Selecting),
    /// The program in the pane has the mouse: the press went to it, and so
    /// do motion and the release.
    Report { pane: PaneId, button: MouseButton },
    /// A pane picked up with Command-Option, and where it would go: a zone
    /// of another pane, which is in another window once the drag has been
    /// to that window's tab.
    Move {
        source: PaneId,
        target: Option<(PaneId, Zone)>,
    },
}

impl Gesture {
    /// The button that started it, whose release ends it.
    fn button(self) -> u8 {
        match self {
            Gesture::Report { button, .. } => match button {
                MouseButton::Right => 1,
                MouseButton::Middle => 2,
                _ => 0,
            },
            Gesture::Divider(_) | Gesture::Select(_) | Gesture::Move { .. } => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Selecting {
    pane: PaneId,
    kind: SelectionKind,
    /// Where a plain press was, until a drag starts the selection there; a
    /// click without a drag selects nothing.
    anchor: Option<CellPos>,
}

/// A pointer position as a cell of a pane's grid. The row is outside the
/// grid when the pointer is above or below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CellPos {
    row: i32,
    col: u16,
    side: Side,
}

/// What the mouse code needs to know about a pane.
#[derive(Clone, Copy)]
struct Spot {
    id: PaneId,
    rect: Rect,
    origin: Point,
    grid: (u16, u16),
    modes: Modes,
}

/// The cell under a point, and which half of it.
fn cell_at(origin: Point, grid: (u16, u16), m: &CellMetrics, at: Point) -> CellPos {
    let cw = i32::try_from(m.cell_w.max(1)).unwrap_or(i32::MAX);
    let ch = i32::try_from(m.cell_h.max(1)).unwrap_or(i32::MAX);
    let dx = at.x.saturating_sub(origin.x);
    let dy = at.y.saturating_sub(origin.y);
    let cols = i32::from(grid.0.max(1));
    let (col, side) = if dx < 0 {
        (0, Side::Left)
    } else if dx / cw >= cols {
        (cols - 1, Side::Right)
    } else if dx % cw < cw / 2 {
        (dx / cw, Side::Left)
    } else {
        (dx / cw, Side::Right)
    };
    CellPos {
        row: dy.div_euclid(ch),
        // Below cols, which is a u16.
        col: col as u16,
        side,
    }
}

/// A cell inside the grid, for reports and links.
fn clamp_to_grid(pos: CellPos, grid: (u16, u16)) -> (u16, u16) {
    let last = i32::from(grid.1.max(1)) - 1;
    (pos.col, pos.row.clamp(0, last) as u16)
}

/// The report button for an AppKit button number.
fn report_button(button: u8) -> MouseButton {
    match button {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        _ => MouseButton::Middle,
    }
}

/// Whole lines a wheel event scrolls (positive: back into the history),
/// with what is left over carried in rest, in lines. A wheel's delta is in
/// notches, a trackpad's in points; wheels that send fractions of a notch
/// (smooth-scrolling mice) add up to whole lines like a trackpad.
fn wheel_lines(input: &ScrollInput, rest: &mut f64, line_pt: f64) -> i32 {
    let delta = if input.precise {
        if input.phase & PHASE_BEGAN != 0 {
            *rest = 0.0;
        }
        input.dy / line_pt.max(1.0)
    } else {
        input.dy * WHEEL_NOTCH_LINES
    };
    if delta == 0.0 {
        return 0;
    }
    if delta.signum() != rest.signum() {
        // Turning back starts afresh.
        *rest = 0.0;
    }
    *rest += delta;
    let lines = rest.trunc();
    *rest -= lines;
    if !rest.is_finite() {
        *rest = 0.0;
    }
    // A runaway delta saturates rather than wrapping.
    lines.clamp(f64::from(-1_000_000), f64::from(1_000_000)) as i32
}

impl WindowState {
    /// Every mouse event in the view, in view pixels.
    pub(crate) fn pointer(&self, input: &MouseInput) {
        self.pointer_at.set(Some((input.at, input.mods)));
        match input.kind {
            MouseKind::Move => {
                self.pointer_hidden.set(false);
                self.report_motion(input);
                self.hover();
            }
            MouseKind::Down => self.press(input),
            MouseKind::Drag => self.drag(input),
            MouseKind::Up => self.release(input),
        }
    }

    fn spot(&self, id: PaneId) -> Option<Spot> {
        self.with_pane(id, |p| Spot {
            id: p.id,
            rect: p.rect,
            origin: p.origin,
            grid: p.grid,
            modes: p.session.modes(),
        })
    }

    fn spot_at(&self, at: Point) -> Option<Spot> {
        self.pane_at(at).and_then(|id| self.spot(id))
    }

    fn cell_of(&self, spot: &Spot, at: Point) -> CellPos {
        cell_at(spot.origin, spot.grid, &self.metrics.get(), at)
    }

    fn press(&self, input: &MouseInput) {
        if let Some(gesture) = self.gesture.get() {
            if gesture.button() != input.button {
                // Another button while one is held: the first one's
                // gesture goes on. A program that has the mouse hears of
                // the other button too, as xterm reports it.
                if let Gesture::Report { pane, .. } = gesture {
                    self.report_other_button(pane, input, Report::Press);
                }
                return;
            }
            // The same button again: its release went missing (it came up
            // outside the app), so that gesture is over. A program that had
            // the press gets the release it missed.
            self.gesture.set(None);
            if let Gesture::Report { pane, button } = gesture
                && let Some(spot) = self.spot(pane) {
                    let pos = self.cell_of(&spot, input.at);
                    self.report(&spot, Report::Release, button, pos, input.mods);
                }
            if let Gesture::Move { target, .. } = gesture {
                app().move_ended(self.id, target);
            }
            self.stop_autoscroll();
            self.update_quads();
            self.request_frame();
        }
        if input.button > 2 {
            // Programs know three buttons; the others mean nothing here.
            return;
        }
        let at = input.at;
        let mods = input.mods;
        if input.button == 0
            && let Some(d) = self.divider_at(at) {
                if input.clicks == 2 {
                    self.equalize();
                } else {
                    self.gesture
                        .set(Some(Gesture::Divider(self.grab_divider(&d, at))));
                }
                return;
            }
        let Some(spot) = self.spot_at(at) else {
            return;
        };
        if input.button == 0 && mods.contains(Mods::SUPER) && mods.contains(Mods::ALT) {
            // Picking a pane up; never a click for the program. A pane
            // alone in its window has nowhere to go but another tab.
            let beside = self.with_panes(|panes| panes.iter().filter(|p| p.shown).count()) > 1;
            if beside || app().has_other_windows(self.id) {
                self.gesture.set(Some(Gesture::Move {
                    source: spot.id,
                    target: None,
                }));
                self.update_quads();
                self.request_frame();
                self.set_cursor(CursorStyle::Grab);
            }
            return;
        }
        self.focus_pane(spot.id);
        let pos = self.cell_of(&spot, at);
        if input.button == 0 && mods.contains(Mods::SUPER)
            && let Some(target) = self.link_at(&spot, pos) {
                app().open_target(&target);
                return;
            }
        if wants_mouse(spot.modes) && !mods.contains(Mods::SHIFT) {
            let button = report_button(input.button);
            self.report(&spot, Report::Press, button, pos, mods);
            self.gesture.set(Some(Gesture::Report {
                pane: spot.id,
                button,
            }));
            return;
        }
        if input.button != 0 {
            return;
        }
        let selecting = self.with_pane(spot.id, |p| {
            let s = &p.session;
            let kind = match input.clicks {
                2 => SelectionKind::Word,
                n if n >= 3 => SelectionKind::Line,
                _ if mods.contains(Mods::SHIFT) && s.has_selection() => {
                    s.selection_update(pos.row, pos.col, pos.side);
                    return Selecting {
                        pane: spot.id,
                        kind: SelectionKind::Simple,
                        anchor: None,
                    };
                }
                _ => {
                    // A plain click clears; a drag from here selects.
                    s.selection_clear();
                    let kind = if mods.contains(Mods::ALT) {
                        SelectionKind::Block
                    } else {
                        SelectionKind::Simple
                    };
                    return Selecting {
                        pane: spot.id,
                        kind,
                        anchor: Some(pos),
                    };
                }
            };
            s.selection_start(pos.row, pos.col, pos.side, kind);
            s.selection_update(pos.row, pos.col, pos.side);
            Selecting {
                pane: spot.id,
                kind,
                anchor: None,
            }
        });
        if let Some(selecting) = selecting {
            self.gesture.set(Some(Gesture::Select(selecting)));
            self.pane_changed(spot.id);
        }
    }

    fn drag(&self, input: &MouseInput) {
        match self.gesture.get() {
            Some(Gesture::Divider(grab)) => self.drag_divider(grab, input.at),
            Some(Gesture::Select(s)) => self.drag_select(s, input.at),
            Some(Gesture::Report { pane, button }) => {
                let Some(spot) = self.spot(pane) else { return };
                if matches!(
                    spot.modes.mouse,
                    MouseTracking::Drag | MouseTracking::Motion
                ) {
                    let pos = self.cell_of(&spot, input.at);
                    if self.newly_reported(pane, pos, spot.grid) {
                        self.report(&spot, Report::Motion, button, pos, input.mods);
                    }
                }
            }
            Some(Gesture::Move { source, target }) => {
                // Over another tab in the tab bar, that tab comes up, and
                // the pane can be dropped among its panes.
                let app = app();
                app.show_tab_under(self, input.at);
                let now = self.move_target(source, input);
                if now != target {
                    self.gesture.set(Some(Gesture::Move {
                        source,
                        target: now,
                    }));
                    self.update_quads();
                    self.request_frame();
                    app.move_marks_changed(self.id, [target, now]);
                }
            }
            None => {}
        }
    }

    fn release(&self, input: &MouseInput) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if gesture.button() != input.button {
            self.gesture.set(Some(gesture));
            if let Gesture::Report { pane, .. } = gesture {
                self.report_other_button(pane, input, Report::Release);
            }
            return;
        }
        self.stop_autoscroll();
        match gesture {
            Gesture::Divider(_) => {}
            Gesture::Select(s) => {
                // A click that selected nothing copies nothing.
                if s.anchor.is_none() && app().config().copy_on_select {
                    self.copy_selection(s.pane);
                }
            }
            Gesture::Report { pane, button } => {
                if let Some(spot) = self.spot(pane) {
                    let pos = self.cell_of(&spot, input.at);
                    self.report(&spot, Report::Release, button, pos, input.mods);
                }
            }
            Gesture::Move {
                source,
                target: was,
            } => {
                // Where the button came up decides: outside the window, or
                // over the pane itself, nothing moves.
                let app = app();
                match self.move_target(source, input) {
                    Some((target, zone)) if self.contains_pane(target) => {
                        let moved = self.layout.borrow_mut().move_pane(source, target, zone);
                        match moved {
                            Ok(()) => {
                                self.relayout();
                                self.focus_pane(source);
                                self.mark_dirty();
                            }
                            Err(e) => eprintln!("tern: cannot move the pane: {e}"),
                        }
                    }
                    // Among the panes of the tab the drag went to.
                    Some((target, zone)) => app.move_pane_to_tab(self, source, target, zone),
                    // Nowhere: the tab it was picked up in shows again.
                    None => app.show_tab(self.id),
                }
                self.update_quads();
                self.request_frame();
                app.move_ended(self.id, was);
            }
        }
        self.hover();
    }

    /// A press or release of a button other than the one whose report
    /// gesture is under way: reported to the same program.
    fn report_other_button(&self, pane: PaneId, input: &MouseInput, kind: Report) {
        if input.button > 2 {
            return;
        }
        if let Some(spot) = self.spot(pane) {
            let pos = self.cell_of(&spot, input.at);
            self.report(&spot, kind, report_button(input.button), pos, input.mods);
        }
    }

    /// A pane closed: a gesture about it ends, so nothing is left on screen
    /// for it and its release does nothing.
    pub(super) fn pane_closed(&self, id: PaneId) {
        let held = self.gesture.get();
        let gesture = match held {
            Some(Gesture::Move { source, .. }) if source == id => None,
            Some(Gesture::Move {
                source,
                target: Some((target, _)),
            }) if target == id => Some(Gesture::Move {
                source,
                target: None,
            }),
            Some(Gesture::Select(s)) if s.pane == id => None,
            Some(Gesture::Report { pane, .. }) if pane == id => None,
            other => other,
        };
        if gesture.is_none() {
            self.stop_autoscroll();
        }
        self.gesture.set(gesture);
        // A move that ended here may have marked a pane in another tab.
        if let (Some(Gesture::Move { target, .. }), None) = (held, gesture) {
            app().move_ended(self.id, target);
        }
    }

    /// The window is closing: a pane move under way in it is over.
    pub(super) fn drop_move(&self) {
        if let Some(Gesture::Move { target, .. }) = self.gesture.get() {
            self.gesture.set(None);
            app().move_ended(self.id, target);
        }
    }

    /// The pane picked up in this window, and where it would go.
    pub(crate) fn pane_move(&self) -> Option<(PaneId, Option<(PaneId, Zone)>)> {
        match self.gesture.get() {
            Some(Gesture::Move { source, target }) => Some((source, target)),
            _ => None,
        }
    }

    /// Escape while a pane is picked up puts it back, and shows its tab
    /// again if the drag had gone to another. True when it did.
    pub(crate) fn cancel_move(&self) -> bool {
        let Some(Gesture::Move { target, .. }) = self.gesture.get() else {
            return false;
        };
        self.gesture.set(None);
        self.update_quads();
        self.request_frame();
        self.hover();
        let app = app();
        app.show_tab(self.id);
        app.move_ended(self.id, target);
        true
    }

    /// Where a picked-up pane would go if let go where the pointer is: a
    /// zone of another pane in this window, another tab, or another open
    /// Tern window.
    fn move_target(&self, source: PaneId, input: &MouseInput) -> Option<(PaneId, Zone)> {
        let app = app();
        if let Some(other) = app.window_under_move(self, input.at) {
            let at = app.point_in(self, &other, input.at)?;
            return other.hosts_move(at, input.mods);
        }
        if app.tab_shown_for(self.id).is_some() {
            return None;
        }
        self.drop_spot(input.at).filter(|&(id, _)| id != source)
    }

    /// The pane under a point and the zone of it the point is in: where a
    /// pane let go there would land.
    fn drop_spot(&self, at: Point) -> Option<(PaneId, Zone)> {
        if !self.area().contains(at) {
            return None;
        }
        self.spot_at(at).map(|s| (s.id, drop_zone(s.rect, at)))
    }

    /// A pane picked up in another tab is over this window at a point: the
    /// pointer is the hand that holds it. Where the pane would land here.
    pub(crate) fn hosts_move(&self, at: Point, mods: Mods) -> Option<(PaneId, Zone)> {
        self.pointer_at.set(Some((at, mods)));
        self.set_cursor(CursorStyle::Grab);
        self.drop_spot(at)
    }

    /// A pane move that came over this window from another tab is over:
    /// the pointer gets its shape for where it is.
    pub(crate) fn release_grab(&self) {
        if self.cursor.get() == CursorStyle::Grab && self.gesture.get().is_none() {
            self.set_cursor(CursorStyle::Text);
            self.hover();
        }
    }

    /// The moving end of a selection follows the pointer; past the top or
    /// bottom of the pane, the view scrolls too.
    fn drag_select(&self, s: Selecting, at: Point) {
        let Some(spot) = self.spot(s.pane) else {
            self.gesture.set(None);
            return;
        };
        let pos = self.cell_of(&spot, at);
        self.with_pane(s.pane, |p| {
            if let Some(anchor) = s.anchor {
                p.session
                    .selection_start(anchor.row, anchor.col, anchor.side, s.kind);
            }
            p.session.selection_update(pos.row, pos.col, pos.side);
        });
        self.gesture
            .set(Some(Gesture::Select(Selecting { anchor: None, ..s })));
        self.pane_changed(s.pane);
        if pos.row < 0 || pos.row >= i32::from(spot.grid.1) {
            self.start_autoscroll();
        } else {
            self.stop_autoscroll();
        }
    }

    fn start_autoscroll(&self) {
        let mut timer = self.autoscroll.borrow_mut();
        if timer.is_some() {
            return;
        }
        let id = self.id;
        let block = RcBlock::new(move |_: NonNull<NSTimer>| {
            if let Some(state) = app().state(id) {
                state.autoscroll_step();
            }
        });
        // SAFETY: the block only runs on the main thread, where the timer is
        // scheduled.
        let t = unsafe {
            NSTimer::timerWithTimeInterval_repeats_block(AUTOSCROLL_INTERVAL, true, &block)
        };
        // SAFETY: the main run loop, on the main thread; common modes keep it
        // going while AppKit tracks the mouse.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&t, NSRunLoopCommonModes) };
        *timer = Some(t);
    }

    pub(super) fn stop_autoscroll(&self) {
        if let Some(t) = self.autoscroll.take() {
            t.invalidate();
        }
    }

    /// One step of scrolling while a selection is held past an edge: more
    /// lines the farther past it the pointer is.
    fn autoscroll_step(&self) {
        let (Some(Gesture::Select(s)), Some((at, _))) = (self.gesture.get(), self.pointer_at.get())
        else {
            self.stop_autoscroll();
            return;
        };
        let Some(spot) = self.spot(s.pane) else {
            self.stop_autoscroll();
            return;
        };
        let pos = self.cell_of(&spot, at);
        let rows = i32::from(spot.grid.1);
        let lines = if pos.row < 0 {
            (-pos.row).min(AUTOSCROLL_MAX_LINES)
        } else if pos.row >= rows {
            -(pos.row - rows + 1).min(AUTOSCROLL_MAX_LINES)
        } else {
            self.stop_autoscroll();
            return;
        };
        self.with_pane(s.pane, |p| {
            p.session.scroll(Scroll::Lines(lines));
            p.session.selection_update(pos.row, pos.col, pos.side);
        });
        self.pane_changed(s.pane);
    }

    /// Whether a report for this cell would be news: motion is reported
    /// once per cell, as xterm does.
    fn newly_reported(&self, pane: PaneId, pos: CellPos, grid: (u16, u16)) -> bool {
        let (col, row) = clamp_to_grid(pos, grid);
        self.reported.replace(Some((pane, col, row))) != Some((pane, col, row))
    }

    fn report(&self, spot: &Spot, kind: Report, button: MouseButton, pos: CellPos, mods: Mods) {
        let (col, row) = clamp_to_grid(pos, spot.grid);
        if kind != Report::Motion {
            self.reported.set(Some((spot.id, col, row)));
        }
        let event = MouseEvent {
            kind,
            button,
            col,
            row,
            mods,
        };
        if let Some(bytes) = encode_mouse(event, spot.modes) {
            self.with_pane(spot.id, |p| p.session.write(&bytes));
        }
    }

    /// Motion with no button held, for programs that asked for all motion.
    fn report_motion(&self, input: &MouseInput) {
        let Some(spot) = self.spot_at(input.at) else {
            return;
        };
        if spot.modes.mouse != MouseTracking::Motion || input.mods.contains(Mods::SHIFT) {
            return;
        }
        let pos = self.cell_of(&spot, input.at);
        if self.newly_reported(spot.id, pos, spot.grid) {
            self.report(&spot, Report::Motion, MouseButton::None, pos, input.mods);
        }
    }

    /// The link or local file path under a cell: an OSC 8 link, a web address,
    /// or an existing local file path (`path:line:col`).
    fn link_at(&self, spot: &Spot, pos: CellPos) -> Option<links::ClickTarget> {
        let row = u16::try_from(pos.row).ok().filter(|&r| r < spot.grid.1)?;
        self.with_pane(spot.id, |p| {
            if let Some(url) = p
                .session
                .hyperlink_at(row, pos.col)
                .or_else(|| links::url_at(&p.frame, row, pos.col))
            {
                return Some(links::ClickTarget::Url(url));
            }
            let cwd = p.session.cwd();
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            links::path_at(&p.frame, row, pos.col, cwd.as_deref(), home.as_deref())
                .map(links::ClickTarget::File)
        })
        .flatten()
    }

    /// Prepares a right-click context menu for the pane at `at`, unless a
    /// mouse-reporting program wants right-clicks (Shift overrides the TUI).
    pub(crate) fn pane_context_at(&self, at: Point, mods: Mods) -> Option<crate::app::PaneContext> {
        let spot = self.spot_at(at)?;
        if wants_mouse(spot.modes) && !mods.contains(Mods::SHIFT) {
            return None;
        }
        self.focus_pane(spot.id);
        let pos = self.cell_of(&spot, at);
        let target = self.link_at(&spot, pos);
        let (has_selection, pane_count) = self.with_panes(|panes| {
            let sel = panes
                .iter()
                .find(|p| p.id == spot.id)
                .is_some_and(|p| p.session.has_selection());
            (sel, panes.len())
        });
        Some(crate::app::PaneContext {
            pane: spot.id,
            target,
            has_selection,
            pane_count,
            is_zoomed: self.is_zoomed(),
        })
    }

    /// The wheel or trackpad over a pane: reported to a program that has the
    /// mouse, arrows to a full-screen program that asked for them, and
    /// otherwise a scroll through the history.
    pub(crate) fn wheel(&self, input: &ScrollInput) {
        let Some(spot) = self.spot_at(input.at) else {
            return;
        };
        let (pane, mut rest) = self.wheel_rest.get();
        if pane != spot.id {
            rest = 0.0;
        }
        let line_pt = f64::from(self.metrics.get().cell_h) / self.scale.get();
        let lines = wheel_lines(input, &mut rest, line_pt);
        self.wheel_rest.set((spot.id, rest));
        if lines == 0 {
            return;
        }
        if wants_mouse(spot.modes) && !input.mods.contains(Mods::SHIFT) {
            let pos = self.cell_of(&spot, input.at);
            let button = if lines > 0 {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            for _ in 0..lines.abs().min(MAX_WHEEL_REPORTS) {
                self.report(&spot, Report::Wheel, button, pos, input.mods);
            }
        } else if spot.modes.alt_screen && spot.modes.alternate_scroll {
            let bytes = alternate_scroll_bytes(lines, spot.modes);
            self.with_pane(spot.id, |p| p.session.write(&bytes));
        } else {
            self.with_pane(spot.id, |p| p.session.scroll(Scroll::Lines(lines)));
            self.pane_changed(spot.id);
        }
    }

    /// The modifier keys changed: Command over a link shows the hand.
    pub(crate) fn modifiers_changed(&self, mods: Mods) {
        if let Some((at, _)) = self.pointer_at.get() {
            self.pointer_at.set(Some((at, mods)));
            self.hover();
        }
    }

    /// The pointer's shape for where it is: resize arrows on a divider, a
    /// hand on a link while Command is held, an arrow over a program that
    /// has the mouse, and the I-beam over text.
    pub(super) fn hover(&self) {
        let Some((at, mods)) = self.pointer_at.get() else {
            return;
        };
        let style = match self.gesture.get() {
            Some(Gesture::Move { .. }) => CursorStyle::Grab,
            Some(_) => return,
            None => match self.divider_at(at) {
                Some(d) if d.axis == Axis::Horizontal => CursorStyle::ColumnResize,
                Some(_) => CursorStyle::RowResize,
                None => match self.spot_at(at) {
                    Some(spot) => {
                        let link = mods.contains(Mods::SUPER)
                            && !mods.contains(Mods::ALT)
                            && self.link_at(&spot, self.cell_of(&spot, at)).is_some();
                        if link {
                            CursorStyle::Pointer
                        } else if wants_mouse(spot.modes) && !mods.contains(Mods::SHIFT) {
                            CursorStyle::Arrow
                        } else {
                            CursorStyle::Text
                        }
                    }
                    None => CursorStyle::Text,
                },
            },
        };
        self.set_cursor(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> CellMetrics {
        CellMetrics {
            cell_w: 8,
            cell_h: 16,
            baseline: 12,
            underline_top: 14,
            underline_thickness: 1,
            strike_top: 8,
            stroke: 1,
        }
    }

    fn pos(row: i32, col: u16, side: Side) -> CellPos {
        CellPos { row, col, side }
    }

    #[test]
    fn points_become_cells_and_halves() {
        let o = Point { x: 10, y: 20 };
        let m = metrics();
        let at = |x, y| cell_at(o, (5, 3), &m, Point { x, y });
        assert_eq!(at(10, 20), pos(0, 0, Side::Left));
        assert_eq!(at(13, 35), pos(0, 0, Side::Left));
        assert_eq!(at(14, 36), pos(1, 0, Side::Right));
        assert_eq!(at(18, 20), pos(0, 1, Side::Left));
        // In the padding or past the grid: the nearest column's outer half.
        assert_eq!(at(2, 20), pos(0, 0, Side::Left));
        assert_eq!(at(60, 20), pos(0, 4, Side::Right));
        // Above and below the grid: rows outside it.
        assert_eq!(at(10, 19), pos(-1, 0, Side::Left));
        assert_eq!(at(10, 4), pos(-1, 0, Side::Left));
        assert_eq!(at(10, 3), pos(-2, 0, Side::Left));
        assert_eq!(at(10, 68), pos(3, 0, Side::Left));
        assert_eq!(clamp_to_grid(pos(-4, 2, Side::Left), (5, 3)), (2, 0));
        assert_eq!(clamp_to_grid(pos(9, 2, Side::Left), (5, 3)), (2, 2));
    }

    fn wheel(dy: f64, precise: bool, phase: usize) -> ScrollInput {
        ScrollInput {
            at: Point::default(),
            dx: 0.0,
            dy,
            precise,
            phase,
            momentum: 0,
            mods: Mods::NONE,
        }
    }

    #[test]
    fn wheel_notches_scroll_three_lines_and_trackpads_add_up() {
        let mut rest = 0.0;
        assert_eq!(wheel_lines(&wheel(1.0, false, 0), &mut rest, 16.0), 3);
        // A fraction of a notch is a fraction of three lines, kept until
        // it makes a whole one; turning back forgets it.
        assert_eq!(wheel_lines(&wheel(0.1, false, 0), &mut rest, 16.0), 0);
        assert_eq!(wheel_lines(&wheel(0.3, false, 0), &mut rest, 16.0), 1);
        assert_eq!(wheel_lines(&wheel(-0.1, false, 0), &mut rest, 16.0), 0);
        assert_eq!(wheel_lines(&wheel(2.4, false, 0), &mut rest, 16.0), 7);
        assert_eq!(wheel_lines(&wheel(0.0, false, 0), &mut rest, 16.0), 0);
        // Precise deltas are points, carried over until they make a line.
        assert_eq!(
            wheel_lines(&wheel(10.0, true, PHASE_BEGAN), &mut rest, 16.0),
            0
        );
        assert_eq!(wheel_lines(&wheel(10.0, true, 0), &mut rest, 16.0), 1);
        assert_eq!(rest, 0.25);
        assert_eq!(wheel_lines(&wheel(-40.0, true, 0), &mut rest, 16.0), -2);
        assert_eq!(rest, -0.5);
        // A new gesture forgets what was left.
        assert_eq!(
            wheel_lines(&wheel(14.0, true, PHASE_BEGAN), &mut rest, 16.0),
            0
        );
        assert_eq!(rest, 0.875);
        assert_eq!(
            wheel_lines(&wheel(f64::INFINITY, true, 0), &mut rest, 16.0),
            1_000_000
        );
        assert_eq!(rest, 0.0);
    }
}
