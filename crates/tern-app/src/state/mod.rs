//! The state of one window (one tab): its panes and their layout, its
//! geometry, and the frame loop that draws it.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSCursor, NSWindow};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSString, NSTimer};
use objc2_metal::MTLDrawable;
use objc2_quartz_core::{kCAGravityTopLeft, CADisplayLink, CALayer, CAMetalLayer};
use tern_config::Config;
use tern_core::Scroll;
use tern_layout::{Divider, Layout};
use tern_render::{CellMetrics, PaneDraw, Present, PresentedHandler, Quad};
use tern_types::{Frame, Mods, PaneId, Point, Rect, Rgb};

use crate::app::app;
use crate::identity;
use crate::input::{InputLog, KeyState};
use crate::mainthread::post_to_main;
use crate::pane::{FrameRequest, Pane};
use crate::startup::StartupProbe;

mod activity;
mod frames;
mod latency;
mod mouse;
mod overlay;
mod preedit;
mod search;
mod splits;
mod transfer;

pub use frames::FrameTime;
use frames::FrameTimes;
use latency::LatencyProbe;
use mouse::Gesture;

/// The largest texture Metal makes on the GPUs macOS 14 runs on. A larger
/// drawable gets no frames at all, and a window can be wider than this in
/// pixels (one spanning displays, or 8192 points wide at 2x), so the layer
/// draws at this size and scales up.
const MAX_DRAWABLE: i32 = 16384;
/// The longest title kept from a program, in characters.
const MAX_TITLE_CHARS: usize = 256;
/// How often, at most, a window without a program-set title looks at its
/// pane's working directory again.
const DIRECTORY_TITLE_CHECK: Duration = Duration::from_secs(1);

/// The mouse pointer over the view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorStyle {
    Text,
    Arrow,
    Pointer,
    ColumnResize,
    RowResize,
    /// A pane picked up to move.
    Grab,
}

impl CursorStyle {
    pub const ALL: [CursorStyle; 6] = [
        CursorStyle::Text,
        CursorStyle::Arrow,
        CursorStyle::Pointer,
        CursorStyle::ColumnResize,
        CursorStyle::RowResize,
        CursorStyle::Grab,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CursorStyle::Text => "text",
            CursorStyle::Arrow => "arrow",
            CursorStyle::Pointer => "pointer",
            CursorStyle::ColumnResize => "col-resize",
            CursorStyle::RowResize => "row-resize",
            CursorStyle::Grab => "grab",
        }
    }
}

