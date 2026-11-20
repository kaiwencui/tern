//! A pane: one terminal session, the frame it is drawn from, and the bridge
//! that carries the session's events over to the main thread.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use tern_config::Config;
use tern_core::{Progress, Session, SessionConfig, SessionEvents};
use tern_render::CellMetrics;
use tern_types::{Frame, PaneId, Point, Rect};

use crate::app::app;
use crate::mainthread::post_to_main;

static NEXT_PANE: AtomicU64 = AtomicU64::new(1);
/// A shell that fails this soon after it started is shown failing, rather
/// than its pane flashing and vanishing.
const QUICK_EXIT: Duration = Duration::from_secs(2);
/// What stands in for a program that could not be started, so its pane can
/// still say why: it exits at once.
const STAND_IN: &str = "/usr/bin/true";
/// Panes alive in the process, each with its session.
static LIVE_PANES: AtomicUsize = AtomicUsize::new(0);

/// A fresh pane id, unique in the process.
pub fn new_pane_id() -> PaneId {
    NEXT_PANE.fetch_add(1, Ordering::Relaxed)
}

/// How a window's frame loop and its sessions agree on when to draw. A
/// session thread asks for a frame with an atomic store while the loop is
/// running, and only crosses to the main thread to start a stopped loop.
#[derive(Default)]
pub struct FrameRequest {
    dirty: AtomicBool,
    running: AtomicBool,
}

impl FrameRequest {
    /// Any thread: asks for a frame. True when the loop is stopped and the
    /// caller must have the main thread start it.
    fn request(&self) -> bool {
        self.dirty.store(true, Ordering::SeqCst);
        !self.running.swap(true, Ordering::SeqCst)
    }

    /// Main thread: whether a frame was asked for since the last call.
    pub fn take(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }

    /// Main thread: the loop is running.
    pub fn started(&self) {
        self.running.store(true, Ordering::SeqCst);
    }

    /// Main thread: the loop is about to stop. False when a request came in
    /// meanwhile, and the loop must keep running instead.
    pub fn stop(&self) -> bool {
        self.running.store(false, Ordering::SeqCst);
        if self.dirty.load(Ordering::SeqCst) {
            self.running.store(true, Ordering::SeqCst);
            return false;
        }
        true
    }
}

/// What a session reported that the main thread has not handled yet.
#[derive(Default)]
pub struct Pending {
    /// Some(None): the program reset the title.
    pub title: Option<Option<String>>,
    pub bell: bool,
    pub notifications: Vec<(Option<String>, String)>,
    pub progress: Option<Progress>,
    /// Some: the child exited, with its exit code if it had one.
    pub exited: Option<Option<i32>>,
    pub clipboard: Option<String>,
}

/// The session's listener. Every callback runs on a session thread.
pub struct PaneEvents {
    pub pane: PaneId,
    me: Weak<PaneEvents>,
    frames: Mutex<Arc<FrameRequest>>,
    /// New output since the window last took a snapshot of this pane.
    changed: AtomicBool,
    pending: Mutex<Pending>,
    posted: AtomicBool,
}

impl PaneEvents {
    fn new(pane: PaneId, frames: Arc<FrameRequest>) -> Arc<PaneEvents> {
        Arc::new_cyclic(|me| PaneEvents {
            pane,
            me: me.clone(),
            frames: Mutex::new(frames),
            changed: AtomicBool::new(true),
            pending: Mutex::new(Pending::default()),
            posted: AtomicBool::new(false),
        })
    }

    /// Points frame requests at the window the pane now belongs to.
    pub fn set_frames(&self, frames: Arc<FrameRequest>) {
        *lock(&self.frames) = frames;
    }

    /// Main thread: the pane shows something new that the session did not
    /// report (a selection, a scroll), so its next frame snapshots it.
    pub fn mark_changed(&self) {
        self.changed.store(true, Ordering::Release);
    }

    /// Main thread: whether the session has new output since the last call.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }

    /// Main thread: everything reported since the last call.
    pub fn take(&self) -> Pending {
        // Cleared first, so an event arriving while this runs posts again.
        self.posted.store(false, Ordering::SeqCst);
        std::mem::take(&mut *lock(&self.pending))
    }

