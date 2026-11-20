//! Drives the app's windows through its automation socket: frames, keys,
//! text, mouse, scroll, drops, the menu, tabs, closing and quitting.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::{Duration, SystemTime};

use support::{
    check_on_screen, crash_reports_since, event, ids, px, read, window, windows, App, Json,
};

/// A key spec and the key, modifiers, text and Option sides it should give.
type KeyCase = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static str,
    bool,
    bool,
);
/// A mouse request and the kind, button, position in points and modifiers it should give.
type MouseCase = (
    &'static str,
    &'static str,
    i64,
    f64,
    f64,
    &'static [&'static str],
);

fn sorted(mut v: Vec<i64>) -> Vec<i64> {
    v.sort_unstable();
    v
}

#[test]
#[ignore = "opens windows"]
fn drives_the_app() {
    let started = SystemTime::now();
    let mut app = App::launch("d", &[]);

    // A window exists at launch without activating the app, and draws.
    let state = app.wait_for("the first frame", |s| {
        windows(s).len() == 1 && windows(s)[0].get("frames").int() > 0
    });
    assert!(!state.get("active").bool(), "the app activated itself");
    let first = &windows(&state)[0];
    let id1 = first.get("id").int();
    assert!(first.get("visible").bool() && !first.get("occluded").bool());
    assert!(
        read(&app.stderr).contains("first frame presented"),
        "{}",
        read(&app.stderr)
    );
    check_on_screen(&app, first.get("number").int());
    app.wait_prompt();

    // Resizing changes the size in points and pixels.
    app.ok("resize 480 300");
    let state = app.state();
    let w = window(&state, id1);
    let scale = w.get("scale").num();
    assert_eq!(
        w.get("size_pt")
            .arr()
            .iter()
            .map(Json::num)
            .collect::<Vec<_>>(),
        [480.0, 300.0]
    );
    assert_eq!(
        w.get("size_px").ints(),
        [px(480.0, scale), px(300.0, scale)]
    );

    // Keys arrive translated: (spec, key, mods, text, left option, right option).
    let keys: [KeyCase; 16] = [
        ("a", "Char('a')", &[], "a", false, false),
        ("shift+a", "Char('a')", &["shift"], "A", false, false),
        ("ctrl+c", "Char('c')", &["ctrl"], "", false, false),
        ("alt+left", "Left", &["alt"], "", true, false),
        ("ralt+left", "Left", &["alt"], "", false, true),
        ("cmd+k", "Char('k')", &["super"], "", false, false),
        ("escape", "Escape", &[], "", false, false),
        ("enter", "Enter", &[], "", false, false),
        ("tab", "Tab", &[], "", false, false),
        ("ctrl+tab", "Tab", &["ctrl"], "", false, false),
        ("up", "Up", &[], "", false, false),
        ("down", "Down", &[], "", false, false),
        ("left", "Left", &[], "", false, false),
        ("right", "Right", &[], "", false, false),
        ("f5", "F(5)", &[], "", false, false),
        ("shift+1", "Char('1')", &["shift"], "!", false, false),
    ];
    for (spec, key, mods, text, left, right) in keys {
        let reply = app.ok(&format!("key {spec}"));
        let e = event(&reply, "key");
        assert!(
            matches!(e, Json::Obj(o) if o.contains_key("sent") || o.contains_key("action")),
            "{spec}: {e:?}"
        );
        assert_eq!(e.get("key").str(), key, "{spec}: {e:?}");
        assert_eq!(e.get("mods").strs(), mods, "{spec}: {e:?}");
        assert_eq!(e.get("text").str(), text, "{spec}: {e:?}");
        assert_eq!(e.get("left_option").bool(), left, "{spec}: {e:?}");
        assert_eq!(e.get("right_option").bool(), right, "{spec}: {e:?}");
        assert!(!e.get("composing").bool(), "{spec}: {e:?}");
    }
    // Option with a letter reaches the view as Alt plus the base key; what
    // it types depends on the keyboard layout.
    let reply = app.ok("key alt+a");
    let e = event(&reply, "key");
    assert_eq!(
        (e.get("key").str(), e.get("mods").strs()),
        ("Char('a')", vec!["alt"])
    );
    assert!(e.get("left_option").bool());
    // A modifier on its own arrives through flagsChanged:, side included.
    let reply = app.ok("key ralt");
    let changes: Vec<(String, bool)> = reply
        .get("events")
        .arr()
        .iter()
        .filter(|e| e.get("type").str() == "modifier")
        .map(|e| (e.get("key").str().to_string(), e.get("down").bool()))
        .collect();
    assert_eq!(
        changes,
        [
            ("AltRight".to_string(), true),
            ("AltRight".to_string(), false)
        ]
    );
    let ctrl_c = app.ok("key ctrl+c");
    assert_eq!(event(&ctrl_c, "key").get("characters").str(), "\u{3}");

    // Text insertion.
    let reply = app.ok("text héllo\\ wörld");
    assert_eq!(event(&reply, "text").get("text").str(), "héllo wörld");

    // Mouse and scroll, reported in view pixels.
    let mouse: [MouseCase; 5] = [
        ("mouse down 10 20", "down", 0, 10.0, 20.0, &[]),
        ("mouse drag 30.5 40.25", "drag", 0, 30.5, 40.25, &[]),
        ("mouse up 30.5 40.25", "up", 0, 30.5, 40.25, &[]),
        (
            "mouse down 5 6 right shift",
            "down",
            1,
            5.0,
            6.0,
            &["shift"],
        ),
        ("mouse move 100 50", "move", 0, 100.0, 50.0, &[]),
    ];
    for (line, kind, button, x, y, mods) in mouse {
        let reply = app.ok(line);
        let e = event(&reply, "mouse");
        assert_eq!(e.get("kind").str(), kind, "{line}");
        assert_eq!(e.get("button").int(), button, "{line}");
        assert_eq!(
            (e.get("x").int(), e.get("y").int()),
            (px(x, scale), px(y, scale)),
            "{line}"
        );
        assert_eq!(e.get("mods").strs(), mods, "{line}");
    }
    let reply = app.ok("scroll 50 60 0 -30");
    let e = event(&reply, "scroll");
    assert_eq!(
        (e.get("x").int(), e.get("y").int()),
        (px(50.0, scale), px(60.0, scale))
    );
    assert_eq!((e.get("dx").num(), e.get("dy").num()), (0.0, -30.0));
    assert!(e.get("precise").bool());

    // Drops deliver paths and text at the drop point.
    let reply = app.ok("drop 11 12 path /tmp/a /tmp/b\\ c");
    let e = event(&reply, "drop");
    assert_eq!(
        (e.get("x").int(), e.get("y").int()),
        (px(11.0, scale), px(12.0, scale))
    );
    assert_eq!(e.get("paths").strs(), ["/tmp/a", "/tmp/b c"]);
    let reply = app.ok("drop 3 4 text hello there");
    let e = event(&reply, "drop");
    assert_eq!(
        (e.get("x").int(), e.get("y").int()),
        (px(3.0, scale), px(4.0, scale))
    );
    assert_eq!(e.get("text").str(), "hello there");

    // Input method composition: marked text, then a key that commits it.
    let reply = app.ok("marked か");
    assert_eq!(event(&reply, "preedit").get("text").str(), "か");
    let reply = app.ok("key a");
    assert!(
        event(&reply, "key").get("composing").bool(),
        "the key ends a composition"
    );
    assert_eq!(event(&reply, "preedit").get("text").str(), "");
    // The input method's candidate window goes inside the window.
    let state = app.state();
    let w = window(&state, id1);
    let frame: Vec<f64> = w.get("frame").arr().iter().map(Json::num).collect();
    let ime: Vec<f64> = w.get("ime_rect").arr().iter().map(Json::num).collect();
    assert!(ime[2] > 0.0 && ime[3] > 0.0, "{ime:?}");
    assert!(
        ime[0] >= frame[0] && ime[0] + ime[2] <= frame[0] + frame[2],
        "{ime:?} in {frame:?}"
    );
    assert!(
        ime[1] >= frame[1] && ime[1] + ime[3] <= frame[1] + frame[3],
        "{ime:?} in {frame:?}"
    );

    // The cursor can be changed.
    app.ok("cursor col-resize");
    assert_eq!(window(&app.state(), id1).get("cursor").str(), "col-resize");
    app.ok("cursor text");

    // The menu has the expected shortcuts.
    let menu = app.ok("menu");
    let shell = menu
        .get("menus")
        .arr()
        .iter()
        .find(|m| m.get("title").str() == "Shell")
        .expect("Shell menu");
    let new_tab = shell
        .get("items")
        .arr()
        .iter()
        .find(|i| matches!(i, Json::Obj(o) if o.get("title") == Some(&Json::Str("New Tab".into()))))
        .expect("New Tab");
    assert_eq!(
        (new_tab.get("key").str(), new_tab.get("mods").strs()),
        ("t", vec!["super"])
    );
    assert_eq!(new_tab.get("command").str(), "new_tab");
    assert!(new_tab.get("enabled").bool());
    // Split Right shows its shortcut and is enabled.
    let split = shell
        .get("items")
        .arr()
        .iter()
        .find(|i| matches!(i, Json::Obj(o) if o.get("command") == Some(&Json::Str("split:right".into()))))
        .expect("Split Right");
    assert_eq!(
        (split.get("key").str(), split.get("enabled").bool()),
        ("d", true)
    );

    // Tabs: the menu shortcut, the action, and the tab bar's + button each
    // add a tab to the same group, and the new tab is selected.
    app.ok("key cmd+t");
    let state = app.wait_for("a second tab", |s| windows(s).len() == 2);
    let id2 = *ids(&state).last().expect("id");
    app.ok("action new_tab");
    let state = app.wait_for("a third tab", |s| windows(s).len() == 3);
    let id3 = *ids(&state).last().expect("id");
    app.ok("tab-plus");
    let state = app.wait_for("a fourth tab", |s| windows(s).len() == 4);
    let id4 = *ids(&state).last().expect("id");
    let group = vec![id1, id2, id3, id4];
    for w in windows(&state) {
        assert_eq!(
            w.get("tabs").ints(),
            group,
            "every window shares the tab group"
        );
        assert_eq!(w.get("selected_tab").int(), id4);
    }
    assert_eq!(state.get("current").int(), id4);
    let selected = |app: &mut App| app.state().get("current").int();
    app.ok("action goto_tab:1");
    assert_eq!(selected(&mut app), id1);
    app.ok("action next_tab");
    assert_eq!(selected(&mut app), id2);
    app.ok("action previous_tab");
    assert_eq!(selected(&mut app), id1);
    app.ok("action previous_tab");
    assert_eq!(
        selected(&mut app),
        id4,
        "previous from the first tab wraps around"
    );
    app.ok("action goto_tab:3");
    assert_eq!(selected(&mut app), id3);
    app.ok("action goto_tab:9");
    assert_eq!(selected(&mut app), id4, "tab 9 is the last tab");
    app.ok("action goto_tab:1");
    app.ok("action goto_tab:8");
    assert_eq!(
        selected(&mut app),
        id1,
        "a tab number past the last does nothing"
    );
    app.ok("key cmd+1");
    assert_eq!(selected(&mut app), id1, "cmd+1 selects the first tab");

    // Moving a tab into a window of its own (what dragging it out of the
    // tab bar does) and merging it back are reflected in the tab groups.
    app.ok("action goto_tab:2");
    app.ok("tab-detach");
    let state = app.wait_for("the tab to move out", |s| {
        window(s, id2).get("tabs").ints() == [id2]
    });
    assert_eq!(window(&state, id1).get("tabs").ints(), [id1, id3, id4]);
    app.ok("tab-merge");
    let state = app.wait_for("the windows to merge", |s| {
        window(s, id1).get("tabs").arr().len() == 4
    });
    for w in windows(&state) {
        assert_eq!(sorted(w.get("tabs").ints()), group);
    }

    // A new window gets a tab group of its own.
    app.ok("action new_window");
    let state = app.wait_for("a separate window", |s| windows(s).len() == 5);
    let id5 = *ids(&state).last().expect("id");
    assert_eq!(window(&state, id5).get("tabs").ints(), [id5]);
    assert_eq!(state.get("current").int(), id5);
    app.ok("action close_tab");
    let state = app.wait_for("the window to close", |s| windows(s).len() == 4);
    assert!(!ids(&state).contains(&id5));

    // Closing tabs down to the last window.
    let mut open = 4;
    while open > 0 {
        let before = app.state();
        let current = before.get("current").int();
        app.ok("action close_tab");
        open -= 1;
        let state = app.wait_for("a tab to close", |s| windows(s).len() == open);
        assert!(!ids(&state).contains(&current), "the current tab closed");
        for w in windows(&state) {
            assert_eq!(
                sorted(w.get("tabs").ints()),
                ids(&state),
                "the rest stay in one group"
            );
        }
    }
    // Still running with no windows: quitting is a separate decision.
    assert!(app.child.try_wait().expect("wait").is_none());

    app.ok("quit");
    let status = app.wait_exit();
    assert!(status.success(), "exit status {status}");
    assert!(!app.socket.exists(), "the socket file is removed on quit");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        crash_reports_since(started).is_empty(),
        "crash report: {:?}",
        crash_reports_since(started)
    );
}

