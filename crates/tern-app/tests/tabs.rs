//! Docking tabs: a tab dragged out of its tab bar and let go over the edge
//! of a pane joins that pane's tab.
//!
//! The drags are AppKit's own, started by a press on a tab and followed by
//! its tab bar; only the pointer is the automation socket's. A tab that is
//! let go without docking becomes a window where the real pointer is, as
//! AppKit puts it there.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::Duration;

use support::{window, windows, App, Json};
use tern_layout::{zone_rect, Layout, Zone};
use tern_types::Rect;

fn rect(j: &Json) -> [i64; 4] {
    let v = j.ints();
    [v[0], v[1], v[2], v[3]]
}

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

/// Where a window marks that a held tab would dock: its tinted area.
fn mark(state: &Json, id: i64) -> Option<[i64; 4]> {
    window(state, id)
        .get("quads")
        .arr()
        .iter()
        .find(|q| q.get("alpha").num() < 1.0)
        .map(|q| rect(q.get("rect")))
}

/// The half of a pane's rectangle that a zone marks.
fn half(r: [i64; 4], zone: Zone) -> [i64; 4] {
    let px = |v: i64| i32::try_from(v).expect("a pixel");
    let h = zone_rect(Rect::new(px(r[0]), px(r[1]), px(r[2]), px(r[3])), zone);
    [h.x.into(), h.y.into(), h.w.into(), h.h.into()]
}

fn layout_of(state: &Json, id: i64) -> String {
    window(state, id).get("layout").str().to_string()
}

/// What a layout is after a tab with another layout docks at a zone of one
/// of its panes.
fn docked(layout: &str, pane: u64, zone: Zone, tab: &str) -> String {
    let mut layout: Layout = layout.parse().expect("a layout");
    let tab: Layout = tab.parse().expect("a layout");
    layout
        .insert_layout(pane, zone, &tab)
        .expect("a valid dock");
    layout.to_string()
}

/// Waits until no tab is held.
fn let_go(app: &mut App) -> Json {
    app.wait_for("the tab let go", |s| s.get("tab_drag") == &Json::Null)
}

/// Checks that a pane shows what it showed before it moved, has the grid
/// its program sees, and still answers.
fn still_working(app: &mut App, id: i64) {
    let screen = app.screen_of(id);
    assert!(screen.contains(&format!("pane-{id}-was-here")), "{screen}");
    app.check_stty(id);
    app.run_in(id, &format!("echo still-{id}-here"));
    app.wait_screen_of(id, "the answer", |t| {
        t.contains(&format!("\nstill-{id}-here"))
    });
}