    fn report(&self, update: impl FnOnce(&mut Pending)) {
        update(&mut lock(&self.pending));
        if self.posted.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(me) = self.me.upgrade() {
            // An exit closes a window, so the event loop must wake to free it.
            post_to_main(true, move |_| app().pane_events(&me));
        }
    }
}

impl SessionEvents for PaneEvents {
    fn wakeup(&self) {
        // Before the frame request, so the frame it brings sees the flag.
        self.changed.store(true, Ordering::Release);
        let frames = Arc::clone(&lock(&self.frames));
        if frames.request() {
            let pane = self.pane;
            post_to_main(false, move |_| app().pane_woke(pane));
        }
    }

    fn title(&self, title: Option<String>) {
        self.report(|p| p.title = Some(title));
    }

    fn bell(&self) {
        self.report(|p| p.bell = true);
    }

    fn exited(&self, status: Option<i32>) {
        self.report(|p| p.exited = Some(status));
    }

    fn clipboard_store(&self, text: String) {
        self.report(|p| p.clipboard = Some(text));
    }

    fn notify(&self, title: Option<String>, body: String) {
        self.report(|p| p.notifications.push((title, body)));
    }

    fn progress(&self, progress: Progress) {
        self.report(|p| p.progress = Some(progress));
    }
}

/// What a new pane runs and where.
pub struct Launch {
    /// From [new_pane_id].
    pub id: PaneId,
    /// None: the configured shell.
    pub command: Option<Vec<String>>,
    pub working_dir: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    /// Cell size in pixels, for programs that ask the terminal.
    pub cell: (u16, u16),
}

pub struct Pane {
    pub id: PaneId,
    pub session: Arc<Session>,
    /// Filled by every snapshot of this pane, and by nothing else.
    pub frame: Frame,
    pub title: Option<String>,
    pub progress: Progress,
    pub events: Arc<PaneEvents>,
    /// The pane's area in the window's drawable, in pixels.
    pub rect: Rect,
    /// Top left corner of the cell grid.
    pub origin: Point,
    /// The grid the session was last given.
    pub grid: (u16, u16),
    /// The layout shows the pane; false while another pane is zoomed.
    pub shown: bool,
    /// What the pane runs, for messages: the command given, or "the shell".
    what: String,
    /// What the pane runs as a window title: the command line, or the shell.
    name: String,
    /// The program was given with -e rather than being the shell.
    explicit: bool,
    started: Instant,
    /// Why the program could not be started; the session runs a stand-in.
    failure: Option<String>,
    /// The program has ended and the pane shows why, until a key closes it.
    pub held: bool,
}

impl Pane {
    /// Starts the pane's program. When it cannot be started, the pane still
    /// opens, to say why; only a failure to start anything at all is an
    /// error.
    pub fn spawn(config: &Config, launch: Launch, frames: Arc<FrameRequest>) -> io::Result<Pane> {
        let id = launch.id;
        let events = PaneEvents::new(id, frames);
        let explicit = launch.command.is_some();
        let shell = config.shell.as_deref().filter(|s| !s.is_empty());
        let (what, name) = match (&launch.command, shell) {
            (Some(argv), _) => (describe(argv), argv.join(" ")),
            (None, Some(shell)) => (format!("the shell {shell}"), shell.to_string()),
            (None, None) => ("the shell".into(), "shell".into()),
        };
        let mut session_config = SessionConfig {
            cols: launch.cols,
            rows: launch.rows,
            cell_width: launch.cell.0,
            cell_height: launch.cell.1,
            shell: config.shell.clone(),
            command: launch.command,
            working_dir: launch.working_dir,
            env: Vec::new(),
            scrollback_lines: config.scrollback_lines as usize,
            theme: config.theme.clone(),
            cursor_shape: config.cursor_style,
            cursor_blink: config.cursor_style_blink,
        };
        let listener = Arc::clone(&events) as Arc<dyn SessionEvents>;
        let (session, failure) = match Session::spawn(session_config.clone(), Arc::clone(&listener))
        {
            Ok(session) => (session, None),
            Err(e) => {
                session_config.command = Some(vec![STAND_IN.into()]);
                session_config.working_dir = None;
                let failure = format!("could not start {what}: {e}");
                (Session::spawn(session_config, listener)?, Some(failure))
            }
        };
        LIVE_PANES.fetch_add(1, Ordering::Relaxed);
        Ok(Pane {
            id,
            session: Arc::new(session),
            frame: Frame::default(),
            title: None,
            progress: Progress::Hidden,
            events,
            rect: Rect::default(),
            origin: Point::default(),
            grid: (launch.cols, launch.rows),
            shown: true,
            what,
            name,
            explicit,
            started: Instant::now(),
            failure,
            held: false,
        })
    }