#[test]
#[ignore = "opens windows"]
fn quits_after_the_last_window_when_asked() {
    let started = SystemTime::now();
    let mut app = App::launch("q", &["--quit-after-last-window-closed=true"]);
    app.wait_for("the first window", |s| windows(s).len() == 1);
    app.ok("action close_tab");
    let status = app.wait_exit();
    assert!(status.success(), "exit status {status}");
    std::thread::sleep(Duration::from_millis(500));
    assert!(crash_reports_since(started).is_empty());
}

/// After the window's backing scale became want: the font is measured
/// again for it, every grid follows, and the program in pane 2 sees its
/// grid. Returns the cell size.
fn check_scale(app: &mut App, want: f64) -> Vec<i64> {
    let state = app.wait_for("the backing scale to follow", |s| {
        windows(s)[0].get("scale").num() == want
    });
    let w = &windows(&state)[0];
    let pt: Vec<f64> = w.get("size_pt").arr().iter().map(Json::num).collect();
    assert_eq!(w.get("size_px").ints(), [px(pt[0], want), px(pt[1], want)]);
    let cell = w.get("cell").ints();
    for id in [1, 2] {
        let p = app.pane(id);
        let r = p.get("rect").ints();
        let grid = p.get("grid").ints();
        assert!(
            grid[0] * cell[0] <= r[2] && grid[1] * cell[1] <= r[3],
            "{p:?}"
        );
    }
    app.check_stty(2);
    cell
}

