//! The public face of one terminal session.

use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use libc::pid_t;
use tern_types::{CursorShape, Frame, Modes, Theme};
use tern_vt::engine::{
    self, CursorShape as EngineShape, CursorStyle, Options, Terminal, ViewScroll, WindowSize,
};

use crate::io::{self as threads, Inner, Poller};
use crate::snapshot::{self, TermState};
use crate::{modes, pty, watch};

/// Smallest grid the emulator accepts; a double-width character needs two
/// columns.
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 1;

/// How to start a session.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub cols: u16,
    pub rows: u16,
    /// Cell size in pixels, reported to programs through the PTY's window size.
    pub cell_width: u16,
    pub cell_height: u16,
    /// None: the user's shell from the password database, then $SHELL, then
    /// /bin/zsh. Started as a login shell.
    pub shell: Option<String>,
    /// Some: run this argv (looked up on PATH) instead of a login shell.
    pub command: Option<Vec<String>>,
    /// None: the home directory.
    pub working_dir: Option<PathBuf>,
    /// Set after the inherited environment and the terminal's own variables,
    /// so these win.
    pub env: Vec<(String, String)>,
    pub scrollback_lines: usize,
    pub theme: Theme,
    /// The cursor until a program asks for another (DECSCUSR).
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
}

impl Default for SessionConfig {
    fn default() -> SessionConfig {
        SessionConfig {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
            shell: None,
            command: None,
            working_dir: None,
            env: Vec::new(),
            scrollback_lines: 10_000,
            theme: Theme::default(),
            cursor_shape: CursorShape::Block,
            cursor_blink: false,
        }
    }
}

/// Progress indicator state reported by a program via `OSC 9;4`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Progress {
    #[default]
    Hidden,
    Set(u8),
    Error(u8),
    Indeterminate,
    Pause(u8),
}

/// What a session tells the app. Every callback arrives on one of the
/// session's own threads, never on a thread that called into the session, and
/// never while the terminal is locked, so a callback may call back into the
/// session. A callback already under way when the session is dropped can
/// finish after Drop returns; none starts once the threads see the shutdown.
pub trait SessionEvents: Send + Sync + 'static {
    /// New content is ready. Coalesced: after one call there is no other until
    /// the next [Session::snapshot], so a flood costs at most one per frame.
    fn wakeup(&self);
    /// The program set the title (OSC 0/2); None: back to the default.
    fn title(&self, title: Option<String>);
    fn bell(&self);
    /// The child exited, after its last output was parsed. The exit code, or
    /// None when it was killed by a signal. Called at most once, and not at
    /// all when the session is dropped first.
    fn exited(&self, status: Option<i32>);
    /// The program asked to copy text to the clipboard (OSC 52).
    fn clipboard_store(&self, text: String);
    /// The program asked to read the clipboard (OSC 52). None sends no reply.
    fn clipboard_load(&self) -> Option<String> {
        None
    }
    /// The program sent a desktop notification (OSC 9 / OSC 777).
    fn notify(&self, _title: Option<String>, _body: String) {}
    /// The program reported progress via `OSC 9;4`.
    fn progress(&self, _progress: Progress) {}
}

/// How to move the view through the scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    /// Positive scrolls back into the history.
    Lines(i32),
    /// Positive scrolls back into the history.
    Pages(i32),
    Top,
    Bottom,
}

/// Which half of a cell the pointer is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Character by character.
    Simple,
    /// A rectangle.
    Block,
    /// Whole words (double click).
    Word,
    /// Whole lines (triple click).
    Line,
}

/// Inclusive grid span of a search match (line 0 is top of active screen;
/// negative lines are in scrollback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub start_line: i32,
    pub start_col: u16,
    pub end_line: i32,
    pub end_col: u16,
}

impl SearchMatch {
    /// Whether `(line, col)` lies inside this match span.
    #[inline]
    pub fn contains(self, line: i32, col: u16) -> bool {
        if line < self.start_line || line > self.end_line {
            return false;
        }
        let after_start = line > self.start_line || col >= self.start_col;
        let before_end = line < self.end_line || col <= self.end_col;
        after_start && before_end
    }
}

