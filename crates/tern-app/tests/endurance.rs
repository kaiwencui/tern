//! Long-running checks: a hidden app keeps its speed, and opening and
//! closing many splits and tabs leaves nothing behind.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::{Duration, Instant};

use support::{test_dir, windows, App};

#[test]
#[ignore = "opens windows and waits a minute"]
fn a_hidden_app_is_not_napped() {
    // App Nap lowers a hidden or covered app's threads to priority 4 after
    // somewhere between ten seconds and a minute; the session threads would
    // crawl.
    let mut app = App::launch("nap", &[]);
    app.wait_prompt();
    app.ok("action hide");
    app.wait_for("the window hidden", |s| {
        !windows(s)[0].get("visible").bool()
    });
    // Even out of App Nap, macOS lowers a hidden app's threads below the
    // default priority after about a minute, unless its activity is
    // latency critical: the main thread and the sessions' user-initiated
    // threads must keep theirs.
    let before = app.thread_priorities();
    let session_level = *before.iter().skip(1).max().expect("threads");
    assert!(session_level >= 37, "session threads at {before:?}");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(75) {
        let now = app.thread_priorities();
        assert!(now[0] > 4, "napped after {:?}", start.elapsed());
        assert!(
            now[0] >= before[0] && now.iter().skip(1).max() >= Some(&session_level),
            "demoted after {:?}: {now:?}, at first {before:?}",
            start.elapsed()
        );
        std::thread::sleep(Duration::from_secs(5));
    }
    // And output is read at full speed.
    let file = test_dir().join("nap.txt");
    std::fs::write(
        &file,
        "0123456789abcdefghijklmnopqrstuvwxyz\n".repeat(200_000),
    )
    .expect("file");
    let begun = Instant::now();
    app.type_line(&format!("cat '{}'; echo cat-done", file.display()));
    app.wait_screen("cat to finish", |s| s.contains("\ncat-done\n"));
    let took = begun.elapsed();
    eprintln!("7.4 MB through a hidden window in {took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    app.quit();
}

/// Opens and closes up to 20 splits, then 20 tabs. The window is wide
/// enough for 21 panes side by side, each split sharing the space out
/// again, when the screen is.
fn cycle(app: &mut App) {
    for _ in 0..20 {
        app.ok("action split:right");
        app.ok("action equalize_splits");
    }
    // As many as fit; a window kept inside a smaller screen holds fewer.
    let state = app.state();
    let panes = state.get("sessions").int();
    assert!(panes >= 8, "only {panes} panes");
    assert_eq!(state.get("live_sessions").int(), panes);
    for _ in 1..panes {
        app.ok("action close_pane");
    }
    for _ in 0..20 {
        app.ok("action new_tab");
    }
    app.wait_for("21 tabs", |s| windows(s).len() == 21);
    for _ in 0..20 {
        app.ok("action close_tab");
    }
    app.wait_for("one tab", |s| windows(s).len() == 1);
}

#[test]
#[ignore = "opens windows"]
fn opening_and_closing_many_splits_and_tabs_leaves_nothing_behind() {
    let mut app = App::launch_with("lk", "window-width = 400\n", &[]);
    app.wait_prompt();
    // One round first, so what the system makes once and keeps (buffers,
    // window objects, a logging connection) is not counted as a leak.
    cycle(&mut app);
    std::thread::sleep(Duration::from_millis(500));
    let baseline = Counts {
        sessions: app.state().get("live_sessions").int(),
        ..Counts::of(&app)
    };
    assert_eq!((baseline.sessions, baseline.children), (1, 1));
    cycle(&mut app);
    let after_one = settle(&mut app, &baseline);
    cycle(&mut app);
    let after_two = settle(&mut app, &baseline);
    eprintln!(
        "descriptors: {} at first, {} and {} after two more rounds of 20 splits and 20 tabs",
        baseline.files.len(),
        after_one.len(),
        after_two.len()
    );
    // The system opens one connection of its own (a Unix socket to a
    // logging service that dozens of system processes share) lazily, in
    // whichever round it pleases, and keeps it. Anything else is a leak,
    // and so is a second such socket.
    for (round, files) in [(1, &after_one), (2, &after_two)] {
        let new: Vec<&String> = files
            .iter()
            .filter(|f| !baseline.files.contains(f))
            .collect();
        assert!(
            new.len() <= 1 && new.iter().all(|f| f.contains(" unix ")),
            "opened by round {round}: {new:?}"
        );
    }
    // Every closed window and tab let go of its window, view, delegate,
    // layer and display link: what is left is the one window open.
    let classes = [
        "TernView",
        "TernWindowDelegate",
        "CAMetalLayer",
        "CADisplayLink",
        "NSKVONotifying_TernTabWindow",
    ];
    let counts = app.heap_counts(&classes);
    assert_eq!(counts, [1, 1, 1, 1, 1], "{classes:?}");
    app.quit();
}

/// What the app holds that a leak would grow.
struct Counts {
    sessions: i64,
    children: usize,
    files: Vec<String>,
    threads: usize,
}

impl Counts {
    fn of(app: &App) -> Counts {
        Counts {
            sessions: 0,
            children: app.children().len(),
            files: app.open_files(),
            threads: app.threads(),
        }
    }
}

/// Waits for the sessions, their children and their threads to be gone
/// after a round, and returns the descriptors open then.
fn settle(app: &mut App, baseline: &Counts) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let sessions = app.state().get("live_sessions").int();
        let now = Counts {
            sessions,
            ..Counts::of(app)
        };
        // Threads of the system's own (dispatch workers, Metal) come and go;
        // what matters is that the sessions' threads are gone.
        let done = (now.sessions, now.children) == (baseline.sessions, baseline.children)
            && now.threads <= baseline.threads
            && app.zombies() == 0;
        if done {
            return now.files;
        }
        assert!(
            Instant::now() < deadline,
            "sessions, children, threads: {:?}, at first {:?}; {} zombies",
            (now.sessions, now.children, now.threads),
            (baseline.sessions, baseline.children, baseline.threads),
            app.zombies()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
