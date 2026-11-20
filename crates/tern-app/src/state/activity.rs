//! Native macOS tab activity indicator (`NSWindowTab.accessoryView`):
//! shows an `OSC 9;4` progress ring or spinning indicator while a command runs,
//! and a subtle completion/bell dot on unfocused tabs until selected.

use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSColor, NSControlSize, NSProgressIndicator, NSProgressIndicatorStyle, NSView,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSTimer};
use objc2_quartz_core::CALayer;
use tern_core::Progress;
use tern_types::{PaneId, Rgb};

use super::WindowState;
use crate::app::app;

const ACTIVITY_CHECK_INTERVAL: Duration = Duration::from_millis(600);
const BUSY_POLL_SECS: f64 = 0.75;

const UNREAD_DOT_COLOR: Rgb = 0x3d8af7;
const ERROR_DOT_COLOR: Rgb = 0xe06c75;
const PAUSE_DOT_COLOR: Rgb = 0xe5c07b;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Indicator {
    #[default]
    None,
    Spinner,
    Determinate(u8),
    Dot(Rgb),
}

pub(super) struct TabActivity {
    shown: Indicator,
    busy: bool,
    unread: bool,
    last_check: Instant,
    poll_timer: Option<Retained<NSTimer>>,
    spinner: Option<Retained<NSProgressIndicator>>,
    dot: Option<(Retained<NSView>, Retained<CALayer>)>,
}

impl Default for TabActivity {
    fn default() -> Self {
        Self {
            shown: Indicator::None,
            busy: false,
            unread: false,
            last_check: Instant::now()
                .checked_sub(ACTIVITY_CHECK_INTERVAL)
                .unwrap_or_else(Instant::now),
            poll_timer: None,
            spinner: None,
            dot: None,
        }
    }
}

impl TabActivity {
    fn ensure_spinner(&mut self, mtm: MainThreadMarker) -> Retained<NSProgressIndicator> {
        if let Some(s) = &self.spinner {
            return s.clone();
        }
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(14.0, 14.0));
        let s = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), frame);
        s.setStyle(NSProgressIndicatorStyle::Spinning);
        s.setControlSize(NSControlSize::Small);
        s.setDisplayedWhenStopped(true);
        s.setMinValue(0.0);
        s.setMaxValue(100.0);
        self.spinner = Some(s.clone());
        s
    }

    fn ensure_dot(&mut self, mtm: MainThreadMarker, color: Rgb) -> Retained<NSView> {
        let (view, circle) = match &self.dot {
            Some((v, c)) => (v.clone(), c.clone()),
            None => {
                let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(12.0, 14.0));
                let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
                view.setWantsLayer(true);
                let circle = CALayer::new();
                circle.setFrame(CGRect::new(CGPoint::new(3.0, 4.0), CGSize::new(6.0, 6.0)));
                circle.setCornerRadius(3.0);
                if let Some(root) = view.layer() {
                    root.addSublayer(&circle);
                }
                self.dot = Some((view.clone(), circle.clone()));
                (view, circle)
            }
        };
        let unit = |shift: u32| f64::from((color >> shift) & 0xff) / 255.0;
        let ns_color = NSColor::colorWithSRGBRed_green_blue_alpha(unit(16), unit(8), unit(0), 1.0);
        let cg_color = ns_color.CGColor();
        circle.setBackgroundColor(Some(&cg_color));
        view
    }

    pub(super) fn stop(&mut self) {
        if let Some(t) = self.poll_timer.take() {
            t.invalidate();
        }
        if let Some(s) = &self.spinner {
            // SAFETY: stopAnimation: accepts a nil sender on the main thread.
            unsafe { s.stopAnimation(None) };
        }
    }
}

impl WindowState {
    pub fn set_pane_progress(&self, id: PaneId, progress: Progress) {
        let changed = self
            .panes
            .borrow_mut()
            .iter_mut()
            .find(|p| p.id == id)
            .is_some_and(|p| {
                let diff = p.progress != progress;
                p.progress = progress;
                diff
            });
        if changed {
            self.sync_tab_activity(true);
        }
    }

    /// Marks the tab as having an unread bell or notification if it is not the
    /// active key window.
    pub fn mark_tab_attention(&self) {
        if self.focused.get() {
            return;
        }
        self.activity.borrow_mut().unread = true;
        self.sync_tab_activity(false);
    }

    pub(super) fn clear_tab_unread(&self) {
        let had = {
            let mut act = self.activity.borrow_mut();
            let was = act.unread;
            act.unread = false;
            was
        };
        if had {
            self.sync_tab_activity(false);
        }
    }

    pub(super) fn check_activity_after_output(&self) {
        let due = self.activity.borrow().last_check.elapsed() >= ACTIVITY_CHECK_INTERVAL;
        if due {
            self.sync_tab_activity(true);
        }
    }