/// One terminal: the PTY, the child process, the emulator and its threads.
pub struct Session {
    inner: Arc<Inner>,
    writes: Sender<Vec<u8>>,
    master: Arc<OwnedFd>,
    /// The reader sleeps inside read() and is woken through the master.
    framed: bool,
    /// Write end of the wake pipe; closing it wakes the threads blocked in the
    /// kernel so they notice the shutdown.
    wake: Option<OwnedFd>,
    pid: pid_t,
    login_shell: bool,
    scrollback_lines: usize,
}

impl Session {
    /// Opens a PTY, starts the child on it and starts the session's threads.
    pub fn spawn(config: SessionConfig, events: Arc<dyn SessionEvents>) -> io::Result<Session> {
        let cols = config.cols.max(MIN_COLS);
        let rows = config.rows.max(MIN_ROWS);
        let cmd = pty::Command::new(
            config.shell.as_deref(),
            config.command.as_deref(),
            config.working_dir.as_deref(),
            &config.env,
        )?;
        let child = pty::spawn(
            &cmd,
            pty::winsize(cols, rows, config.cell_width, config.cell_height),
        )?;
        // Until the reader thread owns the child, an error must not leave it
        // running or unreaped.
        let mut guard = ChildGuard(Some(child.pid));
        let master = Arc::new(child.master);
        // Framed, the reader sleeps inside read(), which needs someone to
        // tell it of the child's exit; otherwise the master is non-blocking
        // and the reader waits on a queue that tells it everything.
        let framed = watch::available() && pty::frame_reads(&master);
        let child_gone = framed && !watch::exit(child.pid, &master);
        if !framed {
            pty::set_nonblocking(&master)?;
        }
        let mode: threads::FdMode = framed.then(|| Arc::new(Mutex::new(false)));
        let (wake_read, wake_write) = pty::pipe()?;
        let poller = Poller::new(&master, &wake_read, child.pid)?;

        let options = options(
            config.scrollback_lines,
            config.cursor_shape,
            config.cursor_blink,
        );
        let term = Terminal::new(&options, cols as usize, rows as usize);
        let size = WindowSize {
            num_cols: cols,
            num_lines: rows,
            cell_width: config.cell_width,
            cell_height: config.cell_height,
        };
        let inner = Arc::new(Inner::new(
            TermState::new(term, config.theme),
            events,
            size,
        ));
        let (writes, rx) = mpsc::channel();

        let session = Session {
            inner: inner.clone(),
            writes: writes.clone(),
            master: master.clone(),
            framed,
            wake: Some(wake_write),
            pid: child.pid,
            login_shell: cmd.login_shell,
            scrollback_lines: config.scrollback_lines,
        };
        let wake_read = Arc::new(wake_read);
        {
            let (master, wake, mode) = (master.clone(), wake_read.clone(), mode.clone());
            spawn_thread("tern-pty-writer", move || {
                threads::run_writer(rx, master, wake, mode)
            })?;
        }
        {
            let (inner, master, writes) = (inner.clone(), Arc::downgrade(&master), writes.clone());
            spawn_thread("tern-pty-parser", move || {
                threads::run_parser(inner, writes, master)
            })?;
        }
        {
            let pid = child.pid;
            spawn_thread("tern-pty-reader", move || {
                // Keeps the pipe's read end open as long as the poller watches it.
                let _wake = wake_read;
                threads::run_reader(inner, master, poller, pid, mode, child_gone, writes)
            })?;
        }
        guard.0 = None;
        Ok(session)
    }

