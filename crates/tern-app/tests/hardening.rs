//! Robustness checks: windows shrunk to nothing, resizes and
//! closes in the middle of floods, programs that misbehave (ignoring
//! hangup, writing garbage), huge pastes and many busy panes.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::{Duration, Instant};

use support::{escape, test_dir, windows, App, Png};

/// A file of plain text lines, for floods.
fn flood_file(name: &str, mb: usize) -> std::path::PathBuf {
    let path = test_dir().join(name);
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(2) + "\n";
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) < (mb << 20) as u64 {
        std::fs::write(&path, line.repeat((mb << 20) / line.len() + 1)).expect("flood file");
    }
    path
}

/// The slowest of a run of requests.
fn timed(app: &mut App, request: &str) -> Duration {
    let start = Instant::now();
    app.ok(request);
    start.elapsed()
}

#[test]
#[ignore = "opens windows"]
fn a_window_shrunk_to_almost_nothing_comes_back() {
    let mut app = App::launch("tn", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.wait_prompt_of(2);
    // What is drawn is the whole window at its size, and each pane shows
    // its own screen.
    let check_drawn = |app: &mut App, name: &str| {
        let s = app.state();
        let px = windows(&s)[0].get("size_px").ints();
        let shot = test_dir().join(format!("{name}.png"));
        app.ok(&format!("screenshot {}", shot.display()));
        let png = Png::read(&shot);
        assert_eq!(
            (i64::from(png.width), i64::from(png.height)),
            (px[0].max(1), px[1].max(1)),
            "{name}"
        );
        for id in [1, 2] {
            let drawn = app.ok(&format!("drawn {id}")).get("text").str().to_string();
            let screen = app.screen_of(id);
            assert_eq!(drawn.trim_end(), screen.trim_end(), "{name}, pane {id}");
        }
        png
    };
    for live in ["", " live"] {
        app.ok(&format!("resize 1 1{live}"));
        let s = app.state();
        let w = &windows(&s)[0];
        let size = w.get("size_pt").ints();
        assert!(size[0] >= 1 && size[1] >= 1, "{size:?}");
        for p in app.panes() {
            let grid = p.get("grid").ints();
            assert!(grid[0] >= 2 && grid[1] >= 1, "{p:?}");
        }
        check_drawn(&mut app, "tiny");
        app.ok(&format!("resize 900 600{live}"));
        for id in [1, 2] {
            app.check_stty(id);
        }
        // Back at full size: the divider is where the layout says, in its
        // color, and both panes are drawn whole.
        let png = check_drawn(&mut app, "back");
        let w = app.first_window();
        let divider = w.get("quads").arr()[0].clone();
        let d = divider.get("rect").ints();
        assert_eq!(
            png.rgb((d[0] + d[2] / 2) as u32, (d[1] + d[3] / 2) as u32),
            divider.get("color").int() as u32
        );
    }
    app.type_line("echo still-alive");
    app.wait_screen_of(2, "the echo", |s| s.contains("\nstill-alive\n"));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn hundreds_of_resizes_during_a_flood() {
    let file = flood_file("flood-2m.txt", 2);
    let mut app = App::launch_with("rz", "scrollback-lines = 50000\n", &[]);
    app.wait_prompt();
    app.type_line(&format!("while :; do cat '{}'; done", file.display()));
    std::thread::sleep(Duration::from_millis(300));
    let frames = app.first_window().get("frames").int();
    let start = Instant::now();
    let mut slowest = Duration::ZERO;
    for i in 0..300 {
        let (w, h) = (500 + (i * 37) % 700, 300 + (i * 23) % 400);
        let live = if i % 2 == 0 { " live" } else { "" };
        slowest = slowest.max(timed(&mut app, &format!("resize {w} {h}{live}")));
    }
    let took = start.elapsed();
    let during = app.first_window().get("frames").int() - frames;
    // Back-to-back resizes outpace the display, and the window server
    // shows none of the frames drawn for a size already gone; once the
    // size settles, frames come again while the flood goes on.
    let settled = app.first_window().get("frames").int();
    std::thread::sleep(Duration::from_secs(1));
    let after = app.first_window().get("frames").int() - settled;
    eprintln!(
        "300 resizes during a flood in {took:?}, slowest {slowest:?}; \
         {during} frames shown meanwhile, {after} in the second after"
    );
    assert!(slowest < Duration::from_secs(2), "{slowest:?}");
    assert!(after >= 20, "{after} frames in the second after");
    app.ok("key ctrl+c");
    app.wait_prompt();
    app.ok("resize 800 500");
    app.check_stty(1);
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn closing_every_pane_while_each_floods() {
    let file = flood_file("flood-2m.txt", 2);
    let mut app = App::launch_with("cf", "window-width = 160\nwindow-height = 40\n", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.ok("action split:down");
    app.ok("focus-pane 1");
    app.ok("action split:down");
    for id in 1..=4 {
        app.wait_prompt_of(id);
        app.run_in(id, &format!("while :; do cat '{}'; done", file.display()));
    }
    std::thread::sleep(Duration::from_millis(500));
    let mut slowest = Duration::ZERO;
    for _ in 0..4 {
        slowest = slowest.max(timed(&mut app, "action close_pane"));
    }
    app.wait_for("the window to close", |s| windows(s).is_empty());
    eprintln!("closing a flooding pane took at most {slowest:?}");
    assert!(slowest < Duration::from_secs(1), "{slowest:?}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(app.children().is_empty() && app.zombies() == 0) {
        assert!(
            Instant::now() < deadline,
            "children left: {:?}",
            app.children()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(app.state().get("live_sessions").int(), 0);
    app.ok("action new_window");
    app.wait_prompt();
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_program_that_ignores_hangup_does_not_hold_up_its_pane() {
    let mut app = App::launch(
        "hu",
        &[
            "-e",
            "/bin/sh",
            "-c",
            "trap '' HUP; echo ready; exec /bin/sleep 600",
        ],
    );
    app.wait_screen("the program", |s| s.starts_with("ready\n"));
    let pid = app.pane(1).get("pid").int() as i32;
    app.ok("action split:right");
    app.wait_prompt_of(2);
    app.ok("focus-pane 1");
    let took = timed(&mut app, "action close_pane");
    app.wait_for("one pane", |s| windows(s)[0].get("panes").ints() == [2]);
    assert!(took < Duration::from_millis(500), "{took:?}");
    // The program lives on, as nohup would have it, without holding the
    // app up: the other pane works and the app quits at once.
    // SAFETY: signal 0 only checks that the process exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "the sleep was killed");
    app.type_line("echo after-close");
    app.wait_screen("the echo", |s| s.contains("\nafter-close\n"));
    let start = Instant::now();
    app.quit();
    assert!(start.elapsed() < Duration::from_secs(3));
    // The test started it, so the test ends it.
    // SAFETY: kill takes a pid and a signal; the pid is the test's own sleep.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

#[test]
#[ignore = "opens windows"]
fn megabytes_of_garbage_do_no_harm() {
    // 20 MB of random bytes: invalid UTF-8, broken and valid escape
    // sequences, titles, modes. The bell character is taken out so the
    // test does not beep. A reset and a marker follow.
    let mut app = App::launch(
        "gb",
        &[
            "-e",
            "/bin/sh",
            "-c",
            "head -c 20000000 /dev/urandom | LC_ALL=C tr -d '\\007'; printf '\\033c'; echo GARBAGE-DONE; exec sleep 600",
        ],
    );
    let start = Instant::now();
    let deadline = start + Duration::from_secs(120);
    loop {
        // The first window comes a moment after the control socket.
        let reply = app.send("screen");
        if reply.get("ok").bool() && reply.get("text").str().contains("GARBAGE-DONE") {
            break;
        }
        assert!(Instant::now() < deadline, "the garbage never ended");
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("20 MB of garbage in {:?}", start.elapsed());
    let s = app.state();
    assert_eq!(windows(&s).len(), 1);
    assert!(windows(&s)[0].get("title").str().chars().count() <= 257);
    let stats = app.stats();
    assert!(stats.get("windows").arr()[0].get("frames").int() > 10);
    let shot = test_dir().join("garbage.png");
    app.ok(&format!("screenshot {}", shot.display()));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_megabyte_paste_arrives_whole() {
    let mut app = App::launch("mp", &[]);
    app.wait_prompt();
    let out = test_dir().join("pasted.txt");
    let _ = std::fs::remove_file(&out);
    // Lines well under the terminal's 1024-byte line limit.
    let text: String = (0..10_000)
        .map(|i| format!("{i:05} {}\n", "x".repeat(93)))
        .collect();
    assert_eq!(text.len(), 1_000_000);
    app.type_line(&format!("cat > '{}'", out.display()));
    app.wait_on("panes", "cat running", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0].get("foreground")
            == &support::Json::Str("cat".into())
    });
    app.ok(&format!("clipboard-set {}", escape(&text)));
    let took = timed(&mut app, "action paste");
    assert!(took < Duration::from_millis(500), "{took:?}");
    let deadline = Instant::now() + Duration::from_secs(60);
    while std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) < 1_000_000 {
        assert!(Instant::now() < deadline, "the paste did not arrive");
        std::thread::sleep(Duration::from_millis(100));
    }
    app.ok("key ctrl+d");
    app.wait_prompt();
    assert!(
        support::read(&out) == text,
        "the file differs from the paste"
    );
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn sixteen_panes_flooding_at_once() {
    let mut app = App::launch_with(
        "sx",
        "window-width = 240\nwindow-height = 64\nscrollback-lines = 1000\n",
        &[],
    );
    app.wait_prompt();
    for _ in 0..3 {
        app.ok("action split:right");
    }
    app.ok("action equalize_splits");
    let columns: Vec<i64> = app.panes().iter().map(|p| p.get("id").int()).collect();
    for column in columns {
        app.ok(&format!("focus-pane {column}"));
        for _ in 0..3 {
            app.ok("action split:down");
        }
    }
    app.ok("action equalize_splits");
    let ids: Vec<i64> = app.panes().iter().map(|p| p.get("id").int()).collect();
    assert_eq!(ids.len(), 16);
    for &id in &ids {
        app.wait_prompt_of(id);
        app.run_in(id, &format!("yes {id}-$(printf %060d 0)"));
    }
    app.ok("stats-reset");
    let frames = app.first_window().get("frames").int();
    let start = Instant::now();
    let mut slowest = Duration::ZERO;
    while start.elapsed() < Duration::from_secs(3) {
        slowest = slowest.max(timed(&mut app, "state"));
        std::thread::sleep(Duration::from_millis(100));
    }
    let drawn = app.first_window().get("frames").int() - frames;
    let stats = app.stats();
    let w = &stats.get("windows").arr()[0];
    eprintln!(
        "16 flooding panes: {drawn} frames in 3 s, slowest state request {slowest:?}, \
         frame total {:?} µs, snapshots per frame {:?}",
        w.get("total_us"),
        w.get("snapshots")
    );
    assert!(drawn >= 30, "{drawn} frames in 3 s");
    assert!(slowest < Duration::from_secs(1), "{slowest:?}");
    // Each pane shows its own program's output.
    for &id in &ids {
        let drawn = app.ok(&format!("drawn {id}")).get("text").str().to_string();
        assert!(drawn.contains(&format!("{id}-000")), "pane {id}: {drawn}");
    }
    for &id in &ids {
        app.ok(&format!("focus-pane {id}"));
        app.ok("key ctrl+c");
    }
    for &id in &ids {
        app.wait_prompt_of(id);
    }
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_key_that_changes_nothing_is_not_charged_with_later_output() {
    let mut app = App::launch("kl", &[]);
    app.wait_prompt();
    app.ok("stats-reset");
    let keys = |app: &mut App| {
        app.stats().get("windows").arr()[0]
            .get("key_latency_us")
            .get("count")
            .int()
    };
    // Control-U on an empty line changes nothing; the output after it
    // comes from other input.
    app.ok("key ctrl+u");
    std::thread::sleep(Duration::from_millis(100));
    app.ok(&format!("text {}", escape("echo later-output\n")));
    app.wait_screen("the output", |s| s.contains("\nlater-output\n"));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(keys(&mut app), 0, "the key was charged with the output");
    // A key that does change the screen is timed.
    app.ok("key x");
    app.wait_on("stats", "the key timed", |s| {
        s.get("windows").arr()[0]
            .get("key_latency_us")
            .get("count")
            .int()
            == 1
    });
    app.quit();
}
