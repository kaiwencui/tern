//! The three threads behind a session.
//!
//! Reader: reads the PTY master. What it reads it either interprets itself
//! or hands to the parser thread, whichever moves output faster (see [Way]).
//! When about a megabyte is waiting for the parser it stops reading, so a
//! flood pushes back on the child instead of growing memory. It also
//! notices the child's exit and reaps it.
//!
//! macOS gives a terminal its output a kilobyte at a time: the child fills
//! the kernel's small buffer and sleeps, and wakes when the terminal has
//! read it. How quickly that hand-over turns around bounds how fast output
//! can arrive at all, so the reader spends one system call on each: it
//! sleeps inside read() itself. Waiting on a queue and then reading, as it
//! once did, measured a tenth to a fifth slower.
//!
//! Parser: interprets what the reader hands over, in pieces, holding the
//! terminal lock only briefly each time. It takes the lock unfairly while
//! every other caller takes the fair lock; between pieces it also steps aside
//! for a caller that is waiting, so a flood cannot starve the UI's snapshot.
//! It also applies new sizes and ends synchronized updates that run past
//! their time.
//!
//! Writer: writes input and the emulator's replies to the master in order.

use std::io;
use std::ops::DerefMut;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use libc::pid_t;
use tern_vt::engine::{Event, Turn, WindowSize, BEGIN_SYNC_LEN};

use crate::blink::{self, BlinkModeScan};
use crate::session::SessionEvents;
use crate::snapshot::{self, TermState};
use crate::sync::{lock, FairMutex};
use crate::{modes, pty, watch};

/// Bytes the reader lets pile up before it waits for the parser.
const MAX_PENDING: usize = 1 << 20;
/// Room for one read. The macOS PTY returns at most 1,024 bytes, and one
/// more in front of them when reads are framed.
const READ_BUF: usize = 4096;
/// The buffers between reader and parser are given back once they are larger
/// than this and nothing has arrived for [IDLE_BEFORE_SHRINK].
const SMALL_BUF: usize = 64 * 1024;
const IDLE_BEFORE_SHRINK: Duration = Duration::from_millis(100);
/// Output counts as a flood while at least this much arrives per window.
const FLOOD_BYTES: usize = 64 * 1024;
const FLOOD_WINDOW: Duration = Duration::from_millis(2);
/// A reader stays awake between reads once this much output arrives per
/// [FLOOD_WINDOW] (230 MB/s), and sleeps in read() again below
/// [AWAKE_LEAVE] (200 MB/s). Faster than that the child is back at the PTY
/// within two or three microseconds, less than putting the reader to sleep
/// and waking it takes. Slower output is slow because the kernel works on
/// each character (line ends, escapes), and a reader waiting at the PTY's
/// lock all that while only gets in the child's way.
const AWAKE_ENTER: usize = 460 * 1024;
const AWAKE_LEAVE: usize = 400 * 1024;
/// A reader also stays awake for this many windows at the start of a flood.
/// The processor is slow to get going after a rest, and for a tenth of a
/// second nothing arrives fast enough to pass the test above, however fast
/// it is going to be.
const AWAKE_START: u32 = 30;
/// How long an awake reader keeps trying an empty PTY before it sleeps.
const AWAKE_DRY: Duration = Duration::from_micros(100);
/// Readers awake at once in the whole process: each keeps a core busy, and
/// with many panes flooding the cores are better left to the parsers.
const AWAKE_MAX: usize = 2;
static AWAKE: AtomicUsize = AtomicUsize::new(0);
/// With more than this waiting for the parser thread, interpreting is what
/// holds output up, and the reader does not try doing it too.
const OWN_BACKLOG_MAX: usize = 64 * 1024;
/// The reader interprets output itself while that takes it less than this
/// share of the time the child takes to come back with more, in percent,
/// and hands output to the parser thread once it takes more than
/// [OWN_LEAVE]. Measured on eleven kinds of output, the reader was the
/// faster of the two up to 61 percent and the slower from 70 on.
const OWN_ENTER: u64 = 60;
const OWN_LEAVE: u64 = 70;
/// Windows between two looks at the way not in use, one window long: this
/// many at first, twice as many after each look that changed nothing, up
/// to [LOOK_SELDOM]. A young flood is looked at sooner, at these ages in
/// windows: the processor is still getting going then, which makes
/// interpreting look dearer than it will be.
const LOOK_EVERY: u32 = 32;
const LOOK_SELDOM: u32 = 512;
const LOOK_EARLY: [u32; 2] = [8, 16];
/// For tests: the way every reader goes, 1 + its place in [Way::ALL], or 0
/// to let each choose.
pub(crate) static FORCED_WAY: AtomicUsize = AtomicUsize::new(0);

/// Whether the reader wants the master non-blocking, shared with the writer:
/// whether a descriptor blocks is a property of the open file both use.
/// Present when reads are framed; otherwise the master never blocks.
pub(crate) type FdMode = Option<Arc<Mutex<bool>>>;

/// How long the parser keeps looking for more output before it sleeps
/// during a flood, when the next kilobyte is a few microseconds away.
/// Sleeping would have the reader make a wake-up call for every one, which
/// costs it more than reading them does.
const PARSER_LINGER: Duration = Duration::from_micros(30);
/// Lines of the scrollback packed per hold of the terminal lock once output
/// has stopped (see [settle_scrollback]): some tens of microseconds.
const SETTLE_LINES: usize = 2_000;
/// Bytes parsed per hold of the terminal lock. Plain text takes about half a
/// millisecond per piece on an M-series core, so a snapshot never waits long.
const PIECE: usize = 32 * 1024;
/// Longest the parser waits for a blocked caller to take the lock before it
/// carries on; a guard against a stalled caller, not a tuning knob.
const STEP_ASIDE_LIMIT: Duration = Duration::from_millis(2);
/// How often a caller that must have the terminal checks whether a reflow
/// has finished.
const REFLOW_POLL: Duration = Duration::from_millis(1);

/// Work a caller asked for while a reflow held the terminal, done by the
/// parser as soon as the reflow ends.
type Deferred = Box<dyn FnOnce(&mut TermState) + Send>;

/// State shared by the session handle and its threads.
pub(crate) struct Inner {
    term: FairMutex<TermState>,
    /// Callers blocked on the fair lock. The parser yields to them between
    /// pieces, because the lock alone would let it take the lock straight
    /// back before a woken caller gets to run.
    lockers_waiting: AtomicUsize,
    pub(crate) events: Arc<dyn SessionEvents>,
    io: Mutex<IoState>,
    data_ready: Condvar,
    space_ready: Condvar,
    shutdown: AtomicBool,
    /// Packed [tern_types::Modes], published by the parser after each piece.
    pub(crate) modes: AtomicU32,
    /// A wakeup was sent and no snapshot has been taken since.
    pub(crate) wake_pending: AtomicBool,
    /// cols, rows, cell width, cell height, 16 bits each: the size last
    /// asked for, which the parser applies.
    size: AtomicU64,
    /// A resize waits for the parser.
    resize_pending: AtomicBool,
    /// How many threads hold, or are about to hold, the terminal for long: a
    /// reflow for a new size, or the text of a large selection being built.
    /// Snapshots skip the terminal meanwhile instead of waiting, as either
    /// can take tens of milliseconds or more. Changed only while holding the
    /// fair lock's lease, which every other caller takes before the lock, so
    /// no caller can start waiting for the lock once a long hold has begun.
    long_holds: AtomicUsize,
    /// What callers asked for during a long hold, in order.
    deferred: Mutex<Vec<Deferred>>,
    pub(crate) alive: AtomicBool,
    /// Bytes waiting in `io.data`, for the parser to look at without the
    /// lock while it lingers.
    pending: AtomicUsize,
    /// Output is arriving as a flood; kept by the reader.
    flood: AtomicBool,
    /// The app came for the screen and found a synchronized update under
    /// way. The interpreter puts the screen aside for it where the update
    /// ends, if another follows before the app could look.
    pub(crate) screen_wanted: AtomicBool,
    /// The parser thread knows of the time limit of the synchronized update
    /// under way, and will end the update when it runs out.
    limit_watched: AtomicBool,
    /// When the synchronized update under way is to be ended for lasting
    /// too long, as the terminal last said: nanoseconds after `epoch`, plus
    /// one, or 0 when none is under way. Written by whoever holds `interp`;
    /// an atomic so the parser can read it while it holds `io`, where it
    /// must not wait for `interp`.
    sync_deadline: AtomicU64,
    epoch: Instant,
    /// The interpreter, for whichever of reader and parser has the output.
    interp: Mutex<Interp>,
    /// Output left lines of the scrollback as cells, for the parser thread
    /// to pack once output has stopped. Set under the terminal lock by
    /// whoever interprets, cleared by the parser thread.
    settle_wanted: AtomicBool,
    /// Pieces of output interpreted: output has stopped only if this has not
    /// moved, since the reader may be interpreting without the parser
    /// thread knowing of any output.
    pieces: AtomicU32,
}

