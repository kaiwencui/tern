//! Moving a pane to another tab. A pane picked up with Command-Option can be
//! dragged onto a tab in the tab bar: that tab comes up, and the pane is
//! dropped among its panes as it would be among its own window's.
//!
//! Each tab is a window, so this is where two windows' states meet. The
//! gesture stays with the window the pane was picked up in, which is hidden
//! once another tab is up. AppKit may then send the rest of the drag to that
//! window, to the one now under the pointer, or to none, so an event monitor
//! hands it to the view the pane was picked up in whichever it is.

use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType, NSWindow};
use objc2_foundation::NSPoint;
use tern_layout::Zone;
use tern_types::{PaneId, Point};

use super::{app, keep_in_front, App};
use crate::state::WindowState;

impl App {
    /// Starts handing the mouse events of pane moves to the right view (see
    /// reroute_mouse). Once, at launch; the monitor lives as long as the app.
    pub(super) fn watch_pane_moves(&self) {
        let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit passes an event that is valid for the call.
            let ev = unsafe { event.as_ref() };
            if app().intercept_mouse(ev) {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        let mask = NSEventMask::LeftMouseDown
            | NSEventMask::RightMouseDown
            | NSEventMask::OtherMouseDown
            | NSEventMask::LeftMouseDragged
            | NSEventMask::LeftMouseUp;
        // SAFETY: the block returns the event it was given, or null.
        let monitor =
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        if monitor.is_none() {
            eprintln!("tern: cannot watch the mouse; panes may not move between tabs");
        }
    }

    fn intercept_mouse(&self, event: &NSEvent) -> bool {
        match event.r#type() {
            NSEventType::LeftMouseDown
            | NSEventType::RightMouseDown
            | NSEventType::OtherMouseDown => {
                // `TabWindow::sendEvent:` handles presses on `TabWindow`s;
                // full-screen titlebar/tabbar clicks arrive on AppKit's
                // `NSToolbarFullScreenWindow` instead.
                let Some(ev_win) = event.window(self.mtm) else {
                    return false;
                };
                if self.id_of(&ev_win).is_some() {
                    return false;
                }
                if let Some(owner) = self.owner_of_bar_window(&ev_win) {
                    if self.tab_bar_event_on(&owner, &ev_win, event) {
                        return true;
                    }
                    self.tab_pressed_polled(&owner, event);
                    return false;
                }
                if self.renaming().is_some() {
                    self.finish_rename(true);
                }
                false
            }
            _ => self.reroute_mouse(event),
        }
    }

    /// The window a pane is picked up in, while a pane move is under way.
    fn mover(&self) -> Option<Rc<WindowState>> {
        self.windows
            .borrow()
            .iter()
            .find(|t| t.state.pane_move().is_some())
            .map(|t| Rc::clone(&t.state))
    }

