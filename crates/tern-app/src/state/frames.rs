//! The frame loop: a display link that draws a window when something in it
//! changed and pauses when nothing has, and the blinking cursor.

use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2_app_kit::NSColor;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDate, NSRunLoop, NSRunLoopCommonModes, NSTimer};
use objc2_quartz_core::{CADisplayLink, CATransaction};
use tern_render::{CellMetrics, Image, PaneDraw, Present, RenderError, Scene};

use super::WindowState;
use crate::app::app;
use crate::pane::Pane;
use tern_types::{PaneId, Rect, Rgb};

/// How long the display link keeps running after the last frame, so a burst
/// of output does not stop and restart it on every frame, while stopping
/// between key presses so each one draws as soon as its echo arrives instead
/// of waiting for the display link's next beat.
const IDLE_PAUSE: Duration = Duration::from_millis(64);
/// How many times in a row a frame Metal dropped is drawn again. A frame
/// drawn just before the window reaches the screen is dropped; after this
/// many tries the occlusion notification brings the next frame.
const MAX_REDRAWS: u8 = 3;
/// Frames that may wait for the compositor at once. With more, the next
/// one would wait in nextDrawable (up to a second) on the main thread, so
/// the display link skips a beat instead.
const MAX_IN_FLIGHT: u64 = 2;
/// Beats skipped in a row before drawing anyway, in case a presented
/// handler never came.
const MAX_HELD_BACK: u32 = 30;
/// Half a blink: how long the cursor stays shown, then hidden.
const BLINK: f64 = 0.6;
/// Frames whose timings the automation socket reports.
const TIMED_FRAMES: usize = 120;

/// The main thread's cost of one frame, in wall-clock time (waits for a
/// session's lock included).
#[derive(Clone, Copy, Default, Debug)]
pub struct FrameTime {
    /// How many panes were snapshotted: those with new output, or all of
    /// them after a change on the main thread.
    pub snapshots: u32,
    /// Taking those snapshots.
    pub snapshot: Duration,
    /// The renderer's preparation and encoding.
    pub prepare: Duration,
    pub encode: Duration,
    /// The whole frame, snapshots and drawing.
    pub total: Duration,
}

/// The most recent frames' timings, in a fixed ring so recording them never
/// allocates.
pub struct FrameTimes {
    ring: [FrameTime; TIMED_FRAMES],
    next: usize,
    len: usize,
}

impl FrameTimes {
    pub(super) fn new() -> FrameTimes {
        FrameTimes {
            ring: [FrameTime::default(); TIMED_FRAMES],
            next: 0,
            len: 0,
        }
    }

    /// Forgets every timing, so what comes next is measured on its own.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    fn push(&mut self, t: FrameTime) {
        self.ring[self.next] = t;
        self.next = (self.next + 1) % TIMED_FRAMES;
        self.len = (self.len + 1).min(TIMED_FRAMES);
    }

    /// Oldest first.
    pub fn recent(&self) -> impl Iterator<Item = &FrameTime> {
        let start = (self.next + TIMED_FRAMES - self.len) % TIMED_FRAMES;
        (0..self.len).map(move |i| &self.ring[(start + i) % TIMED_FRAMES])
    }
}

impl WindowState {
    // ---- the frame loop ----

    /// Takes the display link the view created.
    pub fn attach_display_link(&self, link: Retained<CADisplayLink>) {
        if let Some(old) = self.link.replace(Some(link)) {
            old.invalidate();
        }
        self.mark_dirty();
    }

    /// Something on the main thread changed what the panes show: the next
    /// frame snapshots every pane.
    pub fn mark_dirty(&self) {
        self.snapshot_all.set(true);
        self.request_frame();
    }

    /// One pane's content changed on the main thread (a selection, a
    /// scroll): only it is snapshotted.
    pub fn pane_changed(&self, id: PaneId) {
        self.with_pane(id, |p| p.events.mark_changed());
        self.request_frame();
    }

    /// Asks for a frame, starting the display link if it was idle. Only the
    /// panes flagged as changed are snapshotted.
    pub fn request_frame(&self) {
        self.dirty.set(true);
        self.redraws.set(0);
        if self.occluded.get() {
            return;
        }
        if let Some(link) = &*self.link.borrow() {
            self.frames.started();
            if link.isPaused() {
                link.setPaused(false);
            }
        }
    }

