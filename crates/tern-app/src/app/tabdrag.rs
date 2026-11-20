//! Docking a tab. A tab dragged out of its tab bar and let go over the edge
//! of a pane joins that pane's tab: its panes go beside the pane, on the
//! side that was highlighted, arranged as they were, and its own tab is
//! gone. Let go anywhere else, the tab does what AppKit has always done
//! with it: it becomes a window, or moves along or into a tab bar.
//!
//! The tab bar and the drag are AppKit's. A press on a tab does not return
//! until the mouse is let go, and the events in between reach none of the
//! app's windows. So the window notes the press on its way in (see
//! TabWindow), a timer looks at where the pointer is while AppKit follows
//! it, and when the press returns the tab docks if it was let go over a
//! pane's edge. By then AppKit has made the tab a window of its own, not
//! yet visible, which closes once its panes have left it.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSWindow, NSWindowOrderingMode,
};
use objc2_foundation::{NSPoint, NSRunLoop, NSRunLoopCommonModes, NSTimer};
use tern_layout::Zone;
use tern_types::PaneId;

use super::{app, App};

/// How often, in seconds, the pointer is looked at while a tab is held.
const LOOK_INTERVAL: f64 = 1.0 / 30.0;
/// The most windows looked through, front to back, for the one a point is
/// over.
const MAX_WINDOWS_DEEP: usize = 64;
/// The left button in NSEvent's pressedMouseButtons.
const LEFT_BUTTON: usize = 1;

/// A tab the mouse holds, from the press on it until the press returns.
pub(super) struct TabDrag {
    /// The window whose tab it is.
    tab: u64,
    /// Looks at the pointer while AppKit has the mouse.
    timer: Retained<NSTimer>,
    /// Where the tab would dock if let go now.
    over: Option<Dock>,
    /// Started from the full-screen toolbar monitor rather than `TabWindow::sendEvent:`.
    polled_release: bool,
}

/// Where a tab docks: beside a pane in another window, or merged into a
/// single-tab window's titlebar as a new tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dock {
    Split {
        window: u64,
        pane: PaneId,
        zone: Zone,
    },
    Tab {
        window: u64,
    },
}

impl Dock {
    fn window(self) -> u64 {
        match self {
            Dock::Split { window, .. } | Dock::Tab { window } => window,
        }
    }
}

impl App {
    /// A mouse press on its way to a window. True when it is a left press
    /// on a tab, which may drag the tab: the pointer is watched from here
    /// until tab_released.
    pub fn tab_pressed(&self, window: &NSWindow, event: &NSEvent) -> bool {
        self.tab_pressed_with_mode(window, event, false)
    }

    pub fn tab_pressed_polled(&self, window: &NSWindow, event: &NSEvent) -> bool {
        self.tab_pressed_with_mode(window, event, true)
    }