    /// A drag or a release of the left button, before AppKit delivers it.
    /// While a pane is picked up these belong to the view it was picked up
    /// in. A window that is showing gets them from AppKit as ever; once the
    /// drag has brought up another tab, they are handed to that view here,
    /// whatever window they arrived for. True when this delivered the event,
    /// so nothing else must.
    pub fn reroute_mouse(&self, event: &NSEvent) -> bool {
        let Some(mover) = self.mover() else {
            return false;
        };
        let Some(window) = mover.window() else {
            return false;
        };
        let own = event
            .window(self.mtm)
            .is_some_and(|w| std::ptr::eq(&*w, &*window));
        if own && self.tab_shown_for(mover.id).is_none() {
            return false;
        }
        let Some(view) = self.view_of(&window) else {
            return false;
        };
        match event.r#type() {
            NSEventType::LeftMouseDragged => view.mouseDragged(event),
            NSEventType::LeftMouseUp => view.mouseUp(event),
            _ => return false,
        }
        true
    }

    /// Whether a pane is picked up in a tab that is no longer the one
    /// showing, so its mouse events are rerouted.
    pub fn pane_move_hidden(&self) -> bool {
        self.mover()
            .is_some_and(|mover| self.tab_shown_for(mover.id).is_some())
    }

    /// Whether any other window or tab exists for a pane to move into.
    pub fn has_other_windows(&self, id: u64) -> bool {
        self.windows.borrow().iter().any(|t| t.state.id != id)
    }

    /// Whether two or more separate top-level window tab groups exist.
    pub fn has_multiple_window_groups(&self) -> bool {
        let windows = self.windows.borrow();
        let Some(first) = windows.first() else {
            return false;
        };
        let first_tabs = first.window.tabbedWindows();
        windows.iter().skip(1).any(|t| {
            !first_tabs
                .as_ref()
                .is_some_and(|tabs| tabs.iter().any(|w| std::ptr::eq(&*w, &*t.window)))
        })
    }

    /// The window showing where a window is: the selected tab of its tab
    /// group, or the window itself.
    fn shown_window(&self, id: u64) -> Option<Retained<NSWindow>> {
        let window = self.window(id)?;
        let selected = window.tabGroup().and_then(|group| group.selectedWindow());
        Some(selected.unwrap_or(window))
    }

    /// The tab showing in a window's place, when the window is a tab that
    /// is not the selected one.
    pub fn tab_shown_for(&self, id: u64) -> Option<Rc<WindowState>> {
        let shown = self.id_of(&*self.shown_window(id)?)?;
        if shown == id {
            return None;
        }
        self.state(shown)
    }

    fn screen_point(&self, from: &WindowState, at: Point) -> Option<NSPoint> {
        let window = from.window()?;
        let view = self.view_of(&window)?;
        let s = from.scale();
        // The middle of the pixel, so rounding on the way cannot leave it.
        let in_view = NSPoint::new((f64::from(at.x) + 0.5) / s, (f64::from(at.y) + 0.5) / s);
        Some(window.convertPointToScreen(view.convertPoint_toView(in_view, None)))
    }

    /// A point of a window's view, in pixels, in another window's
    /// coordinates.
    fn window_point(&self, from: &WindowState, at: Point, to: &NSWindow) -> Option<NSPoint> {
        let on_screen = self.screen_point(from, at)?;
        Some(to.convertPointFromScreen(on_screen))
    }

    /// A point of one window's view as a point of another's, both in view
    /// pixels.
    pub fn point_in(&self, from: &WindowState, to: &WindowState, at: Point) -> Option<Point> {
        let window = to.window()?;
        let view = self.view_of(&window)?;
        let in_window = self.window_point(from, at, &window)?;
        Some(view.to_px(view.convertPoint_fromView(in_window, None)))
    }

    /// Another Tern window or tab under the pointer while `from` drags a pane.
    pub fn window_under_move(&self, from: &WindowState, at: Point) -> Option<Rc<WindowState>> {
        let on_screen = self.screen_point(from, at)?;
        if let Some((id, _)) = self.terminal_at(on_screen, self.records_input())
            && id != from.id
        {
            return self.state(id);
        }
        None
    }

    /// A picked-up pane is dragged at a point of its window's view. Over
    /// another tab in a tab bar (this window's or another window's), that tab
    /// comes up, so the pane can be dropped among its panes.
    pub fn show_tab_under(&self, from: &WindowState, at: Point) {
        if let Some(shown) = self.shown_window(from.id) {
            let bar_win = crate::window::bar_window(&shown);
            if let Some(pt) = self.window_point(from, at, &bar_win)
                && let Some(tab) = self.tab_under(&shown, pt)
            {
                self.show_tab(tab);
                return;
            }
        }
        if let Some(on_screen) = self.screen_point(from, at)
            && let Some((id, win)) = self.terminal_at(on_screen, self.records_input())
            && id != from.id
        {
            let bar_win = crate::window::bar_window(&win);
            let pt = bar_win.convertPointFromScreen(on_screen);
            if let Some(tab) = self.tab_under(&win, pt) {
                self.show_tab(tab);
            }
        }
    }

    /// Brings a window's tab up in its tab group, unless it is the one
    /// showing.
    pub fn show_tab(&self, id: u64) {
        let Some(window) = self.window(id) else {
            return;
        };
        let Some(group) = window.tabGroup() else {
            return;
        };
        let showing = group
            .selectedWindow()
            .is_some_and(|w| std::ptr::eq(&*w, &*window));
        if !showing {
            group.setSelectedWindow(Some(&window));
            keep_in_front(&window);
        }
    }

    /// A pane was dropped on a zone of a pane in another tab: it moves
    /// there, and a tab left without panes closes.
    pub fn move_pane_to_tab(&self, from: &WindowState, pane: PaneId, target: PaneId, zone: Zone) {
        let Some(to) = self.state_with_pane(target) else {
            return;
        };
        match from.send_pane(pane, &to, target, zone) {
            Ok(true) => {
                if let Some(window) = from.window() {
                    window.close();
                }
                if let Some(dest) = to.window() {
                    self.show(&dest);
                }
            }
            Ok(false) => {
                if let Some(dest) = to.window() {
                    self.show(&dest);
                }
            }
            Err(e) => eprintln!("tern: cannot move the pane: {e}"),
        }
    }

    /// Escape, in whichever window has the keyboard, puts back a pane picked
    /// up in another tab. True when it did.
    pub fn cancel_pane_move(&self) -> bool {
        self.mover().is_some_and(|state| state.cancel_move())
    }

    /// What a pane move under way in another window points at, which a
    /// window marks when the pane is one of its own.
    pub fn move_mark_elsewhere(&self, id: u64) -> Option<(PaneId, Zone)> {
        let mover = self.mover()?;
        if mover.id == id {
            return None;
        }
        mover.pane_move()?.1
    }

    /// What a pane move points at changed from one pane to another: the
    /// other windows that showed or now show its mark draw again.
    pub fn move_marks_changed(&self, mover: u64, targets: [Option<(PaneId, Zone)>; 2]) {
        let mut told = None;
        for (pane, _) in targets.into_iter().flatten() {
            let Some(state) = self.state_with_pane(pane) else {
                continue;
            };
            if state.id != mover && told != Some(state.id) {
                told = Some(state.id);
                state.marks_changed();
            }
        }
    }

    /// A pane move is over: its mark goes from the tab it pointed into, and
    /// the pointer gets its own shape back in the tabs it passed over.
    pub fn move_ended(&self, mover: u64, target: Option<(PaneId, Zone)>) {
        self.move_marks_changed(mover, [target, None]);
        for (state, _, _) in self.all_windows() {
            if state.id != mover {
                state.release_grab();
            }
        }
    }
}