    /// Queues input for the child. Never blocks.
    pub fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            // Fails only once the terminal is gone, when input has no reader.
            drop(self.writes.send(bytes.to_vec()));
        }
    }

    /// Changes the grid size. Returns at once: the session's parser thread
    /// reflows the grid (which takes tens of milliseconds with a long
    /// history), then sends the child SIGWINCH and asks for a frame. Until
    /// then snapshots show the old size.
    pub fn resize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        let cols = cols.max(MIN_COLS);
        let rows = rows.max(MIN_ROWS);
        let size = WindowSize {
            num_cols: cols,
            num_lines: rows,
            cell_width,
            cell_height,
        };
        let old = self.inner.size();
        // Layout passes repeat sizes; an unchanged size must not send the
        // child a SIGWINCH and make full-screen programs redraw.
        if (old.num_cols, old.num_lines, old.cell_width, old.cell_height)
            == (cols, rows, cell_width, cell_height)
        {
            return;
        }
        self.inner.set_size(size);
        if !self.inner.request_resize() {
            // The parser has finished: the child is gone, and the grid is
            // resized here.
            self.inner.apply_resize(&self.master);
        }
    }

    /// Brings the frame up to date and returns true when it changed (its seq
    /// advanced). Pass the same frame every time: only changed rows are
    /// rewritten, and a frame this session did not fill last is rewritten
    /// whole.
    ///
    /// While the program has a synchronized update under way (private mode
    /// 2026), the frame is brought up to the screen as it was when the
    /// update began, or as a later update left it, and no further: a
    /// redraw is never shown half done. An update that lasts longer than
    /// 150 ms is ended, and the session calls for a snapshot then.
    pub fn snapshot(&self, frame: &mut Frame) -> bool {
        // Cleared before reading, so output parsed after this point wakes the
        // app again.
        self.inner.wake_pending.store(false, Ordering::Release);
        // A reflow holds the lock for long; the frame is left as it is, and
        // the parser asks for another when the reflow is done.
        match self.inner.try_lock_term() {
            Some(mut state) => {
                let (changed, update_under_way) = snapshot::show(&mut state, frame);
                // The interpreter then puts the screen aside the next time
                // an update ends with another to follow, so that a stream
                // of updates one after another is shown as it goes.
                self.inner
                    .screen_wanted
                    .store(update_under_way, Ordering::Release);
                changed
            }
            None => false,
        }
    }

    /// The modes that change input encoding, without taking the terminal lock.
    pub fn modes(&self) -> Modes {
        modes::unpack(self.inner.modes.load(Ordering::Acquire))
    }

    /// Moves the view. Like the other calls that change the terminal from
    /// the app's side, it never waits for a reflow: during one it takes
    /// effect when the reflow ends.
    pub fn scroll(&self, scroll: Scroll) {
        let scroll = match scroll {
            Scroll::Lines(n) => ViewScroll::Lines(n),
            Scroll::Pages(n) => ViewScroll::Pages(n),
            Scroll::Top => ViewScroll::Top,
            Scroll::Bottom => ViewScroll::Bottom,
        };
        self.inner.with_term(move |state| state.term.scroll(scroll));
    }

    /// Starts a selection at a viewport cell. Rows outside the viewport are
    /// clamped to its edge.
    pub fn selection_start(&self, row: i32, col: u16, side: Side, kind: SelectionKind) {
        let kind = match kind {
            SelectionKind::Simple => engine::SelectionKind::Simple,
            SelectionKind::Block => engine::SelectionKind::Block,
            SelectionKind::Word => engine::SelectionKind::Word,
            SelectionKind::Line => engine::SelectionKind::Line,
        };
        let side = engine_side(side);
        self.inner.with_term(move |state| {
            state.term.selection_start(row, col as usize, side, kind)
        });
    }

    /// Moves the selection's moving end. Rows outside the viewport (a drag past
    /// its edge) are clamped to the edge.
    pub fn selection_update(&self, row: i32, col: u16, side: Side) {
        let side = engine_side(side);
        self.inner
            .with_term(move |state| state.term.selection_update(row, col as usize, side));
    }

    pub fn selection_clear(&self) {
        self.inner.with_term(|state| state.term.selection_clear());
    }

    /// The selected text. Building it takes as long as the selection is
    /// big (a second or more for a million lines), so the app calls this off
    /// its main thread: meanwhile the terminal counts as busy, as during a
    /// reflow, and the main thread's calls skip it or are queued instead of
    /// waiting. None while a reflow or another copy is under way.
    pub fn selection_text(&self) -> Option<String> {
        let text = self
            .inner
            .try_hold_long(|state| state.term.selection_text())?;
        // Snapshots skipped the terminal meanwhile, and queued changes may
        // have been applied: the app draws again.
        self.inner.request_wakeup();
        text
    }

    /// Selects the whole scrollback and screen.
    pub fn select_all(&self) {
        self.inner.with_term(|state| state.term.select_all());
    }

    /// Whether anything is selected; false while a reflow is under way.
    pub fn has_selection(&self) -> bool {
        self.inner
            .try_lock_term()
            .is_some_and(|state| state.term.has_selection())
    }

    /// Case-insensitive substring matches from oldest scrollback to bottom of
    /// screen, spanning soft-wrapped lines. Empty while a reflow is under way.
    pub fn search(&self, query: &str) -> Vec<SearchMatch> {
        let Some(state) = self.inner.try_lock_term() else {
            return Vec::new();
        };
        state
            .term
            .search(query)
            .into_iter()
            .map(|m| SearchMatch {
                start_line: m.start.line.0,
                start_col: m.start.column.0 as u16,
                end_line: m.end.line.0,
                end_col: m.end.column.0 as u16,
            })
            .collect()
    }

    /// Scrolls the viewport minimally so `m` is visible and selects `m`.
    pub fn focus_match(&self, m: SearchMatch) {
        let m = engine::SearchMatch {
            start: engine::Point::new(
                engine::Line(m.start_line),
                engine::Column(m.start_col as usize),
            ),
            end: engine::Point::new(
                engine::Line(m.end_line),
                engine::Column(m.end_col as usize),
            ),
        };
        self.inner.with_term(move |state| state.term.focus_match(m));
    }

    /// Scrolls to the previous (`delta < 0`) or next (`delta > 0`) OSC 133
    /// semantic prompt mark.
    pub fn jump_prompt(&self, delta: i32) {
        self.inner.with_term(move |state| {
            let _ = state.term.jump_prompt(delta);
        });
    }

    /// Selects the output of the most recent OSC 133;C command.
    pub fn select_last_output(&self) {
        self.inner.with_term(|state| {
            let _ = state.term.select_last_output();
        });
    }

    /// Tells the terminal whether its pane has keyboard focus, sending CSI I or
    /// CSI O when the program asked for focus events.
    pub fn focus(&self, focused: bool) {
        let writes = self.writes.clone();
        self.inner.with_term(move |state| {
            if let Some(report) = state.term.set_focused(focused) {
                // Fails only once the terminal is gone.
                drop(writes.send(report.to_vec()));
            }
        });
    }

    /// Replaces the colors the terminal starts from. Colors a program set at
    /// run time stay in effect.
    pub fn set_theme(&self, theme: Theme) {
        let is_dark = |bg: u32| {
            let (r, g, b) = ((bg >> 16) & 0xff, (bg >> 8) & 0xff, bg & 0xff);
            299 * r + 587 * g + 114 * b <= 127_500
        };
        let new_dark = is_dark(theme.bg);
        let writes = self.writes.clone();
        self.inner.with_term(move |state| {
            let old_dark = is_dark(state.theme.bg);
            state.theme = theme;
            if old_dark != new_dark && state.term.color_scheme_updates() {
                let report: &[u8] = if new_dark {
                    b"\x1b[?997;1n"
                } else {
                    b"\x1b[?997;2n"
                };
                // Fails only once the terminal is gone.
                drop(writes.send(report.to_vec()));
            }
        });
    }

    /// Changes the cursor the terminal shows until a program asks for
    /// another (DECSCUSR), and goes back to when one resets it.
    pub fn set_cursor_style(&self, shape: CursorShape, blinking: bool) {
        let scrollback = self.scrollback_lines;
        self.inner.with_term(move |state| {
            state
                .term
                .set_options(&options(scrollback, shape, blinking))
        });
    }

    /// Writes text to the screen as if the program had printed it: the
    /// app's own messages, such as why the program ended. Works after the
    /// child has exited too.
    pub fn show_message(&self, text: &str) {
        let bytes = text.as_bytes().to_vec();
        self.inner
            .with_term(move |state| state.term.show_message(&bytes));
        self.inner.request_wakeup();
    }

    /// What Cmd+K does: drops the scrollback and clears the screen, then sends
    /// a form feed so a shell redraws its prompt. Does nothing while a
    /// full-screen program has the alternate screen, which it owns.
    pub fn clear(&self) {
        let writes = self.writes.clone();
        self.inner.with_term(move |state| {
            if state.term.clear_all() {
                drop(writes.send(b"\x0c".to_vec()));
            }
        });
    }

    /// Working directory of the pane: prefers a valid local directory reported
    /// by the shell via OSC 7, falling back to inspecting the foreground
    /// process or child PID. None once the child has exited.
    pub fn cwd(&self) -> Option<PathBuf> {
        // A reaped child's pid may already belong to an unrelated process.
        if !self.is_alive() {
            return None;
        }
        if let Some(osc_dir) = self
            .inner
            .try_lock_term()
            .and_then(|s| s.term.working_directory().map(PathBuf::from))
            .filter(|p| p.is_dir())
        {
            return Some(osc_dir);
        }
        pty::foreground_group(&self.master)
            .and_then(pty::process_cwd)
            .or_else(|| pty::process_cwd(self.pid))
    }

    /// Name of the program running in the foreground; None when the shell
    /// itself is at its prompt (or the child is gone).
    pub fn foreground_process(&self) -> Option<String> {
        if !self.is_alive() {
            return None;
        }
        let group = pty::foreground_group(&self.master)?;
        if group == self.pid && self.login_shell {
            return None;
        }
        pty::process_name(group)
    }

    pub fn child_pid(&self) -> u32 {
        self.pid as u32
    }

    /// False once the child has exited and been reaped.
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::Acquire)
    }

    /// The visible screen as text, one line per row with trailing blanks
    /// removed (the same format as [Frame::text]). Waits for a reflow under
    /// way, so it is for tests and tools, not the app's main thread. It is
    /// the terminal as the program has left it so far, a synchronized
    /// update under way included, which a frame leaves out.
    pub fn text(&self) -> String {
        self.inner.lock_term().term.screen_text()
    }

    /// How many lines the scrollback has, how many of them are packed (all
    /// but the few dozen newest), and how many bytes the packed ones take.
    /// For tests and measurements.
    #[doc(hidden)]
    pub fn scrollback_stats(&self) -> (usize, usize, usize) {
        self.inner.lock_term().term.history_stats()
    }

    /// The OSC 8 link target under a viewport cell.
    /// None while a reflow is under way.
    pub fn hyperlink_at(&self, row: u16, col: u16) -> Option<String> {
        let state = self.inner.try_lock_term()?;
        state.term.hyperlink_at(row as usize, col as usize)
    }
}

