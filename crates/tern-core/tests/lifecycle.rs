//! Dropping sessions: quick, and no child left behind as a zombie.

mod common;

use std::time::{Duration, Instant};

use common::{config, process_exists, sh, spawn};
use tern_core::SessionConfig;

fn assert_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) {
        assert!(
            Instant::now() < deadline,
            "child {pid} still exists (running or zombie)"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn drop_quickly(h: common::Harness) -> u32 {
    let pid = h.session.child_pid();
    let events = h.events.clone();
    let start = Instant::now();
    drop(h);
    let took = start.elapsed();
    assert!(took < Duration::from_millis(20), "drop took {took:?}");
    assert_gone(pid);
    // The app let go of the session; it hears nothing more from it.
    std::thread::sleep(Duration::from_millis(50));
    assert!(events.with(|r| r.exits.is_empty()));
    pid
}

#[test]
fn dropping_a_running_child_is_quick_and_reaps_it() {
    let h = spawn(config(&["sleep", "100"]));
    h.wait_until("child running", || h.session.is_alive());
    drop_quickly(h);
}

#[test]
fn dropping_a_login_shell() {
    let zdotdir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("zdotdir-drop");
    std::fs::create_dir_all(&zdotdir).expect("zdotdir");
    let h = spawn(SessionConfig {
        shell: Some("/bin/zsh".into()),
        env: vec![("ZDOTDIR".into(), zdotdir.display().to_string())],
        ..SessionConfig::default()
    });
    h.wait_text("a prompt", |t| !t.trim().is_empty());
    drop_quickly(h);
}

#[test]
fn dropping_while_the_child_floods() {
    let h = sh("yes");
    h.wait_for("y\ny\n");
    drop_quickly(h);
}

#[test]
fn dropping_while_input_is_stuck() {
    // The child never reads, so the writer thread is blocked on a full queue.
    let h = sh("sleep 100");
    h.session.write(&vec![b'x'; 1 << 20]);
    std::thread::sleep(Duration::from_millis(50));
    drop_quickly(h);
}

#[test]
fn dropping_with_a_full_scrollback() {
    let h = spawn(SessionConfig {
        cols: 200,
        rows: 60,
        ..config(&["seq", "1", "30000"])
    });
    h.wait_exit();
    let start = Instant::now();
    drop(h);
    let took = start.elapsed();
    assert!(took < Duration::from_millis(20), "drop took {took:?}");
}

#[test]
fn dropping_a_child_that_closed_its_terminal() {
    // The PTY reports end of file while the child keeps running; dropping
    // the session must still hang it up and reap it.
    let h = sh("printf ready; exec 0<&- 1>&- 2>&-; sleep 100");
    h.wait_for("ready");
    std::thread::sleep(Duration::from_millis(50));
    assert!(h.session.is_alive());
    drop_quickly(h);
}

#[test]
fn dropping_after_exit() {
    let h = sh("exit 0");
    assert_eq!(h.wait_exit(), Some(0));
    assert!(!h.session.is_alive());
    let pid = h.session.child_pid();
    drop(h);
    assert!(!process_exists(pid));
}

#[test]
fn dropping_a_child_that_ignores_sighup_lets_the_session_go() {
    // The hang-up does not end it, so it outlives the session; what the
    // session held must not wait for it.
    let h = sh("trap '' HUP; echo ready; exec sleep 30");
    h.wait_for("ready");
    let pid = h.session.child_pid();
    let events = h.events.clone();
    drop(h);
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::sync::Arc::strong_count(&events) > 1 {
        assert!(
            Instant::now() < deadline,
            "the session's state is still held while its child runs"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(process_exists(pid), "the child should have ignored SIGHUP");
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .expect("kill");
    assert!(killed.success());
    // Still reaped, by the thread that waited for it.
    assert_gone(pid);
}