/// Everything one window owns. Lives on the main thread; the view, the
/// window delegate and the app call into it.
pub struct WindowState {
    pub id: u64,
    window: Weak<NSWindow>,
    layer: Retained<CAMetalLayer>,
    link: RefCell<Option<Retained<CADisplayLink>>>,
    /// Shared with the panes' sessions, which ask for frames through it.
    frames: Arc<FrameRequest>,
    dirty: Cell<bool>,
    occluded: Cell<bool>,
    /// The window is key: the focused pane shows a solid, blinking cursor.
    focused: Cell<bool>,
    live_resize: Cell<bool>,
    last_frame: Cell<Instant>,
    /// Counted by the presented handler on a Metal thread.
    presented_count: Arc<AtomicU64>,
    /// Set by the presented handler when Metal dropped a frame.
    dropped: Arc<AtomicBool>,
    /// Frames drawn that the compositor has not yet shown or dropped.
    in_flight: Arc<AtomicU64>,
    /// Display link callbacks in a row that skipped drawing because too
    /// many frames were in flight.
    held_back: Cell<u32>,
    presented: PresentedHandler,
    redraws: Cell<u8>,
    draws: Cell<u64>,
    startup: Option<Arc<StartupProbe>>,
    size_pt: Cell<(f64, f64)>,
    scale: Cell<f64>,
    font_size: Cell<f32>,
    metrics: Cell<CellMetrics>,
    cursor: Cell<CursorStyle>,
    layout: RefCell<Layout>,
    /// In the layout's visual order.
    panes: RefCell<Vec<Pane>>,
    /// The color the window, and so its title bar, was last given.
    chrome: Cell<Rgb>,
    /// The layout's dividers in the drawable, and the quads that draw them.
    /// Both change only when the layout or the geometry does.
    dividers: RefCell<Vec<Divider>>,
    quads: RefCell<Vec<Quad>>,
    /// What the held mouse button is doing.
    gesture: Cell<Option<Gesture>>,
    /// The pointer's last position in the view and the modifiers then.
    pointer_at: Cell<Option<(Point, Mods)>>,
    /// Typing hid the pointer; moving the mouse shows it again.
    pointer_hidden: Cell<bool>,
    /// The pane the wheel last scrolled, and the movement, in lines, not
    /// yet a whole line.
    wheel_rest: Cell<(PaneId, f64)>,
    /// Scrolls while a selection is dragged past a pane's top or bottom.
    autoscroll: RefCell<Option<Retained<NSTimer>>>,
    /// The cell a mouse report last named, so motion is reported once per cell.
    reported: Cell<Option<(PaneId, u16, u16)>>,
    /// The pane a drag from another app is over.
    drop_target: Cell<Option<PaneId>>,
    /// The next frame snapshots every pane, not only those with new output:
    /// something on the main thread changed what a pane shows.
    snapshot_all: Cell<bool>,
    /// Holds the allocation of each frame's pane list between frames.
    scratch: RefCell<Vec<PaneDraw<'static>>>,
    /// The input method's uncommitted text, empty when not composing.
    preedit: RefCell<String>,
    /// While composing: the frame drawn for the focused pane, which is its
    /// own frame with the uncommitted text over it.
    preedit_frame: RefCell<Option<(PaneId, Frame)>>,
    /// The last such frame's buffers, kept for the next composition.
    spare_frame: RefCell<Frame>,
    search: RefCell<search::SearchState>,
    blink: RefCell<Option<Retained<NSTimer>>>,
    /// The blink phase: false while a blinking cursor is hidden.
    cursor_on: Cell<bool>,
    /// Hides the cursor of an idle window without a frame being drawn: a
    /// rectangle in the background color, laid over the cursor's cell.
    cover: Retained<CALayer>,
    /// The cover is up: the frame on screen has the cursor in it, and the
    /// blink phase says hidden.
    covered: Cell<bool>,
    /// The frame last drawn shows the focused pane's cursor, solid.
    shown_on: Cell<bool>,
    /// Blink phases that have passed, for the automation socket.
    blinks: Cell<u64>,
    times: RefCell<FrameTimes>,
    /// The name the user gave the tab, which is then its title whatever
    /// its panes do.
    name: RefCell<Option<String>>,
    /// When the title last followed the working directory.
    cwd_checked: Cell<Instant>,
    /// Looks at the working directory again when the second is up.
    cwd_timer: RefCell<Option<Retained<NSTimer>>>,
    activity: RefCell<activity::TabActivity>,
    /// A key press on its way to the screen: when it was pressed, the pane
    /// it went to, and that pane's frame seq then.
    key_wait: Cell<Option<(f64, PaneId, u64)>>,
    /// Times key presses to the frames that show them.
    latency: Arc<LatencyProbe>,
    pub(crate) keys: RefCell<KeyState>,
    /// Only kept when the automation socket is on.
    pub(crate) log: Option<RefCell<InputLog>>,
}

