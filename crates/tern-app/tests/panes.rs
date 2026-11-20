//! Rearranging panes with Command-Option drags, within a tab and from one
//! tab to another, moving tabs between windows, split limits, questions that
//! wait their turn, the pointer after a layout change, and blinking in
//! hidden tabs.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::Duration;

use support::{test_dir, window, windows, App, Json, Png};
use tern_layout::{Layout, Zone};

fn rect(j: &Json) -> [i64; 4] {
    let v = j.ints();
    [v[0], v[1], v[2], v[3]]
}

/// The window's scale and padding in pixels, and cell size.
fn scale(app: &mut App) -> f64 {
    app.first_window().get("scale").num()
}

/// A point inside a pane rectangle (pixels) as view points: fx and fy are
/// fractions of its width and height.
fn inside(r: [i64; 4], fx: f64, fy: f64, scale: f64) -> String {
    let x = r[0] as f64 + r[2] as f64 * fx;
    let y = r[1] as f64 + r[3] as f64 * fy;
    format!("{} {}", x / scale, y / scale)
}

fn pane_rect(app: &mut App, id: i64) -> [i64; 4] {
    rect(app.pane(id).get("rect"))
}

fn layout(app: &mut App) -> String {
    app.first_window().get("layout").str().to_string()
}

#[test]
#[ignore = "opens windows"]
fn splits_stop_before_a_pane_gets_too_small() {
    let mut app = App::launch("sm", &[]);
    app.wait_prompt();
    let w = app.first_window();
    let cell = w.get("cell").ints();
    for _ in 0..8 {
        app.ok("action split:right");
    }
    for _ in 0..8 {
        app.ok("action split:down");
    }
    let panes = app.panes();
    // Each split halves the focused pane until a half would hold fewer
    // than 6 columns or 3 rows: 100 columns give four panes across, 30
    // rows two more below.
    assert!((5..=8).contains(&panes.len()), "{panes:?}");
    for p in &panes {
        let r = rect(p.get("rect"));
        let grid = p.get("grid").ints();
        assert!(grid[0] >= 6 && grid[1] >= 3, "{p:?}");
        // The grid fits inside the rectangle: nobody types into a pane
        // too small to see.
        assert!(
            grid[0] * cell[0] <= r[2] && grid[1] * cell[1] <= r[3],
            "{p:?}"
        );
    }
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_pane_that_closes_while_picked_up_ends_the_move() {
    let mut app = App::launch("mc", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.ok("action split:down");
    for id in [1, 2, 3] {
        app.wait_prompt_of(id);
    }
    let before = layout(&mut app);
    let s = scale(&mut app);
    let r2 = pane_rect(&mut app, 2);
    let r3 = pane_rect(&mut app, 3);
    app.ok(&format!("mouse down {} cmd+alt", inside(r3, 0.5, 0.5, s)));
    app.ok(&format!("mouse drag {} cmd+alt", inside(r2, 0.5, 0.1, s)));
    let w = app.first_window();
    assert_eq!(w.get("gesture").str(), "move");
    // The picked-up pane's shell exits.
    app.run_in(3, "exit");
    let w = app.wait_for("the pane gone", |s| {
        windows(s)[0].get("panes").ints() == [1, 2]
    });
    let w = &windows(&w)[0];
    assert_eq!(w.get("gesture").str(), "none");
    assert_ne!(w.get("cursor").str(), "grab");
    assert!(
        w.get("quads")
            .arr()
            .iter()
            .all(|q| q.get("alpha").num() == 1.0),
        "no highlight is left: {:?}",
        w.get("quads")
    );
    // Letting go does nothing and reports nothing.
    app.ok(&format!("mouse up {} cmd+alt", inside(r2, 0.5, 0.1, s)));
    assert_eq!(layout(&mut app), "h0.5(1,*2)");
    assert_ne!(before, layout(&mut app));
    assert!(!support::read(&app.stderr).contains("cannot move"));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_second_question_waits_for_the_first() {
    let mut app = App::launch_with("qq", "confirm-close = true\n", &[]);
    app.wait_prompt();
    app.type_line("/usr/bin/vim -u NONE");
    app.wait_on("panes", "vim", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0].get("foreground")
            == &Json::Str("vim".into())
    });
    app.ok("action close_pane");
    app.wait_for("the pane question", |s| windows(s)[0].get("sheet").bool());
    // Quit while the pane's question is showing: it is not lost, it waits.
    app.ok("menu-click quit");
    app.ok("action close_pane");
    let s = app.state();
    let questions: Vec<(String, bool)> = s
        .get("questions")
        .arr()
        .iter()
        .map(|q| (q.get("message").str().to_string(), q.get("shown").bool()))
        .collect();
    assert_eq!(
        questions,
        [
            ("Close this pane?".to_string(), true),
            ("Quit Tern?".to_string(), false)
        ],
        "the same request is not asked twice"
    );
    app.ok("confirm no");
    let s = app.wait_for("the quit question", |s| {
        let q = s.get("questions").arr();
        q.len() == 1 && q[0].get("shown").bool() && windows(s)[0].get("sheet").bool()
    });
    assert_eq!(
        s.get("questions").arr()[0].get("message").str(),
        "Quit Tern?"
    );
    let _ = app.try_send("confirm yes");
    assert!(app.wait_exit().success());
}

#[test]
#[ignore = "opens windows"]
fn the_pointer_follows_a_layout_change_without_moving() {
    let mut app = App::launch("pc", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    let s = scale(&mut app);
    let d = rect(app.first_window().get("dividers").arr()[0].get("rect"));
    let (x, y) = ((d[0] + d[2] / 2) as f64 / s, 100.0);
    app.ok(&format!("mouse down {x} {y}"));
    app.ok(&format!("mouse drag {} {y}", x - 40.0));
    app.ok(&format!("mouse up {} {y}", x - 40.0));
    assert_eq!(app.first_window().get("cursor").str(), "col-resize");
    // Zooming takes the divider away from under the still pointer.
    app.ok("action toggle_zoom");
    assert_eq!(app.first_window().get("cursor").str(), "text");
    app.ok("action toggle_zoom");
    assert_eq!(app.first_window().get("cursor").str(), "col-resize");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_hidden_tab_stops_its_blink() {
    let mut app = App::launch_with("hb", "cursor-style-blink = true\n", &[]);
    app.wait_prompt();
    app.ok("focus-window on");
    let blinking = |s: &Json, id: i64| {
        s.get("windows")
            .arr()
            .iter()
            .find(|w| w.get("id").int() == id)
            .map(|w| w.get("blinking").bool())
    };
    app.wait_on("stats", "the blink", |s| blinking(s, 1) == Some(true));
    app.ok("action new_tab");
    app.wait_for("the first tab hidden", |s| {
        window(s, 1).get("occluded").bool()
    });
    app.wait_on("stats", "the hidden tab's blink stopped", |s| {
        blinking(s, 1) == Some(false)
    });
    app.ok("action goto_tab:1");
    app.wait_for("the first tab shown", |s| {
        !window(s, 1).get("occluded").bool()
    });
    app.ok("focus-window on");
    app.wait_on("stats", "the blink back", |s| blinking(s, 1) == Some(true));
    app.quit();
}

/// Where to let go of a pane for each zone of the target, as fractions of
/// the target's rectangle.
fn zone_point(zone: Zone) -> (f64, f64) {
    match zone {
        Zone::Left => (0.1, 0.5),
        Zone::Right => (0.9, 0.5),
        Zone::Top => (0.5, 0.1),
        Zone::Bottom => (0.5, 0.9),
        Zone::Center => (0.5, 0.5),
    }
}

#[test]
#[ignore = "opens windows"]
fn command_option_drags_move_panes_to_every_zone() {
    let mut app = App::launch_with("mv", "window-width = 120\nwindow-height = 40\n", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.ok("action split:down");
    for id in [1, 2, 3] {
        app.wait_prompt_of(id);
        app.run_in(id, &format!("echo pane-{id}-was-here"));
    }
    let s = scale(&mut app);
    for (source, target, zone) in [
        (1i64, 3i64, Zone::Left),
        (2, 1, Zone::Right),
        (3, 2, Zone::Top),
        (1, 2, Zone::Bottom),
        (3, 1, Zone::Center),
    ] {
        let before = layout(&mut app);
        let mut expected: Layout = before.parse().expect("the app's layout text");
        let (s_id, t_id) = (source as u64, target as u64);
        expected.move_pane(s_id, t_id, zone).expect("a valid move");
        expected.set_focus(s_id);
        let from = pane_rect(&mut app, source);
        let to = pane_rect(&mut app, target);
        let (fx, fy) = zone_point(zone);
        app.ok(&format!("mouse down {} cmd+alt", inside(from, 0.5, 0.5, s)));
        app.ok(&format!("mouse drag {} cmd+alt", inside(to, 0.5, 0.5, s)));
        app.ok(&format!("mouse drag {} cmd+alt", inside(to, fx, fy, s)));
        let w = app.first_window();
        assert_eq!(w.get("gesture").str(), "move");
        assert_eq!(w.get("cursor").str(), "grab");
        app.ok(&format!("mouse up {} cmd+alt", inside(to, fx, fy, s)));
        assert_eq!(
            layout(&mut app),
            expected.to_string(),
            "{source} onto {target} {zone:?} from {before}"
        );
        // Every pane's grid follows its new rectangle, and both programs
        // are the same ones, still answering.
        for id in [source, target] {
            let screen = app.screen_of(id);
            assert!(screen.contains(&format!("pane-{id}-was-here")), "{screen}");
            app.check_stty(id);
            app.run_in(id, &format!("echo pane-{id}-was-here"));
            app.wait_screen_of(id, "the answer", |t| {
                t.contains(&format!("pane-{id}-was-here"))
            });
            app.run_in(id, &format!("echo still-{id}-{zone:?}"));
            app.wait_screen_of(id, "the answer", |t| {
                t.contains(&format!("still-{id}-{zone:?}"))
            });
        }
    }
    assert_eq!(app.state().get("live_sessions").int(), 3);
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_pane_move_can_be_called_off() {
    let mut app = App::launch("mc", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.wait_prompt_of(2);
    let s = scale(&mut app);
    let helper = env!("CARGO_MANIFEST_DIR").to_string() + "/tests/helpers/bytes.pl";
    // The left pane's program has the mouse; the gesture never reaches it.
    app.run_in(1, &format!("clear; {helper} 1000 1002 1006"));
    app.wait_screen_of(1, "the helper", |t| {
        t.starts_with("READY") && !t.contains('%')
    });
    let start = layout(&mut app);
    let left = pane_rect(&mut app, 1);
    let right = pane_rect(&mut app, 2);
    // Escape.
    app.ok(&format!("mouse down {} cmd+alt", inside(left, 0.5, 0.5, s)));
    app.ok(&format!(
        "mouse drag {} cmd+alt",
        inside(right, 0.1, 0.5, s)
    ));
    assert_eq!(app.first_window().get("quads").arr().len(), 1 + 1 + 5);
    let reply = app.ok("key escape");
    assert!(
        !reply
            .get("events")
            .arr()
            .iter()
            .any(|e| e.get("down").bool()),
        "the Escape press went nowhere: {reply:?}"
    );
    let w = app.first_window();
    assert_eq!(w.get("gesture").str(), "none");
    assert_eq!(w.get("quads").arr().len(), 1, "only the divider is left");
    app.ok(&format!("mouse up {} cmd+alt", inside(right, 0.1, 0.5, s)));
    assert_eq!(layout(&mut app), start);
    // Let go over the pane itself.
    app.ok(&format!("mouse down {} cmd+alt", inside(left, 0.5, 0.5, s)));
    app.ok(&format!(
        "mouse drag {} cmd+alt",
        inside(right, 0.5, 0.5, s)
    ));
    app.ok(&format!("mouse drag {} cmd+alt", inside(left, 0.9, 0.5, s)));
    app.ok(&format!("mouse up {} cmd+alt", inside(left, 0.9, 0.5, s)));
    assert_eq!(layout(&mut app), start);
    // Let go outside the window.
    app.ok(&format!("mouse down {} cmd+alt", inside(left, 0.5, 0.5, s)));
    app.ok(&format!(
        "mouse drag {} cmd+alt",
        inside(right, 0.1, 0.5, s)
    ));
    app.ok("mouse up -40 -40 cmd+alt");
    assert_eq!(layout(&mut app), start);
    assert_eq!(app.first_window().get("quads").arr().len(), 1);
    // A lone pane is not picked up.
    app.ok("focus-pane 2");
    app.ok("action toggle_zoom");
    app.ok(&format!("mouse down {} cmd+alt", inside(left, 0.5, 0.5, s)));
    assert_eq!(app.first_window().get("gesture").str(), "none");
    app.ok(&format!("mouse up {} cmd+alt", inside(left, 0.5, 0.5, s)));
    app.ok("action toggle_zoom");
    let screen = app.screen_of(1);
    assert!(
        !screen.contains("GOT:"),
        "the program saw the gesture: {screen}"
    );
    app.ok("focus-pane 1");
    app.ok("key ctrl+d");
    app.wait_prompt_of(1);
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn the_move_highlights_look_right() {
    let mut app = App::launch_with("mh", "window-width = 100\nwindow-height = 30\n", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.ok("action split:down");
    app.wait_prompt_of(3);
    let s = scale(&mut app);
    let src = pane_rect(&mut app, 1);
    let dst = pane_rect(&mut app, 3);
    for (name, fx, fy, zone) in [
        ("edge", 0.9, 0.5, Zone::Right),
        ("center", 0.5, 0.5, Zone::Center),
    ] {
        app.ok(&format!("mouse down {} cmd+alt", inside(src, 0.5, 0.5, s)));
        app.ok(&format!("mouse drag {} cmd+alt", inside(dst, fx, fy, s)));
        let shot = test_dir().join(format!("move-{name}.png"));
        app.ok(&format!("screenshot {}", shot.display()));
        let png = Png::read(&shot);
        let number = app.first_window().get("number").int();
        // The window as the screen shows it, for a person to look at.
        let window_shot = test_dir().join(format!("move-{name}-window.png"));
        let _ = std::process::Command::new("/usr/sbin/screencapture")
            .args(["-x", "-l", &number.to_string()])
            .arg(&window_shot)
            .status();
        // The target zone is tinted blue; its outline is solid blue.
        let half = match zone {
            Zone::Right => [dst[0] + dst[2] / 2, dst[1], dst[2] - dst[2] / 2, dst[3]],
            _ => dst,
        };
        let outline = png.rgb(half[0] as u32, (half[1] + half[3] / 2) as u32);
        assert_eq!(outline, 0x3d8af7, "{name}: the outline");
        let tinted = png.rgb(
            (half[0] + half[2] / 2) as u32,
            (half[1] + half[3] - 20) as u32,
        );
        let (r, g, b) = (tinted >> 16, (tinted >> 8) & 0xff, tinted & 0xff);
        assert!(
            b > r + 20 && b > g,
            "{name}: the zone is tinted: {tinted:06x}"
        );
        if zone == Zone::Right {
            // The other half of the target is not.
            let plain = png.rgb((dst[0] + dst[2] / 4) as u32, (dst[1] + dst[3] - 20) as u32);
            let (r, _, b) = (plain >> 16, (plain >> 8) & 0xff, plain & 0xff);
            assert!(b < r + 20, "{name}: the left half is plain: {plain:06x}");
        }
        app.ok("key escape");
        app.ok(&format!("mouse up {} cmd+alt", inside(dst, fx, fy, s)));
    }
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_tab_moved_to_another_window_keeps_working() {
    let mut app = App::launch("tm", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.wait_prompt_of(2);
    app.ok("action new_window");
    let s = app.wait_for("two windows", |s| windows(s).len() == 2);
    let other = support::ids(&s)[1];
    // Window 1, with its two panes, joins the other window's tab bar.
    app.ok(&format!("tab-move 1 {other}"));
    let s = app.wait_for("one tab group", |s| {
        window(s, 1).get("tabs").ints().len() == 2
    });
    assert_eq!(window(&s, other).get("tabs").ints().len(), 2);
    assert_eq!(window(&s, 1).get("selected_tab").int(), 1);
    app.wait_for("the moved tab shown", |s| {
        !window(s, 1).get("occluded").bool()
    });
    // It draws what its programs print.
    let frames = |s: &Json| window(s, 1).get("frames").int();
    let before = frames(&app.state());
    app.run_in(2, "echo after-the-move");
    app.wait_screen_of(2, "the output", |t| t.contains("\nafter-the-move"));
    app.wait_on("drawn 2", "the output drawn", |r| {
        r.get("text").str().contains("\nafter-the-move")
    });
    app.wait_for("new frames", |s| frames(s) > before);
    // Its panes still split, take input and pass stty.
    app.ok("action split:down");
    app.check_stty(2);
    app.check_stty(1);
    // It moves out again, as dragging a tab off the bar does, and back.
    app.ok("tab-detach");
    app.wait_for("its own window", |s| {
        window(s, 1).get("tabs").ints().len() == 1
    });
    app.check_stty(1);
    app.ok("tab-merge");
    app.wait_for("merged", |s| window(s, 1).get("tabs").ints().len() == 2);
    // And it closes cleanly: the tab and its three panes.
    app.ok("focus-window on");
    app.ok("action close_tab");
    let s = app.wait_for("one window", |s| windows(s).len() == 1);
    assert_eq!(s.get("live_sessions").int(), 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(app.children().len(), 1);
    assert_eq!(app.zombies(), 0);
    app.quit();
}

/// The middle of the current window's nth tab (from 1), as view points:
/// above the view, in the tab bar.
fn tab_point(app: &mut App, n: usize) -> String {
    let at = app.wait_on(&format!("tab-center {n}"), "the tab bar", |_| true);
    format!("{} {}", at.get("x").num(), at.get("y").num())
}

fn layout_of(app: &mut App, id: i64) -> Layout {
    let state = app.state();
    let text = window(&state, id).get("layout").str().to_string();
    text.parse().expect("the app's layout text")
}

/// Picks a pane up in the current window and drags it onto the nth tab,
/// which then comes up.
fn carry_to_tab(app: &mut App, pane: i64, n: usize, tab: i64, s: f64) {
    let from = pane_rect(app, pane);
    app.ok(&format!("mouse down {} cmd+alt", inside(from, 0.5, 0.5, s)));
    let over_tab = tab_point(app, n);
    app.ok(&format!("mouse drag {over_tab} cmd+alt"));
    app.wait_for("the tab under the pane up", |s| {
        s.get("current").int() == tab && window(s, tab).get("selected_tab").int() == tab
    });
}

/// No move is under way, and nothing of one is left on screen.
fn assert_no_move(state: &Json) {
    for w in windows(state) {
        assert_eq!(w.get("gesture").str(), "none", "{w:?}");
        assert_ne!(w.get("cursor").str(), "grab", "{w:?}");
        assert!(
            w.get("quads")
                .arr()
                .iter()
                .all(|q| q.get("alpha").num() == 1.0),
            "no highlight is left: {:?}",
            w.get("quads")
        );
    }
}

#[test]
#[ignore = "opens windows"]
fn a_pane_dragged_onto_a_tab_moves_to_that_tab() {
    let mut app = App::launch_with("xt", "window-width = 120\nwindow-height = 40\n", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    for id in [1, 2] {
        app.wait_prompt_of(id);
        app.run_in(id, &format!("echo pane-{id}-was-here"));
    }
    app.ok("action new_tab");
    let state = app.wait_for("a second tab", |s| {
        windows(s).len() == 2 && s.get("current").int() != 1
    });
    let tab2 = windows(&state)[1].get("id").int();
    app.ok("action split:down");
    for id in [3, 4] {
        app.wait_prompt_of(id);
        app.run_in(id, &format!("echo pane-{id}-was-here"));
    }
    app.ok("action goto_tab:1");
    app.wait_for("the first tab up", |s| s.get("current").int() == 1);
    let s = scale(&mut app);

    // Onto an edge of a pane in the other tab: the pane leaves its tab and
    // lands beside that pane, with the focus.
    let mut first = layout_of(&mut app, 1);
    let mut second = layout_of(&mut app, tab2);
    carry_to_tab(&mut app, 2, 2, tab2, s);
    assert_eq!(window(&app.state(), 1).get("gesture").str(), "move");
    let to = pane_rect(&mut app, 3);
    app.ok(&format!("mouse drag {} cmd+alt", inside(to, 0.1, 0.5, s)));
    let state = app.state();
    assert_eq!(window(&state, 1).get("gesture").str(), "move");
    let over = window(&state, tab2);
    assert_eq!(over.get("cursor").str(), "grab");
    // The half the pane would take is marked in the tab it is over.
    let quads = over.get("quads").arr();
    assert_eq!(quads.len(), 1 + 5, "{quads:?}");
    assert_eq!(rect(quads[1].get("rect")), [to[0], to[1], to[2] / 2, to[3]]);
    app.ok(&format!("mouse up {} cmd+alt", inside(to, 0.1, 0.5, s)));
    first.take(2).expect("pane 2 leaves");
    second.insert(3, Zone::Left, 2).expect("pane 2 arrives");
    let state = app.state();
    assert_eq!(window(&state, 1).get("layout").str(), first.to_string());
    assert_eq!(window(&state, tab2).get("layout").str(), second.to_string());
    assert_eq!(window(&state, 1).get("panes").ints(), [1]);
    assert_eq!(window(&state, tab2).get("focused_pane").int(), 2);
    assert_eq!(
        state.get("current").int(),
        tab2,
        "the tab it went to stays up"
    );
    assert_no_move(&state);
    // The program is the same one, in a grid that fits its new place.
    let screen = app.screen_of(2);
    assert!(screen.contains("pane-2-was-here"), "{screen}");
    app.check_stty(2);
    app.run_in(2, "echo still-2-here");
    app.wait_screen_of(2, "the answer", |t| t.contains("\nstill-2-here"));
    app.wait_on("drawn 2", "the answer drawn", |r| {
        r.get("text").str().contains("\nstill-2-here")
    });
    app.check_stty(3);

    // Onto the middle of a pane in the other tab: the two change places.
    let mut first = layout_of(&mut app, 1);
    let mut second = layout_of(&mut app, tab2);
    carry_to_tab(&mut app, 4, 1, 1, s);
    let to = pane_rect(&mut app, 1);
    app.ok(&format!("mouse drag {} cmd+alt", inside(to, 0.5, 0.5, s)));
    let state = app.state();
    let quads = window(&state, 1).get("quads").arr();
    assert_eq!(quads.len(), 5, "{quads:?}");
    assert_eq!(rect(quads[0].get("rect")), to);
    app.ok(&format!("mouse up {} cmd+alt", inside(to, 0.5, 0.5, s)));
    first.replace(1, 4).expect("pane 4 for pane 1");
    first.set_focus(4);
    second.replace(4, 1).expect("pane 1 for pane 4");
    let state = app.state();
    assert_eq!(window(&state, 1).get("layout").str(), first.to_string());
    assert_eq!(window(&state, tab2).get("layout").str(), second.to_string());
    assert_eq!(state.get("current").int(), 1);
    assert_no_move(&state);
    assert_eq!(state.get("live_sessions").int(), 4);
    let screen = app.screen_of(1);
    assert!(screen.contains("pane-1-was-here"), "{screen}");
    app.check_stty(4);

    // A tab's only pane goes too, and the tab it leaves empty closes.
    let mut second = layout_of(&mut app, tab2);
    carry_to_tab(&mut app, 4, 2, tab2, s);
    let to = pane_rect(&mut app, 3);
    app.ok(&format!("mouse drag {} cmd+alt", inside(to, 0.5, 0.9, s)));
    app.ok(&format!("mouse up {} cmd+alt", inside(to, 0.5, 0.9, s)));
    second.insert(3, Zone::Bottom, 4).expect("pane 4 arrives");
    let state = app.wait_for("the emptied tab closed", |s| windows(s).len() == 1);
    assert_eq!(window(&state, tab2).get("layout").str(), second.to_string());
    let mut panes = window(&state, tab2).get("panes").ints();
    panes.sort_unstable();
    assert_eq!(panes, [1, 2, 3, 4]);
    assert_eq!(state.get("live_sessions").int(), 4, "no program ended");
    assert_no_move(&state);
    for id in [4, 1] {
        app.check_stty(id);
        app.run_in(id, &format!("echo still-{id}-here"));
        app.wait_screen_of(id, "the answer", |t| {
            t.contains(&format!("\nstill-{id}-here"))
        });
    }

    // Escape, in the tab the drag brought up, puts the pane back and shows
    // its own tab again.
    app.ok("action new_tab");
    let state = app.wait_for("a new tab", |s| {
        windows(s).len() == 2 && s.get("current").int() != tab2
    });
    let tab3 = state.get("current").int();
    app.wait_prompt_of(5);
    app.ok("action goto_tab:1");
    app.wait_for("the first tab up", |s| s.get("current").int() == tab2);
    let before = (layout_of(&mut app, tab2), layout_of(&mut app, tab3));
    carry_to_tab(&mut app, 2, 2, tab3, s);
    let to = pane_rect(&mut app, 5);
    app.ok(&format!("mouse drag {} cmd+alt", inside(to, 0.5, 0.9, s)));
    assert_eq!(window(&app.state(), tab3).get("quads").arr().len(), 5);
    let reply = app.ok("key escape");
    assert!(
        !reply
            .get("events")
            .arr()
            .iter()
            .any(|e| e.get("down").bool()),
        "the Escape press went nowhere: {reply:?}"
    );
    let state = app.wait_for("its own tab up again", |s| s.get("current").int() == tab2);
    assert_no_move(&state);
    app.ok(&format!("mouse up {} cmd+alt", inside(to, 0.5, 0.9, s)));
    assert_eq!(layout_of(&mut app, tab2), before.0);
    assert_eq!(layout_of(&mut app, tab3), before.1);

    // Let go over the tab bar, where no pane is: the same.
    carry_to_tab(&mut app, 2, 2, tab3, s);
    let over_tab = tab_point(&mut app, 2);
    app.ok(&format!("mouse up {over_tab} cmd+alt"));
    let state = app.wait_for("its own tab up again", |s| s.get("current").int() == tab2);
    assert_no_move(&state);
    assert_eq!(layout_of(&mut app, tab2), before.0);
    assert_eq!(layout_of(&mut app, tab3), before.1);
    assert_eq!(state.get("live_sessions").int(), 5);
    assert!(!support::read(&app.stderr).contains("cannot move"));
    app.quit();
}