#[test]
#[ignore = "opens windows"]
fn moves_between_displays_of_different_scale() {
    let mut app = App::launch("s", &[]);
    let state = app.wait_for("the first window", |s| windows(s).len() == 1);
    let screens = state.get("screens").arr().to_vec();
    let scales: Vec<f64> = screens.iter().map(|s| s.get("scale").num()).collect();
    let start_scale = windows(&state)[0].get("scale").num();
    // Output streams in one pane while the scale changes; the other pane
    // checks the size its program sees.
    app.wait_prompt();
    app.type_line("while :; do date; sleep 0.02; done");
    app.ok("action split:right");
    app.wait_prompt_of(2);
    let mut cells = Vec::new();
    if scales.iter().all(|&s| s == scales[0]) {
        // One scale on this machine: the window is told of the other scale
        // as a move to such a display would tell it, then of its own again.
        eprintln!("every display has scale {start_scale}: changing it through backing-scale");
        let other = if start_scale == 1.0 { 2.0 } else { 1.0 };
        for want in [other, start_scale] {
            app.ok(&format!("backing-scale {want}"));
            cells.push((want, check_scale(&mut app, want)));
        }
    } else {
        // Ending on the screen the window started on, which shows frames.
        let start = screens
            .iter()
            .position(|s| s.get("scale").num() == start_scale)
            .expect("the window's screen");
        let order: Vec<&Json> = (1..=screens.len())
            .map(|i| &screens[(start + i) % screens.len()])
            .collect();
        for screen in order {
            // The bottom-right corner of the screen's visible area.
            let v: Vec<f64> = screen.get("visible").arr().iter().map(Json::num).collect();
            let (x, y) = (v[0] + v[2] - 840.0, v[1] + 20.0);
            app.ok(&format!("move {x} {y}"));
            let want = screen.get("scale").num();
            cells.push((want, check_scale(&mut app, want)));
        }
    }
    let (low, high) = (
        cells.iter().find(|(s, _)| *s == 1.0).expect("a 1x scale"),
        cells.iter().find(|(s, _)| *s == 2.0).expect("a 2x scale"),
    );
    assert!(
        high.1[0] >= 2 * low.1[0] - 1 && high.1[1] >= 2 * low.1[1] - 2,
        "{cells:?}"
    );
    // Back where it started, the streaming pane is drawn and current.
    let frames = app.first_window().get("frames").int();
    app.wait_for("frames again", |s| {
        windows(s)[0].get("frames").int() > frames + 5
    });
    app.ok("focus-pane 1");
    app.ok("key ctrl+c");
    app.wait_prompt_of(1);
    app.check_stty(1);
    app.ok("quit");
    assert!(app.wait_exit().success());
}