impl WindowState {
    pub fn new(
        id: u64,
        window: &NSWindow,
        pane: Pane,
        metrics: CellMetrics,
        startup: Option<Arc<StartupProbe>>,
    ) -> WindowState {
        // Until the pane's first frame says otherwise.
        let chrome = app().config().theme.bg;
        crate::window::tint(window, chrome);
        let layer = CAMetalLayer::new();
        app().renderer().configure_layer(&layer);
        // A frame drawn for the old size stays put while the window grows,
        // instead of being stretched until the next one arrives.
        // SAFETY: reading a framework constant.
        layer.setContentsGravity(unsafe { kCAGravityTopLeft });
        let cover = CALayer::new();
        cover.setHidden(true);
        layer.addSublayer(&cover);
        let presented_count = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let latency = Arc::new(LatencyProbe::default());
        let in_flight = Arc::new(AtomicU64::new(0));
        let presented = {
            let (count, dropped, startup, latency, in_flight) = (
                Arc::clone(&presented_count),
                Arc::clone(&dropped),
                startup.clone(),
                Arc::clone(&latency),
                Arc::clone(&in_flight),
            );
            // Every drawn frame comes through here once, in drawing order,
            // so this numbers them as the window's draw count does.
            let handled = AtomicU64::new(0);
            RcBlock::new(move |d: NonNull<ProtocolObject<dyn MTLDrawable>>| {
                let frame = handled.fetch_add(1, Ordering::Relaxed) + 1;
                // Never below zero, should a frame be reported twice.
                let _ = in_flight
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
                // SAFETY: Metal passes the drawable this handler was added to.
                let at = unsafe { d.as_ref() }.presentedTime(); // Zero means the drawable was dropped instead of shown.
                latency.presented(frame, at);
                if at > 0.0 {
                    count.fetch_add(1, Ordering::Relaxed);
                    if let Some(probe) = &startup {
                        probe.presented(frame, at);
                    }
                } else {
                    dropped.store(true, Ordering::Relaxed);
                }
            })
        };
        let frames = Arc::new(FrameRequest::default());
        pane.events.set_frames(Arc::clone(&frames));
        WindowState {
            id,
            window: Weak::new(window),
            layer,
            link: RefCell::new(None),
            frames,
            dirty: Cell::new(true),
            occluded: Cell::new(false),
            focused: Cell::new(false),
            live_resize: Cell::new(false),
            last_frame: Cell::new(Instant::now()),
            presented_count,
            dropped,
            in_flight,
            held_back: Cell::new(0),
            presented,
            redraws: Cell::new(0),
            draws: Cell::new(0),
            startup,
            size_pt: Cell::new((0.0, 0.0)),
            scale: Cell::new(1.0),
            font_size: Cell::new(app().config().font_size),
            metrics: Cell::new(metrics),
            cursor: Cell::new(CursorStyle::Text),
            layout: RefCell::new(Layout::new(pane.id)),
            panes: RefCell::new(vec![pane]),
            chrome: Cell::new(chrome),
            dividers: RefCell::new(Vec::new()),
            quads: RefCell::new(Vec::new()),
            gesture: Cell::new(None),
            pointer_at: Cell::new(None),
            pointer_hidden: Cell::new(false),
            wheel_rest: Cell::new((0, 0.0)),
            autoscroll: RefCell::new(None),
            reported: Cell::new(None),
            drop_target: Cell::new(None),
            snapshot_all: Cell::new(true),
            scratch: RefCell::new(Vec::new()),
            preedit: RefCell::default(),
            preedit_frame: RefCell::new(None),
            spare_frame: RefCell::default(),
            search: RefCell::default(),
            blink: RefCell::new(None),
            cursor_on: Cell::new(true),
            cover,
            covered: Cell::new(false),
            shown_on: Cell::new(false),
            blinks: Cell::new(0),
            times: RefCell::new(FrameTimes::new()),
            name: RefCell::new(None),
            // Due at the first frame, so a new window is named at once.
            cwd_timer: RefCell::new(None),
            activity: RefCell::default(),
            key_wait: Cell::new(None),
            latency,
            cwd_checked: Cell::new(
                Instant::now()
                    .checked_sub(DIRECTORY_TITLE_CHECK)
                    .unwrap_or_else(Instant::now),
            ),
            keys: RefCell::default(),
            // Kept only for the automation socket, which reports it.
            log: app()
                .records_input()
                .then(|| RefCell::new(InputLog::default())),
        }
    }

    pub fn layer(&self) -> &CAMetalLayer {
        &self.layer
    }

    pub fn window(&self) -> Option<Retained<NSWindow>> {
        self.window.load()
    }

    // ---- geometry ----

    /// The view's size in points and the backing scale. Keeps the layer, the
    /// cell metrics and every pane's grid in step.
    pub fn set_geometry(&self, size_pt: (f64, f64), scale: f64) {
        if self.size_pt.get() == size_pt && self.scale.get() == scale {
            return;
        }
        let rescaled = self.scale.get() != scale;
        self.size_pt.set(size_pt);
        self.scale.set(scale);
        self.layer.setContentsScale(scale);
        let (w, h) = self.drawable_size();
        self.layer
            .setDrawableSize(CGSize::new(f64::from(w), f64::from(h)));
        if rescaled {
            self.metrics.set(
                app()
                    .renderer()
                    .metrics_at(self.font_size.get(), scale as f32),
            );
        }
        self.relayout();
        if self.live_resize.get() {
            // The frame for the new size goes out with the resize itself.
            self.draw(Present::WithTransaction);
        } else {
            self.mark_dirty();
        }
    }

