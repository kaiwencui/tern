//! Edge cases and abuse: extreme arguments, callbacks that re-enter or drop
//! the session, concurrent callers, many sessions at once.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{config, process_exists, sh, spawn, Recorder};
use tern_core::{Scroll, SelectionKind, Session, SessionConfig, SessionEvents, Side};
use tern_types::Frame;

#[test]
fn huge_scroll_deltas_clamp_to_the_history() {
    let mut h = sh("seq 1 100");
    h.wait_exit();
    let history = h.snapshot().history;
    assert!(history > 0);
    h.session.scroll(Scroll::Lines(5));
    h.session.scroll(Scroll::Lines(i32::MAX));
    assert_eq!(h.snapshot().display_offset, history);
    h.session.scroll(Scroll::Lines(i32::MIN));
    assert_eq!(h.snapshot().display_offset, 0);
    h.session.scroll(Scroll::Lines(5));
    h.session.scroll(Scroll::Pages(i32::MAX));
    assert_eq!(h.snapshot().display_offset, history);
    h.session.scroll(Scroll::Pages(i32::MIN));
    assert_eq!(h.snapshot().display_offset, 0);
}

#[test]
fn clear_with_a_selection_reaching_into_the_history() {
    let mut h = sh("seq 1 100; read x");
    h.wait_for("100");
    h.session.scroll(Scroll::Top);
    h.session
        .selection_start(0, 0, Side::Left, SelectionKind::Simple);
    h.session.selection_update(500, 0, Side::Right);
    assert!(h.session.has_selection());
    h.session.clear();
    let frame = h.snapshot();
    assert_eq!((frame.history, frame.display_offset), (0, 0));
    // Whatever survived of the selection must still be readable.
    let _ = h.session.selection_text();
    h.session.select_all();
    let _ = h.session.selection_text();
}

#[test]
fn selection_and_links_while_scrolled_back() {
    let mut h = sh(
        "printf '\\033]8;;https://example.com/top\\033\\\\top\\033]8;;\\033\\\\ word\\n'; seq 1 60",
    );
    h.wait_exit();
    h.session.scroll(Scroll::Top);
    let frame = h.snapshot();
    assert_eq!(frame.row_text(0), "top word");
    assert_eq!(
        h.session.hyperlink_at(0, 1).as_deref(),
        Some("https://example.com/top")
    );
    h.session
        .selection_start(0, 4, Side::Left, SelectionKind::Word);
    assert_eq!(h.session.selection_text().as_deref(), Some("word"));
    h.session
        .selection_start(1, 0, Side::Left, SelectionKind::Line);
    assert_eq!(h.session.selection_text().as_deref(), Some("1\n"));
}

#[test]
fn tiny_and_degenerate_sizes() {
    let mut h = spawn(SessionConfig {
        cols: 0,
        rows: 0,
        cell_width: 0,
        cell_height: 0,
        ..config(&[
            "/bin/sh",
            "-c",
            "stty -echo; read x; stty size; read y; printf '日本語'; sleep 5",
        ])
    });
    assert_eq!((h.snapshot().cols, h.frame.rows), (2, 1));
    h.session.resize(3, 2, 0, 0);
    h.wait_grid(3, 2);
    h.session.write(b"\r");
    h.wait_for("2 3\n");
    h.session.write(b"\r");
    // Three columns hold one double-width character per line.
    h.wait_for("本\n語\n");
    let frame = h.snapshot();
    assert_eq!((frame.cols, frame.rows), (3, 2));
    h.session.resize(0, 0, 0, 0);
    h.wait_grid(2, 1);
}

#[test]
fn last_output_is_complete_after_a_flood() {
    // Several megabytes, so the reader hits its limit and waits for the
    // parser more than once before the exit.
    let h = sh("seq 1 400000; exit 5");
    assert_eq!(h.wait_exit(), Some(5));
    let text = h
        .events
        .with(|r| r.exit_text.clone())
        .expect("screen at exit");
    assert!(
        text.contains("399999\n400000\n"),
        "last output missing at exit:\n{text}"
    );
}

#[test]
fn input_after_the_child_exited_is_harmless() {
    let mut h = sh("exit 0");
    h.wait_exit();
    let start = Instant::now();
    h.session.write(b"hello\r");
    h.session.write(&vec![b'x'; 1 << 20]);
    h.session.resize(100, 40, 8, 16);
    h.session.focus(true);
    h.session.clear();
    // Nothing waits on the dead child; the bound only absorbs a loaded
    // machine's scheduling delays.
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(h.snapshot().cols, 100);
    assert_eq!(h.session.foreground_process(), None);
    assert_eq!(h.session.cwd(), None);
}

/// Owns its session and lets go of it from inside a callback.
#[derive(Default)]
struct SelfDropping {
    session: Mutex<Option<Arc<Session>>>,
    frame: Mutex<Frame>,
    snapshots: Mutex<usize>,
    exited: Mutex<Option<Option<i32>>>,
}