#[derive(Default)]
struct IoState {
    data: Vec<u8>,
    /// Set once the child is gone and every byte it wrote has been handed
    /// over; holds its exit code.
    exit: Option<Option<i32>>,
    parser_waiting: bool,
    reader_waiting: bool,
    /// The parser thread has finished; a resize is then applied by its caller.
    parser_done: bool,
    /// The parser thread holds output it has not finished interpreting.
    parser_busy: bool,
}

impl Inner {
    /// Stores the `sync_deadline` field. SeqCst, as is the parser's clearing of
    /// `limit_watched` before it reads this, and the reader's reading of
    /// that flag after writing this: one of the two sees the other.
    fn set_sync_deadline(&self, deadline: Option<Instant>) {
        let word = deadline.map_or(0, |d| {
            let nanos = d.saturating_duration_since(self.epoch).as_nanos();
            u64::try_from(nanos).unwrap_or(u64::MAX - 1) + 1
        });
        self.sync_deadline.store(word, Ordering::SeqCst);
    }

    /// When the synchronized update under way is to be ended, without
    /// waiting for the interpreter.
    fn sync_deadline(&self) -> Option<Instant> {
        match self.sync_deadline.load(Ordering::SeqCst) {
            0 => None,
            word => Some(self.epoch + Duration::from_nanos(word - 1)),
        }
    }

    pub(crate) fn new(
        term: TermState,
        events: Arc<dyn SessionEvents>,
        size: WindowSize,
    ) -> Inner {
        let modes = modes::pack(term.term.mode());
        let inner = Inner {
            term: FairMutex::new(term),
            lockers_waiting: AtomicUsize::new(0),
            events,
            io: Mutex::new(IoState::default()),
            data_ready: Condvar::new(),
            space_ready: Condvar::new(),
            shutdown: AtomicBool::new(false),
            modes: AtomicU32::new(modes),
            wake_pending: AtomicBool::new(false),
            size: AtomicU64::new(0),
            resize_pending: AtomicBool::new(false),
            long_holds: AtomicUsize::new(0),
            deferred: Mutex::new(Vec::new()),
            alive: AtomicBool::new(true),
            pending: AtomicUsize::new(0),
            flood: AtomicBool::new(false),
            screen_wanted: AtomicBool::new(false),
            limit_watched: AtomicBool::new(false),
            sync_deadline: AtomicU64::new(0),
            epoch: Instant::now(),
            settle_wanted: AtomicBool::new(false),
            pieces: AtomicU32::new(0),
            interp: Mutex::new(Interp {
                blink_mode: BlinkModeScan::default(),
                events: Vec::new(),
                outgoing: Vec::new(),
            }),
        };
        inner.set_size(size);
        inner
    }

