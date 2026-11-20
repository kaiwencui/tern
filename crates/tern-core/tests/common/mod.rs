//! Shared helpers for the integration tests: a recording SessionEvents and
//! waiting with timeouts.

#![allow(dead_code)]

use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use tern_core::{Session, SessionConfig, SessionEvents};
use tern_types::Frame;

pub const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Recorded {
    pub wakeups: usize,
    pub titles: Vec<Option<String>>,
    pub bells: usize,
    pub exits: Vec<Option<i32>>,
    /// The screen text at the moment exited() was called.
    pub exit_text: Option<String>,
    pub clipboard: Vec<String>,
}

#[derive(Default)]
pub struct Recorder {
    pub state: Mutex<Recorded>,
    changed: Condvar,
    session: OnceLock<Weak<Session>>,
    pub clipboard_reply: Mutex<Option<String>>,
    /// While true, clipboard_load blocks the parser thread (to test
    /// back-pressure).
    pub hold_parser: Mutex<bool>,
    hold_changed: Condvar,
}

impl Recorder {
    fn record(&self, f: impl FnOnce(&mut Recorded)) {
        f(&mut self.state.lock().unwrap());
        self.changed.notify_all();
    }

    pub fn with<T>(&self, f: impl FnOnce(&Recorded) -> T) -> T {
        f(&self.state.lock().unwrap())
    }

    /// Waits until the predicate holds for what was recorded.
    pub fn wait(&self, what: &str, pred: impl Fn(&Recorded) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        let mut state = self.state.lock().unwrap();
        while !pred(&state) {
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for {what}");
            state = self.changed.wait_timeout(state, deadline - now).unwrap().0;
        }
    }

    pub fn release_parser(&self) {
        *self.hold_parser.lock().unwrap() = false;
        self.hold_changed.notify_all();
    }
}

impl SessionEvents for Recorder {
    fn wakeup(&self) {
        self.record(|r| r.wakeups += 1);
    }

    fn title(&self, title: Option<String>) {
        self.record(|r| r.titles.push(title));
    }

    fn bell(&self) {
        self.record(|r| r.bells += 1);
    }

    fn exited(&self, status: Option<i32>) {
        // Taking a snapshot from inside the callback shows what was parsed
        // before the exit was reported.
        let text = self.session.get().and_then(Weak::upgrade).map(|s| s.text());
        self.record(|r| {
            r.exits.push(status);
            r.exit_text = text;
        });
    }

    fn clipboard_store(&self, text: String) {
        self.record(|r| r.clipboard.push(text));
    }

    fn clipboard_load(&self) -> Option<String> {
        let mut hold = self.hold_parser.lock().unwrap();
        while *hold {
            hold = self.hold_changed.wait(hold).unwrap();
        }
        self.clipboard_reply.lock().unwrap().clone()
    }
}

pub struct Harness {
    pub session: Arc<Session>,
    pub events: Arc<Recorder>,
    pub frame: Frame,
}

pub fn config(argv: &[&str]) -> SessionConfig {
    SessionConfig {
        command: Some(argv.iter().map(|s| s.to_string()).collect()),
        ..SessionConfig::default()
    }
}

/// Runs a shell snippet with /bin/sh.
pub fn sh(script: &str) -> Harness {
    spawn(config(&["/bin/sh", "-c", script]))
}

pub fn spawn(config: SessionConfig) -> Harness {
    spawn_with(config, Recorder::default())
}

pub fn spawn_with(config: SessionConfig, recorder: Recorder) -> Harness {
    // TERN_TEST_WAY runs every test with readers that deal with output one
    // way (asleep or awake, then handed or own), instead of letting each
    // reader choose by speed: "awake-own", say.
    if let Ok(way) = std::env::var("TERN_TEST_WAY") {
        let (awake, own) = way.split_once('-').expect("TERN_TEST_WAY: asleep|awake, -, handed|own");
        assert!(matches!(awake, "asleep" | "awake") && matches!(own, "handed" | "own"), "{way}");
        tern_core::force_reader_way(Some((awake == "awake", own == "own")));
    }
    let events = Arc::new(recorder);
    let session = Arc::new(Session::spawn(config, events.clone()).expect("spawn"));
    let _ = events.session.set(Arc::downgrade(&session));
    Harness {
        session,
        events,
        frame: Frame::default(),
    }
}

impl Harness {
    /// Polls the screen text until the predicate holds; panics with the
    /// screen on timeout.
    pub fn wait_text(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let text = self.session.text();
            if pred(&text) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; screen:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn wait_for(&self, needle: &str) -> String {
        self.wait_text(needle, |t| t.contains(needle))
    }

    pub fn wait_until(&self, what: &str, pred: impl Fn() -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !pred() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Snapshots until the frame has this grid; a resize is applied by the
    /// session's parser thread.
    pub fn wait_grid(&mut self, cols: u16, rows: u16) {
        let deadline = Instant::now() + TIMEOUT;
        while (self.snapshot().cols, self.frame.rows) != (cols, rows) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for a {cols}x{rows} grid"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn wait_exit(&self) -> Option<i32> {
        self.events.wait("exit", |r| !r.exits.is_empty());
        self.events.with(|r| r.exits[0])
    }

    pub fn snapshot(&mut self) -> &Frame {
        self.session.snapshot(&mut self.frame);
        &self.frame
    }
}

/// Is a process with this pid still around (running or a zombie)?
pub fn process_exists(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
