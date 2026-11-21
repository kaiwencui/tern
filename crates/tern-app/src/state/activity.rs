//! Native macOS tab activity indicator (`NSWindowTab.accessoryView`):
//! shows an `OSC 9;4` progress ring or spinning indicator while a command runs,
//! and a subtle completion/bell dot on unfocused tabs until selected.

use std::cell::Cell;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSControlSize, NSProgressIndicator, NSProgressIndicatorStyle,
    NSResponder, NSView,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSTimer};
use tern_core::Progress;
use tern_types::{PaneId, Rgb};

use super::WindowState;
use crate::app::app;

const ACTIVITY_CHECK_INTERVAL: Duration = Duration::from_millis(600);
const BUSY_POLL_SECS: f64 = 0.75;
/// Output arriving right after a keystroke in an interactive raw-mode app
/// (e.g. typing in Claude Code or vim) is treated as echo, not background work.
const INPUT_ECHO_GRACE: Duration = Duration::from_millis(500);
/// Interactive raw-mode/TUI apps (`!ICANON`) stay busy only while emitting
/// output; once quiet for this duration they are considered idle at their prompt.
const INTERACTIVE_QUIET_TIMEOUT: Duration = Duration::from_secs(3);

/// Matches `NSProgressIndicator` with `NSControlSize::Small` (16×16 pt) so
/// AppKit's `NSTabButton` Auto Layout places the spinner and completion dot at
/// the exact same trailing, vertically-centered slot.
const INDICATOR_SIZE: f64 = 16.0;
const DOT_DIAMETER: f64 = 6.0;

const UNREAD_DOT_COLOR: Rgb = 0x3d8af7;
const ERROR_DOT_COLOR: Rgb = 0xe06c75;
const PAUSE_DOT_COLOR: Rgb = 0xe5c07b;

struct DotIvars {
    color: Cell<Rgb>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements and DotIvars is plain data.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TernTabDotView"]
    #[ivars = DotIvars]
    struct TabDotView;

    // SAFETY: signatures match NSView's intrinsicContentSize and drawRect:.
    impl TabDotView {
        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            NSSize::new(INDICATOR_SIZE, INDICATOR_SIZE)
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let rect = centered_dot_rect(self.bounds(), DOT_DIAMETER);
            let color = self.ivars().color.get();
            let unit = |shift: u32| f64::from((color >> shift) & 0xff) / 255.0;
            NSColor::colorWithSRGBRed_green_blue_alpha(unit(16), unit(8), unit(0), 1.0).setFill();
            NSBezierPath::bezierPathWithOvalInRect(rect).fill();
        }
    }
);

impl TabDotView {
    fn new(mtm: MainThreadMarker, color: Rgb) -> Retained<Self> {
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(INDICATOR_SIZE, INDICATOR_SIZE),
        );
        let this = Self::alloc(mtm).set_ivars(DotIvars {
            color: Cell::new(color),
        });
        // SAFETY: NSView's designated initializer with an NSRect frame.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn set_color(&self, color: Rgb) {
        if self.ivars().color.replace(color) != color {
            self.setNeedsDisplay(true);
        }
    }
}

fn centered_dot_rect(bounds: NSRect, diameter: f64) -> NSRect {
    let x = bounds.origin.x + (bounds.size.width - diameter) * 0.5;
    let y = bounds.origin.y + (bounds.size.height - diameter) * 0.5;
    NSRect::new(NSPoint::new(x, y), NSSize::new(diameter, diameter))
}

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
    watch_echo: bool,
    unread: bool,
    last_check: Instant,
    poll_timer: Option<Retained<NSTimer>>,
    spinner: Option<Retained<NSProgressIndicator>>,
    dot: Option<Retained<TabDotView>>,
}

