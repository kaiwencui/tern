//! The mouse in panes. Selection of every kind, the
//! clipboard, mouse reports to programs, the wheel, links, the pointer, and
//! drops from other apps.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::{Duration, Instant};

use support::{escape, test_dir, App, Json, Png};

/// The helper that prints the bytes a program receives.
fn helper() -> String {
    env!("CARGO_MANIFEST_DIR").to_string() + "/tests/helpers/bytes.pl"
}

/// Waits for the helper to be running in a pane, after a clear: the
/// screen starts with what it printed, and no prompt from before is left.
fn wait_helper(app: &mut App, pane: i64, first: &str) {
    app.wait_screen_of(pane, "the helper", |s| {
        s.starts_with(first) && !s.contains('%')
    });
}

/// Where cells are: a pane's grid origin and the cell size, in pixels, and
/// the window's scale.
struct Cells {
    origin: (f64, f64),
    cell: (f64, f64),
    scale: f64,
}

impl Cells {
    fn of(app: &mut App, pane: i64) -> Cells {
        let w = app.first_window();
        let cell = w.get("cell").ints();
        let scale = w.get("scale").num();
        let origin = app.pane(pane).get("origin").ints();
        Cells {
            origin: (origin[0] as f64, origin[1] as f64),
            cell: (cell[0] as f64, cell[1] as f64),
            scale,
        }
    }

    /// A point in a cell, in view points: in its left or right half.
    fn at(&self, col: i64, row: i64, right: bool) -> String {
        let x = self.origin.0
            + col as f64 * self.cell.0
            + if right { 0.75 } else { 0.25 } * self.cell.0;
        let y = self.origin.1 + row as f64 * self.cell.1 + self.cell.1 / 2.0;
        format!("{} {}", x / self.scale, y / self.scale)
    }
}

fn selection(app: &mut App) -> String {
    app.ok("selection").get("text").str().to_string()
}

/// A press, drags through the points, and a release, with modifiers.
fn drag(app: &mut App, from: &str, to: &[&str], mods: &str) {
    app.ok(format!("mouse down {from} left {mods}").trim_end());
    for p in to {
        app.ok(format!("mouse drag {p} left {mods}").trim_end());
    }
    let last = to.last().copied().unwrap_or(from);
    app.ok(format!("mouse up {last} left {mods}").trim_end());
}

fn click(app: &mut App, at: &str, extra: &str) {
    app.ok(format!("mouse down {at} {extra}").trim_end());
    app.ok(format!("mouse up {at} {extra}").trim_end());
}