    pub fn size_pt(&self) -> (f64, f64) {
        self.size_pt.get()
    }

    pub fn size_px(&self) -> (i32, i32) {
        let (w, h) = self.size_pt.get();
        let s = self.scale.get();
        ((w * s).round() as i32, (h * s).round() as i32)
    }

    /// The layer's drawable: the view in pixels, at least one pixel and no
    /// more than Metal makes. Panes are laid out in this space.
    pub fn drawable_size(&self) -> (u32, u32) {
        drawable_size(self.size_px())
    }

    pub fn scale(&self) -> f64 {
        self.scale.get()
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics.get()
    }

    /// The configured padding, in pixels.
    pub fn padding_px(&self) -> (i32, i32) {
        let app = app();
        let config = app.config();
        let s = self.scale.get();
        let px = |pt: u16| (f64::from(pt) * s).round() as i32;
        (px(config.window_padding_x), px(config.window_padding_y))
    }

    /// Keeps the title bar in the background color of the pane below its
    /// left end, the first one shown, which follows the theme and whatever
    /// the pane's program makes of it (OSC 11). A pane that has not been
    /// drawn yet has no colors to give.
    pub(super) fn sync_chrome(&self, panes: &[Pane]) {
        let Some(bg) = panes
            .iter()
            .find(|p| p.shown && p.frame.cols > 0)
            .map(|p| p.frame.bg)
        else {
            return;
        };
        if self.chrome.replace(bg) != bg
            && let Some(window) = self.window() {
                crate::window::tint(&window, bg);
            }
    }

    pub fn reapply_chrome(&self) {
        if let Some(window) = self.window() {
            crate::window::tint(&window, self.chrome.get());
        }
    }

    pub fn font_size(&self) -> f32 {
        self.font_size.get()
    }

    pub fn set_font_size(&self, size: f32) {
        if (self.font_size.get() - size).abs() < 0.001 {
            return;
        }
        self.font_size.set(size);
        self.font_changed();
    }

    /// The font changed: new cell metrics, and new grids to match.
    pub fn font_changed(&self) {
        self.metrics.set(
            app()
                .renderer()
                .metrics_at(self.font_size.get(), self.scale.get() as f32),
        );
        self.relayout();
        self.mark_dirty();
    }

    /// The configuration was reloaded: open panes take its colors and
    /// cursor, and the layout its padding, dividers and the font's cells.
    pub fn config_changed(&self, old: &Config, new: &Config, font: bool) {
        let theme = old.theme != new.theme;
        let cursor = (old.cursor_style, old.cursor_style_blink)
            != (new.cursor_style, new.cursor_style_blink);
        for p in self.panes.borrow().iter() {
            if theme {
                p.session.set_theme(new.theme.clone());
            }
            if cursor {
                p.session
                    .set_cursor_style(new.cursor_style, new.cursor_style_blink);
            }
        }
        if new.font_size != old.font_size {
            self.font_size.set(new.font_size);
        }
        if font {
            self.metrics.set(
                app()
                    .renderer()
                    .metrics_at(self.font_size.get(), self.scale.get() as f32),
            );
        }
        self.relayout();
        self.mark_dirty();
    }

    // ---- panes ----

