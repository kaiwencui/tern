//! A flooding child while the parser is stalled: the reader must stop
//! reading instead of buffering without bound. Alone in its own test binary
//! because it measures the process's peak memory.

mod common;

use std::time::Duration;

use common::{config, spawn_with, Recorder};

fn peak_rss_bytes() -> i64 {
    // SAFETY: rusage is a plain C struct, valid when zeroed.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage writes into the local struct.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    // Bytes on macOS.
    usage.ru_maxrss
}

#[test]
fn memory_stays_bounded_while_the_parser_is_stalled() {
    let recorder = Recorder::default();
    *recorder.hold_parser.lock().unwrap() = true;
    *recorder.clipboard_reply.lock().unwrap() = Some(String::new());
    // The clipboard query stalls the parser inside clipboard_load; yes then
    // writes as fast as the terminal accepts.
    let h = spawn_with(
        config(&["/bin/sh", "-c", "printf '\\033]52;c;?\\007'; exec yes"]),
        recorder,
    );
    let before = peak_rss_bytes();
    std::thread::sleep(Duration::from_millis(1500));
    let grown = peak_rss_bytes() - before;
    assert!(grown < 16 << 20, "memory grew by {} MB", grown >> 20);
    assert!(h.session.is_alive());

    // Once the parser runs again, output flows.
    h.events.release_parser();
    h.wait_for("y\ny\ny");
    let mut frame = tern_types::Frame::default();
    h.session.snapshot(&mut frame);
    assert!(frame.history > 0);
}
