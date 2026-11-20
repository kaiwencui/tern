//! Handing a pane from one window to another, for a pane dragged to another
//! tab, and all of a window's panes, for a tab dragged onto a pane. The
//! session, and so the program in it, is the same before and after: only the
//! window that lays the pane out and draws it changes.

use std::cell::RefCell;
use std::sync::Arc;

use tern_layout::{drop_zone, Axis, Layout, LayoutError, Zone};
use tern_types::{PaneId, Point};

use super::WindowState;
use crate::app::app;
use crate::pane::Pane;

/// Takes a pane out of a window's list.
fn pull(panes: &RefCell<Vec<Pane>>, id: PaneId) -> Option<Pane> {
    let mut panes = panes.borrow_mut();
    let i = panes.iter().position(|p| p.id == id)?;
    Some(panes.remove(i))
}

impl WindowState {
    /// Moves one of this window's panes to a zone of a pane in another
    /// window, as for a pane dropped there, and focuses it there. An edge
    /// zone takes the pane out of this window (its neighbor fills the gap)
    /// and puts it on that side of the target, half and half; the middle
    /// swaps the two panes between the windows. True when the pane was this
    /// window's last, so the window should close. A refused move changes
    /// neither window.
    pub(crate) fn send_pane(
        &self,
        id: PaneId,
        to: &WindowState,
        target: PaneId,
        zone: Zone,
    ) -> Result<bool, LayoutError> {
        // Both layouts are worked out before either window changes.
        let mut from_layout = self.layout.borrow().clone();
        let mut to_layout = to.layout.borrow().clone();
        let swap = zone == Zone::Center;
        let emptied = if swap {
            to_layout.replace(target, id)?;
            to_layout.set_focus(id);
            from_layout.replace(id, target)?;
            false
        } else {
            to_layout.insert(target, zone, id)?;
            !from_layout.take(id)?
        };
        let (from_focus, to_focus) = (self.focused_pane_id(), to.focused_pane_id());
        let Some(moved) = pull(&self.panes, id) else {
            return Err(LayoutError::NotFound(id));
        };
        let back = if swap {
            let Some(back) = pull(&to.panes, target) else {
                self.panes.borrow_mut().push(moved);
                return Err(LayoutError::NotFound(target));
            };
            Some(back)
        } else {
            None
        };
        // A pane that leaves a window with the keyboard loses the focus
        // there; the window it arrives in decides afresh.
        if self.focused.get() && from_focus == Some(id) {
            moved.session.focus(false);
        }
        if let Some(back) = &back
            && to.focused.get() && to_focus == Some(target) {
                back.session.focus(false);
            }
        *self.layout.borrow_mut() = from_layout;
        *to.layout.borrow_mut() = to_layout;
        to.adopt(moved);
        self.pane_closed(id);
        if let Some(back) = back {
            self.adopt(back);
            to.pane_closed(target);
        }
        if !emptied {
            self.settle(from_focus);
        }
        to.settle(to_focus);
        Ok(emptied)
    }

    /// Moves every pane of this window to a zone of a pane in another
    /// window, as for a tab dropped there. The panes go beside the target
    /// on that side, half and half, arranged among themselves as they were
    /// here, and the one focused here is focused there. This window is
    /// left without panes, so it should close. A refused move changes
    /// neither window.
    pub(crate) fn send_all(
        &self,
        to: &WindowState,
        target: PaneId,
        zone: Zone,
    ) -> Result<(), LayoutError> {
        // The layout is worked out before either window changes.
        let from_layout = self.layout.borrow().clone();
        let mut to_layout = to.layout.borrow().clone();
        to_layout.insert_layout(target, zone, &from_layout)?;
        let (from_focus, to_focus) = (self.focused_pane_id(), to.focused_pane_id());
        let moved = std::mem::take(&mut *self.panes.borrow_mut());
        // As in send_pane: the window the panes arrive in decides afresh
        // which of them has the keyboard.
        if self.focused.get()
            && let Some(pane) = moved.iter().find(|p| Some(p.id) == from_focus) {
                pane.session.focus(false);
            }
        {
            let mut layout = self.layout.borrow_mut();
            for pane in &moved {
                // Every one of them is in it.
                let _ = layout.close(pane.id);
            }
        }
        *to.layout.borrow_mut() = to_layout;
        for pane in moved {
            let id = pane.id;
            to.adopt(pane);
            self.pane_closed(id);
        }
        to.settle(to_focus);
        Ok(())
    }

    /// Where a tab dragged over this window would dock if let go at a
    /// point: the pane under the point and the edge of it the point is at,
    /// when there is room beside the pane for every pane of the tab. The
    /// middle of a pane is nowhere, and there the tab stays a tab.
    pub(crate) fn dock_spot(&self, at: Point, tab: &WindowState) -> Option<(PaneId, Zone)> {
        let area = self.area();
        if !area.contains(at) {
            return None;
        }
        let target = self.pane_at(at)?;
        let zone = drop_zone(self.with_pane(target, |p| p.rect)?, at);
        let incoming = tab.layout.borrow();
        if incoming.is_empty() {
            return None;
        }
        // The middle is refused here, as is a tab over its own window.
        let mut docked = self.layout.borrow().clone();
        docked.insert_layout(target, zone, &incoming).ok()?;
        // No pane that moves or makes room ends up too small to use.
        let (min_w, min_h) = (
            self.min_pane_px(Axis::Horizontal),
            self.min_pane_px(Axis::Vertical),
        );
        docked
            .rects(area, self.divider_px())
            .iter()
            .filter(|(id, _)| *id == target || incoming.contains(*id))
            .all(|(_, r)| r.w >= min_w && r.h >= min_h)
            .then_some((target, zone))
    }

    /// Replaces this window's single initial pane layout with a restored
    /// multi-pane split tree and adopts `extra` panes in visual order.
    pub(crate) fn restore_splits(&self, mut layout: Layout, extra: Vec<Pane>) {
        let old_ids = layout.panes();
        let Some(first_id) = self.panes.borrow().first().map(|p| p.id) else {
            return;
        };
        if old_ids.len() != 1 + extra.len() {
            return;
        }
        let mut new_ids = Vec::with_capacity(old_ids.len());
        new_ids.push(first_id);
        new_ids.extend(extra.iter().map(|p| p.id));

        const TAG: PaneId = 1 << 56;
        for &old in &old_ids {
            if layout.replace(old, old | TAG).is_err() {
                return;
            }
        }
        for (&old, &new) in old_ids.iter().zip(&new_ids) {
            if layout.replace(old | TAG, new).is_err() {
                return;
            }
        }
        *self.layout.borrow_mut() = layout;
        for pane in extra {
            self.adopt(pane);
        }
        self.settle(None);
    }

    /// Takes in a pane from another window: its session asks this window
    /// for frames from now on, and the renderer builds it anew where it is.
    fn adopt(&self, pane: Pane) {
        pane.events.set_frames(Arc::clone(&self.frames));
        app().renderer().forget_pane(pane.id);
        self.panes.borrow_mut().push(pane);
    }

    /// After panes came or went: every pane gets its place, and the focus,
    /// the title and the screen follow.
    fn settle(&self, old_focus: Option<PaneId>) {
        self.relayout();
        self.focus_moved(old_focus);
        self.mark_dirty();
    }
}