    /// A session has output while the display link was stopped: the frame
    /// is drawn now rather than at the next vsync, so it is presented one
    /// refresh sooner (an echoed key shows a frame earlier), and the link
    /// takes over from there.
    pub fn woke(&self) {
        let paused = self.link.borrow().as_ref().is_some_and(|l| l.isPaused());
        self.request_frame();
        if self.occluded.get() {
            self.check_activity_after_output();
            return;
        }
        if paused && !self.live_resize.get() {
            self.tick();
        }
    }

    /// One display link callback: draw if something changed, and pause the
    /// link once the window has been idle for a while.
    pub fn tick(&self) {
        // Drawables and command buffers are autoreleased; without a pool of
        // its own a stream of frames with no events would pile them up.
        autoreleasepool(|_| self.tick_inner());
    }

    fn tick_inner(&self) {
        let start = Instant::now();
        if self.frames.take() {
            self.dirty.set(true);
        }
        if self.dropped.swap(false, Ordering::Relaxed) && self.redraws.get() < MAX_REDRAWS {
            self.redraws.set(self.redraws.get() + 1);
            self.dirty.set(true);
        }
        if self.dirty.get()
            && !self.occluded.get()
            && self.in_flight.load(Ordering::Acquire) >= MAX_IN_FLIGHT
            && self.held_back.get() < MAX_HELD_BACK
        {
            // The compositor is behind; drawing now would block.
            self.held_back.set(self.held_back.get() + 1);
            return;
        }
        if self.dirty.get() && !self.occluded.get() {
            self.held_back.set(0);
            self.dirty.set(false);
            let present = if self.live_resize.get() {
                Present::WithTransaction
            } else {
                Present::Async
            };
            if !self.draw(present) {
                // No drawable this time; try again on the next vsync.
                self.dirty.set(true);
            }
            self.last_frame.set(start);
        } else if self.occluded.get() || start.duration_since(self.last_frame.get()) >= IDLE_PAUSE {
            self.pause();
        }
    }

    /// Draws now instead of at the next vsync, so a new window shows its
    /// content as soon as it is on screen.
    pub fn draw_now(&self) {
        if self.occluded.get() {
            return;
        }
        self.dirty.set(!self.draw(Present::Async));
        self.last_frame.set(Instant::now());
    }

    fn pause(&self) {
        let link = self.link.borrow();
        let Some(link) = &*link else { return };
        if link.isPaused() {
            return;
        }
        if self.frames.stop() {
            link.setPaused(true);
        } else {
            // A session asked for a frame as the loop was stopping.
            self.dirty.set(true);
        }
    }

