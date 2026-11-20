//! Smoke tests against real full-screen programs, skipped when a program is
//! not installed.

mod common;

use std::path::Path;

use common::{config, spawn, Harness};
use tern_core::SessionConfig;

fn installed(path: &str) -> bool {
    let ok = Path::new(path).exists();
    if !ok {
        eprintln!("skipping: {path} is not installed");
    }
    ok
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn vim_edits_and_quits() {
    if !installed("/usr/bin/vim") {
        return;
    }
    let dir = scratch("vim");
    let h = spawn(SessionConfig {
        working_dir: Some(dir),
        ..config(&["/usr/bin/vim", "-u", "NONE", "-N", "-n", "-i", "NONE"])
    });
    h.wait_text("vim's empty buffer", |t| {
        t.lines().filter(|l| l.starts_with('~')).count() > 10
    });
    h.session.write(b"ihello from tern\x1b");
    h.wait_for("hello from tern");
    h.session.write(b"yyp");
    h.wait_for("hello from tern\nhello from tern");
    h.session.write(b":q!\r");
    assert_eq!(h.wait_exit(), Some(0));
}

#[test]
fn less_pages_and_quits() {
    if !installed("/usr/bin/less") {
        return;
    }
    let dir = scratch("less");
    let file = dir.join("lines.txt");
    let text: String = (1..=300).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&file, text).expect("write file");
    let h: Harness = spawn(SessionConfig {
        env: vec![
            ("LESS".into(), String::new()),
            ("LESSHISTFILE".into(), "-".into()),
        ],
        ..config(&["/usr/bin/less", file.to_str().expect("utf-8 path")])
    });
    h.wait_for("line 1\nline 2\n");
    assert!(h.session.modes().alt_screen);
    h.session.write(b" ");
    h.wait_text("second page", |t| {
        t.contains("line 30\n") && !t.contains("line 1\n")
    });
    h.session.write(b"G");
    h.wait_for("line 300");
    h.session.write(b"q");
    assert_eq!(h.wait_exit(), Some(0));
    assert!(!h.session.modes().alt_screen);
}

#[test]
fn tmux_runs_a_shell() {
    let tmux = "/opt/homebrew/bin/tmux";
    if !installed(tmux) {
        return;
    }
    // A private server socket keeps this away from any tmux the user runs.
    let socket = format!("tern-core-test-{}", std::process::id());
    let h = spawn(SessionConfig {
        env: vec![
            ("SHELL".into(), "/bin/sh".into()),
            ("ENV".into(), String::new()),
        ],
        ..config(&[tmux, "-L", &socket, "-f", "/dev/null", "new-session"])
    });
    h.wait_text("tmux status line", |t| {
        t.lines()
            .last()
            .is_some_and(|l| l.contains("[0]") || l.contains("0:"))
    });
    h.session.write(b"echo tern-$((40+2))\r");
    h.wait_for("tern-42");
    h.session.write(b"exit\r");
    let status = h.wait_exit();
    // Make sure the private server is gone even if the session failed.
    let _ = std::process::Command::new(tmux)
        .args(["-L", &socket, "kill-server"])
        .output();
    assert_eq!(status, Some(0));
}