    /// The terminal, for everyone but the parser: the fair lock, so a flood
    /// cannot starve the caller. None while a long hold has it, so the
    /// caller never waits longer than one parsed piece.
    pub(crate) fn try_lock_term(&self) -> Option<impl DerefMut<Target = TermState> + '_> {
        self.lockers_waiting.fetch_add(1, Ordering::AcqRel);
        // The lease, then the flag, then the lock: the parser sets the flag
        // under the lease too, so a caller that saw it clear gets the lock
        // before any reflow starts.
        let lease = self.term.lease();
        let guard = (!self.is_held_long()).then(|| self.term.lock_unfair());
        drop(lease);
        self.lockers_waiting.fetch_sub(1, Ordering::AcqRel);
        guard
    }

    /// The terminal, waiting out a long hold if one is under way. Not for
    /// the app's main thread.
    pub(crate) fn lock_term(&self) -> impl DerefMut<Target = TermState> + '_ {
        loop {
            if let Some(guard) = self.try_lock_term() {
                return guard;
            }
            std::thread::sleep(REFLOW_POLL);
        }
    }

    /// Runs f on the terminal now, or, while a long hold has it, right after
    /// that, so the caller does not wait.
    pub(crate) fn with_term(&self, f: impl FnOnce(&mut TermState) + Send + 'static) {
        self.lockers_waiting.fetch_add(1, Ordering::AcqRel);
        let lease = self.term.lease();
        if self.is_held_long() {
            lock(&self.deferred).push(Box::new(f));
            drop(lease);
            self.lockers_waiting.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        let mut guard = self.term.lock_unfair();
        drop(lease);
        self.lockers_waiting.fetch_sub(1, Ordering::AcqRel);
        f(&mut guard);
    }

    /// Called by the parser after releasing the lock: lets a blocked caller
    /// take it first.
    fn step_aside(&self) {
        if self.lockers_waiting.load(Ordering::Acquire) == 0 {
            return;
        }
        let deadline = Instant::now() + STEP_ASIDE_LIMIT;
        while self.lockers_waiting.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
    }

    /// Monotonic counter incremented whenever a chunk of child output is
    /// interpreted. Used to detect active output even when a tab is occluded.
    pub(crate) fn output_counter(&self) -> u32 {
        self.pieces.load(Ordering::Acquire)
    }

    /// Calls the wakeup callback unless one is already outstanding.
    pub(crate) fn request_wakeup(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            self.events.wakeup();
        }
    }

    pub(crate) fn set_size(&self, s: WindowSize) {
        let packed = (s.num_cols as u64) << 48
            | (s.num_lines as u64) << 32
            | (s.cell_width as u64) << 16
            | s.cell_height as u64;
        self.size.store(packed, Ordering::Release);
    }

    pub(crate) fn size(&self) -> WindowSize {
        let p = self.size.load(Ordering::Acquire);
        WindowSize {
            num_cols: (p >> 48) as u16,
            num_lines: (p >> 32) as u16,
            cell_width: (p >> 16) as u16,
            cell_height: p as u16,
        }
    }

    /// Hands the size last stored with [Inner::set_size] to the parser, which
    /// reflows the grid off the caller's thread. Returns false when the
    /// parser has finished, and the caller must apply it with
    /// [Inner::apply_resize] itself.
    pub(crate) fn request_resize(&self) -> bool {
        let io = lock(&self.io);
        if io.parser_done {
            return false;
        }
        self.resize_pending.store(true, Ordering::Release);
        if io.parser_waiting {
            self.data_ready.notify_one();
        }
        true
    }

    /// Whether a thread holds the terminal for long, so the lock may not
    /// come free for a while.
    pub(crate) fn is_held_long(&self) -> bool {
        self.long_holds.load(Ordering::Acquire) > 0
    }

    /// Runs f with the terminal locked for what may be a long time. Others
    /// do not wait meanwhile: snapshots and reads skip the terminal, and
    /// changes are queued, then applied here before the lock is let go.
    fn hold_long<R>(&self, f: impl FnOnce(&mut TermState) -> R) -> R {
        {
            let _lease = self.term.lease();
            self.long_holds.fetch_add(1, Ordering::AcqRel);
        }
        self.locked_long(f)
    }

    /// As [Inner::hold_long], but None at once when another long hold is
    /// under way, so the caller never waits for one.
    pub(crate) fn try_hold_long<R>(&self, f: impl FnOnce(&mut TermState) -> R) -> Option<R> {
        {
            let _lease = self.term.lease();
            if self.is_held_long() {
                return None;
            }
            self.long_holds.fetch_add(1, Ordering::AcqRel);
        }
        Some(self.locked_long(f))
    }

    /// The second half of a long hold, its count already raised.
    fn locked_long<R>(&self, f: impl FnOnce(&mut TermState) -> R) -> R {
        // Counted as waiting, so a flooding parser steps aside for it.
        self.lockers_waiting.fetch_add(1, Ordering::AcqRel);
        let mut state = self.term.lock_unfair();
        self.lockers_waiting.fetch_sub(1, Ordering::AcqRel);
        let result = f(&mut state);
        {
            // Under the lease, so nothing is deferred after the queue is
            // drained; holders of the lease never wait for the lock while a
            // long hold is counted, so taking it here cannot deadlock.
            let _lease = self.term.lease();
            for work in lock(&self.deferred).drain(..) {
                work(&mut state);
            }
            self.long_holds.fetch_sub(1, Ordering::AcqRel);
        }
        result
    }

    /// Resizes the emulator to the size last asked for, then tells the child.
    /// The emulator goes first, so output the child draws for the new size
    /// is parsed at that size.
    pub(crate) fn apply_resize(&self, master: &OwnedFd) {
        let size = self.size();
        self.hold_long(|state| {
            state
                .term
                .resize(size.num_cols as usize, size.num_lines as usize)
        });
        pty::set_winsize(
            master,
            &pty::winsize(
                size.num_cols,
                size.num_lines,
                size.cell_width,
                size.cell_height,
            ),
        );
        self.request_wakeup();
    }

    /// Tells every thread to stop. The caller then closes the wake pipe so a
    /// thread blocked in the kernel notices too.
    pub(crate) fn shut_down(&self) {
        self.shutdown.store(true, Ordering::Release);
        // Taking the lock orders the flag before any waiter's next check.
        drop(lock(&self.io));
        self.data_ready.notify_all();
        self.space_ready.notify_all();
    }

    fn is_shut_down(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

// ---------------------------------------------------------------------------
// Reader

/// Waits on the master, the wake pipe and the child's exit at once.
pub(crate) struct Poller {
    kq: OwnedFd,
    master: RawFd,
    wake: RawFd,
    /// The child had already exited when the poller was set up.
    child_gone: bool,
}

#[derive(Default)]
struct Ready {
    child_exited: bool,
    shutdown: bool,
}

impl Poller {
    pub(crate) fn new(master: &OwnedFd, wake: &OwnedFd, pid: pid_t) -> io::Result<Poller> {
        // SAFETY: kqueue has no preconditions.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: kqueue returned a fresh descriptor we now own.
        let kq = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(kq) };
        let mut poller = Poller {
            kq,
            master: master.as_raw_fd(),
            wake: wake.as_raw_fd(),
            child_gone: false,
        };
        poller.register(poller.master as usize, libc::EVFILT_READ, 0)?;
        poller.register(poller.wake as usize, libc::EVFILT_READ, 0)?;
        match poller.register(pid as usize, libc::EVFILT_PROC, libc::NOTE_EXIT) {
            Ok(()) => {}
            // The child exited before we could watch it.
            Err(e) if e.raw_os_error() == Some(libc::ESRCH) => poller.child_gone = true,
            Err(e) => return Err(e),
        }
        Ok(poller)
    }

    fn register(&self, ident: usize, filter: i16, fflags: u32) -> io::Result<()> {
        let flags = if filter == libc::EVFILT_PROC {
            libc::EV_ADD | libc::EV_ONESHOT
        } else {
            libc::EV_ADD
        };
        let change = libc::kevent {
            ident,
            filter,
            flags,
            fflags,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one change from a local struct, no events requested.
        let rc = unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn wait(&self) -> io::Result<Ready> {
        // SAFETY: kevent is a plain C struct, valid when zeroed.
        let mut events: [libc::kevent; 4] = unsafe { std::mem::zeroed() };
        let n = loop {
            // SAFETY: the kernel writes at most events.len() entries.
            let n = unsafe {
                libc::kevent(
                    self.kq.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    std::ptr::null(),
                )
            };
            if n >= 0 {
                break n as usize;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        };
        let mut ready = Ready::default();
        for ev in &events[..n] {
            if ev.filter == libc::EVFILT_PROC {
                ready.child_exited = true;
            } else if ev.ident == self.wake as usize {
                ready.shutdown = true;
            }
        }
        Ok(ready)
    }
}

enum ReadEnd {
    /// The child is gone and everything it wrote was handed over.
    Exited,
    Shutdown,
}

/// `mode` is present when reads are framed: they sleep until there is
/// output and carry a status byte in front (see [pty::frame_reads]).
/// Otherwise the master is non-blocking and the poller does the waiting.
/// `child_gone`: the child had exited before anything could watch it.
/// `writes`: where replies to what the reader interprets itself go.
pub(crate) fn run_reader(
    inner: Arc<Inner>,
    master: Arc<OwnedFd>,
    poller: Poller,
    pid: pid_t,
    mode: FdMode,
    child_gone: bool,
    writes: Sender<Vec<u8>>,
) {
    let end = read_loop(
        &inner,
        &master,
        &poller,
        pid,
        mode.as_deref(),
        child_gone,
        &writes,
    );
    // The writer runs, and holds the master open, for as long as anyone can
    // send it something; the hang-up below needs it gone.
    drop(writes);
    if mode.is_some() {
        watch::forget(pid);
    }
    match end {
        ReadEnd::Exited => {
            // The session still holds the master; holding it here as well
            // would keep the terminal from hanging up when the session drops
            // while a child that closed its terminal is still running.
            drop(master);
            drop(poller);
            let status = pty::reap(pid);
            inner.alive.store(false, Ordering::Release);
            let mut io = lock(&inner.io);
            io.exit = Some(status);
            inner.data_ready.notify_one();
        }
        ReadEnd::Shutdown => {
            // The last reference to the master to go closes it, which hangs
            // up the terminal and sends the child SIGHUP.
            drop(master);
            drop(poller);
            inner.alive.store(false, Ordering::Release);
            // The session is gone, so its terminal and scrollback go now
            // rather than with the child: one that ignores SIGHUP may run
            // for as long as it likes, and only this thread waits for it.
            drop(inner);
            pty::reap(pid);
        }
    }
}

fn read_loop(
    inner: &Inner,
    master: &OwnedFd,
    poller: &Poller,
    pid: pid_t,
    mode: Option<&Mutex<bool>>,
    child_gone: bool,
    writes: &Sender<Vec<u8>>,
) -> ReadEnd {
    let fd = master.as_raw_fd();
    let framed = mode.is_some();
    let mut buf = [0u8; READ_BUF];
    let mut child_exited = poller.child_gone || child_gone;
    let mut pace = Pace::new();
    let mut chooser = Chooser::new();
    let mut awake = Awake {
        master,
        mode,
        on: false,
    };
    // Since when an awake reader has found the PTY empty.
    let mut dry: Option<Instant> = None;
    loop {
        if inner.is_shut_down() {
            return ReadEnd::Shutdown;
        }
        // A framed read would sleep on an empty PTY, and after the child's
        // exit nothing may ever write to it again.
        if framed && !awake.on && child_exited && !pty::readable_now(master) {
            return ReadEnd::Exited;
        }
        // SAFETY: reading into buf.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            let got = &buf[..n as usize];
            let output = if !framed {
                got
            } else if got[0] == 0 {
                &got[1..]
            } else {
                // A wake-up call: the session is shutting down, which the
                // top of the loop sees, or the child may have exited.
                child_exited |= pty::has_exited(pid);
                continue;
            };
            if output.is_empty() {
                continue;
            }
            dry = None;
            let (window, now) = pace.read(output.len());
            inner.flood.store(pace.flooding, Ordering::Relaxed);
            let forced = Way::forced();
            let way = match forced {
                Some(way) => way,
                // Without framed reads there is one way to wait, the
                // poller, and nothing to choose.
                None if !framed => Way::HANDED,
                None => {
                    if let Some(moved) = window {
                        let can_own = inner.pending.load(Ordering::Acquire) <= OWN_BACKLOG_MAX;
                        chooser.window(moved, pace.flooding, can_own);
                    }
                    chooser.way
                }
            };
            if way.awake {
                awake.enter(forced.is_some());
            } else {
                awake.leave();
            }
            // Output the parser thread still has goes first, so while it
            // has any, it gets this too.
            if way.own && parser_idle(inner) {
                let update_under_way = interpret(inner, writes, output);
                chooser.interpreted(now, Instant::now());
                // The parser thread ends an update that lasts too long.
                // It is told of the first one it does not know of, and
                // looks for itself from then on.
                if update_under_way && !inner.limit_watched.load(Ordering::SeqCst) {
                    nudge_parser(inner);
                }
            } else {
                chooser.handed(now);
                if !publish(inner, output) {
                    return ReadEnd::Shutdown;
                }
            }
            continue;
        }
        if n == 0 {
            return ReadEnd::Exited;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => {}
            // Awake, and the PTY is empty: the child is on its way back to
            // it, or the flood is over.
            Some(libc::EAGAIN) if awake.on => {
                if child_exited {
                    // Drained everything the child wrote before exiting.
                    return ReadEnd::Exited;
                }
                let now = Instant::now();
                if now.duration_since(*dry.get_or_insert(now)) < AWAKE_DRY {
                    std::hint::spin_loop();
                } else {
                    awake.leave();
                    dry = None;
                }
            }
            // Nothing to read. Unframed, that is how a read ends when the
            // PTY is dry; framed, the writer had the descriptor
            // non-blocking for a moment (see write_now).
            Some(libc::EAGAIN) => {
                if child_exited {
                    return ReadEnd::Exited;
                }
                match poller.wait() {
                    Ok(ready) if ready.shutdown => return ReadEnd::Shutdown,
                    Ok(ready) => child_exited |= ready.child_exited,
                    Err(_) => return ReadEnd::Exited,
                }
            }
            // EIO: every holder of the slave side has closed it.
            _ => return ReadEnd::Exited,
        }
    }
}

/// How the reader deals with output: two choices, so four ways.
///
/// Which is fastest depends on what the output is and on the machine. The
/// child refills the kernel's buffer in a time that depends on the bytes
/// (the kernel works on line ends and escapes one by one), and then sleeps
/// until the reader has emptied it. A reader that interprets what it read
/// while the child refills, and is back as the buffer fills, wastes nothing
/// and needs no second thread. But if interpreting takes longer than
/// refilling, the child waits for it, the longer it has waited the slower
/// it is to wake, and handing the bytes to the parser thread is faster. A
/// reader that never sleeps is back at the buffer soonest; but it keeps a
/// core busy, and gets in the way of a child the kernel keeps busy. See
/// [Chooser] for how the way is picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Way {
    /// Never sleeps between reads: the master is non-blocking and an empty
    /// PTY is tried again (see [Awake]). Otherwise the reader sleeps in
    /// read().
    awake: bool,
    /// Interprets what it reads on its own thread, when the parser thread
    /// has nothing left. Otherwise hands it to the parser thread.
    own: bool,
}

impl Way {
    /// Between floods. Nothing is cheaper than sleeping, and nothing puts
    /// output on the screen sooner than interpreting it where it was read.
    const QUIET: Way = Way {
        awake: false,
        own: true,
    };
    /// Sleeping in read() and handing over.
    const HANDED: Way = Way {
        awake: false,
        own: false,
    };
    const ALL: [Way; 4] = [
        Way::HANDED,
        Way {
            awake: true,
            own: false,
        },
        Way::QUIET,
        Way {
            awake: true,
            own: true,
        },
    ];

    /// The way tests have asked every reader to go.
    fn forced() -> Option<Way> {
        let index = FORCED_WAY.load(Ordering::Relaxed);
        (index > 0).then(|| Way::ALL[(index - 1) % Way::ALL.len()])
    }
}

/// Picks the reader's [Way] during a flood, window by window.
///
/// Whether the reader interprets comes from two measurements: what
/// interpreting a read costs the reader, taken while it does so, and how
/// long the child takes to come back with the next read, taken while the
/// reader hands output over and does nothing else. The first as a share of
/// the second decides ([OWN_ENTER], [OWN_LEAVE]). Each is only known from
/// windows spent that way, so a flood starts with a window of each, and
/// every so often one window goes to the way not in use.
///
/// Whether the reader stays awake comes from how much arrives
/// ([AWAKE_ENTER], [AWAKE_LEAVE], [AWAKE_START]).
struct Chooser {
    /// For the window under way.
    way: Way,
    /// The reader interprets; `way.own` differs from this for the one
    /// window of a look at the other way.
    own: bool,
    step: Step,
    /// Windows since the flood began.
    age: u32,
    /// Windows from one look at the way not in use to the next.
    look_after: u32,
    /// What interpreting a read costs, in nanoseconds; None until measured
    /// in this flood. The middle one of the last three windows' costs: a
    /// window in which the terminal made room for a thousand lines of
    /// history, or drew a frame's worth of a synchronized update at once,
    /// costs several times the others and says nothing about them.
    own_cost: Option<u64>,
    recent_costs: [u64; 3],
    /// Whether the window before this one was costed too. A cost taken
    /// after a stretch of handing over stands alone: the ones before it are
    /// from another time.
    costed_last: bool,
    /// The usual time from one read to the next while handing over, in
    /// nanoseconds; None until measured in this flood.
    handed_cycle: Option<u64>,
    /// This window: reads interpreted and the time that took, and the
    /// times between reads handed over.
    interpreted: (u32, u64),
    between: Spread,
    last_handed: Option<Instant>,
}

enum Step {
    /// No flood.
    Quiet,
    /// The flood's second window: handing over, to time the child.
    Timing,
    /// Its third: interpreting again, to cost that.
    Costing,
    /// Windows since the last look at the way not in use.
    Settled(u32),
    /// A look at the way not in use is under way.
    Looking,
}

impl Chooser {
    fn new() -> Chooser {
        Chooser {
            way: Way::QUIET,
            own: true,
            step: Step::Quiet,
            age: 0,
            look_after: LOOK_EVERY,
            own_cost: None,
            recent_costs: [0; 3],
            costed_last: false,
            handed_cycle: None,
            interpreted: (0, 0),
            between: Spread::new(),
            last_handed: None,
        }
    }

    /// The reader interpreted a read from `from` to `to`.
    #[inline]
    fn interpreted(&mut self, from: Instant, to: Instant) {
        self.interpreted.0 += 1;
        self.interpreted.1 += to.duration_since(from).as_nanos() as u64;
        self.last_handed = None;
    }

    /// The reader handed a read over at `now`.
    #[inline]
    fn handed(&mut self, now: Instant) {
        if let Some(last) = self.last_handed {
            self.between.add(now.duration_since(last).as_nanos() as u64);
        }
        self.last_handed = Some(now);
    }

    /// A window has ended in which `moved` bytes arrived. `can_own`: the
    /// parser thread is not behind.
    fn window(&mut self, moved: usize, flooding: bool, can_own: bool) {
        let (own_reads, own_time) = std::mem::take(&mut self.interpreted);
        let handed_reads = self.between.count;
        let usual = self.between.middle();
        self.between = Spread::new();
        if !flooding {
            self.way = Way::QUIET;
            self.own = true;
            self.step = Step::Quiet;
            self.age = 0;
            self.look_after = LOOK_EVERY;
            self.own_cost = None;
            self.recent_costs = [0; 3];
            self.costed_last = false;
            self.handed_cycle = None;
            return;
        }
        self.age += 1;
        // A window spent one way says nothing about the other; one spent
        // partly each way, as when the parser thread had output left, says
        // little about either.
        // Not the window in which the flood was noticed: its first reads
        // met a processor at rest and memory never touched.
        let costed = own_reads >= 32
            && handed_reads < own_reads / 8
            && !matches!(self.step, Step::Quiet);
        if costed {
            let cost = own_time / own_reads as u64;
            self.recent_costs = if self.costed_last {
                [self.recent_costs[1], self.recent_costs[2], cost]
            } else {
                [cost; 3]
            };
            let mut sorted = self.recent_costs;
            sorted.sort_unstable();
            self.own_cost = Some(sorted[1]);
        } else if handed_reads >= 32 && own_reads < handed_reads / 8 {
            self.handed_cycle = usual;
        }
        self.costed_last = costed;
        self.step = match self.step {
            Step::Quiet => Step::Timing,
            Step::Timing => Step::Costing,
            Step::Costing => {
                self.decide();
                Step::Settled(0)
            }
            Step::Looking => {
                // A look that changes nothing makes the next one later.
                self.look_after = if self.decide() {
                    LOOK_EVERY
                } else {
                    (self.look_after * 2).min(LOOK_SELDOM)
                };
                Step::Settled(0)
            }
            Step::Settled(since)
                if since + 1 >= self.look_after || LOOK_EARLY.contains(&self.age) =>
            {
                Step::Looking
            }
            Step::Settled(since) => {
                self.decide();
                Step::Settled(since + 1)
            }
        };
        let own = match self.step {
            Step::Timing => false,
            Step::Costing => true,
            Step::Looking => !self.own,
            Step::Quiet | Step::Settled(_) => self.own,
        };
        // While the parser thread is behind, it stays the interpreter.
        let own = own && can_own;
        let awake = if moved >= AWAKE_ENTER || self.age <= AWAKE_START {
            true
        } else if moved < AWAKE_LEAVE {
            false
        } else {
            self.way.awake
        };
        self.way = Way { awake, own };
    }

    /// Whether the reader interprets, by what the two measurements say.
    /// Returns whether that changed.
    fn decide(&mut self) -> bool {
        let own = match (self.own_cost, self.handed_cycle) {
            (Some(cost), Some(cycle)) if self.own => cost * 100 <= cycle * OWN_LEAVE,
            (Some(cost), Some(cycle)) => cost * 100 < cycle * OWN_ENTER,
            // No window could be spent interpreting, because the parser
            // thread always had output left: it is behind, and interpreting
            // is not for the reader.
            (None, _) => false,
            (Some(_), None) => self.own,
        };
        std::mem::replace(&mut self.own, own) != own
    }
}

/// Times, kept as counts in bands a sixth of their value wide, for their
/// middle value: unlike their mean it is not moved by the few that are a
/// hundred times the rest, when the reader was put off the processor or a
/// frame was being drawn.
struct Spread {
    bands: [u16; Spread::BANDS],
    count: u32,
}

impl Spread {
    /// Four bands to each doubling, from one nanosecond to a third of a
    /// second.
    const BANDS: usize = 4 * 28;

    fn new() -> Spread {
        Spread {
            bands: [0; Spread::BANDS],
            count: 0,
        }
    }

    #[inline]
    fn add(&mut self, nanos: u64) {
        let nanos = nanos.max(4);
        let doubling = 63 - nanos.leading_zeros() as usize;
        // The two bits after the leading one.
        let quarter = ((nanos >> (doubling - 2)) & 3) as usize;
        let band = (4 * doubling + quarter).min(Spread::BANDS - 1);
        self.bands[band] = self.bands[band].saturating_add(1);
        self.count += 1;
    }

    /// The middle of the band the middle time fell in.
    fn middle(&self) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        let mut before = 0;
        for (band, &count) in self.bands.iter().enumerate() {
            before += count as u32;
            if before * 2 >= self.count {
                let (doubling, quarter) = (band / 4, (band % 4) as u64);
                let low = (4 + quarter) << doubling >> 2;
                return Some(low + (low >> 3));
            }
        }
        None
    }
}