    /// The program ended. When that went wrong (it could not start, a
    /// command given with -e failed, or the shell failed as it started),
    /// the pane shows why and stays until a key closes it, named after
    /// what failed unless its program set a title, and this returns true;
    /// otherwise the pane should close.
    pub fn hold_after_exit(&mut self, status: Option<i32>) -> bool {
        let message = match (&self.failure, status) {
            (Some(failure), _) => failure.clone(),
            (None, Some(0)) => return false,
            (None, _) if !self.explicit && self.started.elapsed() > QUICK_EXIT => return false,
            (None, Some(code)) => format!("{} exited with status {code}", self.what),
            (None, None) => format!("{} was ended by a signal", self.what),
        };
        // A line of its own below what the program printed; a program that
        // never started printed nothing.
        let gap = if self.failure.is_some() { "" } else { "\r\n" };
        self.session.show_message(&format!(
            "{gap}\x1b[0;1;31m[{message}]\x1b[0m\r\n\x1b[2mPress any key to close.\x1b[0m"
        ));
        if self.title.is_none() {
            self.title = Some(self.name.clone());
        }
        self.held = true;
        true
    }

    /// Puts the pane in a rectangle and gives the session the grid that
    /// fits there. The session ignores sizes it already has. A pane that
    /// has the window to itself is centered; panes that share it start
    /// exactly at their padding, so the text of neighbors lines up.
    pub fn place(
        &mut self,
        rect: Rect,
        metrics: &CellMetrics,
        padding: (i32, i32),
        centered: bool,
    ) {
        self.rect = rect;
        let (cols, rows) = grid(rect, metrics, padding);
        self.origin = if centered {
            centered_origin(rect, metrics, padding, (cols, rows))
        } else {
            Point {
                x: rect.x.saturating_add(padding.0),
                y: rect.y.saturating_add(padding.1),
            }
        };
        self.grid = (cols, rows);
        self.session
            .resize(cols, rows, cell_px(metrics.cell_w), cell_px(metrics.cell_h));
    }

    /// Whether the visible screen shows anything but blanks.
    pub fn shows_content(&self) -> bool {
        self.frame.cells.iter().any(|c| c.ch != u32::from(b' '))
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        LIVE_PANES.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A command line for messages, shortened when long.
fn describe(argv: &[String]) -> String {
    const LONGEST: usize = 60;
    let line = argv.join(" ");
    match line.char_indices().nth(LONGEST) {
        Some((cut, _)) => format!("“{}…”", &line[..cut]),
        None => format!("“{line}”"),
    }
}

/// How many panes, and so sessions, are alive, for the automation socket.
pub fn live_panes() -> usize {
    LIVE_PANES.load(Ordering::Relaxed)
}

/// The grid that fits in a pane: its rectangle less the padding on each
/// side, in whole cells, and never smaller than the terminal accepts.
pub fn grid(rect: Rect, metrics: &CellMetrics, padding: (i32, i32)) -> (u16, u16) {
    let fit = |length: i32, pad: i32, cell: u32| {
        let inner = i64::from(length) - 2 * i64::from(pad);
        let cells = inner.max(0) / i64::from(cell.max(1));
        u16::try_from(cells).unwrap_or(u16::MAX)
    };
    (
        fit(rect.w, padding.0, metrics.cell_w).max(2),
        fit(rect.h, padding.1, metrics.cell_h).max(1),
    )
}

/// Where the grid of a pane starts when it is centered: inside the padding,
/// in the middle of the space there, so what the whole cells leave over is
/// shared between the two sides instead of gathering at the right and the
/// bottom.
fn centered_origin(
    rect: Rect,
    metrics: &CellMetrics,
    padding: (i32, i32),
    (cols, rows): (u16, u16),
) -> Point {
    let start = |at: i32, length: i32, pad: i32, cells: u16, cell: u32| {
        let used = i64::from(cells) * i64::from(cell) + 2 * i64::from(pad);
        let spare = (i64::from(length) - used).max(0) / 2;
        at.saturating_add(pad)
            .saturating_add(i32::try_from(spare).unwrap_or(0))
    };
    Point {
        x: start(rect.x, rect.w, padding.0, cols, metrics.cell_w),
        y: start(rect.y, rect.h, padding.1, rows, metrics.cell_h),
    }
}

fn cell_px(v: u32) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// The cell size in pixels, as a session reports it to programs.
pub fn cell_size(metrics: &CellMetrics) -> (u16, u16) {
    (cell_px(metrics.cell_w), cell_px(metrics.cell_h))
}

/// Locks a mutex, ignoring poisoning: the values guarded here stay whole
/// even if a holder panicked.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(cell_w: u32, cell_h: u32) -> CellMetrics {
        CellMetrics {
            cell_w,
            cell_h,
            baseline: 0,
            underline_top: 0,
            underline_thickness: 1,
            strike_top: 0,
            stroke: 1,
        }
    }

