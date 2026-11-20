//! Split panes, the dividers between them, tabs, and closing.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::{Duration, Instant};

use support::{escape, test_dir, window, windows, App, Json, Png};

/// The default theme's background.
const BG: u32 = 0x181a1f;
/// The divider color the default theme gives: 15% of the way from the
/// background to the foreground.
const DIVIDER: u32 = 0x35383d;

fn rect(j: &Json) -> [i64; 4] {
    let v = j.ints();
    [v[0], v[1], v[2], v[3]]
}

fn by_id(panes: &[Json], id: i64) -> Json {
    panes
        .iter()
        .find(|p| p.get("id").int() == id)
        .cloned()
        .unwrap_or_else(|| panic!("no pane {id} in {panes:?}"))
}

fn focused(app: &mut App) -> i64 {
    app.first_window().get("focused_pane").int()
}

/// The window's geometry: drawable size in pixels, scale, divider width,
/// cell size and padding in pixels.
struct Geometry {
    w: i64,
    h: i64,
    scale: f64,
    divider: i64,
    cell: (i64, i64),
    pad: i64,
}

fn geometry(app: &mut App) -> Geometry {
    let w = app.first_window();
    let size = w.get("size_px").ints();
    let scale = w.get("scale").num();
    let cell = w.get("cell").ints();
    Geometry {
        w: size[0],
        h: size[1],
        scale,
        divider: (1.0 * scale).round() as i64,
        cell: (cell[0], cell[1]),
        pad: (10.0 * scale).round() as i64,
    }
}

/// A pixel position as the automation socket's points.
fn pt(px: i64, scale: f64) -> String {
    format!("{}", px as f64 / scale)
}

fn mouse(app: &mut App, kind: &str, x: i64, y: i64, scale: f64, extra: &str) {
    app.ok(&format!(
        "mouse {kind} {} {} {extra}",
        pt(x, scale),
        pt(y, scale)
    ));
}