    fn tab_pressed_with_mode(
        &self,
        window: &NSWindow,
        event: &NSEvent,
        polled_release: bool,
    ) -> bool {
        // With Control the press opens the tab's menu.
        if event.r#type() != NSEventType::LeftMouseDown
            || event
                .modifierFlags()
                .contains(NSEventModifierFlags::Control)
            || self.tab_drag.borrow().is_some()
        {
            return false;
        }
        let Some(tab) = self.tab_under(window, event.locationInWindow()) else {
            return false;
        };
        let block = RcBlock::new(|_: NonNull<NSTimer>| app().tab_drag_moved());
        // SAFETY: the block only runs on the main thread, where the timer is
        // scheduled.
        let timer =
            unsafe { NSTimer::timerWithTimeInterval_repeats_block(LOOK_INTERVAL, true, &block) };
        // SAFETY: the main run loop, on the main thread; common modes keep it
        // going while AppKit tracks the mouse.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        *self.tab_drag.borrow_mut() = Some(TabDrag {
            tab,
            timer,
            over: None,
            polled_release,
        });
        true
    }

    /// The press on a tab has returned, so AppKit is done with the mouse:
    /// a tab let go over the edge of a pane docks there.
    pub fn tab_released(&self) {
        let Some(drag) = self.tab_drag.take() else {
            return;
        };
        drag.timer.invalidate();
        let stand_in = self.tab_pointer.take().map(|(_, at)| at);
        // A press that returned with the button still down was called off.
        let let_go = stand_in.is_some() || NSEvent::pressedMouseButtons() & LEFT_BUTTON == 0;
        let dock = if let_go {
            self.dock_at(drag.tab, stand_in)
        } else {
            None
        };
        // The drag is over, and its mark goes.
        self.dock_marks_changed([drag.over, None]);
        if let Some(dock) = dock {
            self.dock_tab(drag.tab, dock);
        }
    }

    /// The timer while a tab is held: where the tab would dock follows the
    /// pointer, and so does the mark that shows it.
    fn tab_drag_moved(&self) {
        let held = self
            .tab_drag
            .borrow()
            .as_ref()
            .map(|d| (d.tab, d.over, d.polled_release));
        let Some((tab, was, polled_release)) = held else {
            return;
        };
        let stand_in = self.tab_pointer.get().map(|(_, at)| at);
        // With the button up the press is about to return, or was no drag.
        let down = stand_in.is_some() || NSEvent::pressedMouseButtons() & LEFT_BUTTON != 0;
        if !down && polled_release {
            self.tab_released();
            return;
        }
        let now = if down {
            self.dock_at(tab, stand_in)
        } else {
            None
        };
        if now == was {
            return;
        }
        if let Some(drag) = self.tab_drag.borrow_mut().as_mut() {
            drag.over = now;
        }
        self.dock_marks_changed([was, now]);
    }

    /// The windows whose marks for a held tab changed draw again.
    fn dock_marks_changed(&self, docks: [Option<Dock>; 2]) {
        let mut told = None;
        for dock in docks.into_iter().flatten() {
            let win = dock.window();
            if told == Some(win) {
                continue;
            }
            told = Some(win);
            if let Some(state) = self.state(win) {
                state.marks_changed();
            }
        }
    }

    /// Where a held tab would dock as a split in a window, which the window marks.
    pub fn tab_dock_mark(&self, id: u64) -> Option<(PaneId, Zone)> {
        match self.tab_drag.borrow().as_ref()?.over? {
            Dock::Split { window, pane, zone } if window == id => Some((pane, zone)),
            _ => None,
        }
    }

    /// Whether a held tab is poised over `id`'s titlebar to merge as a tab.
    pub fn tab_merge_mark(&self, id: u64) -> bool {
        matches!(
            self.tab_drag.borrow().as_ref().and_then(|d| d.over),
            Some(Dock::Tab { window }) if window == id
        )
    }

    /// The window whose tab the mouse holds.
    pub fn tab_dragged(&self) -> Option<u64> {
        self.tab_drag.borrow().as_ref().map(|d| d.tab)
    }

    /// Where the automation socket has the pointer while it drags a tab,
    /// on the screen, and the number of the window its events go to.
    pub fn tab_pointer(&self) -> Option<(isize, NSPoint)> {
        self.tab_pointer.get()
    }

    /// The automation socket drags a tab: the pointer is where it says,
    /// not where the user's mouse is, until the tab is let go.
    pub fn set_tab_pointer(&self, pointer: (isize, NSPoint)) {
        self.tab_pointer.set(Some(pointer));
    }

    /// Where a tab would dock if let go where the pointer is: the edge of a
    /// pane in the window showing there, or nowhere.
    fn dock_at(&self, tab: u64, stand_in: Option<NSPoint>) -> Option<Dock> {
        let at = stand_in.unwrap_or_else(NSEvent::mouseLocation);
        let from = self.state(tab)?;
        let (id, window) = self.terminal_at(at, stand_in.is_some())?;
        if id == tab {
            return None;
        }
        // A question on either window is answered first.
        if window.attachedSheet().is_some() || from.window()?.attachedSheet().is_some() {
            return None;
        }
        let view = self.view_of(&window)?;
        let in_view = view.convertPoint_fromView(window.convertPointFromScreen(at), None);
        if in_view.y < 0.0 {
            // Over another window's titlebar: if that window has no visible
            // tab bar yet, accept the drop to combine both into a tabbed window.
            let from_win = from.window()?;
            let same_group = window
                .tabbedWindows()
                .is_some_and(|tabs| tabs.iter().any(|w| std::ptr::eq(&*w, &*from_win)));
            if !same_group && !self.tab_bar_visible(&window) {
                return Some(Dock::Tab { window: id });
            }
            return None;
        }
        let (pane, zone) = self.state(id)?.dock_spot(view.to_px(in_view), &from)?;
        Some(Dock::Split {
            window: id,
            pane,
            zone,
        })
    }

    /// The terminal window showing at a point of the screen, if that is
    /// what is frontmost there. The picture of the tab that follows the
    /// pointer does not count: the window server leaves out windows that
    /// take no clicks. Behind another app's window there is nothing to drop
    /// on. The automation socket's pointer is not the user's, so it looks
    /// through other apps' windows, and finds the app's own even on a
    /// desktop that is not the one showing.
    pub(super) fn terminal_at(&self, at: NSPoint, through: bool) -> Option<(u64, Retained<NSWindow>)> {
        let ns = NSApplication::sharedApplication(self.mtm);
        let mut below = 0;
        for _ in 0..MAX_WINDOWS_DEEP {
            let number =
                NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(at, below, self.mtm);
            if number <= 0 {
                break;
            }
            match ns.windowWithWindowNumber(number) {
                Some(window) => return self.id_of(&window).map(|id| (id, window)),
                None if through => below = number,
                None => return None,
            }
        }
        if !through {
            return None;
        }
        // Nothing of the app's is on the screen there: by the app's own
        // order, the tab showing in the frontmost window around the point.
        let window = ns.orderedWindows().iter().find(|w| {
            let f = w.frame();
            self.id_of(w).is_some()
                && w.isVisible()
                && (f.origin.x..f.origin.x + f.size.width).contains(&at.x)
                && (f.origin.y..f.origin.y + f.size.height).contains(&at.y)
        })?;
        let shown = window
            .tabGroup()
            .and_then(|group| group.selectedWindow())
            .filter(|w| self.id_of(w).is_some())
            .unwrap_or(window);
        self.id_of(&shown).map(|id| (id, shown))
    }

    /// Moves a tab's panes to where the tab was let go, or merges the tab into
    /// another window's tab group.
    fn dock_tab(&self, tab: u64, dock: Dock) {
        match dock {
            Dock::Tab { window: dest_id } => {
                let (Some(from_win), Some(to_win)) = (self.window(tab), self.window(dest_id))
                else {
                    return;
                };
                if let Some(group) = from_win.tabGroup() {
                    group.removeWindow(&from_win);
                }
                to_win.addTabbedWindow_ordered(&from_win, NSWindowOrderingMode::Above);
                if let Some(group) = from_win.tabGroup() {
                    group.setSelectedWindow(Some(&from_win));
                }
                self.show(&from_win);
            }
            Dock::Split {
                window: dest_id,
                pane,
                zone,
            } => {
                let (Some(from), Some(to)) = (self.state(tab), self.state(dest_id)) else {
                    return;
                };
                let window = from.window();
                // Text still being composed in the tab was meant for where it was.
                if let Some(view) = window.as_ref().and_then(|w| self.view_of(w)) {
                    view.discard_composition();
                }
                if let Err(e) = from.send_all(&to, pane, zone) {
                    eprintln!("tern: cannot move the tab: {e}");
                    return;
                }
                // Nothing is left in the tab, wherever AppKit put it.
                if let Some(window) = window {
                    window.close();
                }
                if let Some(window) = to.window() {
                    self.show(&window);
                }
            }
        }
    }
}