    pub fn contains_pane(&self, id: PaneId) -> bool {
        self.panes.borrow().iter().any(|p| p.id == id)
    }

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.layout.borrow().focused()
    }

    /// Runs f on the focused pane.
    pub fn with_focused<R>(&self, f: impl FnOnce(&Pane) -> R) -> Option<R> {
        let id = self.focused_pane_id()?;
        self.panes.borrow().iter().find(|p| p.id == id).map(f)
    }

    /// Runs f on a pane of this window.
    pub fn with_pane<R>(&self, id: PaneId, f: impl FnOnce(&Pane) -> R) -> Option<R> {
        self.panes.borrow().iter().find(|p| p.id == id).map(f)
    }

    /// Runs f on every pane, in layout order.
    pub fn with_panes<R>(&self, f: impl FnOnce(&[Pane]) -> R) -> R {
        f(&self.panes.borrow())
    }

    /// A pane's program ended: true when the pane stays to show why.
    pub fn hold_after_exit(&self, id: PaneId, status: Option<i32>) -> bool {
        let held = self
            .panes
            .borrow_mut()
            .iter_mut()
            .find(|p| p.id == id)
            .is_some_and(|p| p.hold_after_exit(status));
        if held {
            self.pane_changed(id);
            self.update_title();
        }
        held
    }

    pub fn set_pane_title(&self, id: PaneId, title: Option<String>) {
        let title = title.map(|t| shorten_title(&t));
        if let Some(pane) = self.panes.borrow_mut().iter_mut().find(|p| p.id == id) {
            pane.title = title;
        }
        self.update_title();
    }

    /// Names the tab, or with None gives its title back to its panes.
    pub fn set_name(&self, name: Option<String>) {
        *self.name.borrow_mut() = name.map(|n| shorten_title(&n));
        self.update_title();
    }

    /// The name the user gave the tab.
    pub fn name(&self) -> Option<String> {
        self.name.borrow().clone()
    }

    /// The window's (and so the tab's) title is the name the user gave it.
    /// Without one it follows the focused pane: the title its program set,
    /// or else the name of its working directory.
    fn update_title(&self) {
        let set = self.name().or_else(|| {
            self.with_focused(|p| p.title.clone())
                .flatten()
                .filter(|t| !t.trim().is_empty())
        });
        let title = match set {
            Some(title) => title,
            None => {
                self.cwd_checked.set(Instant::now());
                self.with_focused(|p| p.session.cwd())
                    .flatten()
                    .map_or_else(|| identity::app_name().to_owned(), |dir| directory_title(&dir))
            }
        };
        if let Some(window) = self.window()
            && window.title().to_string() != title {
                window.setTitle(&NSString::from_str(&title));
            }
    }

    /// After output was drawn: a shell that set no title may have changed
    /// directory, so the title is looked at again, at most once a second;
    /// output within that second is looked at when it is up.
    pub(super) fn refresh_directory_title(&self) {
        if self.cwd_timer.borrow().is_some() || self.name.borrow().is_some() {
            return;
        }
        let wait = DIRECTORY_TITLE_CHECK.saturating_sub(self.cwd_checked.get().elapsed());
        if wait.is_zero() {
            self.title_from_directory();
            return;
        }
        let id = self.id;
        let block = RcBlock::new(move |_: NonNull<NSTimer>| {
            if let Some(state) = app().state(id) {
                state.cwd_timer.take();
                state.title_from_directory();
            }
        });
        // SAFETY: the block only runs on the main thread, where the timer
        // is scheduled.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(wait.as_secs_f64(), false, &block)
        };
        *self.cwd_timer.borrow_mut() = Some(timer);
    }

    /// Names the window after its focused pane's directory, unless the
    /// user named the tab or the pane's program set a title.
    fn title_from_directory(&self) {
        let untitled = self.name.borrow().is_none()
            && self
                .with_focused(|p| p.title.as_deref().is_none_or(|t| t.trim().is_empty()))
                .unwrap_or(false);
        if untitled {
            self.update_title();
        }
    }

    // ---- terminal actions on the focused pane ----

    pub fn scroll(&self, scroll: Scroll) {
        self.with_focused(|p| p.session.scroll(scroll));
        self.mark_dirty();
    }

    pub fn clear(&self) {
        self.with_focused(|p| p.session.clear());
        self.mark_dirty();
    }

    /// Writes bytes as if typed: the view returns to the bottom and the
    /// cursor stops blinking for a moment.
    pub fn send(&self, bytes: &[u8]) {
        if let Some(id) = self.focused_pane_id() {
            self.send_to(id, bytes);
        }
    }

    /// Writes bytes to a pane as if typed there.
    pub fn send_to(&self, id: PaneId, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.typed();
        let scrolled = self.panes.borrow().iter().find(|p| p.id == id).map(|p| {
            p.session.write(bytes);
            let back = p.frame.display_offset > 0;
            if back {
                p.session.scroll(Scroll::Bottom);
            }
            back
        });
        self.restart_blink();
        if scrolled == Some(true) {
            self.mark_dirty();
        }
    }

    pub fn select_all(&self) {
        self.with_focused(|p| p.session.select_all());
        self.mark_dirty();
    }

    pub fn select_last_output(&self) {
        if let Some(id) = self.focused_pane_id() {
            self.with_pane(id, |p| p.session.select_last_output());
            self.pane_changed(id);
            self.mark_dirty();
        }
    }

    pub fn jump_prompt(&self, delta: i32) {
        if let Some(id) = self.focused_pane_id() {
            self.with_pane(id, |p| p.session.jump_prompt(delta));
            self.pane_changed(id);
            self.mark_dirty();
        }
    }

    pub fn has_selection(&self) -> bool {
        self.with_focused(|p| p.session.has_selection())
            .unwrap_or(false)
    }

    /// Puts a pane's selection on the clipboard. The text is built on a
    /// thread of its own: a large selection takes a second or more, and
    /// meanwhile the session lets the main thread carry on without it.
    pub fn copy_selection(&self, id: PaneId) {
        let Some(session) = self.with_pane(id, |p| Arc::clone(&p.session)) else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("tern-copy".into())
            .spawn(move || {
                if let Some(text) = session.selection_text() {
                    post_to_main(false, move |_| app().clipboard().set_text(&text));
                }
            });
        if let Err(e) = spawned {
            eprintln!("tern: cannot copy: {e}");
        }
    }

    pub fn paste(&self, text: &str) {
        if let Some(id) = self.focused_pane_id() {
            self.paste_to(id, text);
        }
    }

    /// Text as a paste into a pane: bracketed when its program asked.
    pub fn paste_to(&self, id: PaneId, text: &str) {
        let bracketed = self
            .panes
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .is_some_and(|p| p.session.modes().bracketed_paste);
        let bytes = tern_input::paste_bytes(text, bracketed);
        self.send_to(id, &bytes);
    }

    // ---- focus and visibility ----

    pub fn frames_presented(&self) -> u64 {
        self.presented_count.load(Ordering::Relaxed)
    }

    pub fn draws(&self) -> u64 {
        self.draws.get()
    }

    pub fn frame_times(&self) -> std::cell::Ref<'_, FrameTimes> {
        self.times.borrow()
    }

    pub fn clear_frame_times(&self) {
        self.times.borrow_mut().clear();
    }

    pub fn link_running(&self) -> bool {
        self.link
            .borrow()
            .as_ref()
            .is_some_and(|link| !link.isPaused())
    }

    pub fn blinking(&self) -> bool {
        self.blink.borrow().is_some()
    }

    /// How many times the blink has shown or hidden the cursor.
    pub fn blinks(&self) -> u64 {
        self.blinks.get()
    }

    /// The cursor is hidden by the cover rather than by a frame without it.
    pub fn covered(&self) -> bool {
        self.covered.get()
    }

    pub fn occluded(&self) -> bool {
        self.occluded.get()
    }

    pub fn set_occluded(&self, occluded: bool) {
        self.occluded.set(occluded);
        if occluded {
            // Nothing is drawn while hidden, so the blink would only wake
            // the main thread.
            self.update_blink();
        } else {
            self.mark_dirty();
        }
    }

    /// Whether typing has hidden the pointer.
    pub fn pointer_hidden(&self) -> bool {
        self.pointer_hidden.get()
    }

    /// Typing hides the pointer until the mouse moves, when the
    /// configuration says so. Under automation only the state is kept: the
    /// pointer is the user's.
    pub fn hide_pointer_while_typing(&self) {
        let app = app();
        if !app.config().mouse_hide_while_typing || self.pointer_hidden.replace(true) {
            return;
        }
        if !app.records_input() {
            NSCursor::setHiddenUntilMouseMoves(true);
        }
    }

    /// The name of what the mouse is doing, for the automation socket.
    pub fn gesture_name(&self) -> &'static str {
        match self.gesture.get() {
            None => "none",
            Some(Gesture::Divider(_)) => "divider",
            Some(Gesture::Select(_)) => "select",
            Some(Gesture::Report { .. }) => "report",
            Some(Gesture::Move { .. }) => "move",
        }
    }

    pub fn focused(&self) -> bool {
        self.focused.get()
    }

    /// The window became key or stopped being key.
    pub fn set_focused(&self, focused: bool) {
        if self.focused.replace(focused) == focused {
            return;
        }
        if focused {
            self.clear_tab_unread();
        }
        self.with_focused(|p| p.session.focus(focused));
        self.cursor_on.set(true);
        self.mark_dirty();
    }

    pub fn set_live_resize(&self, on: bool) {
        self.live_resize.set(on);
        self.layer.setPresentsWithTransaction(on);
        self.mark_dirty();
    }

    // ---- other ----

    pub fn cursor(&self) -> CursorStyle {
        self.cursor.get()
    }

    pub fn set_cursor(&self, style: CursorStyle) {
        if self.cursor.replace(style) == style {
            return;
        }
        if let Some(window) = self.window()
            && let Some(view) = window.contentView() {
                window.invalidateCursorRectsForView(&view);
            }
    }

    /// Where input method windows should appear, in view pixels: the
    /// focused pane's cursor cell.
    pub fn ime_cursor_rect(&self) -> Rect {
        let m = self.metrics.get();
        let (w, h) = (m.cell_w as i32, m.cell_h as i32);
        self.with_focused(|p| {
            let (row, col) = p
                .frame
                .cursor_point
                .map_or((0, 0), |(row, col)| (i32::from(row), i32::from(col)));
            Rect::new(p.origin.x + col * w, p.origin.y + row * h, w, h)
        })
        .unwrap_or(Rect::new(0, 0, w, h))
    }

    /// Stops the frame loop and the sessions; the window is closing.
    pub fn shutdown(&self) {
        if let Some(link) = self.link.take() {
            // The link retains the view; invalidating breaks that cycle.
            link.invalidate();
        }
        for timer in [self.blink.take(), self.cwd_timer.take()]
            .into_iter()
            .flatten()
        {
            timer.invalidate();
        }
        self.stop_autoscroll();
        self.stop_activity();
        self.drop_move();
        let panes = std::mem::take(&mut *self.panes.borrow_mut());
        let app = app();
        let mut renderer = app.renderer();
        for pane in panes {
            renderer.forget_pane(pane.id);
        }
    }
}