#[test]
#[ignore = "opens windows"]
fn selections_of_every_kind_give_the_right_text() {
    let mut app = App::launch("se", &[]);
    app.wait_prompt();
    app.type_line("clear; printf 'alpha beta gamma\\nsecond line here\\nthird row text\\n'");
    app.wait_screen("the text", |s| {
        s.starts_with("alpha beta gamma\nsecond line here\nthird row text\n%")
    });
    let c = Cells::of(&mut app, 1);
    // Characters, ending on the right half of a cell: that cell is in.
    drag(
        &mut app,
        &c.at(6, 0, false),
        &[&c.at(2, 1, true), &c.at(5, 1, true)],
        "",
    );
    assert_eq!(selection(&mut app), "beta gamma\nsecond");
    assert!(app.pane(1).get("selection").bool());
    // Ending on the left half of a cell leaves it out.
    drag(&mut app, &c.at(6, 0, false), &[&c.at(5, 1, false)], "");
    assert_eq!(selection(&mut app), "beta gamma\nsecon");
    // Starting on the right half of a cell leaves that one out.
    drag(&mut app, &c.at(5, 0, true), &[&c.at(9, 0, true)], "");
    assert_eq!(selection(&mut app), "beta");
    let shot = test_dir().join("selection-lines.png");
    drag(&mut app, &c.at(6, 0, false), &[&c.at(5, 1, true)], "");
    app.ok(&format!("screenshot {}", shot.display()));
    // The selected cells are drawn in the selection colors, others not.
    let png = Png::read(&shot);
    let cell_px = |col: i64, row: i64| {
        (
            (c.origin.0 + col as f64 * c.cell.0 + 1.0) as u32,
            (c.origin.1 + row as f64 * c.cell.1 + 1.0) as u32,
        )
    };
    let (x, y) = cell_px(7, 0);
    let selected = png.rgb(x, y);
    let (x, y) = cell_px(2, 0);
    let plain = png.rgb(x, y);
    let (x, y) = cell_px(2, 1);
    assert_eq!(
        png.rgb(x, y),
        selected,
        "the second row's start is selected"
    );
    let (x, y) = cell_px(8, 1);
    assert_eq!(png.rgb(x, y), plain, "past the end is not");
    assert_ne!(selected, plain);
    // A double click selects a word, a triple click a line.
    click(&mut app, &c.at(13, 0, false), "x2");
    assert_eq!(selection(&mut app), "gamma");
    click(&mut app, &c.at(2, 1, false), "x3");
    assert_eq!(selection(&mut app), "second line here\n");
    // Dragging after a double click goes word by word, after a triple
    // click line by line.
    app.ok(&format!("mouse down {} x2", c.at(7, 0, false)));
    app.ok(&format!("mouse drag {}", c.at(8, 1, false)));
    app.ok(&format!("mouse up {} x2", c.at(8, 1, false)));
    assert_eq!(selection(&mut app), "beta gamma\nsecond line");
    app.ok(&format!("mouse down {} x3", c.at(3, 0, false)));
    app.ok(&format!("mouse drag {}", c.at(3, 1, false)));
    app.ok(&format!("mouse up {} x3", c.at(3, 1, false)));
    assert_eq!(selection(&mut app), "alpha beta gamma\nsecond line here\n");
    // Option makes a block.
    drag(&mut app, &c.at(1, 0, false), &[&c.at(4, 2, true)], "alt");
    assert_eq!(selection(&mut app), "lpha\necon\nhird");
    app.ok(&format!(
        "screenshot {}",
        test_dir().join("selection-block.png").display()
    ));
    // Shift-click moves the selection's end.
    drag(&mut app, &c.at(0, 0, false), &[&c.at(4, 0, true)], "");
    assert_eq!(selection(&mut app), "alpha");
    click(&mut app, &c.at(9, 0, true), "shift");
    assert_eq!(selection(&mut app), "alpha beta");
    // A plain click clears it.
    click(&mut app, &c.at(3, 2, false), "");
    assert_eq!(selection(&mut app), "");
    assert!(!app.pane(1).get("selection").bool());
    // The window as the screen shows a block selection.
    drag(&mut app, &c.at(1, 0, false), &[&c.at(4, 2, true)], "alt");
    std::thread::sleep(Duration::from_millis(200));
    let number = app.first_window().get("number").int();
    let _ = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-l", &number.to_string()])
        .arg(test_dir().join("selection-block-window.png"))
        .status();
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_selection_dragged_past_the_edge_scrolls() {
    let mut app = App::launch("ed", &[]);
    app.wait_prompt();
    app.type_line("clear; seq 1 300");
    app.wait_prompt();
    let c = Cells::of(&mut app, 1);
    let grid = app.pane(1).get("grid").ints();
    let first_shown: i64 = app.screen().lines().next().unwrap().trim().parse().unwrap();
    // Press on a row, drag above the window and hold there.
    app.ok(&format!("mouse down {}", c.at(0, 5, false)));
    app.ok(&format!(
        "mouse drag {} {}",
        c.origin.0 / c.scale + 2.0,
        -20.0
    ));
    std::thread::sleep(Duration::from_millis(600));
    let offset = app.pane(1).get("display_offset").int();
    assert!(offset >= 5, "scrolled {offset} lines");
    app.ok(&format!(
        "mouse up {} {}",
        c.origin.0 / c.scale + 2.0,
        -20.0
    ));
    let text = selection(&mut app);
    let top: i64 = text.lines().next().unwrap().trim().parse().unwrap();
    assert!(
        top < first_shown,
        "the selection reaches back to {top}: {text:?}"
    );
    assert_eq!(text.lines().last(), Some(&*(first_shown + 4).to_string()));
    // And below the bottom, back down.
    let bottom_y = (c.origin.1 + grid[1] as f64 * c.cell.1 + 30.0) / c.scale;
    app.ok(&format!("mouse down {}", c.at(0, 2, false)));
    app.ok(&format!(
        "mouse drag {} {bottom_y}",
        c.origin.0 / c.scale + 2.0
    ));
    app.wait_on("panes", "back at the bottom", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0]
            .get("display_offset")
            .int()
            == 0
    });
    app.ok(&format!(
        "mouse up {} {bottom_y}",
        c.origin.0 / c.scale + 2.0
    ));
    let text = selection(&mut app);
    assert!(text.contains("300\n"), "{text:?}");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn copy_and_paste_go_through_the_private_pasteboard() {
    let mut app = App::launch("cp", &[]);
    app.wait_prompt();
    let enabled = |app: &mut App, command: &str| {
        let menu = app.ok("menu");
        menu.get("menus")
            .arr()
            .iter()
            .flat_map(|m| m.get("items").arr().to_vec())
            .find(|i| matches!(i, Json::Obj(m) if m.get("command") == Some(&Json::Str(command.into()))))
            .map(|i| i.get("enabled").bool())
            .expect("the menu item")
    };
    // Nothing selected and nothing on the pasteboard: nothing to do.
    assert!(!enabled(&mut app, "copy"));
    assert!(!enabled(&mut app, "paste"));
    assert!(enabled(&mut app, "select_all"));
    app.type_line("clear; echo copy-me-please");
    app.wait_screen("the text", |s| s.starts_with("copy-me-please\n%"));
    let c = Cells::of(&mut app, 1);
    click(&mut app, &c.at(3, 0, false), "x2");
    assert_eq!(selection(&mut app), "copy-me-please");
    assert!(enabled(&mut app, "copy"));
    // Cmd+C copies; the clipboard is the test's own pasteboard.
    app.ok("key cmd+c");
    app.wait_on("clipboard", "the copied text", |r| {
        r.get("text").str() == "copy-me-please"
    });
    assert!(enabled(&mut app, "paste"));
    // Paste into a second pane.
    app.ok("action split:right");
    app.wait_prompt_of(2);
    app.ok("key cmd+v");
    app.wait_screen_of(2, "the paste", |s| s.contains("% copy-me-please"));
    app.ok("key ctrl+u");
    // Bracketed when the program asked for it, plain when it did not.
    app.type_line(&format!("clear; {} 2004", helper()));
    wait_helper(&mut app, 2, "READY");
    app.ok("clipboard-set two\\ words");
    app.ok("menu-click paste");
    app.wait_screen_of(2, "a bracketed paste", |s| {
        s.contains("GOT:<1b>[200~two words<1b>[201~")
    });
    app.ok("key ctrl+d");
    app.wait_prompt_of(2);
    app.type_line(&format!("clear; {}", helper()));
    wait_helper(&mut app, 2, "READY");
    app.ok("action paste");
    app.wait_screen_of(2, "a plain paste", |s| s.contains("GOT:two words"));
    app.ok("key ctrl+d");
    app.wait_prompt_of(2);
    // Select All takes the whole history and screen.
    app.ok("focus-pane 1");
    app.ok("action select_all");
    let all = selection(&mut app);
    assert!(
        all.contains("echo copy-me-please") && all.contains("copy-me-please\n"),
        "{all:?}"
    );
    app.quit();

    // With copy-on-select, a selection is copied as it is made.
    let mut app = App::launch_with("cs", "copy-on-select = true\n", &[]);
    app.wait_prompt();
    app.type_line("clear; echo selected-and-copied");
    app.wait_screen("the text", |s| s.starts_with("selected-and-copied\n%"));
    let c = Cells::of(&mut app, 1);
    click(&mut app, &c.at(2, 0, false), "x2");
    app.wait_on("clipboard", "the selection copied", |r| {
        r.get("text").str() == "selected-and-copied"
    });
    // A plain click selects nothing and copies nothing (copies are made on
    // another thread, so give one time to arrive).
    app.ok("clipboard-set kept");
    click(&mut app, &c.at(2, 0, false), "");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(app.ok("clipboard").get("text").str(), "kept");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn copying_a_huge_selection_leaves_the_app_responsive() {
    // A million lines take a second or more to turn into text; that work
    // happens off the main thread, which keeps answering meanwhile.
    let mut app = App::launch_with("hc", "scrollback-lines = 1000000\n", &[]);
    app.wait_prompt();
    app.type_line("seq -f %.0f 1 1000000");
    app.wait_screen("the last number", |s| s.contains("\n1000000\n%"));
    app.ok("action select_all");
    let start = Instant::now();
    app.ok("action copy");
    let asked = start.elapsed();
    let start = Instant::now();
    app.ok("state");
    app.ok("panes");
    let answered = start.elapsed();
    let copied = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(30);
    let text = loop {
        let text = app.ok("clipboard").get("text").str().to_string();
        if text.contains("\n999999\n1000000\n") {
            break text;
        }
        assert!(Instant::now() < deadline, "the copy never arrived");
        std::thread::sleep(Duration::from_millis(100));
    };
    eprintln!(
        "copy asked in {asked:?}, state and panes answered in {answered:?}, \
         {} lines on the clipboard after {:?}",
        text.lines().count(),
        copied.elapsed()
    );
    assert!(
        text.contains("% seq -f %.0f 1 1000000\n1\n2\n3\n"),
        "{}",
        &text[..40]
    );
    assert!(asked < Duration::from_millis(500), "{asked:?}");
    assert!(answered < Duration::from_millis(500), "{answered:?}");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn mouse_reports_reach_programs_in_every_mode() {
    let mut app = App::launch("mr", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.wait_prompt_of(2);
    let c = Cells::of(&mut app, 2);
    let expect = |app: &mut App, modes: &str, steps: &[String], want: &[&str]| {
        app.ok("focus-pane 2");
        app.type_line(&format!("clear; {} {modes}", helper()));
        wait_helper(app, 2, "READY");
        for step in steps {
            app.ok(step);
        }
        // What the program read, however the reads split it.
        let got =
            |s: &str| -> String { s.lines().filter_map(|l| l.strip_prefix("GOT:")).collect() };
        let all = want.concat();
        let screen = app.wait_screen_of(2, &format!("reports for {modes}"), |s| {
            got(s).len() >= all.len()
        });
        assert_eq!(got(&screen), all, "{modes}");
        app.ok("focus-pane 2");
        app.ok("key ctrl+d");
        app.wait_prompt_of(2);
    };
    // Cell (3, 2) of the right pane is column 4, row 3 to the program.
    let at = c.at(3, 2, false);
    let next = c.at(4, 2, false);
    let down = |a: &str, extra: &str| format!("mouse down {a} {extra}").trim_end().to_string();
    let up = |a: &str, extra: &str| format!("mouse up {a} {extra}").trim_end().to_string();
    // Clicks, SGR.
    expect(
        &mut app,
        "1000 1006",
        &[
            down(&at, ""),
            up(&at, ""),
            down(&at, "right"),
            up(&at, "right"),
            down(&at, "left ctrl"),
            up(&at, "left ctrl"),
        ],
        &[
            "<1b>[<0;4;3M",
            "<1b>[<0;4;3m",
            "<1b>[<2;4;3M",
            "<1b>[<2;4;3m",
            "<1b>[<16;4;3M",
            "<1b>[<16;4;3m",
        ],
    );
    // A second button while the first is held is reported too, and a
    // press whose release went missing gets that release first.
    expect(
        &mut app,
        "1000 1006",
        &[
            down(&at, ""),
            down(&at, "right"),
            up(&at, "right"),
            up(&at, ""),
            down(&at, ""),
            down(&next, ""),
            up(&next, ""),
        ],
        &[
            "<1b>[<0;4;3M",
            "<1b>[<2;4;3M",
            "<1b>[<2;4;3m",
            "<1b>[<0;4;3m",
            "<1b>[<0;4;3M",
            "<1b>[<0;5;3m",
            "<1b>[<0;5;3M",
            "<1b>[<0;5;3m",
        ],
    );
    // Drags, SGR; motion without a button is not reported in this mode.
    expect(
        &mut app,
        "1002 1006",
        &[
            format!("mouse move {next}"),
            down(&at, ""),
            format!("mouse drag {next}"),
            format!("mouse drag {next}"),
            up(&next, ""),
        ],
        &["<1b>[<0;4;3M", "<1b>[<32;5;3M", "<1b>[<0;5;3m"],
    );
    // All motion, SGR.
    expect(
        &mut app,
        "1003 1006",
        &[format!("mouse move {at}"), format!("mouse move {next}")],
        &["<1b>[<35;4;3M", "<1b>[<35;5;3M"],
    );
    // The default and UTF-8 encodings: one byte per value, offset by 32.
    expect(
        &mut app,
        "1000",
        &[down(&at, ""), up(&at, "")],
        &["<1b>[M $#", "<1b>[M#$#"],
    );
    expect(
        &mut app,
        "1000 1005",
        &[down(&at, ""), up(&at, "")],
        &["<1b>[M $#", "<1b>[M#$#"],
    );
    // The wheel, reported as buttons 64 and 65.
    let (x, y) = at.split_once(' ').unwrap();
    expect(
        &mut app,
        "1000 1006",
        &[format!("scroll {x} {y} 0 1 lines")],
        &["<1b>[<64;4;3M", "<1b>[<64;4;3M", "<1b>[<64;4;3M"],
    );
    // While the program has the mouse the pointer is an arrow, and Shift
    // takes the mouse back to select.
    app.ok("focus-pane 2");
    app.type_line(&format!("clear; echo shifted; {} 1000 1006", helper()));
    app.wait_screen_of(2, "the helper", |s| {
        s.starts_with(
            "shifted
READY",
        )
    });
    app.ok(&format!("mouse move {at}"));
    assert_eq!(app.first_window().get("cursor").str(), "arrow");
    app.ok(&format!("mouse move {at} shift"));
    assert_eq!(app.first_window().get("cursor").str(), "text");
    click(&mut app, &c.at(2, 0, false), "shift x2");
    assert_eq!(selection(&mut app), "shifted");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !app.screen_of(2).contains("GOT:"),
        "Shift clicks are not reported"
    );
    app.ok("key ctrl+d");
    app.wait_prompt_of(2);
    // The left pane never saw any of it.
    assert!(!app.screen_of(1).contains("GOT:"));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn the_wheel_scrolls_history_and_sends_arrows_to_less() {
    let mut app = App::launch("wh", &[]);
    app.wait_prompt();
    let file = test_dir().join("wheel.txt");
    let lines: String = (1..=400).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&file, lines).expect("file");
    app.type_line(&format!("clear; cat '{}'", file.display()));
    app.wait_prompt();
    let c = Cells::of(&mut app, 1);
    let at = c.at(10, 5, false);
    let (x, y) = at.split_once(' ').unwrap();
    let offset = |app: &mut App| app.pane(1).get("display_offset").int();
    // A notch is three lines; trackpad movement adds up to whole lines.
    app.ok(&format!("scroll {x} {y} 0 1 lines"));
    assert_eq!(offset(&mut app), 3);
    let line_pt = c.cell.1 / c.scale;
    app.ok(&format!("scroll {x} {y} 0 {}", (line_pt * 0.6) as i64));
    assert_eq!(offset(&mut app), 3);
    app.ok(&format!("scroll {x} {y} 0 {}", (line_pt * 0.6) as i64));
    assert_eq!(offset(&mut app), 4);
    app.ok(&format!("scroll {x} {y} 0 -1 lines"));
    assert_eq!(offset(&mut app), 1);
    // What is drawn follows.
    app.wait_on("drawn", "the scrolled frame", |r| {
        r.get("text").str().contains("line 371\n")
    });
    // Typing goes back to the bottom.
    app.ok("text x");
    app.wait_on("panes", "the bottom", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0]
            .get("display_offset")
            .int()
            == 0
    });
    app.ok("key ctrl+u");
    // In less, the wheel sends arrows: the file moves, the history does not.
    app.type_line(&format!("less '{}'", file.display()));
    app.wait_screen("less", |s| s.starts_with("line 1\n"));
    app.ok(&format!("scroll {x} {y} 0 -1 lines"));
    app.wait_screen("three lines down", |s| s.starts_with("line 4\n"));
    app.ok(&format!("scroll {x} {y} 0 1 lines"));
    app.wait_screen("back up", |s| s.starts_with("line 1\n"));
    assert_eq!(offset(&mut app), 0);
    app.ok("key q");
    app.wait_prompt();
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn command_click_opens_links_and_the_pointer_says_so() {
    let mut app = App::launch("ln", &[]);
    app.wait_prompt();
    app.type_line(
        "clear; printf '\\e]8;;https://example.com/marked\\e\\\\marked link\\e]8;;\\e\\\\ and see https://example.org/plain?q=1.\\n'",
    );
    app.wait_screen("the links", |s| {
        s.starts_with("marked link and see https://example.org/plain?q=1.\n")
    });
    let c = Cells::of(&mut app, 1);
    let marked = c.at(2, 0, false);
    let plain = c.at(25, 0, false);
    let word = c.at(14, 0, false);
    let cursor = |app: &mut App| app.first_window().get("cursor").str().to_string();
    app.ok(&format!("mouse move {marked}"));
    assert_eq!(cursor(&mut app), "text");
    app.ok(&format!("mouse move {marked} cmd"));
    assert_eq!(cursor(&mut app), "pointer");
    // Command let go: the hand goes, without the mouse moving.
    app.ok("key cmd");
    assert_eq!(cursor(&mut app), "text");
    app.ok(&format!("mouse move {word} cmd"));
    assert_eq!(cursor(&mut app), "text");
    app.ok(&format!("mouse move {plain} cmd"));
    assert_eq!(cursor(&mut app), "pointer");
    // A plain click opens nothing; Command-click opens.
    click(&mut app, &plain, "");
    click(&mut app, &word, "cmd");
    assert!(app.state().get("opened_links").arr().is_empty());
    click(&mut app, &marked, "cmd");
    click(&mut app, &plain, "cmd");
    assert_eq!(
        app.state().get("opened_links").strs(),
        [
            "https://example.com/marked",
            "https://example.org/plain?q=1"
        ]
    );
    assert_eq!(selection(&mut app), "", "a link click selects nothing");
    // Only web, mail and file links open, and a file is shown in the
    // Finder rather than opened: output cannot make a word run an app.
    app.type_line(
        "clear; printf '\\e]8;;x-tern-test://run\\e\\\\custom\\e]8;;\\e\\\\ \\e]8;;file:///Applications/Calculator.app\\e\\\\app\\e]8;;\\e\\\\\\n'",
    );
    app.wait_screen("the links", |s| s.starts_with("custom app\n"));
    click(&mut app, &c.at(2, 0, false), "cmd");
    click(&mut app, &c.at(8, 0, false), "cmd");
    assert_eq!(
        app.state().get("opened_links").strs(),
        [
            "https://example.com/marked",
            "https://example.org/plain?q=1",
            "reveal file:///Applications/Calculator.app"
        ]
    );
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn typing_hides_the_pointer_until_it_moves() {
    let mut app = App::launch("hp", &[]);
    app.wait_prompt();
    let hidden = |app: &mut App| app.first_window().get("pointer_hidden").bool();
    assert!(!hidden(&mut app));
    app.ok("text a");
    assert!(hidden(&mut app));
    app.ok("mouse move 100 100");
    assert!(!hidden(&mut app));
    app.ok("key b");
    assert!(hidden(&mut app));
    app.ok("key ctrl+u");
    app.quit();

    let mut app = App::launch_with("hq", "mouse-hide-while-typing = false\n", &[]);
    app.wait_prompt();
    app.ok("text a");
    assert!(!hidden(&mut app));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn drops_highlight_and_reach_the_pane_under_the_pointer() {
    let mut app = App::launch_with("dh", "window-width = 120\nwindow-height = 36\n", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.ok("action split:down");
    for id in [1, 2, 3] {
        app.wait_prompt_of(id);
    }
    let scale = app.first_window().get("scale").num();
    let rect_of = |app: &mut App, id: i64| app.pane(id).get("rect").ints();
    let center = |r: &[i64]| {
        format!(
            "{} {}",
            (r[0] + r[2] / 2) as f64 / scale,
            (r[1] + r[3] / 2) as f64 / scale
        )
    };
    let highlight = |app: &mut App| -> Option<Vec<i64>> {
        let w = app.first_window();
        let quads = w.get("quads").arr();
        quads
            .iter()
            .find(|q| q.get("alpha").num() < 1.0)
            .map(|q| q.get("rect").ints())
    };
    // The highlight follows the drag from pane to pane.
    let r2 = rect_of(&mut app, 2);
    let r3 = rect_of(&mut app, 3);
    let r1 = rect_of(&mut app, 1);
    app.ok(&format!("drag-over {}", center(&r2)));
    assert_eq!(highlight(&mut app), Some(r2.clone()));
    let shot = test_dir().join("drop-highlight.png");
    app.ok(&format!("screenshot {}", shot.display()));
    let png = Png::read(&shot);
    let inside = png.rgb((r2[0] + r2[2] / 2) as u32, (r2[1] + r2[3] - 10) as u32);
    let outside = png.rgb((r3[0] + r3[2] / 2) as u32, (r3[1] + r3[3] - 10) as u32);
    assert!(
        inside & 0xff > (outside & 0xff) + 20,
        "{inside:06x} against {outside:06x}"
    );
    assert_eq!(
        png.rgb(r2[0] as u32, (r2[1] + r2[3] / 2) as u32),
        0x3d8af7,
        "the outline"
    );
    let number = app.first_window().get("number").int();
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-l", &number.to_string()])
        .arg(test_dir().join("drop-highlight-window.png"))
        .status();
    app.ok(&format!("drag-over {}", center(&r3)));
    assert_eq!(highlight(&mut app), Some(r3.clone()));
    app.ok("drag-exit");
    assert_eq!(highlight(&mut app), None);
    // A file whose name has spaces and quotes, dropped after "cat ".
    let file = test_dir().join("it's a \"drop\".txt");
    std::fs::write(&file, "the dropped file's content\n").expect("file");
    app.ok("focus-pane 1");
    app.ok("focus-pane 3");
    app.ok("text cat\\ ");
    app.ok(&format!("drag-over {}", center(&r3)));
    app.ok(&format!(
        "drop {} path {}",
        center(&r3),
        escape(&file.to_string_lossy())
    ));
    assert_eq!(
        highlight(&mut app),
        None,
        "the highlight goes with the drop"
    );
    // Nothing runs until Enter.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!app.screen_of(3).contains("the dropped file's content"));
    app.ok("key enter");
    app.wait_screen_of(3, "the file's content", |s| {
        s.contains("\nthe dropped file's content\n")
    });
    // Two files onto pane 1: it takes the focus, the paths come apart by a
    // space, and no newline follows.
    let other = test_dir().join("plain.txt");
    std::fs::write(&other, "x").expect("file");
    // printf prints each argument it gets on a line of its own, so the file
    // it writes shows both paths arrived whole and apart.
    let out = test_dir().join("two-drops.txt");
    let _ = std::fs::remove_file(&out);
    app.ok("focus-pane 1");
    app.type_line("clear");
    app.ok(&format!("text {}", escape("printf '<%s>\\n' ")));
    app.ok(&format!(
        "drop {} path {} {}",
        center(&r1),
        escape(&file.to_string_lossy()),
        escape(&other.to_string_lossy())
    ));
    assert_eq!(app.first_window().get("focused_pane").int(), 1);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!out.exists(), "the drop ran by itself");
    app.ok(&format!(
        "text {}",
        escape(&format!(" > '{}'", out.display()))
    ));
    app.ok("key enter");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while support::read(&out).lines().count() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "printf never wrote {}",
            out.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        support::read(&out),
        format!("<{}>\n<{}>\n", file.display(), other.display())
    );
    let squeeze = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    // Dropped text with a newline at the end does not run either.
    app.ok(&format!("drop {} text echo\\ not\\ run\\n", center(&r2)));
    std::thread::sleep(Duration::from_millis(400));
    let screen = app.screen_of(2);
    assert!(screen.contains("% echo not run"), "{screen}");
    assert!(!screen.lines().any(|l| l == "not run"), "{screen}");
    app.ok("key ctrl+u");
    // Nor do lines inside dropped text, in a shell that does not bracket
    // pastes (macOS's own bash 3.2): they arrive joined by spaces.
    app.type_line("clear; /bin/bash --norc --noprofile");
    app.wait_screen_of(2, "bash", |s| s.contains("bash-3.2$"));
    app.ok(&format!(
        "drop {} text {}",
        center(&r2),
        escape("echo dropped-ran-$((40+2))\necho second\n")
    ));
    std::thread::sleep(Duration::from_millis(400));
    let screen = app.screen_of(2);
    assert!(
        screen.contains("bash-3.2$ echo dropped-ran-$((40+2)) echo second"),
        "{screen}"
    );
    assert!(!screen.contains("dropped-ran-42"), "{screen}");
    app.ok("key ctrl+u");
    app.type_line("exit");
    app.wait_prompt_of(2);
    // A program that asked for bracketed paste gets the drop bracketed.
    app.ok("focus-pane 2");
    app.type_line(&format!("clear; {} 2004", helper()));
    wait_helper(&mut app, 2, "READY");
    app.ok(&format!(
        "drop {} path {}",
        center(&r2),
        escape(&other.to_string_lossy())
    ));
    let want = format!("GOT:<1b>[200~{}<1b>[201~", other.display());
    let screen = app.wait_screen_of(2, "a bracketed drop", |s| {
        squeeze(s).replace(' ', "").contains(&want.replace(' ', ""))
    });
    assert!(
        !screen.contains("<0a>") && !screen.contains("<0d>"),
        "{screen}"
    );
    app.ok("key ctrl+d");
    app.wait_prompt_of(2);
    app.quit();
}