impl Drop for Session {
    /// Hangs up: the threads stop, the master closes (which sends the child
    /// SIGHUP) and the reader thread reaps the child. Nothing here waits.
    fn drop(&mut self) {
        self.inner.shut_down();
        self.wake.take();
        if self.framed {
            pty::wake_reader(&self.master);
        }
    }
}

fn engine_side(side: Side) -> engine::Side {
    match side {
        Side::Left => engine::Side::Left,
        Side::Right => engine::Side::Right,
    }
}

/// The emulator's settings for a session.
fn options(scrollback_lines: usize, shape: CursorShape, blinking: bool) -> Options {
    // Whether a program may read the clipboard (OSC 52) is the app's call,
    // made in SessionEvents::clipboard_load.
    Options {
        scrollback_lines,
        cursor: CursorStyle {
            shape: engine_shape(shape),
            blinking,
        },
        ..Options::default()
    }
}

pub(crate) fn engine_shape(shape: CursorShape) -> EngineShape {
    match shape {
        CursorShape::Block => EngineShape::Block,
        CursorShape::Beam => EngineShape::Beam,
        CursorShape::Underline => EngineShape::Underline,
        CursorShape::HollowBlock => EngineShape::HollowBlock,
    }
}

/// Starts one of the session's threads. They move the user's output, so
/// they ask for user-initiated service: threads otherwise inherit whatever
/// class the spawning thread has, and macOS lowers default-class threads of
/// an app that is hidden or in the background.
fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) -> io::Result<()> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // SAFETY: sets the calling thread's own class; no memory is involved.
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0)
            };
            f()
        })
        .map(drop)
}

/// Kills and reaps a child that never got its reader thread.
struct ChildGuard(Option<pid_t>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // SAFETY: the pid is our unreaped child, so it cannot have been
            // reused.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            pty::reap(pid);
        }
    }
}