/// A title cut to a length a tab can show; programs may send megabytes.
fn shorten_title(title: &str) -> String {
    match title.char_indices().nth(MAX_TITLE_CHARS) {
        Some((cut, _)) => format!("{}…", &title[..cut]),
        None => title.to_string(),
    }
}

/// A working directory as a window title: its name, or ~ for home.
fn directory_title(dir: &std::path::Path) -> String {
    if std::env::var_os("HOME").is_some_and(|home| dir == std::path::Path::new(&home)) {
        return "~".into();
    }
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The layer's drawable size for a view of this many pixels: at least one
/// pixel, and no more than Metal can make.
fn drawable_size((w, h): (i32, i32)) -> (u32, u32) {
    let clamp = |v: i32| v.clamp(1, MAX_DRAWABLE) as u32;
    (clamp(w), clamp(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLPixelFormat};

    #[test]
    fn titles_are_kept_short_and_directories_named() {
        assert_eq!(shorten_title("vim"), "vim");
        let long = shorten_title(&"x".repeat(100_000));
        assert_eq!(long.chars().count(), MAX_TITLE_CHARS + 1);
        assert!(long.ends_with('…'));
        assert_eq!(directory_title(std::path::Path::new("/usr/share")), "share");
        assert_eq!(directory_title(std::path::Path::new("/")), "/");
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(directory_title(std::path::Path::new(&home)), "~");
        }
    }

    #[test]
    fn drawable_size_stays_within_what_metal_makes() {
        assert_eq!(drawable_size((800, 500)), (800, 500));
        assert_eq!(drawable_size((0, -3)), (1, 1));
        assert_eq!(drawable_size((16384, 16385)), (16384, 16384));
        assert_eq!(drawable_size((40000, 2098)), (16384, 2098));
    }

    #[test]
    fn metal_takes_the_clamped_size() {
        // CAMetalLayer ignores a drawable size above the limit and keeps the
        // one it had, so an unclamped size would leave a stale drawable.
        let device = MTLCreateSystemDefaultDevice().expect("a Metal device");
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        for px in [(800, 500), (40000, 2098), (300, 20000)] {
            let (w, h) = drawable_size(px);
            let size = CGSize::new(f64::from(w), f64::from(h));
            layer.setDrawableSize(size);
            assert_eq!(layer.drawableSize(), size, "{px:?}");
            assert!(layer.nextDrawable().is_some(), "{px:?}");
        }
    }
}