/// A reader that stays awake between reads: the master is non-blocking
/// meanwhile, and an empty PTY is simply tried again.
struct Awake<'a> {
    master: &'a OwnedFd,
    mode: Option<&'a Mutex<bool>>,
    on: bool,
}

impl Awake<'_> {
    /// `always`: without regard to how many readers are awake already.
    fn enter(&mut self, always: bool) {
        let Some(mode) = self.mode else { return };
        if self.on {
            return;
        }
        let limit = if always { usize::MAX } else { AWAKE_MAX };
        let taken = AWAKE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < limit).then_some(n + 1)
        });
        if taken.is_err() {
            return;
        }
        let mut nonblocking = lock(mode);
        pty::set_blocking(self.master, false);
        *nonblocking = true;
        self.on = true;
    }

    fn leave(&mut self) {
        let Some(mode) = self.mode else { return };
        if !self.on {
            return;
        }
        let mut nonblocking = lock(mode);
        pty::set_blocking(self.master, true);
        *nonblocking = false;
        drop(nonblocking);
        AWAKE.fetch_sub(1, Ordering::AcqRel);
        self.on = false;
    }
}

impl Drop for Awake<'_> {
    /// The reader has finished. The master stays as it is, which is what
    /// the writer has been told.
    fn drop(&mut self) {
        if self.on {
            AWAKE.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Counts output in windows, and tells a flood from ordinary output by how
/// much arrived in the last one.
struct Pace {
    window_start: Instant,
    bytes: usize,
    flooding: bool,
}

impl Pace {
    fn new() -> Pace {
        Pace {
            window_start: Instant::now(),
            bytes: 0,
            flooding: false,
        }
    }

    /// Counts a read. When that ends a window, returns what arrived in it.
    /// Also returns the time, which the caller has uses for.
    fn read(&mut self, n: usize) -> (Option<usize>, Instant) {
        self.bytes += n;
        let now = Instant::now();
        let window = now.duration_since(self.window_start);
        if window < FLOOD_WINDOW {
            return (None, now);
        }
        // A window that ran long, after a pause, counts for less.
        let whole = (window.as_nanos() / FLOOD_WINDOW.as_nanos()).max(1) as usize;
        let moved = self.bytes / whole;
        self.flooding = moved >= FLOOD_BYTES;
        self.bytes = 0;
        self.window_start = now;
        (Some(moved), now)
    }
}

/// Hands bytes to the parser, then waits while the parser is a megabyte
/// behind. Returns false on shutdown.
fn publish(inner: &Inner, bytes: &[u8]) -> bool {
    let mut io = lock(&inner.io);
    if inner.is_shut_down() {
        return false;
    }
    io.data.extend_from_slice(bytes);
    inner.pending.store(io.data.len(), Ordering::Release);
    if io.parser_waiting {
        inner.data_ready.notify_one();
    }
    while io.data.len() >= MAX_PENDING && !inner.is_shut_down() {
        io.reader_waiting = true;
        io = inner
            .space_ready
            .wait(io)
            .unwrap_or_else(PoisonError::into_inner);
    }
    io.reader_waiting = false;
    !inner.is_shut_down()
}

/// Whether the parser thread has none of the output left, so that the
/// reader may interpret what it reads next and keep the order. Only the
/// reader hands the parser thread output, so the answer holds until it
/// does.
fn parser_idle(inner: &Inner) -> bool {
    let io = lock(&inner.io);
    io.data.is_empty() && !io.parser_busy
}

/// Has the parser thread look again at what it is waiting for: the reader
/// began a synchronized update, whose time limit the parser thread keeps, or
/// there is scrollback to pack once the terminal is at rest.
pub(crate) fn nudge_parser(inner: &Inner) {
    let io = lock(&inner.io);
    if io.parser_waiting {
        inner.data_ready.notify_one();
    }
}

// ---------------------------------------------------------------------------
// Parser

/// What the interpreter's thread does after letting go of the terminal
/// lock.
pub(crate) enum Outgoing {
    Write(Vec<u8>),
    Title(Option<String>),
    Bell,
    Notify(Option<String>, String),
    Progress(crate::Progress),
    ClipboardStore(String),
    ClipboardLoad(Arc<dyn Fn(&str) -> String + Sync + Send + 'static>),
}

/// The interpreter and what it keeps between pieces of output. One for the
/// session: the reader and the parser thread take turns with it, never out
/// of order (see [parser_idle]).
struct Interp {
    blink_mode: BlinkModeScan,
    events: Vec<Event>,
    outgoing: Vec<Outgoing>,
}

struct Parser {
    inner: Arc<Inner>,
    writes: Sender<Vec<u8>>,
    /// For telling the child its new size. Weak, so the parser never keeps
    /// the terminal from hanging up when the session is dropped.
    master: Weak<OwnedFd>,
}

pub(crate) fn run_parser(inner: Arc<Inner>, writes: Sender<Vec<u8>>, master: Weak<OwnedFd>) {
    let mut parser = Parser {
        inner,
        writes,
        master,
    };
    parser.run();
    // From now on whoever resizes applies the size; one asked for before
    // this point is applied here.
    lock(&parser.inner.io).parser_done = true;
    parser.resize_if_asked();
}

impl Parser {
    fn run(&mut self) {
        let mut batch: Vec<u8> = Vec::new();
        loop {
            let taken = self.take_batch(&mut batch);
            if let Taken::Stop = taken {
                return;
            }
            self.resize_if_asked();
            for piece in batch.chunks(PIECE) {
                if self.inner.is_shut_down() {
                    return;
                }
                self.resize_if_asked();
                interpret(&self.inner, &self.writes, piece);
            }
            batch.clear();
            lock(&self.inner.io).parser_busy = false;

            if self.sync_deadline().is_some_and(|d| d <= Instant::now()) {
                end_sync(&self.inner, &self.writes, false);
            }
            if let Taken::Last(status) = taken {
                // A synchronized update the child never ended is its last
                // output; show it.
                if self.sync_deadline().is_some() {
                    end_sync(&self.inner, &self.writes, true);
                }
                if !self.inner.is_shut_down() {
                    // Nothing more will come: what output left as cells is
                    // packed now, while this thread is still here to do it.
                    if self.inner.settle_wanted.load(Ordering::Acquire) {
                        settle_scrollback(&self.inner, None);
                    }
                    self.inner.request_wakeup();
                    self.inner.events.exited(status);
                }
                return;
            }
        }
    }

    /// Applies a size the session was given since the last check.
    fn resize_if_asked(&self) {
        if !self.inner.resize_pending.swap(false, Ordering::AcqRel) {
            return;
        }
        if let Some(master) = self.master.upgrade() {
            self.inner.apply_resize(&master);
        }
    }
}

/// What [Parser::take_batch] found.
enum Taken {
    /// The session is shutting down.
    Stop,
    /// More output to parse.
    More,
    /// The child's last output; then report its exit code.
    Last(Option<i32>),
}

impl Parser {
    fn sync_deadline(&self) -> Option<Instant> {
        self.inner.sync_deadline()
    }

    /// Waits for bytes, the child's exit, or the synchronized-update deadline,
    /// then swaps the reader's buffer with `batch`.
    fn take_batch(&mut self, batch: &mut Vec<u8>) -> Taken {
        let inner = &*self.inner;
        if inner.flood.load(Ordering::Relaxed) {
            let until = Instant::now() + PARSER_LINGER;
            while inner.pending.load(Ordering::Acquire) == 0
                && !inner.is_shut_down()
                && Instant::now() < until
            {
                std::hint::spin_loop();
            }
        }
        let mut io = lock(&inner.io);
        loop {
            if inner.is_shut_down() {
                return Taken::Stop;
            }
            if !io.data.is_empty()
                || io.exit.is_some()
                || inner.resize_pending.load(Ordering::Acquire)
            {
                break;
            }
            // Read without the interpreter's lock: the reader holds that
            // while the app's callbacks run, and they may wait for this one.
            // Cleared before asking: a reader that begins an update after
            // the answer finds this clear, and nudges.
            inner.limit_watched.store(false, Ordering::SeqCst);
            let deadline = self.sync_deadline();
            if deadline.is_some() {
                inner.limit_watched.store(true, Ordering::Release);
            }
            let now = Instant::now();
            if deadline.is_some_and(|d| d <= now) {
                break;
            }
            // Buffers a flood grew are given back once output has stopped
            // for a while; until then the next burst finds them ready.
            let grown = io.data.capacity() > SMALL_BUF
                || batch.capacity() > SMALL_BUF
                || inner.settle_wanted.load(Ordering::Acquire);
            let nap = match deadline {
                Some(d) => Some(d - now),
                None if grown => Some(IDLE_BEFORE_SHRINK),
                None => None,
            };
            io.parser_waiting = true;
            let pieces = inner.pieces.load(Ordering::Acquire);
            io = match nap {
                Some(nap) => {
                    let (mut io, result) = inner
                        .data_ready
                        .wait_timeout(io, nap)
                        .unwrap_or_else(PoisonError::into_inner);
                    let idle = result.timed_out() && deadline.is_none() && io.data.is_empty();
                    if idle {
                        io.data = Vec::new();
                        *batch = Vec::new();
                        if inner.settle_wanted.load(Ordering::Acquire)
                            && inner.pieces.load(Ordering::Acquire) == pieces
                        {
                            // Not under the output lock: the reader must be
                            // able to hand output over meanwhile.
                            io.parser_waiting = false;
                            drop(io);
                            settle_scrollback(inner, Some(pieces));
                            lock(&inner.io)
                        } else {
                            io
                        }
                    } else {
                        io
                    }
                }
                None => inner
                    .data_ready
                    .wait(io)
                    .unwrap_or_else(PoisonError::into_inner),
            };
            io.parser_waiting = false;
        }
        std::mem::swap(&mut io.data, batch);
        io.parser_busy = !batch.is_empty();
        inner.pending.store(0, Ordering::Release);
        if io.reader_waiting {
            inner.space_ready.notify_one();
        }
        match io.exit.take() {
            Some(status) => Taken::Last(status),
            None => Taken::More,
        }
    }
}

/// Packs the lines of the scrollback that output left as cells, a part per
/// hold of the terminal lock so that nothing waits long for the terminal,
/// until all are packed or, with `pieces` the count of pieces interpreted
/// when output was seen to stop, output comes again.
fn settle_scrollback(inner: &Inner, pieces: Option<u32>) {
    loop {
        let more = {
            let mut state = inner.term.lock_unfair();
            let more = state.term.settle(SETTLE_LINES);
            if !more {
                inner.settle_wanted.store(false, Ordering::Release);
            }
            more
        };
        inner.step_aside();
        let moved = pieces.is_some_and(|seen| inner.pieces.load(Ordering::Acquire) != seen);
        if !more || moved || inner.pending.load(Ordering::Acquire) > 0 || inner.is_shut_down() {
            return;
        }
    }
}

/// Interprets a piece of output and acts on what it asked for: replies,
/// the title, the bell, a frame. Returns whether a synchronized update is
/// under way afterwards.
fn interpret(inner: &Inner, writes: &Sender<Vec<u8>>, piece: &[u8]) -> bool {
    let mut guard = lock(&inner.interp);
    let interp = &mut *guard;
    let mut put_aside = false;
    let update_under_way = {
        let mut state = inner.term.lock_unfair();
        let flooding = inner.flood.load(Ordering::Relaxed);
        let mut feed = |state: &mut TermState, from: usize, to: usize| {
            put_aside |= feed(inner, state, piece, from..to, flooding);
        };
        // A sequence setting mode 12 is parsed up to its final byte,
        // which goes on its own so the blinking can be put back.
        let mut at = 0;
        while let Some(i) = interp.blink_mode.find(&piece[at..]) {
            feed(&mut state, at, at + i);
            let style = state.term.cursor_style();
            feed(&mut state, at + i, at + i + 1);
            blink::keep_blinking(&mut state.term, style);
            at += i + 1;
        }
        feed(&mut state, at, piece.len());
        inner.set_sync_deadline(state.term.sync_deadline());
        collect(inner, &mut state, &mut interp.events, &mut interp.outgoing);
        // While an update is under way the terminal is not shown, so the
        // app has nothing to come for: unless a screen was put aside for
        // it, or an update ended on the way, which it should know of to ask
        // for one.
        let ended = state.term.take_ended();
        let woken = state.term.take_wake() | ended;
        // Lines left as cells are packed by the parser thread once output
        // stops; it is told once, below, without the locks held here.
        inner.pieces.fetch_add(1, Ordering::Release);
        let settle = state.term.unsettled() && !inner.settle_wanted.swap(true, Ordering::AcqRel);
        (state.term.sync_update(), woken, settle)
    };
    let (update_under_way, woken, settle) = update_under_way;
    inner.step_aside();
    dispatch(inner, writes, &mut interp.outgoing);
    if woken | !update_under_way | put_aside {
        inner.request_wakeup();
    }
    drop(guard);
    if settle {
        nudge_parser(inner);
    }
    update_under_way
}

/// Interprets `piece[part]`, and puts the screen aside for the app where
/// the terminal can be seen and soon cannot: see [snapshot::show]. Returns
/// whether it did.
///
/// That is in two places. Where an update begins after output the app has
/// not seen, which would otherwise wait for the update's end to be shown.
/// And where an update ends and more follows, if the app has come for the
/// screen and found an update under way: updates one after another leave
/// no other moment for it. In a flood only the second is looked for, and
/// only once the app has asked, so that interpreting is not slowed by the
/// looking: a flood ends soon, or the app asks.
fn feed(
    inner: &Inner,
    state: &mut TermState,
    piece: &[u8],
    part: std::ops::Range<usize>,
    flooding: bool,
) -> bool {
    let mut put_aside = false;
    let mut at = part.start;
    while at < part.end {
        let watch = !flooding || inner.screen_wanted.load(Ordering::Relaxed);
        let (taken, turn) = state.term.advance_to_turn(&piece[at..part.end], watch);
        at += taken;
        let put = match turn {
            Turn::None => false,
            // Not seen: the app has not been by since it was last called,
            // or something came before the beginning in this very piece.
            Turn::Began => {
                !flooding
                    && (at > BEGIN_SYNC_LEN || inner.wake_pending.load(Ordering::Acquire))
            }
            Turn::Ended => at < piece.len() && inner.screen_wanted.swap(false, Ordering::AcqRel),
        };
        if put {
            snapshot::put_aside(state);
            put_aside = true;
        }
    }
    put_aside
}

/// Ends the synchronized update under way: one that has lasted too long,
/// or with `whatever`, any.
fn end_sync(inner: &Inner, writes: &Sender<Vec<u8>>, whatever: bool) {
    let mut interp = lock(&inner.interp);
    let interp = &mut *interp;
    {
        let mut state = inner.term.lock_unfair();
        // The reader may have ended that update, and begun another, since
        // the caller looked.
        let expired = state
            .term
            .sync_deadline()
            .is_some_and(|d| d <= Instant::now());
        if !(expired || whatever) {
            return;
        }
        state.term.stop_sync();
        inner.set_sync_deadline(None);
        collect(inner, &mut state, &mut interp.events, &mut interp.outgoing);
        state.term.take_wake();
    }
    dispatch(inner, writes, &mut interp.outgoing);
    inner.request_wakeup();
}

/// Runs without the terminal lock, so callbacks may call back into the
/// session.
fn dispatch(inner: &Inner, writes: &Sender<Vec<u8>>, outgoing: &mut Vec<Outgoing>) {
    let events = &inner.events;
    for out in outgoing.drain(..) {
        match out {
            // A send fails only when the writer has stopped because the
            // terminal is gone; the reply has nowhere to go then.
            Outgoing::Write(bytes) => drop(writes.send(bytes)),
            Outgoing::Title(title) => events.title(title),
            Outgoing::Bell => events.bell(),
            Outgoing::Notify(title, body) => events.notify(title, body),
            Outgoing::Progress(progress) => events.progress(progress),
            Outgoing::ClipboardStore(text) => events.clipboard_store(text),
            Outgoing::ClipboardLoad(format) => {
                if let Some(text) = events.clipboard_load() {
                    drop(writes.send(format(&text).into_bytes()));
                }
            }
        }
    }
}

/// Runs with the terminal locked: turns queued events into outgoing work,
/// answering the queries that need terminal state now, and publishes the
/// modes.
fn collect(
    inner: &Inner,
    state: &mut TermState,
    events: &mut Vec<Event>,
    outgoing: &mut Vec<Outgoing>,
) {
    inner
        .modes
        .store(modes::pack(state.term.mode()), Ordering::Release);
    state.term.take_events(events);
    let size = inner.size();
    outgoing.extend(events.drain(..).filter_map(|event| translate(event, state, size)));
}

/// What the session does about an event, answering the queries that need
/// terminal state now. `size` is the size the program is told of.
pub(crate) fn translate(event: Event, state: &TermState, size: WindowSize) -> Option<Outgoing> {
    Some(match event {
        Event::PtyWrite(text) => Outgoing::Write(text.into_bytes()),
        Event::ColorRequest(index, format) => Outgoing::Write(format(state.color(index)).into_bytes()),
        Event::TextAreaSizeRequest(format) => Outgoing::Write(format(size).into_bytes()),
        Event::Title(title) => Outgoing::Title(Some(title)),
        Event::ResetTitle => Outgoing::Title(None),
        Event::Bell => Outgoing::Bell,
        Event::Notify(title, body) => Outgoing::Notify(title, body),
        Event::Progress(p) => Outgoing::Progress(match p {
            tern_vt::engine::Progress::Hidden => crate::Progress::Hidden,
            tern_vt::engine::Progress::Set(v) => crate::Progress::Set(v),
            tern_vt::engine::Progress::Error(v) => crate::Progress::Error(v),
            tern_vt::engine::Progress::Indeterminate => crate::Progress::Indeterminate,
            tern_vt::engine::Progress::Pause(v) => crate::Progress::Pause(v),
        }),
        Event::ClipboardStore(text) => Outgoing::ClipboardStore(text),
        Event::ClipboardLoad(format) => Outgoing::ClipboardLoad(format),
        // The terminal turns these into its wake flag instead of queueing them.
        Event::MouseCursorDirty | Event::CursorBlinkingChange => return None,
    })
}

// ---------------------------------------------------------------------------
// Writer

pub(crate) fn run_writer(
    rx: Receiver<Vec<u8>>,
    master: Arc<OwnedFd>,
    wake: Arc<OwnedFd>,
    mode: FdMode,
) {
    while let Ok(bytes) = rx.recv() {
        if !write_all(&master, &wake, &bytes, mode.as_deref()) {
            return;
        }
    }
}

/// Writes what the terminal's input queue takes right now.
///
/// A descriptor the reader sleeps on blocks, and a write must not, so the
/// descriptor is switched around this one call, with the reader kept from
/// changing its mind meanwhile. A read that starts in that moment and finds
/// the PTY empty comes back at once instead of sleeping, which the reader
/// allows for.
fn write_now(master: &OwnedFd, bytes: &[u8], mode: Option<&Mutex<bool>>) -> io::Result<usize> {
    let nonblocking = mode.map(lock);
    let switch = nonblocking.as_deref() == Some(&false);
    if switch {
        pty::set_blocking(master, false);
    }
    // SAFETY: writing from a live slice.
    let n = unsafe { libc::write(master.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    let result = if n >= 0 {
        Ok(n as usize)
    } else {
        Err(io::Error::last_os_error())
    };
    if switch {
        pty::set_blocking(master, true);
    }
    result
}

/// Writes everything, waiting while the terminal's input queue is full.
/// Returns false when the terminal is gone or the session shuts down.
fn write_all(
    master: &OwnedFd,
    wake: &OwnedFd,
    mut bytes: &[u8],
    mode: Option<&Mutex<bool>>,
) -> bool {
    while !bytes.is_empty() {
        let err = match write_now(master, bytes, mode) {
            Ok(n) => {
                bytes = &bytes[n..];
                continue;
            }
            Err(err) => err,
        };
        match err.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::EAGAIN) => {
                let mut fds = [
                    libc::pollfd {
                        fd: master.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: wake.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // SAFETY: polls two local pollfd entries.
                let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                if rc < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return false;
                }
                if fds[1].revents != 0 {
                    return false;
                }
            }
            // EIO: nothing holds the terminal's other side any more.
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: usize = 700 * 1024;
    const SLOW: usize = 300 * 1024;

    /// Ends a window in which every read took `cycle` nanoseconds to come,
    /// and interpreting one cost `cost` if the reader did that.
    fn window(chooser: &mut Chooser, cost: u64, cycle: u64, moved: usize, can_own: bool) {
        let start = Instant::now();
        for read in 0..200u32 {
            let at = start + Duration::from_nanos(cycle * read as u64);
            if chooser.way.own && can_own {
                chooser.interpreted(at, at + Duration::from_nanos(cost));
            } else {
                chooser.handed(at);
            }
        }
        chooser.window(moved, true, can_own);
    }

    fn settle(cost: u64, cycle: u64, moved: usize) -> Chooser {
        let mut chooser = Chooser::new();
        // Past the looks a young flood gets.
        for _ in 0..12 {
            window(&mut chooser, cost, cycle, moved, true);
        }
        chooser
    }

    #[test]
    fn the_reader_interprets_what_costs_it_little() {
        for (cost, cycle, own) in [
            (1000, 3000, true),
            (1700, 3000, true),
            (2300, 3000, false),
            (3500, 3000, false),
            (2400, 5200, true),
            (4200, 5200, false),
        ] {
            let chooser = settle(cost, cycle, FAST);
            assert_eq!(chooser.own, own, "{cost} of {cycle}");
            assert_eq!(chooser.way.own, own, "{cost} of {cycle}");
        }
    }

    #[test]
    fn a_flood_starts_with_a_window_each_way() {
        let mut chooser = Chooser::new();
        assert_eq!(chooser.way, Way::QUIET);
        window(&mut chooser, 3500, 3000, FAST, true);
        assert!(!chooser.way.own, "the second window times the child");
        window(&mut chooser, 3500, 3000, FAST, true);
        assert!(chooser.way.own, "the third costs interpreting again");
        window(&mut chooser, 3500, 3000, FAST, true);
        assert!(!chooser.way.own, "and then it is decided");
    }

    #[test]
    fn a_cost_near_the_line_changes_nothing() {
        // Between the two shares, the way in use stays in use, whichever.
        let mut chooser = settle(1000, 3000, FAST);
        assert!(chooser.own);
        for _ in 0..100 {
            window(&mut chooser, 1900, 3000, FAST, true);
        }
        assert!(chooser.own);
        let mut chooser = settle(3500, 3000, FAST);
        assert!(!chooser.own);
        for _ in 0..100 {
            window(&mut chooser, 1900, 3000, FAST, true);
        }
        assert!(!chooser.own);
    }

    #[test]
    fn output_that_changes_is_noticed_at_the_next_look() {
        let mut chooser = settle(1000, 3000, FAST);
        assert!(chooser.own);
        // Interpreting gets dear: seen within two windows, since it is
        // being done; one dear window alone is passed over.
        window(&mut chooser, 3500, 3000, FAST, true);
        assert!(chooser.own);
        window(&mut chooser, 3500, 3000, FAST, true);
        assert!(!chooser.own);
        // And cheap again: seen when interpreting is next looked at.
        let mut windows = 0;
        while !chooser.own {
            windows += 1;
            assert!(windows <= 2 * LOOK_EVERY + 4, "not noticed");
            window(&mut chooser, 1000, 3000, FAST, true);
        }
    }

    #[test]
    fn one_dear_window_among_cheap_ones_changes_nothing() {
        let mut chooser = settle(1000, 3000, FAST);
        for at in 0..60 {
            let cost = if at % 4 == 0 { 9000 } else { 1000 };
            window(&mut chooser, cost, 3000, FAST, true);
            assert!(chooser.own, "window {at}");
        }
    }

    #[test]
    fn looks_that_change_nothing_grow_rare() {
        let mut chooser = settle(3500, 3000, FAST);
        let mut looks = Vec::new();
        for at in 0..3000u32 {
            if chooser.way.own {
                looks.push(at);
            }
            window(&mut chooser, 3500, 3000, FAST, true);
        }
        assert!(!chooser.own);
        let gaps: Vec<u32> = looks.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(gaps.windows(2).all(|pair| pair[1] >= pair[0]), "{gaps:?}");
        assert_eq!(gaps.last(), Some(&(LOOK_SELDOM + 1)), "{gaps:?}");
        assert!(looks.len() <= 12, "{} windows of 3000 spent looking", looks.len());
    }

    #[test]
    fn a_young_flood_is_looked_at_again_soon() {
        // Interpreting looks dear at first and is cheap once the processor
        // has got going.
        let mut chooser = Chooser::new();
        for _ in 0..4 {
            window(&mut chooser, 6000, 3000, FAST, true);
        }
        assert!(!chooser.own);
        let mut windows = 4;
        while !chooser.own {
            windows += 1;
            assert!(windows <= 12, "not looked at");
            window(&mut chooser, 1000, 3000, FAST, true);
        }
    }

    #[test]
    fn the_reader_leaves_interpreting_to_a_parser_that_is_behind() {
        let mut chooser = settle(1000, 3000, FAST);
        assert!(chooser.way.own);
        window(&mut chooser, 1000, 3000, FAST, false);
        assert!(!chooser.way.own);
        window(&mut chooser, 1000, 3000, FAST, true);
        assert!(chooser.way.own);
        // And a flood in which the parser thread is behind from the start
        // is never the reader's to interpret.
        let mut chooser = Chooser::new();
        for _ in 0..40 {
            window(&mut chooser, 1000, 3000, FAST, false);
        }
        assert!(!chooser.own);
        window(&mut chooser, 1000, 3000, FAST, true);
        assert!(!chooser.way.own);
    }

    #[test]
    fn the_reader_stays_awake_for_fast_output_and_at_the_start() {
        let mut chooser = Chooser::new();
        window(&mut chooser, 1000, 3000, SLOW, true);
        assert!(chooser.way.awake, "at the start, whatever the pace");
        for _ in 0..AWAKE_START {
            window(&mut chooser, 1000, 3000, SLOW, true);
        }
        assert!(!chooser.way.awake);
        window(&mut chooser, 1000, 3000, FAST, true);
        assert!(chooser.way.awake);
        // Between the two paces it stays as it was.
        window(&mut chooser, 1000, 3000, (AWAKE_ENTER + AWAKE_LEAVE) / 2, true);
        assert!(chooser.way.awake);
        window(&mut chooser, 1000, 3000, SLOW, true);
        assert!(!chooser.way.awake);
        window(&mut chooser, 1000, 3000, (AWAKE_ENTER + AWAKE_LEAVE) / 2, true);
        assert!(!chooser.way.awake);
    }

    #[test]
    fn the_end_of_a_flood_starts_over() {
        let mut chooser = settle(3500, 3000, FAST);
        assert!(!chooser.own);
        chooser.window(10, false, true);
        assert_eq!(chooser.way, Way::QUIET);
        assert!(matches!(chooser.step, Step::Quiet));
        assert!(chooser.own_cost.is_none() && chooser.handed_cycle.is_none());
    }

    #[test]
    fn the_middle_time_ignores_the_few_that_are_long() {
        let mut spread = Spread::new();
        assert_eq!(spread.middle(), None);
        for _ in 0..90 {
            spread.add(3000);
        }
        for _ in 0..10 {
            spread.add(2_000_000);
        }
        let middle = spread.middle().unwrap();
        assert!((2700..=3400).contains(&middle), "{middle}");
        // Within an eighth, across the range.
        for nanos in [1u64, 5, 90, 1000, 2999, 4096, 77_777, 1_000_000, 250_000_000] {
            let mut spread = Spread::new();
            spread.add(nanos);
            let middle = spread.middle().unwrap();
            assert!(middle.abs_diff(nanos.max(4)) * 8 <= nanos.max(4) + 8, "{nanos} gave {middle}");
        }
    }
}