#[test]
#[ignore = "opens windows"]
fn split_panes_tile_the_window_and_each_program_sees_its_own_grid() {
    let mut app = App::launch("sp", &[]);
    app.wait_prompt();
    let reply = app.ok("key cmd+d");
    assert_eq!(
        support::event(&reply, "key").get("action").str(),
        "split:right"
    );
    app.ok("key cmd+shift+d");
    let w = app.first_window();
    assert_eq!(w.get("layout").str(), "h0.5(1,v0.5(2,*3))");
    let g = geometry(&mut app);
    let panes = app.panes();
    let r: Vec<[i64; 4]> = panes.iter().map(|p| rect(p.get("rect"))).collect();
    // The first pane fills the left half; the right half is split in two.
    assert_eq!(r[0], [0, 0, r[0][2], g.h]);
    assert_eq!(
        r[1],
        [r[0][2] + g.divider, 0, g.w - r[0][2] - g.divider, r[1][3]]
    );
    assert_eq!(
        r[2],
        [
            r[1][0],
            r[1][3] + g.divider,
            r[1][2],
            g.h - r[1][3] - g.divider
        ]
    );
    assert!((r[0][2] - r[1][2]).abs() <= 1, "{r:?}");
    assert!((r[1][3] - r[2][3]).abs() <= 1, "{r:?}");
    // The dividers fill the gaps between them, and nothing else is left.
    let dividers: Vec<[i64; 4]> = w
        .get("dividers")
        .arr()
        .iter()
        .map(|d| rect(d.get("rect")))
        .collect();
    assert_eq!(
        dividers,
        [
            [r[0][2], 0, g.divider, g.h],
            [r[1][0], r[1][3], r[1][2], g.divider]
        ]
    );
    let area: i64 = r.iter().chain(&dividers).map(|r| r[2] * r[3]).sum();
    assert_eq!(area, g.w * g.h);
    // Every pane's grid fits its rectangle, and its program sees that size.
    for (p, r) in panes.iter().zip(&r) {
        let grid = p.get("grid").ints();
        assert_eq!(
            grid,
            [(r[2] - 2 * g.pad) / g.cell.0, (r[3] - 2 * g.pad) / g.cell.1]
        );
        assert_eq!(p.get("cwd").str(), std::env::var("HOME").expect("HOME"));
    }
    for id in [1, 2, 3] {
        app.check_stty(id);
    }

    // Focus moves as the layout says, by key and by action.
    app.ok("focus-pane 3");
    app.ok("action focus_split:up");
    assert_eq!(focused(&mut app), 2);
    app.ok("key cmd+alt+left");
    assert_eq!(focused(&mut app), 1);
    // Of the two panes to the right, the one focused last.
    app.ok("action focus_split:right");
    assert_eq!(focused(&mut app), 2);
    app.ok("key cmd+alt+down");
    assert_eq!(focused(&mut app), 3);
    app.ok("key cmd+]");
    assert_eq!(focused(&mut app), 1, "next wraps around");
    app.ok("key cmd+[");
    assert_eq!(focused(&mut app), 3);
    app.ok("action focus_split:right");
    assert_eq!(focused(&mut app), 3, "nothing to the right");

    // The session's focus follows the pane's and the window's: focus
    // reports in two panes.
    for id in [1, 2] {
        app.run_in(id, r"clear; printf '\e[?1004h'; cat -v");
    }
    for id in [1, 2] {
        app.wait_on("panes", "cat reading", |r| {
            r.get("windows").arr()[0]
                .get("panes")
                .arr()
                .iter()
                .any(|p| p.get("id").int() == id && p.get("foreground") == &Json::Str("cat".into()))
        });
    }
    app.ok("focus-window on");
    app.wait_screen_of(2, "pane 2 focused in", |s| {
        s.ends_with("^[[I\n") || s.contains("^[[I")
    });
    app.ok("focus-pane 1");
    app.wait_screen_of(2, "pane 2 focused out", |s| s.contains("^[[I^[[O"));
    app.wait_screen_of(1, "pane 1 focused in", |s| s.contains("^[[I"));
    app.ok("focus-window off");
    app.wait_screen_of(1, "pane 1 focused out with the window", |s| {
        s.contains("^[[I^[[O")
    });
    let two = app.screen_of(2);
    assert!(!two.contains("^[[I^[[O^[[I"), "{two:?}");
    for id in [1, 2] {
        app.ok(&format!("focus-pane {id}"));
        app.ok("key ctrl+c");
    }

    // What is drawn: the dividers in their color, every pane on the
    // background, and what the panes without focus show faded toward it.
    // Pane three starts with a white block to see that by.
    app.ok("focus-pane 1");
    app.run_in(1, r"clear; printf '\e[1;31mpane one\e[0m\n'");
    app.run_in(
        3,
        r"clear; printf '\e[48;2;255;255;255m  \e[0m\n'; echo pane three",
    );
    app.ok("focus-pane 1");
    app.wait_screen_of(3, "pane three's text", |s| {
        s.lines().any(|l| l == "pane three")
    });
    app.wait_screen_of(1, "pane one's text", |s| s.starts_with("pane one\n"));
    let shot = test_dir().join("splits.png");
    app.ok(&format!("screenshot {}", escape(&shot.to_string_lossy())));
    let png = Png::read(&shot);
    let middle = |r: [i64; 4]| ((r[0] + r[2] / 2) as u32, (r[1] + r[3] / 2) as u32);
    for d in &dividers {
        let (x, y) = middle(*d);
        assert_eq!(png.rgb(x, y), DIVIDER, "divider {d:?}");
    }
    // Bottom corners, where no text is: focused or not, a pane keeps its
    // background.
    let corner = |r: [i64; 4]| ((r[0] + 2) as u32, (r[1] + r[3] - 3) as u32);
    for r in &r {
        let (x, y) = corner(*r);
        assert_eq!(png.rgb(x, y), BG, "the background of {r:?}");
    }
    // The white block has gone a quarter of the way to the background.
    let origin = by_id(&panes, 3).get("origin").ints();
    let block = png.rgb(
        (origin[0] + g.cell.0) as u32,
        (origin[1] + g.cell.1 / 2) as u32,
    );
    let faded = [16, 8, 0].iter().all(|&s| {
        let ch = |v: u32| f64::from((v >> s) & 0xff);
        (ch(block) - (255.0 - (255.0 - ch(BG)) * 0.25)).abs() <= 1.5
    });
    assert!(faded, "{block:06x}");
    let number = app.first_window().get("number").int();
    support::check_on_screen(&app, number);
    let capture = test_dir().join("splits-window.png");
    let status = std::process::Command::new("screencapture")
        .args(["-x", "-o", "-l", &number.to_string()])
        .arg(&capture)
        .status()
        .expect("screencapture");
    assert!(status.success());
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn dividers_drag_without_jumping_and_a_double_click_evens_them_out() {
    let mut app = App::launch("dv", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    let g = geometry(&mut app);
    let s = g.scale;
    let w = app.first_window();
    let d = rect(w.get("dividers").arr()[0].get("rect"));
    let center = d[0] + d[2] / 2;
    let y = g.h / 2;
    // Over the divider, and a little beside it, the pointer resizes.
    mouse(&mut app, "move", center, y, s, "");
    assert_eq!(app.first_window().get("cursor").str(), "col-resize");
    mouse(&mut app, "move", d[0] + d[2] + (2.0 * s) as i64, y, s, "");
    assert_eq!(app.first_window().get("cursor").str(), "col-resize");
    mouse(&mut app, "move", center + (40.0 * s) as i64, y, s, "");
    assert_eq!(app.first_window().get("cursor").str(), "text");

    // Grabbed off its center line, it moves with the pointer, not to it.
    let before = app.panes();
    mouse(&mut app, "down", center + 1, y, s, "");
    mouse(&mut app, "drag", center + 1 - 60, y, s, "");
    mouse(&mut app, "drag", center + 1 - 120, y, s, "");
    mouse(&mut app, "up", center + 1 - 120, y, s, "");
    let w = app.first_window();
    let moved = rect(w.get("dividers").arr()[0].get("rect"));
    assert_eq!(moved, [d[0] - 120, d[1], d[2], d[3]]);
    assert_eq!(
        focused(&mut app),
        2,
        "grabbing a divider does not move the focus"
    );
    let after = app.panes();
    let cols = |p: &Json| p.get("grid").ints()[0];
    assert!(cols(&after[0]) < cols(&before[0]) && cols(&after[1]) > cols(&before[1]));
    app.check_stty(1);
    app.check_stty(2);

    // Dragged past the window's edge, a pane keeps a few cells.
    let center = moved[0] + moved[2] / 2;
    mouse(&mut app, "down", center, y, s, "");
    mouse(&mut app, "drag", -500, y, s, "");
    mouse(&mut app, "up", -500, y, s, "");
    let left = by_id(&app.panes(), 1);
    assert!(
        rect(left.get("rect"))[2] >= 2 * g.pad + 6 * g.cell.0,
        "{left:?}"
    );
    assert_eq!(left.get("grid").ints()[0], 6);
    app.check_stty(1);

    // A double click on the divider shares the width out equally again.
    let d = rect(app.first_window().get("dividers").arr()[0].get("rect"));
    let center = d[0] + d[2] / 2;
    mouse(&mut app, "down", center, y, s, "x1");
    mouse(&mut app, "up", center, y, s, "x1");
    mouse(&mut app, "down", center, y, s, "x2");
    mouse(&mut app, "up", center, y, s, "x2");
    let w = app.first_window();
    assert!(w.get("layout").str().starts_with("h0.5("), "{w:?}");
    let panes = app.panes();
    let widths: Vec<i64> = panes.iter().map(|p| rect(p.get("rect"))[2]).collect();
    assert!((widths[0] - widths[1]).abs() <= 1, "{widths:?}");

    // A click in a pane focuses it.
    mouse(&mut app, "down", 20, 20, s, "");
    mouse(&mut app, "up", 20, 20, s, "");
    assert_eq!(focused(&mut app), 1);

    // A split one above the other: its divider resizes up and down.
    app.ok("action split:down");
    let w = app.first_window();
    let d = rect(w.get("dividers").arr()[1].get("rect"));
    assert_eq!(w.get("dividers").arr()[1].get("axis").str(), "vertical");
    let (x, cy) = (d[0] + d[2] / 2, d[1] + d[3] / 2);
    mouse(&mut app, "move", x, cy, s, "");
    assert_eq!(app.first_window().get("cursor").str(), "row-resize");
    mouse(&mut app, "down", x, cy - 1, s, "");
    mouse(&mut app, "drag", x, cy - 1 + 80, s, "");
    mouse(&mut app, "up", x, cy - 1 + 80, s, "");
    let moved = rect(app.first_window().get("dividers").arr()[1].get("rect"));
    assert_eq!(moved, [d[0], d[1] + 80, d[2], d[3]]);
    app.check_stty(3);
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn zoom_resize_and_equalize() {
    let mut app = App::launch("zm", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    let g = geometry(&mut app);
    let unzoomed = app.panes();
    // A program in a pane that the zoom hides keeps running.
    app.wait_prompt_of(2);
    app.run_in(1, "sleep 1; echo done while hidden");
    app.ok("focus-pane 2");
    let reply = app.ok("key cmd+shift+enter");
    assert_eq!(
        support::event(&reply, "key").get("action").str(),
        "toggle_zoom"
    );
    let w = app.first_window();
    assert!(w.get("zoomed").bool());
    assert_eq!(w.get("layout").str(), "h0.5(1,!2)");
    assert!(w.get("dividers").arr().is_empty());
    let panes = app.panes();
    assert!(!by_id(&panes, 1).get("shown").bool());
    let two = by_id(&panes, 2);
    assert!(two.get("shown").bool());
    assert_eq!(rect(two.get("rect")), [0, 0, g.w, g.h]);
    app.check_stty(2);
    app.wait_screen_of(1, "the hidden pane's output", |s| {
        s.contains("\ndone while hidden\n")
    });
    let one = by_id(&app.panes(), 1);
    assert_eq!(
        one.get("grid"),
        by_id(&unzoomed, 1).get("grid"),
        "a hidden pane keeps its size"
    );
    // Moving the focus moves the zoom.
    app.ok("action focus_split:left");
    assert_eq!(app.first_window().get("layout").str(), "h0.5(!1,2)");
    assert_eq!(rect(by_id(&app.panes(), 1).get("rect")), [0, 0, g.w, g.h]);
    app.ok("action toggle_zoom");
    let w = app.first_window();
    assert!(!w.get("zoomed").bool());
    let panes = app.panes();
    for p in &unzoomed {
        let now = by_id(&panes, p.get("id").int());
        assert_eq!(now.get("rect"), p.get("rect"));
        assert!(now.get("shown").bool());
    }

    // Resizing by key moves the focused pane's edge by 10 points.
    let width = |app: &mut App, id| rect(by_id(&app.panes(), id).get("rect"))[2];
    let before = width(&mut app, 1);
    app.ok("action resize_split:right,10");
    assert_eq!(width(&mut app, 1), before + (10.0 * g.scale).round() as i64);
    app.ok("key cmd+ctrl+left");
    app.ok("key cmd+ctrl+left");
    assert_eq!(width(&mut app, 1), before - (10.0 * g.scale).round() as i64);
    app.check_stty(1);

    // Three side by side, equalized, are thirds.
    app.ok("action split:right");
    app.ok("key cmd+ctrl+=");
    let widths: Vec<i64> = app.panes().iter().map(|p| rect(p.get("rect"))[2]).collect();
    assert_eq!(widths.len(), 3);
    let (lo, hi) = (widths.iter().min().unwrap(), widths.iter().max().unwrap());
    assert!(hi - lo <= 1, "{widths:?}");
    for id in [1, 2, 3] {
        let p = by_id(&app.panes(), id);
        assert_eq!(
            p.get("grid").ints()[0],
            (rect(p.get("rect"))[2] - 2 * g.pad) / g.cell.0
        );
    }
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn closing_panes_tabs_and_windows_in_every_order() {
    let mut app = App::launch("cl", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.ok("action split:down");
    let children = app.children().len();
    assert_eq!(children, 3);
    let g = geometry(&mut app);
    // The focused bottom pane closes; the one above takes its space and
    // the focus.
    let reply = app.ok("key cmd+w");
    assert_eq!(
        support::event(&reply, "key").get("action").str(),
        "close_pane"
    );
    let w = app.first_window();
    assert_eq!(w.get("layout").str(), "h0.5(1,*2)");
    let two = by_id(&app.panes(), 2);
    assert_eq!(rect(two.get("rect"))[3], g.h);
    app.check_stty(2);
    // A pane that is not focused, by its shell exiting: the focus stays.
    app.ok("action split:down");
    app.run_in(1, "exit");
    app.wait_for("pane 1 to close", |s| {
        windows(s)[0].get("layout").str() == "v0.5(2,*4)"
    });
    let panes = app.panes();
    assert_eq!(rect(by_id(&panes, 2).get("rect"))[2], g.w);

    // A second tab, with panes of its own.
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.ok("action split:right");
    let state = app.state();
    let tab = &windows(&state)[1];
    assert_eq!(tab.get("panes").ints(), [5, 6]);
    assert_eq!(windows(&state)[0].get("panes").ints(), [2, 4]);
    // Its last pane closes the tab, and only that tab.
    app.ok("action close_pane");
    app.ok("action close_pane");
    app.wait_for("one tab", |s| windows(s).len() == 1);
    // Close Tab, from a tab with two panes.
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.ok("action split:down");
    app.ok("key cmd+shift+w");
    app.wait_for("one tab", |s| windows(s).len() == 1);
    // The red button closes a tab.
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.ok("close-button");
    app.wait_for("one tab", |s| windows(s).len() == 1);
    // Panes close down to the last, which closes the window.
    app.ok("action close_pane");
    let w = app.first_window();
    assert_eq!(w.get("panes").ints().len(), 1);
    app.ok("action close_pane");
    app.wait_for("no windows", |s| windows(s).is_empty());
    assert_eq!(app.state().get("sessions").int(), 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.children().is_empty() {
        assert!(
            Instant::now() < deadline,
            "children left: {:?}",
            app.children()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(app.zombies(), 0);

    // Close Window closes every tab of it, and leaves other windows alone.
    app.ok("action new_window");
    app.wait_for("a window", |s| windows(s).len() == 1);
    app.ok("action new_tab");
    app.ok("action new_window");
    app.wait_for("three windows", |s| windows(s).len() == 3);
    app.ok("action close_window");
    app.wait_for("the other window", |s| windows(s).len() == 2);
    app.ok("action close_window");
    app.wait_for("no windows", |s| windows(s).is_empty());
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn tabs_have_their_own_panes_titles_and_places() {
    let mut app = App::launch("tb", &[]);
    app.wait_prompt();
    app.type_line("cd /usr/bin");
    app.wait_on("panes", "the shell in /usr/bin", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0]
            .get("cwd")
            .str()
            == "/usr/bin"
    });
    // Splits and tabs start where the focused pane is.
    app.ok("action split:right");
    assert_eq!(by_id(&app.panes(), 2).get("cwd").str(), "/usr/bin");
    app.ok("key cmd+t");
    let state = app.wait_for("a second tab", |s| windows(s).len() == 2);
    let tab2 = windows(&state)[1].get("id").int();
    assert_eq!(app.pane(3).get("cwd").str(), "/usr/bin");

    // The tab's title is its focused pane's.
    app.ok("key cmd+1");
    app.wait_prompt_of(2);
    app.run_in(2, r"printf '\e]0;second pane\a'");
    app.wait_for("the title", |s| {
        windows(s)[0].get("title").str() == "second pane"
    });
    // A pane whose program set no title is named by its directory.
    app.ok("focus-pane 1");
    app.wait_for("the first pane's title", |s| {
        windows(s)[0].get("title").str() == "bin"
    });
    app.ok("focus-pane 2");
    app.wait_for("the title back", |s| {
        windows(s)[0].get("title").str() == "second pane"
    });

    // Selecting tabs.
    let selected = |app: &mut App| app.first_window().get("selected_tab").int();
    app.ok("action new_tab");
    let state = app.wait_for("a third tab", |s| windows(s).len() == 3);
    let tab3 = windows(&state)[2].get("id").int();
    assert_eq!(selected(&mut app), tab3);
    // A new tab opens beside the one it was opened from.
    assert_eq!(app.first_window().get("tabs").ints(), [1, tab3, tab2]);
    app.ok("key cmd+1");
    assert_eq!(selected(&mut app), 1);
    app.ok("key cmd+9");
    assert_eq!(selected(&mut app), tab2, "9 is the last tab");
    app.ok("key cmd+shift+]");
    assert_eq!(selected(&mut app), 1, "next wraps around");
    app.ok("key cmd+shift+[");
    assert_eq!(selected(&mut app), tab2);
    app.ok("action previous_tab");
    assert_eq!(selected(&mut app), tab3);
    // Moving a tab keeps it selected.
    app.ok("action move_tab:-1");
    let w = app.first_window();
    assert_eq!(w.get("tabs").ints(), [tab3, 1, tab2]);
    assert_eq!(w.get("selected_tab").int(), tab3);
    app.ok("action move_tab:5");
    assert_eq!(app.first_window().get("tabs").ints(), [1, tab2, tab3]);
    assert_eq!(selected(&mut app), tab3);
    app.ok("action move_tab:-1");
    assert_eq!(app.first_window().get("tabs").ints(), [1, tab3, tab2]);

    // A tab that is not selected does not draw, but its programs run, and
    // it shows what they did as soon as it is selected.
    app.ok("key cmd+1");
    app.run_in(1, "clear; sleep 1; seq 1 5; echo background-done");
    app.ok("key cmd+3");
    let frames = |app: &mut App, id: i64| {
        app.stats()
            .get("windows")
            .arr()
            .iter()
            .find(|w| w.get("id").int() == id)
            .expect("the window")
            .get("draws")
            .int()
    };
    std::thread::sleep(Duration::from_millis(300));
    let hidden = frames(&mut app, 1);
    app.wait_screen_of(1, "the hidden tab's output", |s| {
        s.contains("background-done")
    });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(frames(&mut app, 1), hidden, "a hidden tab drew");
    app.ok("key cmd+1");
    app.wait_on("stats", "the selected tab drawn", |s| {
        s.get("windows")
            .arr()
            .iter()
            .any(|w| w.get("id").int() == 1 && w.get("draws").int() > hidden)
    });

    // The tab bar's + button opens a tab beside the window's.
    app.ok("tab-plus");
    app.wait_for("a fourth tab", |s| windows(s).len() == 4);
    // A tab moved into its own window keeps working there.
    let state = app.state();
    let moved = windows(&state)[3].get("id").int();
    app.ok("tab-detach");
    let state = app.wait_for("the tab in its own window", |s| {
        windows(s)
            .iter()
            .find(|w| w.get("id").int() == moved)
            .is_some_and(|w| w.get("tabs").ints() == [moved])
    });
    assert_eq!(state.get("current").int(), moved);
    app.ok("action split:right");
    let state = app.state();
    let w = windows(&state)
        .iter()
        .find(|w| w.get("id").int() == moved)
        .unwrap();
    assert_eq!(w.get("panes").ints().len(), 2);
    let pane = w.get("panes").ints()[1];
    app.check_stty(pane);
    app.ok("tab-merge");
    app.wait_for("one window again", |s| {
        windows(s).iter().all(|w| w.get("tabs").ints().len() == 4)
    });
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_double_click_on_a_tab_names_it() {
    let mut app = App::launch("rn", &[]);
    app.wait_prompt();
    let title = |s: &Json, id: i64| window(s, id).get("title").str().to_string();
    let renaming = |s: &Json, id: i64| window(s, id).get("renaming").bool();
    // A window with one tab shows no tab bar to click on.
    assert!(!app.send("tab-double-click 1").get("ok").bool());
    app.ok("action new_tab");
    let state = app.wait_for("a second tab", |s| windows(s).len() == 2);
    let tab2 = windows(&state)[1].get("id").int();
    let pane2 = window(&state, tab2).get("focused_pane").int();
    app.wait_prompt_of(pane2);

    // A double click on a tab puts a field over it with the title selected:
    // typing replaces the title and Return keeps what was typed.
    app.wait_on("tab-double-click 2", "the tab bar", |_| true);
    app.wait_for("the name field", |s| renaming(s, tab2));
    for key in ["b", "u", "i", "l", "d", "enter"] {
        app.ok(&format!("key {key}"));
    }
    let state = app.wait_for("the name", |s| title(s, tab2) == "build");
    assert!(!renaming(&state, tab2));
    assert_eq!(window(&state, tab2).get("tab_name").str(), "build");
    assert_eq!(window(&state, 1).get("tab_name"), &Json::Null);

    // The keyboard is the terminal's again, and the name stays whatever
    // the shell and its programs do.
    app.type_line(r"cd /usr/bin; printf '\e]0;from the program\a'");
    app.wait_on("panes", "the program's title", |r| {
        r.get("windows")
            .arr()
            .iter()
            .flat_map(|w| w.get("panes").arr().iter())
            .any(|p| {
                p.get("id").int() == pane2
                    && p.get("title") == &Json::Str("from the program".into())
                    && p.get("cwd").str() == "/usr/bin"
            })
    });
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(title(&app.state(), tab2), "build");

    // Escape leaves the name as it was, and so does Return on a name
    // that was not changed.
    app.ok("tab-double-click 2");
    app.wait_for("the name field", |s| renaming(s, tab2));
    app.ok("key x");
    app.ok("key escape");
    let state = app.wait_for("the field gone", |s| !renaming(s, tab2));
    assert_eq!(title(&state, tab2), "build");
    app.ok("tab-double-click 1");
    app.wait_for("the first tab's field", |s| renaming(s, 1));
    app.ok("key enter");
    let state = app.wait_for("the field gone", |s| !renaming(s, 1));
    assert_eq!(window(&state, 1).get("tab_name"), &Json::Null);

    // Another tab is named from the tab bar of the one in front. Paste,
    // Select All and Copy work on the name, not on the terminal.
    app.ok(&format!("clipboard-set {}", escape("nightly\nlogs")));
    app.ok("tab-double-click 1");
    app.wait_for("the first tab's field", |s| renaming(s, 1));
    app.ok("key cmd+v");
    app.ok("clipboard-set other");
    app.ok("key cmd+a");
    app.ok("key cmd+c");
    assert_eq!(app.ok("clipboard").get("text").str(), "nightly logs");
    app.ok("key enter");
    let state = app.wait_for("the first tab's name", |s| title(s, 1) == "nightly logs");
    assert_eq!(title(&state, tab2), "build");
    assert!(!app.screen_of(1).contains("nightly"));
    assert!(!app.screen_of(pane2).contains("nightly"));

    // An empty name gives the tab's title back to its panes.
    app.ok("tab-double-click 2");
    app.wait_for("the name field", |s| renaming(s, tab2));
    app.ok("key backspace");
    app.ok("key enter");
    let state = app.wait_for("the program's title", |s| {
        title(s, tab2) == "from the program"
    });
    assert_eq!(window(&state, tab2).get("tab_name"), &Json::Null);

    // A tab closed while its name is being typed takes the field with it.
    app.ok("tab-double-click 2");
    app.wait_for("the name field", |s| renaming(s, tab2));
    app.ok("action close_tab");
    let state = app.wait_for("one tab", |s| windows(s).len() == 1);
    assert!(!renaming(&state, 1));
    assert_eq!(title(&state, 1), "nightly logs");
    app.type_line("echo still-typing");
    app.wait_screen("the terminal has the keyboard", |s| {
        s.lines().any(|l| l == "still-typing")
    });
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn closing_asks_first_when_a_program_is_running() {
    let mut app = App::launch_with("cf", "confirm-close = true\n", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    // A shell at its prompt closes without a question.
    app.wait_prompt_of(2);
    app.ok("action close_pane");
    assert_eq!(app.first_window().get("panes").ints(), [1]);
    assert!(!app.first_window().get("sheet").bool());

    app.ok("action split:right");
    app.run_in(3, "sleep 100");
    app.wait_on("panes", "sleep running", |r| {
        r.get("windows").arr()[0]
            .get("panes")
            .arr()
            .iter()
            .any(|p| p.get("foreground") == &Json::Str("sleep".into()))
    });
    // A pane: no keeps it, yes closes it.
    app.ok("action close_pane");
    assert!(app.first_window().get("sheet").bool());
    app.ok("confirm no");
    let w = app.first_window();
    assert!(!w.get("sheet").bool());
    assert_eq!(w.get("panes").ints(), [1, 3]);
    app.ok("key cmd+w");
    assert!(app.first_window().get("sheet").bool());
    app.ok("confirm yes");
    let w = app.first_window();
    assert_eq!(w.get("panes").ints(), [1]);
    assert_eq!(w.get("focused_pane").int(), 1);
    let reply = app.send("confirm yes");
    assert_eq!(reply.get("error").str(), "no question is pending");

    // A tab, through its close button, with a program in any of its panes.
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.ok("action split:down");
    let state = app.state();
    let panes = windows(&state)[1].get("panes").ints();
    app.run_in(panes[0], "sleep 100");
    app.ok(&format!("focus-pane {}", panes[1]));
    app.wait_on("panes", "sleep running", |r| {
        r.get("windows").arr()[1].get("panes").arr()[0].get("foreground")
            == &Json::Str("sleep".into())
    });
    app.ok("close-button");
    let state = app.state();
    assert!(windows(&state)[1].get("sheet").bool());
    app.ok("confirm no");
    assert_eq!(app.state().get("windows").arr().len(), 2);
    app.ok("close-button");
    app.ok("confirm yes");
    app.wait_for("one tab", |s| windows(s).len() == 1);

    // A window, when a tab that is not showing is busy.
    app.run_in(1, "sleep 100");
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.wait_on("panes", "sleep running", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0].get("foreground")
            == &Json::Str("sleep".into())
    });
    app.ok("action close_window");
    let state = app.state();
    assert!(windows(&state).iter().any(|w| w.get("sheet").bool()));
    app.ok("confirm no");
    assert_eq!(app.state().get("windows").arr().len(), 2);
    app.ok("action close_window");
    app.ok("confirm yes");
    app.wait_for("no windows", |s| windows(s).is_empty());

    // A shell that exits by itself closes its pane without a question.
    app.ok("action new_window");
    app.wait_for("a window", |s| windows(s).len() == 1);
    app.wait_prompt();
    app.type_line("exit");
    app.wait_for("no windows", |s| windows(s).is_empty());

    // Quitting.
    app.ok("action new_window");
    app.wait_for("a window", |s| windows(s).len() == 1);
    app.wait_prompt();
    app.type_line("sleep 100");
    app.wait_on("panes", "sleep running", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0].get("foreground")
            == &Json::Str("sleep".into())
    });
    app.ok("quit");
    app.wait_for("the question", |s| windows(s)[0].get("sheet").bool());
    app.ok("confirm no");
    std::thread::sleep(Duration::from_millis(300));
    assert!(app.child.try_wait().expect("wait").is_none(), "quit anyway");
    app.ok("key cmd+q");
    app.wait_for("the question", |s| windows(s)[0].get("sheet").bool());
    let _ = app.try_send("confirm yes");
    assert!(app.wait_exit().success());
}

#[test]
#[ignore = "opens windows"]
fn a_dozen_panes_stay_cheap_and_typing_beats_a_flood() {
    let mut app = App::launch_with("mp", "window-width = 160\nwindow-height = 48\n", &[]);
    app.wait_prompt();
    // Four columns of three.
    for _ in 0..3 {
        app.ok("action split:right");
    }
    app.ok("action equalize_splits");
    for column in [1, 2, 3, 4] {
        app.ok(&format!("focus-pane {column}"));
        app.ok("action split:down");
        app.ok("action split:down");
    }
    app.ok("action equalize_splits");
    let panes = app.panes();
    assert_eq!(panes.len(), 12);
    let last = panes.last().unwrap().get("id").int();
    for p in &panes {
        app.wait_prompt_of(p.get("id").int());
    }
    // Idle: no frames.
    std::thread::sleep(Duration::from_millis(500));
    let idle = app.stats();
    std::thread::sleep(Duration::from_secs(1));
    let still = app.stats();
    let frames = |s: &Json| s.get("windows").arr()[0].get("frames").int();
    assert_eq!(frames(&idle), frames(&still), "frames while idle");

    // A flood in one pane.
    app.run_in(
        1,
        "yes 'flood flood flood flood flood flood flood flood flood flood'",
    );
    app.wait_on("panes", "the flood", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0].get("foreground")
            == &Json::Str("yes".into())
    });
    std::thread::sleep(Duration::from_millis(500));
    // Typing in another pane shows within a few frames.
    app.ok(&format!("focus-pane {last}"));
    let mut worst = Duration::ZERO;
    let mut worst_frames = 0;
    for i in 0..5 {
        let word = format!("typed{i}");
        let start = Instant::now();
        let before = frames(&app.stats());
        app.ok(&format!("text {word}"));
        // What was drawn, not only what the terminal holds.
        app.wait_on(&format!("drawn {last}"), "the typed text drawn", |r| {
            r.get("text").str().contains(&word)
        });
        let took = start.elapsed();
        let after = frames(&app.stats());
        worst = worst.max(took);
        worst_frames = worst_frames.max(after - before);
        app.ok("key ctrl+u");
    }
    let stats = app.stats();
    let w = &stats.get("windows").arr()[0];
    eprintln!(
        "dozen panes in a flood: typing showed within {worst:?} and {worst_frames} frames; snapshots per frame {:?}, frame time {:?}",
        w.get("snapshots"),
        w.get("total_us")
    );
    assert!(worst < Duration::from_millis(500), "{worst:?}");
    assert!(worst_frames <= 30, "{worst_frames} frames");
    // Only the panes with new output were snapshotted.
    assert!(w.get("snapshots").get("median").int() <= 2, "{w:?}");
    assert!(w.get("snapshots").get("max").int() <= 12);
    app.ok("focus-pane 1");
    app.ok("key ctrl+c");
    app.wait_prompt_of(1);

    // Output now and then, so the display link stops between lines and a
    // session restarts it: still only that pane is snapshotted.
    app.run_in(
        last,
        "for i in 1 2 3 4 5 6; do sleep 0.4; echo tick $i; done",
    );
    std::thread::sleep(Duration::from_millis(300));
    app.ok("stats-reset");
    app.wait_screen_of(last, "the ticks", |s| s.contains("tick 6"));
    std::thread::sleep(Duration::from_millis(300));
    let stats = app.stats();
    let w = &stats.get("windows").arr()[0];
    eprintln!(
        "ticks in one of twelve panes: snapshots per frame {:?}",
        w.get("snapshots")
    );
    assert!(w.get("timed_frames").int() >= 6, "{w:?}");
    assert!(w.get("snapshots").get("max").int() <= 1, "{w:?}");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn dropped_files_and_text_go_to_the_pane_under_the_pointer() {
    let mut app = App::launch("dr", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.wait_prompt_of(2);
    let g = geometry(&mut app);
    let file = test_dir().join("drop me.txt");
    std::fs::write(&file, "x").expect("file");
    // Onto the left pane, which is not focused: it takes the focus and the
    // path, quoted for the shell.
    app.ok("focus-pane 2");
    let x = pt(g.w / 4, g.scale);
    let y = pt(g.h / 2, g.scale);
    app.ok(&format!(
        "drop {x} {y} path {}",
        escape(&file.to_string_lossy())
    ));
    assert_eq!(focused(&mut app), 1);
    app.ok("key enter");
    // The line wraps, and a space at the end of a row is not in the text.
    let squeeze = |s: &str| s.split_whitespace().collect::<String>();
    let quoted = squeeze(&format!("% {}", file.to_string_lossy().replace(' ', "\\ ")));
    app.wait_screen_of(1, "the dropped path", |s| squeeze(s).starts_with(&quoted));
    // zsh runs it: the file is not executable.
    app.wait_prompt_of(1);
    assert!(!app.screen_of(2).contains("drop me"));
    // Text onto the right pane.
    let x = pt(g.w * 3 / 4, g.scale);
    app.ok(&format!("drop {x} {y} text echo\\ dropped\\ text"));
    assert_eq!(focused(&mut app), 2);
    app.ok("key enter");
    app.wait_screen_of(2, "the dropped command's output", |s| {
        s.lines().any(|l| l == "dropped text")
    });
    app.quit();
}
