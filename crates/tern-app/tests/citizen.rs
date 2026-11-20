//! The configuration at run time, input methods, the app as a
//! Mac citizen (Dock, minimize, zoom, titles) and programs that fail.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::Duration;

use support::{escape, event, test_dir, windows, App, Json, Png};

fn conf_path(name: &str) -> std::path::PathBuf {
    test_dir().join(format!("{name}.conf"))
}

fn menu_item(menu: &Json, command: &str) -> Json {
    menu.get("menus")
        .arr()
        .iter()
        .flat_map(|m| m.get("items").arr().iter())
        .find(|i| matches!(i, Json::Obj(o) if o.get("command") == Some(&Json::Str(command.into()))))
        .cloned()
        .unwrap_or_else(|| panic!("no menu item for {command}"))
}

#[test]
#[ignore = "opens windows"]
fn a_reloaded_configuration_applies_to_open_windows() {
    let mut app = App::launch_with("rl", "background = #101010\n", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.wait_prompt_of(2);
    let before = app.first_window();
    let scale = before.get("scale").num();
    let cell = before.get("cell").ints();
    assert_eq!(app.pane(2).get("cursor").get("shape").str(), "block");
    // The title bar lets the window's color through, and that color is
    // the terminal's background.
    let bar = before.get("titlebar");
    assert_eq!(bar.get("color").int(), 0x101010);
    assert!(
        bar.get("transparent").bool() && bar.get("dark").bool(),
        "{bar:?}"
    );
    let new_tab = menu_item(&app.ok("menu"), "new_tab");
    assert_eq!(new_tab.get("key").str(), "t");

    // Everything that can change live, and two mistakes.
    std::fs::write(
        conf_path("rl"),
        "shell = /bin/zsh\nconfirm-close = false\n\
         background = #203040\nfont-size = 16\nwindow-padding-x = 20\n\
         unfocused-split-dim = 0.5\nsplit-divider-color = #ff0000\n\
         cursor-style = bar\nkeybind = cmd+t=unbind\nkeybind = cmd+shift+j=new_tab\n\
         option-as-alt = false\ncopy-on-select = true\nnot-a-setting = 1\nfont-size = 900\n",
    )
    .expect("config");
    app.ok("action reload_config");
    let s = app.state();
    // The file's font size was taken; the bad one was refused.
    assert_eq!(s.get("font_size").num(), 16.0);
    let w = &windows(&s)[0];
    let bigger = w.get("cell").ints();
    assert!(bigger[0] > cell[0] && bigger[1] > cell[1], "{bigger:?}");
    // Problems are shown in an alert, and everything else applied.
    let questions = s.get("questions").arr();
    assert_eq!(questions.len(), 1, "{questions:?}");
    assert_eq!(
        questions[0].get("message").str(),
        "The configuration has problems"
    );
    assert!(w.get("sheet").bool());
    let log = support::read(&app.stderr);
    assert!(log.contains("not-a-setting"), "{log}");
    assert!(log.contains("font-size"), "{log}");
    app.ok("confirm yes");
    app.wait_for("the alert gone", |s| {
        !windows(s)[0].get("sheet").bool() && s.get("questions").arr().is_empty()
    });
    // The title bar took the new background.
    app.wait_for("the title bar recolored", |s| {
        windows(s)[0].get("titlebar").get("color").int() == 0x203040
    });
    // Padding and grids: panes that share a window start at the padding.
    for id in [1, 2] {
        let p = app.pane(id);
        let rect = p.get("rect").ints();
        let origin = p.get("origin").ints();
        assert_eq!(origin[0] - rect[0], (20.0 * scale).round() as i64, "{p:?}");
        app.check_stty(id);
    }
    // A white block in the pane that will be without focus, to see it fade.
    app.run_in(1, r"clear; printf '\e[48;2;255;255;255m  \e[0m\n'");
    app.wait_screen_of(1, "the block's line and a prompt", |s| s.starts_with("\n%"));
    app.ok("focus-pane 2");
    // Divider color and colors on screen: both panes have the new
    // background, and what the one without focus shows has gone halfway
    // to it.
    let w = app.first_window();
    let divider = w.get("quads").arr()[0].clone();
    assert_eq!(divider.get("color").int(), 0xff0000);
    let shot = test_dir().join("reload.png");
    app.ok(&format!("screenshot {}", shot.display()));
    let png = Png::read(&shot);
    let r1 = app.pane(1).get("rect").ints();
    let r2 = app.pane(2).get("rect").ints();
    let bottom = |r: &[i64]| ((r[0] + r[2] / 2) as u32, (r[1] + r[3] - 3) as u32);
    let (x, y) = bottom(&r2);
    assert_eq!(png.rgb(x, y), 0x203040, "the focused pane");
    let (x, y) = bottom(&r1);
    assert_eq!(png.rgb(x, y), 0x203040, "the faded pane's background");
    let origin = app.pane(1).get("origin").ints();
    let block = png.rgb(
        (origin[0] + bigger[0]) as u32,
        (origin[1] + bigger[1] / 2) as u32,
    );
    for (shift, bg) in [(16, 0x20), (8, 0x30), (0, 0x40)] {
        let got = f64::from((block >> shift) & 0xff);
        assert!(
            (got - (255.0 + f64::from(bg)) / 2.0).abs() <= 1.5,
            "{block:06x}"
        );
    }
    let d = divider.get("rect").ints();
    assert_eq!(png.rgb(d[0] as u32, (d[1] + d[3] / 2) as u32), 0xff0000);
    // The cursor shape.
    app.wait_on("panes", "the bar cursor", |r| {
        r.get("windows").arr()[0].get("panes").arr()[1]
            .get("cursor")
            .get("shape")
            .str()
            == "beam"
    });
    // Bindings and the menu that shows them.
    let menu = app.ok("menu");
    let new_tab = menu_item(&menu, "new_tab");
    assert_eq!(
        (new_tab.get("key").str(), new_tab.get("mods").strs()),
        ("j", vec!["shift", "super"])
    );
    let reply = app.ok("key cmd+shift+j");
    assert_eq!(event(&reply, "key").get("action").str(), "new_tab");
    app.wait_for("a second tab", |s| windows(s).len() == 2);
    app.ok("action close_tab");
    app.wait_for("one tab", |s| windows(s).len() == 1);
    let reply = app.ok("key cmd+t");
    assert!(event(&reply, "key").get("sent").str().is_empty());
    // Option now types: Option-8 is a bullet on a US layout, not Alt-8.
    app.ok("focus-pane 2");
    let reply = app.ok("key alt+8");
    assert_ne!(event(&reply, "key").get("sent").str(), "\u{1b}8");
    app.ok("key ctrl+u");
    // Copy on select.
    app.type_line("clear; echo reloaded-and-copied");
    app.wait_screen_of(2, "the text", |s| s.starts_with("reloaded-and-copied\n"));
    let p = app.pane(2);
    let origin = p.get("origin").ints();
    let w = app.first_window();
    let cell = w.get("cell").ints();
    let at = format!(
        "{} {}",
        (origin[0] + cell[0] * 3) as f64 / scale,
        (origin[1] + cell[1] / 2) as f64 / scale
    );
    app.ok(&format!("mouse down {at} x2"));
    app.ok(&format!("mouse up {at} x2"));
    app.wait_on("clipboard", "the selection copied", |r| {
        r.get("text").str() == "reloaded-and-copied"
    });

    // A font that does not exist is reported and the old one kept. The
    // background is a light one now.
    std::fs::write(
        conf_path("rl"),
        "shell = /bin/zsh\nconfirm-close = false\nfont-family = No Such Font Anywhere\n\
         background = #fdf6e3\n",
    )
    .expect("config");
    app.ok("action reload_config");
    let s = app.state();
    assert_eq!(s.get("questions").arr().len(), 1);
    // The size the file no longer sets goes back to the default.
    assert_eq!(s.get("font_size").num(), 13.0);
    let log = support::read(&app.stderr);
    assert!(log.contains("No Such Font Anywhere"), "{log}");
    app.ok("confirm yes");
    // A light background brings the light appearance to the title bar.
    app.wait_for("the light title bar", |s| {
        let bar = windows(s)[0].get("titlebar");
        bar.get("color").int() == 0xfdf6e3 && !bar.get("dark").bool()
    });
    // A program that changes its background (OSC 11) takes the title bar
    // along: pane 1 is the one under the title bar's left end.
    app.run_in(1, r"printf '\e]11;#400000\a'");
    app.wait_for("the title bar following the program", |s| {
        let bar = windows(s)[0].get("titlebar");
        bar.get("color").int() == 0x400000 && bar.get("dark").bool()
    });
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn open_config_writes_the_commented_defaults_when_there_is_no_file() {
    let mut app = App::launch("oc", &[]);
    app.wait_prompt();
    let path = conf_path("oc");
    std::fs::remove_file(&path).expect("remove the config");
    app.ok("action open_config");
    let text = std::fs::read_to_string(&path).expect("the config was written");
    assert!(text.starts_with("# Tern configuration\n"), "{text}");
    assert!(text.contains("# font-size = 13\n"));
    // Under automation nothing is opened; the request is recorded.
    assert_eq!(
        app.state().get("opened_links").strs(),
        [format!("open {}", path.display())]
    );
    // An existing file is left as it is.
    std::fs::write(&path, "font-size = 14\n").expect("config");
    app.ok("action open_config");
    assert_eq!(support::read(&path), "font-size = 14\n");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn composed_text_is_drawn_at_the_cursor_and_sent_once() {
    let mut app = App::launch("im", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.wait_prompt_of(2);
    app.ok("text echo\\ ");
    app.ok("marked にほん");
    // Drawn over the focused pane, but not yet sent to the program.
    app.wait_on("drawn", "the composition drawn", |r| {
        r.get("text").str().starts_with("% echo にほん\n")
    });
    assert!(app.screen_of(2).starts_with("% echo\n"));
    assert!(!app.ok("drawn 1").get("text").str().contains("にほん"));
    // Underlined, in the default foreground, at the cursor cell.
    let p = app.pane(2);
    let origin = p.get("origin").ints();
    let cursor = p.get("cursor");
    let col = cursor.get("col").int();
    assert_eq!(col, 7);
    let w = app.first_window();
    let cell = w.get("cell").ints();
    let scale = w.get("scale").num();
    let shot = test_dir().join("composing.png");
    app.ok(&format!("screenshot {}", shot.display()));
    let png = Png::read(&shot);
    let x0 = (origin[0] + col * cell[0]) as u32;
    let width = (cell[0] * 6) as u32;
    let underline = (0..cell[1] as u32).rev().find(|&dy| {
        png.pixels(x0, origin[1] as u32 + dy, width, 1)
            .iter()
            .all(|&c| c == 0xdcdfe4)
    });
    assert!(underline.is_some(), "no underline under the composed text");
    // The input method's window goes at the cursor cell, in screen points.
    let ime = w
        .get("ime_rect")
        .arr()
        .iter()
        .map(Json::num)
        .collect::<Vec<_>>();
    let frame = w
        .get("frame")
        .arr()
        .iter()
        .map(Json::num)
        .collect::<Vec<_>>();
    let want_x = frame[0] + (origin[0] + col * cell[0]) as f64 / scale;
    assert!((ime[0] - want_x).abs() < 1.0, "{ime:?} {want_x}");
    assert_eq!(
        (ime[2], ime[3]),
        (cell[0] as f64 / scale, cell[1] as f64 / scale)
    );
    // Committed text reaches the program once, and the composition goes.
    app.ok("text 日本");
    let screen = app.wait_screen_of(2, "the commit", |s| s.starts_with("% echo 日本\n"));
    assert_eq!(screen.matches("日本").count(), 1, "{screen}");
    app.ok("key enter");
    app.wait_screen_of(2, "the echo", |s| s.contains("\n日本\n"));
    app.wait_on("drawn", "the composition gone from the screen", |r| {
        let drawn = r.get("text").str();
        drawn.contains("\n日本\n") && !drawn.contains("にほん")
    });
    // And from the pixels: no underline is left where it was drawn.
    let shot = test_dir().join("composed.png");
    app.ok(&format!("screenshot {}", shot.display()));
    let png = Png::read(&shot);
    let dy = underline.expect("the underline row");
    let left = png.pixels(x0, origin[1] as u32 + dy, width, 1);
    assert!(
        left.iter().any(|&c| c != 0xdcdfe4),
        "the underline is still drawn"
    );
    app.quit();
}

/// The questions showing or waiting, as (message, showing).
fn questions(state: &Json) -> Vec<(String, bool)> {
    state
        .get("questions")
        .arr()
        .iter()
        .map(|q| (q.get("message").str().to_string(), q.get("shown").bool()))
        .collect()
}

#[test]
#[ignore = "opens windows"]
fn a_quit_question_whose_window_closed_is_asked_again() {
    let mut app = App::launch_with("qw", "confirm-close = true\n", &[]);
    app.wait_prompt();
    app.type_line("sleep 100");
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.wait_prompt();
    app.type_line("sleep 2; exit");
    app.wait_on("panes", "both programs running", |r| {
        r.get("windows")
            .arr()
            .iter()
            .all(|w| w.get("panes").arr()[0].get("foreground") == &Json::Str("sleep".into()))
    });
    app.ok("action quit");
    app.wait_for("the question on the second tab", |s| {
        windows(s)[1].get("sheet").bool()
    });
    // The tab closes by itself, sheet and all; the first tab still runs a
    // program, so it asks there.
    let s = app.wait_for("the question on the first tab", |s| {
        windows(s).len() == 1 && windows(s)[0].get("sheet").bool()
    });
    assert_eq!(questions(&s), [("Quit Tern?".to_string(), true)]);
    let _ = app.try_send("confirm yes");
    assert!(app.wait_exit().success());
}

#[test]
#[ignore = "opens windows"]
fn configuration_notices_are_shown_once_and_never_lost() {
    let mut app = App::launch("nq", &[]);
    app.wait_prompt();
    app.ok("action new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    std::fs::write(conf_path("nq"), "shell = /bin/zsh\nbogus-key = 1\n").expect("config");
    // The same problems reloaded again make one alert, not four.
    for _ in 0..4 {
        app.ok("action reload_config");
    }
    let s = app.state();
    let notice = "The configuration has problems".to_string();
    assert_eq!(questions(&s), [(notice.clone(), true)]);
    // Its window closes; the notice goes to the window that is left.
    app.ok("action close_tab");
    let s = app.wait_for("the notice on the other tab", |s| {
        windows(s).len() == 1 && windows(s)[0].get("sheet").bool()
    });
    assert_eq!(questions(&s), [(notice.clone(), true)]);
    app.ok("confirm yes");
    app.wait_for("the notice answered", |s| {
        s.get("questions").arr().is_empty()
    });
    // With no window open, a reload's problems wait for the next one.
    app.ok("action close_window");
    app.wait_for("no windows", |s| windows(s).is_empty());
    std::fs::write(conf_path("nq"), "shell = /bin/zsh\nnot-a-real-key = 3\n").expect("config");
    app.ok("action reload_config");
    assert_eq!(questions(&app.state()), [(notice.clone(), false)]);
    app.ok("action new_window");
    let s = app.wait_for("the notice on the new window", |s| {
        windows(s).len() == 1 && windows(s)[0].get("sheet").bool()
    });
    assert_eq!(questions(&s), [(notice, true)]);
    let detail = support::read(&app.stderr);
    assert!(detail.contains("not-a-real-key"), "{detail}");
    app.ok("confirm yes");
    app.quit();
}

/// The first line of what is drawn for a pane, composition included.
fn drawn_line(app: &mut App, pane: i64) -> String {
    let drawn = app.ok(&format!("drawn {pane}"));
    drawn
        .get("text")
        .str()
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

#[test]
#[ignore = "opens windows"]
fn a_composition_stays_with_its_pane() {
    let mut app = App::launch("ip", &[]);
    app.wait_prompt();
    app.ok("action split:right");
    app.wait_prompt_of(2);
    app.ok("text echo\\ ");
    app.ok("marked にほん");
    app.wait_on("drawn 2", "the composition", |r| {
        r.get("text").str().starts_with("% echo にほん\n")
    });
    // Another pane takes the focus: the composition ends where it was.
    app.ok("focus-pane 1");
    app.wait_on("drawn 2", "the composition gone", |r| {
        r.get("text").str().starts_with("% echo\n")
    });
    assert_eq!(drawn_line(&mut app, 1), "%");
    // A click ends it too.
    app.ok("marked かな");
    app.wait_on("drawn 1", "the composition", |r| {
        r.get("text").str().starts_with("% かな\n")
    });
    let p = app.pane(1);
    let origin = p.get("origin").ints();
    let w = app.first_window();
    let (cell, scale) = (w.get("cell").ints(), w.get("scale").num());
    let at = format!(
        "{} {}",
        (origin[0] + cell[0] * 5) as f64 / scale,
        (origin[1] + cell[1] * 3) as f64 / scale
    );
    app.ok(&format!("mouse down {at}"));
    app.ok(&format!("mouse up {at}"));
    app.wait_on("drawn 1", "the composition gone", |r| {
        r.get("text").str().starts_with("%\n")
    });
    // Nothing was sent anywhere.
    assert_eq!(app.screen_of(1).lines().next(), Some("%"));
    assert_eq!(app.screen_of(2).lines().next(), Some("% echo"));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_composition_shows_under_a_hidden_cursor_and_after_scrolling_back() {
    let mut app = App::launch("ih", &[]);
    app.wait_prompt();
    // A program that hides the cursor, waiting for input at column 6.
    app.type_line("clear; tput civis; printf 'name: '; read x");
    app.wait_on("panes", "the hidden cursor", |r| {
        let p = &r.get("windows").arr()[0].get("panes").arr()[0];
        p.get("cursor") == &Json::Null
    });
    app.wait_screen("the question", |s| s.starts_with("name:"));
    app.ok("marked かな");
    app.wait_on("drawn", "the composition", |r| {
        r.get("text").str().starts_with("name: かな\n")
    });
    // The input method's window goes to that cell too.
    let w = app.first_window();
    let (cell, scale) = (w.get("cell").ints(), w.get("scale").num());
    let origin = app.pane(1).get("origin").ints();
    let ime: Vec<f64> = w.get("ime_rect").arr().iter().map(Json::num).collect();
    let frame: Vec<f64> = w.get("frame").arr().iter().map(Json::num).collect();
    let want_x = frame[0] + (origin[0] + 6 * cell[0]) as f64 / scale;
    assert!((ime[0] - want_x).abs() < 1.0, "{ime:?} {want_x}");
    app.ok("text 名前");
    app.ok("key enter");
    app.wait_prompt();
    // Scrolled back, composing returns to the bottom where the text goes.
    app.type_line("tput cnorm; clear; seq 1 200");
    app.wait_screen("the numbers", |s| s.contains("\n200\n"));
    app.ok("action scroll:page_up");
    app.wait_on("panes", "scrolled back", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0]
            .get("display_offset")
            .int()
            > 0
    });
    app.ok("marked かな");
    app.wait_on("drawn", "the composition at the prompt", |r| {
        r.get("text").str().contains("\n% かな\n")
    });
    app.ok("text x");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_committed_composition_that_wrapped_leaves_no_trace() {
    let mut app = App::launch("iw", &[]);
    app.wait_prompt();
    let cols = app.pane(1).get("grid").ints()[0] as usize;
    // The composition starts near the end of the first row and runs onto
    // the second, which the shell never draws on.
    let line = "a".repeat(cols - 30);
    app.ok("key ctrl+u");
    app.ok(&format!("text {line}"));
    app.wait_screen("the line", |s| s.starts_with(&format!("% {line}\n")));
    app.ok("marked あいうえおかきくけこさしすせそ");
    app.wait_on("drawn", "the composition on two rows", |r| {
        r.get("text")
            .str()
            .lines()
            .nth(1)
            .is_some_and(|l| l.contains("そ"))
    });
    // The commit's echo arrives in the same draw that drops the overlay.
    app.ok("text x");
    app.wait_screen("the commit", |s| s.starts_with(&format!("% {line}x\n")));
    app.wait_on("drawn", "the composition gone", |r| {
        !r.get("text").str().contains('そ')
    });
    let p = app.pane(1);
    let (rect, origin) = (p.get("rect").ints(), p.get("origin").ints());
    let cell = app.first_window().get("cell").ints();
    let shot = test_dir().join("ime-wrap.png");
    app.ok(&format!("screenshot {}", shot.display()));
    let png = Png::read(&shot);
    let bg = png.rgb(
        (rect[0] + rect[2] / 2) as u32,
        (rect[1] + rect[3] - 3) as u32,
    );
    let row1 = png.pixels(
        origin[0] as u32,
        (origin[1] + cell[1]) as u32,
        (cell[0] * cols as i64) as u32,
        cell[1] as u32,
    );
    let marked = row1.iter().filter(|&&c| c != bg).count();
    assert_eq!(marked, 0, "the second row still shows the composition");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_failed_program_says_why_instead_of_vanishing() {
    // A command that does not exist.
    let mut app = App::launch_with(
        "nx",
        "window-width = 160\n",
        &["-e", "no-such-command-anywhere", "x"],
    );
    let screen = app.wait_screen("the reason", |s| s.contains("Press any key to close."));
    // On the first line: nothing ran, so there is no output to keep apart.
    assert!(
        screen.starts_with("[could not start “no-such-command-anywhere x”: no-such-command-anywhere: command not found]\n"),
        "{screen}"
    );
    // The window is named after what failed.
    let s = app.wait_for("the title", |s| windows(s)[0].get("title").str() != "Tern");
    assert_eq!(
        windows(&s)[0].get("title").str(),
        "no-such-command-anywhere x"
    );
    app.ok("key x");
    // The window closes, and with it the app that ran the command.
    assert!(app.wait_exit().success());

    // A command that fails: the window says how, and with
    // quit-after-last-window-closed the app quits once a key closes it.
    let mut app = App::launch(
        "eq",
        &[
            "--quit-after-last-window-closed=true",
            "-e",
            "/bin/sh",
            "-c",
            "echo failing; exit 3",
        ],
    );
    let screen = app.wait_screen("the status", |s| s.contains("Press any key to close."));
    assert!(screen.starts_with("failing\n"), "{screen}");
    assert!(
        screen.contains("[“/bin/sh -c echo failing; exit 3” exited with status 3]"),
        "{screen}"
    );
    app.ok("key x");
    assert!(app.wait_exit().success());

    // A command that succeeds at once just closes, and so does the app,
    // whatever quit-after-last-window-closed says: the command was all it
    // was started for.
    let mut app = App::launch("et", &["-e", "/usr/bin/true"]);
    assert!(app.wait_exit().success());

    // A shell that cannot start, for every new pane.
    let mut app = App::launch_with("ns", "shell = /no/such/shell\n", &[]);
    let screen = app.wait_screen("the reason", |s| s.contains("Press any key"));
    assert!(
        screen.contains("[could not start the shell /no/such/shell: "),
        "{screen}"
    );
    app.ok("action split:right");
    let p = app.wait_on("panes", "the split", |r| {
        r.get("windows").arr()[0].get("panes").arr().len() == 2
    });
    assert_eq!(p.get("windows").arr()[0].get("panes").arr().len(), 2);
    app.wait_screen_of(2, "the reason", |s| s.contains("could not start the shell"));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn titles_follow_the_directory_and_long_ones_are_cut() {
    let mut app = App::launch("ti", &[]);
    app.wait_prompt();
    app.type_line("cd /usr/share");
    app.wait_for("the directory's name", |s| {
        windows(s)[0].get("title").str() == "share"
    });
    app.type_line("cd");
    app.wait_for("home", |s| windows(s)[0].get("title").str() == "~");
    // A program's title wins, and a huge one is cut to fit a tab.
    app.type_line(r"printf '\e]2;%s\a' $(printf 'x%.0s' {1..100000})");
    let s = app.wait_for("the long title", |s| {
        windows(s)[0].get("title").str().starts_with("xxxx")
    });
    let title = windows(&s)[0].get("title").str().to_string();
    assert_eq!(title.chars().count(), 257, "{}", title.len());
    assert!(title.ends_with('…'));
    app.type_line("echo still-here");
    app.wait_screen("the echo", |s| s.contains("\nstill-here\n"));
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn the_dock_minimize_and_zoom_work() {
    let mut app = App::launch("dk", &[]);
    app.wait_prompt();
    // Folders and files opened with the app get a window in that folder.
    let file = test_dir().join("opened with tern.txt");
    std::fs::write(&file, "x").expect("file");
    app.ok("open /usr/share");
    app.ok(&format!("open {}", escape(&file.to_string_lossy())));
    let s = app.wait_for("two more windows", |s| windows(s).len() == 3);
    assert_eq!(windows(&s).len(), 3);
    let panes = app.all_panes();
    let cwd = |i: usize| panes[i].get("cwd").str().to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut dirs = (cwd(1), cwd(2));
    while dirs.0 != "/usr/share" {
        assert!(std::time::Instant::now() < deadline, "{dirs:?}");
        std::thread::sleep(Duration::from_millis(100));
        let panes = app.all_panes();
        dirs = (
            panes[1].get("cwd").str().to_string(),
            panes[2].get("cwd").str().to_string(),
        );
    }
    let canonical = std::fs::canonicalize(test_dir()).expect("test dir");
    assert_eq!(std::path::PathBuf::from(&dirs.1), canonical);
    app.ok("action close_window");
    app.ok("action close_window");
    app.wait_for("one window", |s| windows(s).len() == 1);
    // A click on the Dock icon with no windows opens one.
    app.ok("action close_window");
    app.wait_for("no windows", |s| windows(s).is_empty());
    app.ok("reopen");
    app.wait_for("a new window", |s| windows(s).len() == 1);
    app.wait_prompt();
    // With a window open, the Dock icon opens nothing more.
    app.ok("reopen");
    assert_eq!(windows(&app.state()).len(), 1);

    // Zoom fills the screen and comes back; the grid follows both ways.
    let before = app.first_window();
    app.ok("action zoom");
    let zoomed = app.wait_for("the zoom", |s| {
        windows(s)[0].get("size_pt") != before.get("size_pt")
    });
    let big = windows(&zoomed)[0].get("size_pt").arr()[0].num();
    assert!(big > before.get("size_pt").arr()[0].num());
    let id = app.panes()[0].get("id").int();
    app.check_stty(id);
    app.ok("action zoom");
    app.wait_for("the size back", |s| {
        windows(s)[0].get("size_pt") == before.get("size_pt")
    });
    let id = app.panes()[0].get("id").int();
    app.check_stty(id);
    // Minimized, the window draws nothing and its program runs on.
    // (Bringing it back from the Dock can activate the app, which a test
    // must never do; that is left to a person.)
    app.ok("action minimize");
    app.wait_for("minimized", |s| !windows(s)[0].get("visible").bool());
    let frames = app.first_window().get("frames").int();
    app.type_line("echo while-minimized");
    app.wait_screen("the output", |s| s.contains("\nwhile-minimized\n"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(app.first_window().get("frames").int(), frames);
    assert!(!app.state().get("active").bool(), "Tern must not activate");
    app.quit();
}