impl SessionEvents for SelfDropping {
    fn wakeup(&self) {
        // A snapshot from inside the callback must not deadlock.
        if let Some(s) = self.session.lock().unwrap().as_ref() {
            s.snapshot(&mut self.frame.lock().unwrap());
            *self.snapshots.lock().unwrap() += 1;
        }
    }
    fn title(&self, _: Option<String>) {}
    fn bell(&self) {}
    fn exited(&self, status: Option<i32>) {
        // The last reference goes away on the session's own thread.
        let session = self.session.lock().unwrap().take();
        drop(session);
        *self.exited.lock().unwrap() = Some(status);
    }
    fn clipboard_store(&self, _: String) {}
}

#[test]
fn callbacks_may_snapshot_and_drop_the_session() {
    let events = Arc::new(SelfDropping::default());
    // The pause gives this thread time to hand the session over below: a
    // wake-up that came before then would find no session to snapshot, and
    // there is no second one until a snapshot has been taken.
    let session = Session::spawn(
        config(&["/bin/sh", "-c", "sleep 0.3; seq 1 2000; exit 9"]),
        events.clone(),
    )
    .expect("spawn");
    let pid = session.child_pid();
    *events.session.lock().unwrap() = Some(Arc::new(session));
    let deadline = Instant::now() + common::TIMEOUT;
    while events.exited.lock().unwrap().is_none() {
        assert!(Instant::now() < deadline, "no exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*events.exited.lock().unwrap(), Some(Some(9)));
    assert!(*events.snapshots.lock().unwrap() >= 1);
    assert!(events.session.lock().unwrap().is_none());
    assert!(!process_exists(pid));
}

#[test]
fn concurrent_callers_during_a_flood() {
    let h = sh("i=0; while :; do printf '\\033[3%dm%s \\033]8;;x\\033\\\\link\\033]8;;\\033\\\\ 日本 e\\314\\201\\n' $((i%8)) $i; i=$((i+1)); done");
    h.wait_for("link");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut workers = Vec::new();
    for id in 0..4u16 {
        let session = h.session.clone();
        let stop = stop.clone();
        workers.push(std::thread::spawn(move || {
            let mut frame = Frame::default();
            let mut n = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                n += 1;
                session.snapshot(&mut frame);
                assert_eq!(frame.cells.len(), frame.cols as usize * frame.rows as usize);
                match id {
                    0 => session.resize(20 + (n % 150) as u16, 5 + (n % 50) as u16, 8, 16),
                    1 => {
                        session.scroll(Scroll::Lines((n % 7) as i32 - 3));
                        let _ = session.hyperlink_at((n % 60) as u16, (n % 200) as u16);
                    }
                    2 => {
                        let row = (n % 90) as i32 - 20;
                        session.selection_start(
                            row,
                            (n % 50) as u16,
                            Side::Left,
                            SelectionKind::Word,
                        );
                        session.selection_update(row + 3, (n % 30) as u16, Side::Right);
                        let _ = session.selection_text();
                        if n.is_multiple_of(5) {
                            session.select_all();
                        }
                    }
                    _ => {
                        let _ = session.text();
                        let _ = session.modes();
                        let _ = session.cwd();
                        let _ = session.foreground_process();
                        session.focus(n.is_multiple_of(2));
                    }
                }
            }
            n
        }));
    }
    std::thread::sleep(Duration::from_millis(1500));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for w in workers {
        let n = w.join().expect("worker panicked");
        assert!(n > 10, "a worker made only {n} calls: starved");
    }
    let start = Instant::now();
    drop(h);
    assert!(start.elapsed() < Duration::from_millis(20));
}

#[test]
fn many_sessions_spawned_at_once() {
    // openpty used to fail now and then when called on several threads at
    // once, so spawn many on many threads.
    let handles: Vec<_> = (0..16)
        .map(|i| {
            std::thread::spawn(move || {
                let mut pids = Vec::new();
                for j in 0..8 {
                    let tag = format!("session-{i}-{j}");
                    let h = common::spawn_with(
                        config(&["/bin/sh", "-c", &format!("echo {tag}; sleep 5")]),
                        Recorder::default(),
                    );
                    h.wait_for(&tag);
                    pids.push(h.session.child_pid());
                }
                pids
            })
        })
        .collect();
    let pids: Vec<u32> = handles
        .into_iter()
        .flat_map(|h| h.join().expect("session thread"))
        .collect();
    for pid in pids {
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_exists(pid) {
            assert!(Instant::now() < deadline, "child {pid} left behind");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn a_synchronized_update_larger_than_its_buffer_is_shown() {
    // Three megabytes inside one synchronized update: the emulator ends the
    // update itself when its buffer fills, long before the timeout.
    let h = sh("printf '\\033[?2026h'; i=0; while [ $i -lt 30000 ]; do printf '%0100d\\n' $i; i=$((i+1)); done; printf 'END'; sleep 5");
    h.wait_for("END");
}