    #[test]
    fn grid_is_the_area_inside_the_padding_in_whole_cells() {
        let m = metrics(8, 16);
        assert_eq!(grid(Rect::new(0, 0, 812, 492), &m, (6, 6)), (100, 30));
        assert_eq!(grid(Rect::new(0, 0, 811, 491), &m, (6, 6)), (99, 29));
        assert_eq!(grid(Rect::new(40, 30, 80, 32), &m, (0, 0)), (10, 2));
    }

    #[test]
    fn a_centered_grid_sits_in_the_middle_of_the_space_inside_the_padding() {
        let m = metrics(8, 16);
        let origin =
            |rect: Rect, padding| centered_origin(rect, &m, padding, grid(rect, &m, padding));
        // An exact fit starts at the padding.
        assert_eq!(
            origin(Rect::new(0, 0, 812, 492), (6, 6)),
            Point { x: 6, y: 6 }
        );
        // 7 and 15 pixels over: 3 and 7 of them go before the grid.
        assert_eq!(
            origin(Rect::new(40, 30, 819, 507), (6, 6)),
            Point { x: 49, y: 43 }
        );
        // No room for the smallest grid: nothing to share.
        assert_eq!(origin(Rect::new(0, 0, 4, 4), (6, 6)), Point { x: 6, y: 6 });
        assert_eq!(
            origin(Rect::new(i32::MAX - 5, 0, 100, 16), (i32::MAX, 0)),
            Point { x: i32::MAX, y: 0 }
        );
    }

    #[test]
    fn grid_never_falls_below_the_smallest_terminal() {
        let m = metrics(8, 16);
        assert_eq!(grid(Rect::new(0, 0, 0, 0), &m, (6, 6)), (2, 1));
        assert_eq!(grid(Rect::new(0, 0, -50, 10), &m, (0, 0)), (2, 1));
        assert_eq!(grid(Rect::new(0, 0, 100, 100), &m, (65535, 65535)), (2, 1));
        assert_eq!(
            grid(Rect::new(0, 0, i32::MAX, i32::MAX), &metrics(1, 1), (0, 0)),
            (u16::MAX, u16::MAX)
        );
        assert_eq!(
            grid(Rect::new(0, 0, 100, 100), &metrics(0, 0), (0, 0)),
            (100, 100)
        );
    }

    #[test]
    fn commands_are_described_briefly() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(describe(&argv(&["vim", "a.txt"])), "“vim a.txt”");
        let long = describe(&argv(&[&"x".repeat(100)]));
        assert_eq!(long.chars().count(), 63);
        assert!(long.ends_with("…”"));
        assert_eq!(describe(&argv(&["日本語"; 30])).chars().count(), 63);
    }

    #[test]
    fn a_stopped_frame_loop_is_started_once() {
        let f = FrameRequest::default();
        assert!(f.request(), "stopped: the caller starts it");
        assert!(!f.request(), "already being started");
        assert!(f.take());
        assert!(!f.take());
        assert!(f.stop(), "nothing came in; it stops");
        assert!(f.request());
        // A request between a frame and the stop keeps the loop going.
        f.started();
        assert!(!f.request());
        assert!(!f.stop());
        assert!(!f.request(), "still running");
        assert!(f.take());
        assert!(f.stop());
    }
}