impl Default for TabActivity {
    fn default() -> Self {
        Self {
            shown: Indicator::None,
            busy: false,
            watch_echo: false,
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
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(INDICATOR_SIZE, INDICATOR_SIZE),
        );
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
        let dot = match &self.dot {
            Some(v) => {
                v.set_color(color);
                v.clone()
            }
            None => {
                let v = TabDotView::new(mtm, color);
                self.dot = Some(v.clone());
                v
            }
        };
        Retained::into_super(dot)
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
        let mut finished_active = false;
        let changed = self
            .panes
            .borrow_mut()
            .iter_mut()
            .find(|p| p.id == id)
            .is_some_and(|p| {
                let prev = p.progress;
                let was_managed = p.progress_managed;
                p.progress = progress;
                p.progress_managed = true;
                if matches!(prev, Progress::Indeterminate | Progress::Set(_))
                    && progress == Progress::Hidden
                {
                    finished_active = true;
                }
                prev != progress || !was_managed
            });
        if finished_active && !self.focused.get() {
            self.activity.borrow_mut().unread = true;
        }
        if changed {
            self.sync_tab_activity(true);
        }
    }

    /// Marks the tab as having an unread bell or notification if it is not the
    /// active key window. For interactive raw-mode apps (e.g. Claude Code
    /// signaling turn completion), also ends heuristic output-busy spinning so
    /// the completion dot shows immediately.
    pub fn mark_tab_attention(&self) {
        if self.focused.get() {
            return;
        }
        for p in self.panes.borrow_mut().iter_mut() {
            if !p.session.is_canonical_input() {
                p.last_autonomous_output = None;
                p.last_output_counter = p.session.output_counter();
            }
        }
        self.activity.borrow_mut().unread = true;
        self.sync_tab_activity(true);
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

        let (mut busy, mut watch_echo) = {
            let act = self.activity.borrow();
            (act.busy, act.watch_echo)
        };
        if check_foreground {
            let now = Instant::now();
            let mut now_busy = false;
            let mut now_watch = false;
            for p in self.panes.borrow_mut().iter_mut() {
                if p.held {
                    p.progress_managed = false;
                    p.last_autonomous_output = None;
                    continue;
                }
                let fg_present = p.session.foreground_process().is_some();
                if !fg_present {
                    p.progress_managed = false;
                }
                let counter = p.session.output_counter();
                let output_moved = counter != p.last_output_counter;
                p.last_output_counter = counter;
                let canonical = fg_present && p.session.is_canonical_input();
                let eval = eval_pane_activity(
                    now,
                    fg_present,
                    canonical,
                    p.progress_managed,
                    output_moved,
                    p.last_input,
                    p.last_autonomous_output,
                );
                p.last_autonomous_output = eval.last_autonomous_output;
                now_busy |= eval.busy;
                now_watch |= eval.watch_echo;
            }
            let mut act = self.activity.borrow_mut();
            act.last_check = now;
            if act.busy && !now_busy && !self.focused.get() {
                act.unread = true;
            }
            act.busy = now_busy;
            act.watch_echo = now_watch;
            busy = now_busy;
            watch_echo = now_watch;
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

        let should_poll = busy || watch_echo || explicit_progress != Progress::Hidden;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaneActivityEval {
    busy: bool,
    watch_echo: bool,
    last_autonomous_output: Option<Instant>,
}

/// Evaluates whether a single pane is heuristically busy:
/// - Shell prompt (`!fg_present`): idle.
/// - Program manages its own progress via `OSC 9;4` (`progress_managed`):
///   never heuristically busy (trust its explicit `OSC 9;4` state).
/// - Canonical foreground command (`cargo build`, `make`, `sleep`): busy until
///   it exits, even during silent compilation phases.
/// - Interactive raw-mode/TUI app (`claude`, `codex`, `vim`, `ssh`): busy only
///   while actively emitting output outside the post-keystroke echo window.
fn eval_pane_activity(
    now: Instant,
    fg_present: bool,
    canonical: bool,
    progress_managed: bool,
    output_moved: bool,
    last_input: Option<Instant>,
    mut last_autonomous_output: Option<Instant>,
) -> PaneActivityEval {
    if !fg_present || progress_managed {
        return PaneActivityEval {
            busy: false,
            watch_echo: false,
            last_autonomous_output: None,
        };
    }
    if canonical {
        return PaneActivityEval {
            busy: true,
            watch_echo: false,
            last_autonomous_output: None,
        };
    }
    let within_echo_grace =
        last_input.is_some_and(|t| now.saturating_duration_since(t) < INPUT_ECHO_GRACE);
    if output_moved && !within_echo_grace {
        last_autonomous_output = Some(now);
    }
    let busy = last_autonomous_output
        .is_some_and(|t| now.saturating_duration_since(t) < INTERACTIVE_QUIET_TIMEOUT);
    let watch_echo = !busy
        && last_input.is_some_and(|t| {
            now.saturating_duration_since(t) < INPUT_ECHO_GRACE + INTERACTIVE_QUIET_TIMEOUT
        });
    PaneActivityEval {
        busy,
        watch_echo,
        last_autonomous_output: busy.then_some(last_autonomous_output).flatten(),
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

    #[test]
    fn dot_is_centered_within_indicator_bounds() {
        let rect = centered_dot_rect(
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(INDICATOR_SIZE, INDICATOR_SIZE),
            ),
            DOT_DIAMETER,
        );
        assert_eq!(rect.origin.x, 5.0);
        assert_eq!(rect.origin.y, 5.0);
        assert_eq!(rect.size.width, DOT_DIAMETER);
        assert_eq!(rect.size.height, DOT_DIAMETER);

        let tall = centered_dot_rect(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(20.0, 24.0)),
            DOT_DIAMETER,
        );
        assert_eq!(tall.origin.x, 7.0);
        assert_eq!(tall.origin.y, 9.0);
    }

    #[test]
    fn interactive_apps_spin_only_while_actively_outputting() {
        let t0 = Instant::now();

        // Idle interactive TUI (e.g. Claude Code or vim sitting at its prompt).
        let idle = eval_pane_activity(t0, true, false, false, false, None, None);
        assert!(!idle.busy && !idle.watch_echo);

        // Keystroke echo within 500ms does not trigger spinner, but watches briefly.
        let echo = eval_pane_activity(
            t0 + Duration::from_millis(100),
            true,
            false,
            false,
            true,
            Some(t0),
            None,
        );
        assert!(!echo.busy);
        assert!(echo.watch_echo);

        // Sustained autonomous output after grace period triggers spinner.
        let streaming = eval_pane_activity(
            t0 + Duration::from_millis(800),
            true,
            false,
            false,
            true,
            Some(t0),
            echo.last_autonomous_output,
        );
        assert!(streaming.busy);
        assert!(streaming.last_autonomous_output.is_some());

        // Still busy during brief thinking pause (< 3s).
        let brief_pause = eval_pane_activity(
            t0 + Duration::from_millis(2500),
            true,
            false,
            false,
            false,
            Some(t0),
            streaming.last_autonomous_output,
        );
        assert!(brief_pause.busy);

        // Quiet for >= 3s after finishing -> stops spinning.
        let finished = eval_pane_activity(
            t0 + Duration::from_millis(3900),
            true,
            false,
            false,
            false,
            Some(t0),
            brief_pause.last_autonomous_output,
        );
        assert!(!finished.busy);
        assert!(!finished.watch_echo);
        assert!(finished.last_autonomous_output.is_none());
    }

    #[test]
    fn batch_commands_stay_busy_and_explicit_osc_9_4_overrides_heuristics() {
        let t0 = Instant::now();

        // Canonical batch command (cargo build, sleep) stays busy even with no output.
        let batch = eval_pane_activity(t0, true, true, false, false, None, None);
        assert!(batch.busy);

        // Program that emitted OSC 9;4 (progress_managed = true) does NOT spin
        // heuristically once it reports Progress::Hidden.
        let managed_done = eval_pane_activity(t0, true, false, true, true, None, Some(t0));
        assert!(!managed_done.busy && !managed_done.watch_echo);

        // Shell idle at prompt is never busy.
        let shell = eval_pane_activity(t0, false, true, false, true, None, Some(t0));
        assert!(!shell.busy);
    }
}