    /// Recomputes busy/progress state and updates `NSWindowTab.accessoryView`.
    pub fn sync_tab_activity(&self, check_foreground: bool) {
        let app = app();
        // Keep automated benchmarks (`-e`) and headless socket tests free of
        // extra AppKit indicator animations and timers.
        if app.records_input() || !app.options.command.is_empty() {
            return;
        }
        let Some(window) = self.window() else {
            return;
        };

        let explicit_progress = self.with_panes(|panes| {
            panes
                .iter()
                .map(|p| p.progress)
                .find(|p| *p != Progress::Hidden)
                .unwrap_or(Progress::Hidden)
        });

        let mut busy = self.activity.borrow().busy;
        if check_foreground {
            let now_busy = self.with_panes(|panes| {
                panes
                    .iter()
                    .any(|p| !p.held && p.session.foreground_process().is_some())
            });
            let mut act = self.activity.borrow_mut();
            act.last_check = Instant::now();
            if act.busy && !now_busy && !self.focused.get() {
                act.unread = true;
            }
            act.busy = now_busy;
            busy = now_busy;
        }

        let desired = desired_indicator(explicit_progress, busy, self.activity.borrow().unread);
        // Only run AppKit's spinning animation when a tab bar can actually show it.
        let show_on_tab = window.tabbedWindows().is_some_and(|t| t.len() > 1)
            || app.tab_bar_visible(&window);
        let applied = if show_on_tab { desired } else { Indicator::None };

        let mut act = self.activity.borrow_mut();
        if act.shown != applied {
            act.shown = applied;
            let tab = window.tab();
            match applied {
                Indicator::None => {
                    if let Some(s) = &act.spinner {
                        // SAFETY: stopAnimation: accepts a nil sender on the main thread.
                        unsafe { s.stopAnimation(None) };
                    }
                    tab.setAccessoryView(None);
                }
                Indicator::Spinner => {
                    let s = act.ensure_spinner(app.mtm);
                    s.setIndeterminate(true);
                    // SAFETY: startAnimation: accepts a nil sender on the main thread.
                    unsafe { s.startAnimation(None) };
                    tab.setAccessoryView(Some(&s));
                }
                Indicator::Determinate(pct) => {
                    let s = act.ensure_spinner(app.mtm);
                    // SAFETY: stopAnimation: accepts a nil sender on the main thread.
                    unsafe { s.stopAnimation(None) };
                    s.setIndeterminate(false);
                    s.setDoubleValue(f64::from(pct));
                    tab.setAccessoryView(Some(&s));
                }
                Indicator::Dot(color) => {
                    if let Some(s) = &act.spinner {
                        // SAFETY: stopAnimation: accepts a nil sender on the main thread.
                        unsafe { s.stopAnimation(None) };
                    }
                    let v = act.ensure_dot(app.mtm, color);
                    tab.setAccessoryView(Some(&v));
                }
            }
        }

        let should_poll = busy || explicit_progress != Progress::Hidden;
        if should_poll && act.poll_timer.is_none() {
            let id = self.id;
            let block = RcBlock::new(move |_: NonNull<NSTimer>| {
                if let Some(state) = crate::app::app().state(id) {
                    state.sync_tab_activity(true);
                }
            });
            // SAFETY: runs on main thread only; invalidated in `shutdown` or when idle.
            let timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_repeats_block(BUSY_POLL_SECS, true, &block)
            };
            act.poll_timer = Some(timer);
        } else if !should_poll
            && let Some(t) = act.poll_timer.take()
        {
            t.invalidate();
        }
    }

    pub(super) fn stop_activity(&self) {
        self.activity.borrow_mut().stop();
        if let Some(window) = self.window() {
            window.tab().setAccessoryView(None);
        }
    }
}

fn desired_indicator(progress: Progress, busy: bool, unread: bool) -> Indicator {
    match progress {
        Progress::Indeterminate => Indicator::Spinner,
        Progress::Set(pct) => Indicator::Determinate(pct),
        Progress::Error(_) => Indicator::Dot(ERROR_DOT_COLOR),
        Progress::Pause(_) => Indicator::Dot(PAUSE_DOT_COLOR),
        Progress::Hidden => {
            if busy {
                Indicator::Spinner
            } else if unread {
                Indicator::Dot(UNREAD_DOT_COLOR)
            } else {
                Indicator::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicator_precedence_prefers_explicit_progress_then_busy_then_unread() {
        assert_eq!(
            desired_indicator(Progress::Set(42), true, true),
            Indicator::Determinate(42)
        );
        assert_eq!(
            desired_indicator(Progress::Indeterminate, false, false),
            Indicator::Spinner
        );
        assert_eq!(
            desired_indicator(Progress::Error(80), true, false),
            Indicator::Dot(ERROR_DOT_COLOR)
        );
        assert_eq!(
            desired_indicator(Progress::Hidden, true, true),
            Indicator::Spinner
        );
        assert_eq!(
            desired_indicator(Progress::Hidden, false, true),
            Indicator::Dot(UNREAD_DOT_COLOR)
        );
        assert_eq!(
            desired_indicator(Progress::Hidden, false, false),
            Indicator::None
        );
    }
}