#[test]
#[ignore = "opens windows"]
fn a_tab_let_go_on_the_edge_of_a_pane_docks_beside_it() {
    let mut app = App::launch_with("td", "window-width = 120\nwindow-height = 40\n", &[]);
    app.wait_prompt();
    // The first tab has two panes, one above the other; the second has one.
    app.ok("action split:down");
    for id in [1, 2] {
        app.wait_prompt_of(id);
        app.run_in(id, &format!("echo pane-{id}-was-here"));
    }
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.wait_prompt_of(3);
    app.run_in(3, "echo pane-3-was-here");
    let s = scale(&mut app);
    let r3 = pane_rect(&mut app, 3);

    // Held over the right edge of the other tab's pane, the first tab
    // would take that half.
    app.ok("tab-pick 2 1");
    app.ok(&format!("tab-drag 2 {}", inside(r3, 0.9, 0.5, s)));
    let state = app.wait_for("the mark", |s| mark(s, 2).is_some());
    assert_eq!(state.get("tab_drag").int(), 1);
    let r3 = pane_rect(&mut app, 3);
    assert_eq!(mark(&app.state(), 2), Some(half(r3, Zone::Right)));
    assert_eq!(mark(&state, 1), None);
    // Over the middle it would dock nowhere.
    app.ok(&format!("tab-drag 2 {}", inside(r3, 0.5, 0.5, s)));
    app.wait_for("no mark", |s| mark(s, 2).is_none());
    // Over the left edge it takes the left half, and is let go there.
    app.ok(&format!("tab-drag 2 {}", inside(r3, 0.1, 0.5, s)));
    app.wait_for("the mark", |s| mark(s, 2) == Some(half(r3, Zone::Left)));
    app.ok("tab-drop");

    let state = app.wait_for("the tab docked", |s| windows(s).len() == 1);
    let state = if state.get("tab_drag") == &Json::Null {
        state
    } else {
        let_go(&mut app)
    };
    let w = window(&state, 2);
    // Its panes are arranged as they were, beside the pane it was let go on.
    assert_eq!(
        w.get("layout").str(),
        docked("*3", 3, Zone::Left, "v0.5(1,*2)")
    );
    assert_eq!(w.get("layout").str(), "h0.5(v0.5(1,*2),3)");
    assert_eq!(w.get("focused_pane").int(), 2);
    assert_eq!(w.get("panes").ints(), [1, 2, 3]);
    assert_eq!(w.get("tabs").ints().len(), 1);
    assert_eq!(mark(&state, 2), None, "the mark went with the drag");
    // The programs are the same ones, each with the grid of its new place.
    for id in [1, 2, 3] {
        still_working(&mut app, id);
    }
    let state = app.state();
    assert_eq!(state.get("live_sessions").int(), 3);
    assert_eq!(app.children().len(), 3);
    assert_eq!(app.zombies(), 0);
    assert!(!support::read(&app.stderr).contains("cannot move"));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn the_tab_that_shows_docks_too_and_the_middle_of_a_pane_takes_no_tab() {
    let mut app = App::launch("ts", &[]);
    app.wait_prompt();
    app.run_in(1, "echo pane-1-was-here");
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.wait_prompt_of(2);
    app.run_in(2, "echo pane-2-was-here");
    let s = scale(&mut app);
    // The second tab is the one showing. Dragged off the bar it leaves the
    // first showing, and docks at the bottom of that tab's pane.
    let r1 = pane_rect(&mut app, 1);
    app.ok("tab-pick 1 2");
    app.ok(&format!("tab-drag 1 {}", inside(r1, 0.5, 0.9, s)));
    app.wait_for("the mark", |s| mark(s, 1).is_some());
    app.ok("tab-drop");
    let state = app.wait_for("the tab docked", |s| windows(s).len() == 1);
    assert_eq!(layout_of(&state, 1), "v0.5(1,*2)");
    for id in [1, 2] {
        still_working(&mut app, id);
    }

    // A tab let go over the middle of a pane docks nowhere: AppKit makes it
    // a window, as it always has.
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.wait_prompt_of(3);
    let r3 = pane_rect(&mut app, 3);
    app.ok("tab-pick 3 1");
    app.ok(&format!("tab-drag 3 {}", inside(r3, 0.5, 0.5, s)));
    app.wait_for("the tab held", |s| s.get("tab_drag") == &Json::Num(1.0));
    // Long enough for the pointer to have been looked at several times.
    std::thread::sleep(Duration::from_millis(250));
    let state = app.state();
    assert_eq!(mark(&state, 3), None);
    assert_eq!(mark(&state, 1), None);
    app.ok("tab-drop");
    let state = let_go(&mut app);
    assert_eq!(windows(&state).len(), 2);
    assert_eq!(layout_of(&state, 1), "v0.5(1,*2)");
    assert_eq!(layout_of(&state, 3), "*3");
    assert_eq!(state.get("live_sessions").int(), 3);
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_tab_docks_in_another_window_but_not_where_its_panes_would_not_fit() {
    let mut app = App::launch_with("tx", "window-width = 60\nwindow-height = 20\n", &[]);
    app.wait_prompt();
    app.run_in(1, "echo pane-1-was-here");
    let frame = app.first_window().get("frame").arr().to_vec();
    // A second window with two tabs, and the two windows apart. (Whichever
    // of them is the current one moves.)
    app.ok("action new_window");
    app.wait_for("two windows", |s| windows(s).len() == 2);
    app.ok(&format!(
        "move {} {}",
        frame[0].num() - frame[2].num() - 40.0,
        frame[1].num()
    ));
    app.ok("action new_window");
    app.wait_for("three windows", |s| windows(s).len() == 3);
    app.ok("tab-move 3 2");
    app.wait_for("two tabs", |s| window(s, 2).get("tabs").ints() == [2, 3]);
    for id in [2, 3] {
        app.wait_prompt_of(id);
    }
    let s = scale(&mut app);

    // The second window's first tab, out of the tab bar its other tab
    // shows, onto the top edge of the first window's pane.
    let r1 = pane_rect(&mut app, 1);
    app.ok("tab-pick 2 1");
    app.ok(&format!("tab-drag 1 {}", inside(r1, 0.5, 0.1, s)));
    let state = app.wait_for("the mark", |s| mark(s, 1).is_some());
    assert_eq!(state.get("tab_drag").int(), 2);
    assert_eq!(mark(&state, 1), Some(half(r1, Zone::Top)));
    assert_eq!(mark(&state, 3), None);
    app.ok("tab-drop");
    let state = app.wait_for("the tab docked", |s| windows(s).len() == 2);
    assert_eq!(layout_of(&state, 1), "v0.5(*2,1)");
    assert_eq!(layout_of(&state, 3), "*3");
    // The window the tab went to comes to the front.
    let state = app.wait_for("the window in front", |s| s.get("current").int() == 1);
    assert_eq!(window(&state, 1).get("focused_pane").int(), 2);
    app.wait_screen_of(1, "pane 1 as it was", |t| t.contains("pane-1-was-here"));
    for id in [1, 2] {
        app.check_stty(id);
    }

    // The lower pane is split until it cannot be split again: beside the
    // last pane there is then no room for another, a tab's either.
    app.ok("focus-pane 1");
    for _ in 0..6 {
        app.ok("action split:right");
    }
    let state = app.state();
    let narrow = window(&state, 1).get("focused_pane").int();
    let before = layout_of(&state, 1);
    assert!(window(&state, 1).get("panes").ints().len() > 3, "{before}");
    app.ok("action new_tab");
    let state = app.wait_for("another tab", |s| windows(s).len() == 3);
    let tab = state.get("current").int();
    let pane = window(&state, tab).get("panes").ints()[0];
    assert_eq!(window(&state, 1).get("tabs").ints(), [1, tab]);
    // The new tab is held over the right edge of the narrow pane.
    let narrow_rect = pane_rect(&mut app, narrow);
    app.ok("tab-pick 1 2");
    app.ok(&format!("tab-drag 1 {}", inside(narrow_rect, 0.9, 0.5, s)));
    app.wait_for("the tab held", |s| {
        s.get("tab_drag") == &Json::Num(tab as f64)
    });
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(mark(&app.state(), 1), None, "no room, no mark");
    // Beside the wide pane above there is room, and there it docks.
    let wide = pane_rect(&mut app, 2);
    app.ok(&format!("tab-drag 1 {}", inside(wide, 0.9, 0.5, s)));
    app.wait_for("the mark", |s| mark(s, 1).is_some());
    let wide = pane_rect(&mut app, 2);
    assert_eq!(mark(&app.state(), 1), Some(half(wide, Zone::Right)));
    app.ok("tab-drop");
    let state = app.wait_for("the tab docked", |s| windows(s).len() == 2);
    assert_eq!(
        layout_of(&state, 1),
        docked(&before, 2, Zone::Right, &format!("*{pane}"))
    );
    assert_eq!(app.zombies(), 0);
    assert!(!support::read(&app.stderr).contains("cannot move"));
    app.quit();
}