    /// Snapshots the panes that changed and draws every pane the layout
    /// shows. A pane whose wakeup arrived while nothing was drawn keeps its
    /// flag until it is drawn, so it is never left stale.
    pub(super) fn draw(&self, present: Present) -> bool {
        let start = Instant::now();
        if self.covered.get() {
            // This frame replaces the one under the cover. It shows the
            // cursor, and the blink starts over, as when a key is typed.
            self.uncover();
            if let Some(t) = &*self.blink.borrow() {
                t.setFireDate(&NSDate::dateWithTimeIntervalSinceNow(BLINK));
            }
        }
        let mut panes = self.panes.borrow_mut();
        let all = self.snapshot_all.replace(false);
        let mut snapshots = 0;
        for pane in panes.iter_mut().filter(|p| p.shown) {
            // Not short-circuited: the flag is taken either way.
            if pane.events.take_changed() | all {
                pane.session.snapshot(&mut pane.frame);
                snapshots += 1;
            }
        }
        let snapshot = start.elapsed();
        self.sync_chrome(&panes);
        if !self.preedit.borrow().is_empty()
            || self.search_open()
            || self.preedit_frame.borrow().is_some()
        {
            self.update_preedit_frame(&mut panes);
        }
        let content = self
            .startup
            .as_ref()
            .filter(|probe| probe.wants_content())
            .is_some_and(|_| panes.iter().any(Pane::shows_content));
        let app = app();
        let mut renderer = app.renderer();
        // Counted before the frame can be reported back.
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let size_pt = self.font_size.get();
        let drawn = self.with_scene(&panes, |scene| {
            renderer.draw_sized(&self.layer, scene, size_pt, present, Some(&self.presented))
        });
        if !drawn {
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
        let stats = renderer.stats();
        drop(renderer);
        drop(panes);
        self.times.borrow_mut().push(FrameTime {
            snapshots,
            snapshot,
            prepare: stats.prepare,
            encode: stats.encode,
            total: start.elapsed(),
        });
        if drawn {
            self.shown_on.set(self.focused.get() && self.cursor_on.get());
            self.draws.set(self.draws.get() + 1);
            let panes = self.panes.borrow();
            self.key_drawn(self.draws.get(), |id| {
                panes.iter().find(|p| p.id == id).map(|p| p.frame.seq)
            });
            drop(panes);
            if let Some(probe) = &self.startup {
                if self.draws.get() == 1 {
                    probe.mark("first frame drawn");
                }
                if content {
                    probe.content_drawn(self.draws.get());
                }
            }
        }
        self.update_blink();
        // Only output can have changed the directory or running command; a blink cannot.
        if drawn && snapshots > 0 {
            self.refresh_directory_title();
            self.check_activity_after_output();
        }
        drawn
    }

    /// Builds the window's scene from the panes' current frames.
    fn with_scene<R>(&self, panes: &[Pane], f: impl FnOnce(&Scene) -> R) -> R {
        let (width, height) = self.drawable_size();
        let focused_id = self.focused_pane_id();
        let dim = if panes.iter().filter(|p| p.shown).nth(1).is_some() {
            app().config().unfocused_split_dim
        } else {
            0.0
        };
        let clear = panes.first().map_or(0, |p| p.frame.bg);
        let preedit = self.preedit_frame.borrow();
        let mut draws = recycle(std::mem::take(&mut *self.scratch.borrow_mut()));
        for pane in panes.iter().filter(|p| p.shown) {
            let focused = Some(pane.id) == focused_id;
            let frame = match &*preedit {
                Some((id, composed)) if *id == pane.id => composed,
                _ => &pane.frame,
            };
            draws.push(PaneDraw {
                id: pane.id,
                rect: pane.rect,
                origin: pane.origin,
                frame,
                focused: focused && self.focused.get(),
                cursor_on: !focused || self.cursor_on.get(),
                dim: if focused { 0.0 } else { dim },
            });
        }
        let quads = self.quads.borrow();
        let scale = self.scale.get();
        let has_scrolled = draws
            .iter()
            .any(|p| p.frame.display_offset > 0 && p.frame.history > 0);
        let mut combined_quads = Vec::new();
        let scene_quads: &[tern_render::Quad] = if has_scrolled {
            combined_quads.reserve(quads.len() + draws.len());
            combined_quads.extend_from_slice(&quads);
            for p in &draws {
                if let Some(thumb) = super::overlay::scrollbar_thumb(
                    p.rect,
                    p.frame.rows,
                    p.frame.history,
                    p.frame.display_offset,
                    p.frame.fg,
                    scale,
                ) {
                    combined_quads.push(thumb);
                }
            }
            &combined_quads
        } else {
            &quads
        };
        let scene = Scene {
            width,
            height,
            scale: scale as f32,
            clear,
            panes: &draws,
            quads: scene_quads,
        };
        let result = f(&scene);
        drop(quads);
        *self.scratch.borrow_mut() = recycle(draws);
        drop(preedit);
        result
    }

    /// Brings every pane's frame up to date outside the frame loop, for
    /// readers that need the screen as it is now.
    pub fn refresh(&self) {
        let mut changed = false;
        for pane in self.panes.borrow_mut().iter_mut() {
            changed |= pane.session.snapshot(&mut pane.frame);
        }
        if changed {
            // The snapshot took the panes' pending changes; draw them on
            // screen too.
            self.mark_dirty();
        }
    }

    /// Renders what the window shows into memory, as of now.
    pub fn render_offscreen(&self) -> Result<Image, RenderError> {
        self.refresh();
        self.update_preedit_frame(&mut self.panes.borrow_mut());
        let panes = self.panes.borrow();
        let app = app();
        let mut renderer = app.renderer();
        let size_pt = self.font_size.get();
        self.with_scene(&panes, |scene| renderer.render_offscreen_sized(scene, size_pt))
    }

    // ---- the cursor blink ----

    /// Starts or stops the blink timer: it runs only while the window is
    /// key and visible and the focused pane's cursor blinks.
    pub(super) fn update_blink(&self) {
        let blinks = self
            .with_focused(|p| p.frame.cursor.is_some_and(|c| c.blinking))
            .unwrap_or(false);
        let want = blinks && self.focused.get() && !self.occluded.get();
        let mut timer = self.blink.borrow_mut();
        if want == timer.is_some() {
            return;
        }
        if let Some(t) = timer.take() {
            t.invalidate();
            // Hidden either way, by the cover or by a frame without it.
            let hidden = !self.cursor_on.get();
            self.uncover();
            if hidden {
                drop(timer);
                self.mark_dirty();
            }
            return;
        }
        let id = self.id;
        let block = RcBlock::new(move |_: NonNull<NSTimer>| {
            if let Some(state) = app().state(id) {
                state.blink_step();
            }
        });
        // SAFETY: the block only runs on the main thread, where the timer is
        // scheduled.
        let t = unsafe { NSTimer::timerWithTimeInterval_repeats_block(BLINK, true, &block) };
        // SAFETY: the main run loop, on the main thread. Common modes keep
        // the cursor blinking during live resize and menu tracking.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&t, NSRunLoopCommonModes) };
        self.cursor_on.set(true);
        *timer = Some(t);
    }

    /// Half a blink has passed: the cursor goes or comes back. Where it can,
    /// it goes under the cover and comes back by the cover's leaving, and
    /// no frame is drawn for either. The GPU's driver lets go of what it
    /// holds for drawing (some twenty megabytes after launch) only once no
    /// frame has been drawn for a couple of seconds, which a frame every
    /// 0.6 s would never allow a window that does nothing but blink.
    fn blink_step(&self) {
        self.blinks.set(self.blinks.get() + 1);
        if self.covered.get() {
            self.uncover();
            return;
        }
        let on = !self.cursor_on.get();
        self.cursor_on.set(on);
        if on || !self.cover() {
            self.blink_frame();
        }
    }

    /// Hides the cursor under the cover, if what would then be on screen is
    /// what a frame without the cursor would show: the frame on screen is
    /// the newest and shows the cursor, and the cursor's cell is an empty
    /// one in the pane's own background with nothing drawn over it.
    fn cover(&self) -> bool {
        if !self.shown_on.get()
            || self.dirty.get()
            || self.live_resize.get()
            || self.in_flight.load(Ordering::Acquire) != 0
            || !self.preedit.borrow().is_empty()
            || self.preedit_frame.borrow().is_some()
        {
            return false;
        }
        let metrics = self.metrics.get();
        let Some((cell, bg)) = self
            .with_focused(|p| empty_cursor_cell(p, &metrics))
            .flatten()
        else {
            return false;
        };
        if self.quads.borrow().iter().any(|q| overlap(q.rect, cell)) {
            return false;
        }
        let scale = self.scale.get();
        let pt = |px: i32| f64::from(px) / scale;
        // Layers count from the bottom unless the view's is flipped.
        let y = if self.layer.contentsAreFlipped() {
            pt(cell.y)
        } else {
            self.layer.bounds().size.height - pt(cell.y + cell.h)
        };
        let unit = |shift: u32| f64::from((bg >> shift) & 0xff) / 255.0;
        // sRGB, as the Metal layer is, so the cover is the color around it.
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(unit(16), unit(8), unit(0), 1.0);
        // Without the transaction's switch the cover would fade in.
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.cover.setFrame(CGRect::new(
            CGPoint::new(pt(cell.x), y),
            CGSize::new(pt(cell.w), pt(cell.h)),
        ));
        self.cover.setBackgroundColor(Some(&color.CGColor()));
        self.cover.setHidden(false);
        CATransaction::commit();
        self.covered.set(true);
        true
    }

    /// Takes the cover away, if it is up: the cursor in the frame under it
    /// shows again, and the blink phase says so.
    fn uncover(&self) {
        self.cursor_on.set(true);
        if !self.covered.replace(false) {
            return;
        }
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.cover.setHidden(true);
        CATransaction::commit();
    }

    /// The blink phase changed and a frame has to show it. Nothing else
    /// changed, so no pane is snapshotted, and an idle window draws the
    /// frame right here: starting the display link would keep it ticking
    /// for IDLE_PAUSE after every blink.
    fn blink_frame(&self) {
        let idle = self.link.borrow().as_ref().is_none_or(|l| l.isPaused());
        // With frames still waiting for the compositor, asking for another
        // drawable here could block; the display link's tick checks for that.
        let free = self.in_flight.load(Ordering::Acquire) < MAX_IN_FLIGHT;
        if idle && free {
            self.draw_now();
        } else {
            self.request_frame();
        }
    }

    /// Typing shows the cursor and starts the blink over.
    pub fn restart_blink(&self) {
        if let Some(t) = &*self.blink.borrow() {
            t.setFireDate(&NSDate::dateWithTimeIntervalSinceNow(BLINK));
        }
        if self.covered.get() {
            // The frame on screen has the cursor in it.
            self.uncover();
        } else if !self.cursor_on.replace(true) {
            self.mark_dirty();
        }
    }
}

/// The cursor's cell in the window's drawable and its color, if the cell is
/// an empty one in the pane's own background, inside the pane: then a frame
/// without the cursor shows that color there and nothing else.
fn empty_cursor_cell(pane: &Pane, m: &CellMetrics) -> Option<(Rect, Rgb)> {
    let frame = &pane.frame;
    let cursor = frame.cursor?;
    if !pane.shown || cursor.wide || cursor.row >= frame.rows || cursor.col >= frame.cols {
        return None;
    }
    let cell = frame.cell(cursor.row, cursor.col);
    if cell.ch != 0x20 || cell.flags != 0 || cell.bg != frame.bg {
        return None;
    }
    let (w, h) = (m.cell_w as i32, m.cell_h as i32);
    let rect = Rect::new(
        pane.origin.x + i32::from(cursor.col) * w,
        pane.origin.y + i32::from(cursor.row) * h,
        w,
        h,
    );
    let (inner, outer) = (rect, pane.rect);
    let inside = inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h;
    inside.then_some((rect, frame.bg))
}

/// Whether two rectangles share any pixel.
fn overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// Reuses a vector's allocation for references of another lifetime. Its
/// elements are dropped first, and collecting an emptied vector's iterator
/// into a vector of a type of the same size and alignment reuses the buffer
/// in place, so no frame allocates its pane list.
fn recycle<'a, 'b>(mut v: Vec<PaneDraw<'a>>) -> Vec<PaneDraw<'b>> {
    v.clear();
    v.into_iter()
        .map(|_| unreachable!("the vector is empty"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_types::{Frame, Point, Rect};

    #[test]
    fn recycling_keeps_the_allocation() {
        let frame = Frame::default();
        let mut v: Vec<PaneDraw> = Vec::with_capacity(8);
        let ptr = v.as_ptr() as usize;
        v.push(PaneDraw {
            id: 1,
            rect: Rect::default(),
            origin: Point::default(),
            frame: &frame,
            focused: true,
            cursor_on: true,
            dim: 0.0,
        });
        let back: Vec<PaneDraw<'static>> = recycle(v);
        assert!(back.is_empty());
        assert_eq!(back.capacity(), 8);
        assert_eq!(back.as_ptr() as usize, ptr);
    }

    #[test]
    fn frame_times_keep_the_most_recent() {
        let mut t = FrameTimes::new();
        assert_eq!(t.recent().count(), 0);
        for i in 0..(TIMED_FRAMES as u64 + 5) {
            t.push(FrameTime {
                total: Duration::from_micros(i),
                ..FrameTime::default()
            });
        }
        let totals: Vec<u64> = t.recent().map(|f| f.total.as_micros() as u64).collect();
        assert_eq!(totals.len(), TIMED_FRAMES);
        assert_eq!(totals[0], 5);
        assert_eq!(*totals.last().unwrap(), TIMED_FRAMES as u64 + 4);
    }
}
