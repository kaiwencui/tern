//! The terminal: a shell in a pane, drawn by the renderer, fed
//! by the keyboard, sized by the window and the font.
//!
//! These tests open windows, so they are ignored by default. Run them with
//!
//!     cargo test -p tern-app -- --include-ignored --test-threads=1
//!
//! See tests/support for the environment they read.

mod support;

use std::time::{Duration, Instant};

use support::{check_on_screen, escape, event, read, test_dir, windows, App, Json, Png};

/// The default theme's colors, as the config crate defines them.
const BG: u32 = 0x181a1f;
const FG: u32 = 0xdcdfe4;
const RED: u32 = 0xe06c75;
const GREEN: u32 = 0x98c379;

fn window_number(app: &mut App) -> i64 {
    let state = app.state();
    windows(&state)[0].get("number").int()
}

/// The cell size in pixels and the grid's origin, for finding cells in a screenshot.
fn geometry(app: &mut App) -> ((u32, u32), (u32, u32)) {
    let state = app.state();
    let cell = windows(&state)[0].get("cell").ints();
    let pane = &app.panes()[0];
    let origin = pane.get("origin").ints();
    (
        (cell[0] as u32, cell[1] as u32),
        (origin[0] as u32, origin[1] as u32),
    )
}

fn grid(pane: &Json) -> (i64, i64) {
    let g = pane.get("grid").ints();
    (g[0], g[1])
}

#[test]
#[ignore = "opens windows"]
fn a_shell_runs_what_is_typed() {
    let mut app = App::launch("sh", &[]);
    app.wait_prompt();
    // The log line comes when Metal reports the frame shown, a moment
    // after the shell's prompt is on the screen.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !read(&app.stderr).contains("first frame with terminal content presented") {
        assert!(Instant::now() < deadline, "{}", read(&app.stderr));
        std::thread::sleep(Duration::from_millis(20));
    }
    let number = window_number(&mut app);
    check_on_screen(&app, number);
    let pane = &app.panes()[0];
    assert_eq!(
        grid(pane),
        (100, 30),
        "the configured window size, in cells"
    );
    assert_eq!(pane.get("frame_grid").ints(), [100, 30]);
    let home = std::env::var("HOME").expect("HOME");
    assert_eq!(
        pane.get("cwd").str(),
        home,
        "the first window starts at home"
    );
    assert!(pane.get("focused").bool());

    // Each key goes through the key path, as typing does.
    for key in "echo hello".chars() {
        let spec = if key == ' ' {
            "space".to_string()
        } else {
            key.to_string()
        };
        let reply = app.ok(&format!("key {spec}"));
        assert_eq!(event(&reply, "key").get("sent").str(), key.to_string());
    }
    let reply = app.ok("key enter");
    assert_eq!(event(&reply, "key").get("sent").str(), "\r");
    app.wait_screen("the output and the next prompt", |s| {
        s.starts_with("% echo hello\nhello\n%")
    });

    // A new tab starts where the focused pane is (working-directory =
    // inherit, the default).
    app.type_line("cd /usr/bin");
    app.wait_on("panes", "the shell to move", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0]
            .get("cwd")
            .str()
            == "/usr/bin"
    });
    app.ok("key cmd+t");
    let reply = app.wait_on("panes", "a second tab", |r| {
        r.get("windows").arr().len() == 2
    });
    let tab = &reply.get("windows").arr()[1].get("panes").arr()[0];
    assert_eq!(tab.get("cwd").str(), "/usr/bin");
    assert_ne!(
        tab.get("pid").int(),
        pane.get("pid").int(),
        "a shell of its own"
    );
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn colors_bold_wide_characters_and_box_drawing_are_drawn() {
    let mut app = App::launch_with("col", "window-width = 60\nwindow-height = 10\n", &[]);
    app.wait_prompt();
    app.type_line(r"clear; printf '\e[1;31mBOLD\e[0m \e[42m    \e[0m 日本 ──┼──\n'");
    let screen = app.wait_screen("the output", |s| s.lines().any(|l| l.starts_with("BOLD")));
    let line = screen.lines().position(|l| l.starts_with("BOLD")).unwrap() as u32;
    assert_eq!(
        screen.lines().nth(line as usize),
        Some("BOLD      日本 ──┼──"),
        "{screen:?}"
    );
    app.wait_prompt();
    let ((cw, ch), (ox, oy)) = geometry(&mut app);
    let cell = |col: u32, row: u32| (ox + col * cw, oy + row * ch);
    let shot = test_dir().join("col.png");
    let _ = std::fs::remove_file(&shot);
    let reply = app.ok(&format!("screenshot {}", escape(&shot.to_string_lossy())));
    let png = Png::read(&shot);
    assert_eq!(
        (i64::from(png.width), i64::from(png.height)),
        (reply.get("width").int(), reply.get("height").int())
    );

    // The padding and empty cells are the background.
    assert_eq!(png.rgb(1, 1), BG);
    assert_eq!(png.rgb(png.width - 2, png.height - 2), BG);
    // "BOLD" is drawn in the red of the palette, on the background.
    let (x, y) = cell(0, line);
    let bold = png.pixels(x, y, 4 * cw, ch);
    assert!(bold.contains(&RED), "no red text in BOLD");
    assert!(
        bold.iter().all(|&p| p == BG || is_tint_of(p, RED)),
        "BOLD has stray colors"
    );
    let lit = |pixels: &[u32]| pixels.iter().filter(|&&p| p != BG).count();
    // Four cells of green background, edge to edge.
    let (x, y) = cell(5, line);
    assert!(png.pixels(x, y, 4 * cw, ch).iter().all(|&p| p == GREEN));
    // The wide characters each cover two cells: ink in both halves.
    let (x, y) = cell(10, line);
    assert!(lit(&png.pixels(x, y, cw, ch)) > 0, "left half of 日");
    assert!(lit(&png.pixels(x + cw, y, cw, ch)) > 0, "right half of 日");
    // Box drawing joins cell to cell: the horizontal line runs unbroken
    // through the middle row of "──┼──".
    let (x, y) = cell(15, line);
    let middle: Vec<u32> = (0..5 * cw)
        .map(|dx| {
            (0..ch)
                .map(|dy| png.rgb(x + dx, y + dy))
                .find(|&p| p != BG)
                .unwrap_or(BG)
        })
        .collect();
    assert!(middle.iter().all(|&p| p != BG), "the line has a gap");
    // The vertical stroke of ┼ reaches the top and bottom of its cell.
    let (x, y) = cell(17, line);
    let column: Vec<u32> = (0..ch)
        .map(|dy| {
            (0..cw)
                .map(|dx| png.rgb(x + dx, y + dy))
                .find(|&p| p != BG)
                .unwrap_or(BG)
        })
        .collect();
    assert!(column.iter().all(|&p| p != BG), "the cross has a gap");

    // The window is not key, so the cursor is a hollow block at the prompt.
    let cursor = &app.panes()[0].get("cursor").clone();
    let (x, y) = cell(
        cursor.get("col").int() as u32,
        cursor.get("row").int() as u32,
    );
    assert_eq!(png.rgb(x, y), FG, "the cursor's corner");
    assert_eq!(
        png.rgb(x + cw / 2, y + ch / 2),
        BG,
        "a hollow cursor's middle"
    );
    // The window on screen shows the same: the window server gives back a
    // picture of the right size (the eyes check the rest).
    let number = window_number(&mut app);
    let capture = test_dir().join("col-window.png");
    let status = std::process::Command::new("screencapture")
        .args(["-x", "-o", "-l", &number.to_string()])
        .arg(&capture)
        .status()
        .expect("screencapture");
    assert!(status.success());
    assert!(std::fs::metadata(&capture).map(|m| m.len()).unwrap_or(0) > 0);
    app.quit();
}

/// Whether c is the color over the background at some coverage, as
/// antialiased text blends it.
fn is_tint_of(c: u32, color: u32) -> bool {
    let ch = |v: u32, s: u32| ((v >> s) & 0xff) as f64;
    let mut t = None;
    for s in [16, 8, 0] {
        let (bg, fg, v) = (ch(BG, s), ch(color, s), ch(c, s));
        if (fg - bg).abs() < 8.0 {
            continue;
        }
        let k = (v - bg) / (fg - bg);
        match t {
            None => t = Some(k),
            Some(t) if (t - k).abs() > 0.08 => return false,
            _ => {}
        }
    }
    true
}

#[test]
#[ignore = "opens windows"]
fn vim_runs_and_the_screen_comes_back() {
    let mut app = App::launch("vim", &[]);
    app.wait_prompt();
    app.type_line("echo before vim");
    app.wait_screen("the echo", |s| s.lines().any(|l| l == "before vim"));
    app.type_line("/usr/bin/vim -u NONE");
    app.wait_on("panes", "the alternate screen", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0]
            .get("modes")
            .get("alt_screen")
            .bool()
    });
    app.wait_screen("vim's empty buffer", |s| s.lines().nth(1) == Some("~"));
    app.ok("key i");
    app.ok("text hello\\ from\\ vim");
    app.ok("key escape");
    app.wait_screen("the typed text", |s| s.starts_with("hello from vim\n~"));
    let pane = &app.panes()[0];
    assert_eq!(pane.get("foreground").str(), "vim");
    app.ok("text :q!");
    app.ok("key enter");
    app.wait_on("panes", "the main screen", |r| {
        !r.get("windows").arr()[0].get("panes").arr()[0]
            .get("modes")
            .get("alt_screen")
            .bool()
    });
    let screen = app.wait_prompt();
    assert!(
        screen.starts_with("% echo before vim\nbefore vim\n% /usr/bin/vim -u NONE\n%"),
        "the screen before vim is back: {screen:?}"
    );
    assert!(!screen.contains('~'), "{screen:?}");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn the_window_size_reaches_the_program_and_the_font_size_changes_the_grid() {
    let mut app = App::launch("rs", &[]);
    app.wait_prompt();
    let state = app.state();
    let w = &windows(&state)[0];
    let scale = w.get("scale").num();
    let cell = w.get("cell").ints();
    // 10 points of padding on each side by default.
    let pad = (10.0 * scale).round() as i64;
    let fits = |pt: f64, cell: i64| ((pt * scale).round() as i64 - 2 * pad) / cell;

    let stty = |app: &mut App, cols: i64, rows: i64| {
        app.type_line("clear; stty size");
        let want = format!("{rows} {cols}");
        app.wait_screen("stty's answer", |s| s.lines().any(|l| l == want));
    };
    app.ok("resize 500 300");
    let pane = &app.panes()[0];
    let (cols, rows) = (fits(500.0, cell[0]), fits(300.0, cell[1]));
    assert_eq!(grid(pane), (cols, rows));
    // A pane with the window to itself is centered: what the whole cells
    // leave over is shared between the two sides.
    let (rect, origin) = (pane.get("rect").ints(), pane.get("origin").ints());
    assert_eq!(
        origin,
        [
            pad + (rect[2] - 2 * pad - cols * cell[0]) / 2,
            pad + (rect[3] - 2 * pad - rows * cell[1]) / 2
        ]
    );
    stty(&mut app, cols, rows);

    // A live resize, as dragging the window's edge does, draws in step
    // with the window.
    let frames = app.stats().get("windows").arr()[0].get("frames").int();
    app.ok("resize 560 340 live");
    let (cols, rows) = (fits(560.0, cell[0]), fits(340.0, cell[1]));
    assert_eq!(grid(&app.panes()[0]), (cols, rows));
    app.wait_on("stats", "a frame of the new size", |s| {
        s.get("windows").arr()[0].get("frames").int() > frames
    });
    stty(&mut app, cols, rows);

    // Bigger font, fewer cells; the window keeps its size.
    let before = grid(&app.panes()[0]);
    let reply = app.ok("key cmd+=");
    assert_eq!(
        event(&reply, "key").get("action").str(),
        "font_size:increase"
    );
    app.ok("key cmd+shift+=");
    assert_eq!(app.state().get("font_size").num(), 15.0);
    let bigger = grid(&app.panes()[0]);
    assert!(
        bigger.0 < before.0 && bigger.1 < before.1,
        "{before:?} -> {bigger:?}"
    );
    stty(&mut app, bigger.0, bigger.1);
    app.ok("key cmd+-");
    assert_eq!(app.state().get("font_size").num(), 14.0);
    app.ok("key cmd+0");
    assert_eq!(app.state().get("font_size").num(), 13.0);
    assert_eq!(grid(&app.panes()[0]), before);
    stty(&mut app, before.0, before.1);
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn a_command_runs_and_its_window_closes_when_it_exits() {
    let mut app = App::launch(
        "e",
        &[
            "-e",
            "/bin/sh",
            "-c",
            r"printf '\033]0;from -e\007'; echo ran; sleep 3",
        ],
    );
    app.wait_screen("the output", |s| s.starts_with("ran\n"));
    let state = app.wait_for("the title", |s| {
        windows(s)
            .first()
            .is_some_and(|w| w.get("title").str() == "from -e")
    });
    assert_eq!(
        state.get("command").strs(),
        [
            "/bin/sh",
            "-c",
            r"printf '\033]0;from -e\007'; echo ran; sleep 3"
        ]
    );
    // The window closes when the command exits, and the app with it: the
    // command was what it was started for.
    assert!(app.wait_exit().success());
    // A command that fails keeps its window to say so; see citizen.rs.
}

#[test]
#[ignore = "opens windows"]
fn the_clipboard_is_written_by_programs_and_used_by_copy_and_paste() {
    let mut app = App::launch("cb", &[]);
    app.wait_prompt();
    // OSC 52 stores; the app's pasteboard in tests is a private one.
    app.type_line(r"printf '\e]52;c;%s\a' $(printf 'stored by osc' | base64)");
    app.wait_on("clipboard", "the stored text", |r| {
        r.get("text").str() == "stored by osc"
    });
    // Reading the clipboard is refused: no reply ever arrives.
    app.type_line(r#"clear; printf '\e]52;c;?\a'; read -t 1 -r reply; echo "[$reply]""#);
    app.wait_screen("no answer", |s| s.lines().any(|l| l == "[]"));

    // Paste: the pasteboard's text arrives as typed, bracketed for zsh.
    app.ok("clipboard-set echo\\ pasted\\ text");
    let reply = app.ok("key cmd+v");
    assert_eq!(event(&reply, "key").get("action").str(), "paste");
    app.ok("key enter");
    app.wait_screen("the pasted command's output", |s| {
        s.lines().any(|l| l == "pasted text")
    });

    // Copy: select all, then copy.
    app.ok("key cmd+a");
    app.ok("key cmd+c");
    // The copy is made on another thread.
    app.wait_on("clipboard", "the copied text", |r| {
        r.get("text")
            .str()
            .contains("% echo pasted text\npasted text\n")
    });
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn keys_follow_the_bindings_and_the_option_setting() {
    let mut app = App::launch_with("keys", "option-as-alt = left\n", &[]);
    app.wait_prompt();
    let sent = |app: &mut App, spec: &str| {
        let reply = app.ok(&format!("key {spec}"));
        event(&reply, "key").get("sent").str().to_string()
    };
    let bound = |app: &mut App, spec: &str| {
        let reply = app.ok(&format!("key {spec}"));
        event(&reply, "key").get("action").str().to_string()
    };
    assert_eq!(sent(&mut app, "ctrl+c"), "\u{3}");
    assert_eq!(sent(&mut app, "up"), "\u{1b}[A");
    assert_eq!(sent(&mut app, "f5"), "\u{1b}[15~");
    assert_eq!(sent(&mut app, "shift+tab"), "\u{1b}[Z");
    // The left Option key is Alt; the right one types what the layout says.
    assert_eq!(sent(&mut app, "alt+f"), "\u{1b}f");
    let right = sent(&mut app, "ralt+f");
    assert!(
        !right.is_empty() && !right.starts_with('\u{1b}'),
        "{right:?}"
    );
    // Command keys without a binding send nothing.
    assert_eq!(sent(&mut app, "cmd+j"), "");
    assert_eq!(bound(&mut app, "cmd+left"), r"text:\x01");
    assert_eq!(bound(&mut app, "alt+left"), "esc:b");
    app.ok("key ctrl+c");
    app.wait_prompt();

    // History, scrolling back, and typing returning to the bottom.
    app.type_line("clear; seq 1 200");
    app.wait_screen("the numbers", |s| s.contains("200\n%"));
    assert_eq!(bound(&mut app, "shift+pageup"), "scroll:page_up");
    let pane = app.wait_on("panes", "scrolled back", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0]
            .get("display_offset")
            .int()
            > 0
    });
    let _ = pane;
    app.ok("action scroll:top");
    let top = app.wait_on("panes", "the top of the history", |r| {
        let pane = &r.get("windows").arr()[0].get("panes").arr()[0];
        pane.get("display_offset").int() == pane.get("history").int()
    });
    let _ = top;
    let screen = app.screen();
    assert!(screen.contains("% clear; seq 1 200\n1\n2\n"), "{screen:?}");
    app.ok("key x");
    app.wait_on("panes", "back at the bottom", |r| {
        r.get("windows").arr()[0].get("panes").arr()[0]
            .get("display_offset")
            .int()
            == 0
    });
    app.ok("key backspace");
    // Cmd+K clears the history and the screen; the shell, told with a form
    // feed, draws its prompt again. (Clearing its screen can push the one
    // empty line it was on into the history.)
    assert_eq!(bound(&mut app, "cmd+k"), "clear");
    app.wait_screen("a clear screen", |s| s.starts_with("%\n"));
    app.ok("action scroll:top");
    let screen = app.screen();
    assert!(!screen.contains("200"), "{screen:?}");
    let pane = &app.panes()[0];
    assert!(pane.get("history").int() <= 1, "{pane:?}");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn programs_that_ask_get_key_releases_but_not_for_swallowed_presses() {
    let mut app = App::launch("kr", &[]);
    app.wait_prompt();
    // The kitty keyboard protocol with disambiguation, event types and
    // every key as an escape code; cat swallows what the keys send.
    app.type_line(r"printf '\e[>11u'; cat >/dev/null");
    app.wait_on("panes", "the kitty flags, and cat reading", |r| {
        let pane = &r.get("windows").arr()[0].get("panes").arr()[0];
        pane.get("modes").get("kitty_flags").int() == 11
            && pane.get("foreground") == &Json::Str("cat".into())
    });
    let sent = |reply: &Json, down: bool| -> String {
        reply
            .get("events")
            .arr()
            .iter()
            .find(|e| e.get("type").str() == "key" && e.get("down").bool() == down)
            .map(|e| e.get("sent").str().to_string())
            .unwrap_or_else(|| panic!("no key event in {reply:?}"))
    };
    let reply = app.ok("key a");
    assert_eq!(sent(&reply, true), "\u{1b}[97u");
    assert_eq!(sent(&reply, false), "\u{1b}[97;1:3u");
    // An unbound Command key reaches the program, press and release.
    let reply = app.ok("key cmd+j");
    assert_eq!(sent(&reply, true), "\u{1b}[106;9u");
    assert_eq!(sent(&reply, false), "\u{1b}[106;9:3u");
    // A bound key's press performed its action, so its release is not sent.
    let reply = app.ok("key cmd+k");
    assert_eq!(event(&reply, "key").get("action").str(), "clear");
    assert_eq!(sent(&reply, false), "");
    // A dead key's accented letter arrives with the next key, which the
    // program gets as a key, release included.
    app.ok("marked ´");
    let reply = app.ok("key e");
    assert!(event(&reply, "key").get("composing").bool());
    assert!(!sent(&reply, true).is_empty());
    assert!(
        !sent(&reply, false).is_empty(),
        "the release was swallowed: {reply:?}"
    );
    // Modifier keys on their own are keys too under flag 8.
    let reply = app.ok("key rshift");
    let changes: Vec<String> = reply
        .get("events")
        .arr()
        .iter()
        .filter(|e| e.get("type").str() == "modifier")
        .map(|e| e.get("sent").str().to_string())
        .collect();
    assert_eq!(changes.len(), 2);
    assert!(
        changes.iter().all(|s| s.starts_with("\u{1b}[57447")),
        "{changes:?}"
    );
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn menu_items_follow_the_bindings_and_work_when_chosen() {
    let mut app = App::launch_with("mn", "keybind = cmd+shift+j=font_size:increase\nkeybind = cmd+equal=unbind\nkeybind = cmd+plus=unbind\n", &[]);
    app.wait_prompt();
    let menu = app.ok("menu");
    let item = |menu: &Json, command: &str| -> Json {
        menu.get("menus")
            .arr()
            .iter()
            .flat_map(|m| m.get("items").arr().iter())
            .find(|i| matches!(i, Json::Obj(o) if o.get("command") == Some(&Json::Str(command.into()))))
            .cloned()
            .unwrap_or_else(|| panic!("no menu item for {command}"))
    };
    // The first binding left for the action is the one shown.
    let bigger = item(&menu, "font_size:increase");
    assert_eq!(
        (bigger.get("key").str(), bigger.get("mods").strs()),
        ("=", vec!["shift", "super"])
    );
    // Chosen from the menu, it acts like the key.
    app.ok("menu-click font_size:increase");
    assert_eq!(app.state().get("font_size").num(), 14.0);
    let reply = app.ok("key cmd+shift+j");
    assert_eq!(
        event(&reply, "key").get("action").str(),
        "font_size:increase"
    );
    assert_eq!(app.state().get("font_size").num(), 15.0);
    // Reloading an unchanged configuration keeps the size the menu chose.
    app.ok("menu-click reload_config");
    assert_eq!(app.state().get("font_size").num(), 15.0);
    // Split Down, chosen from the menu, splits.
    app.ok("menu-click split:down");
    assert_eq!(
        app.state().get("windows").arr()[0]
            .get("panes")
            .ints()
            .len(),
        2
    );

    // Close Window closes every tab of the window.
    app.ok("menu-click new_tab");
    app.wait_for("two tabs", |s| windows(s).len() == 2);
    app.ok("menu-click close_window");
    app.wait_for("no windows", |s| windows(s).is_empty());
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn focus_is_reported_and_the_cursor_blinks_only_with_focus() {
    let mut app = App::launch("fo", &[]);
    app.wait_prompt();
    // cat -v shows the focus reports the terminal sends once asked.
    app.type_line(r"printf '\e[?1004h'; cat -v");
    app.wait_on("panes", "focus reporting, and cat reading", |r| {
        let pane = &r.get("windows").arr()[0].get("panes").arr()[0];
        pane.get("modes").get("focus_events").bool()
            && pane.get("foreground") == &Json::Str("cat".into())
    });
    app.ok("focus-window on");
    app.wait_screen("the focus-in report", |s| s.contains("^[[I"));
    app.ok("focus-window off");
    app.wait_screen("the focus-out report", |s| s.contains("^[[I^[[O"));
    app.ok("key ctrl+c");
    app.wait_prompt();

    // A blinking block (DECSCUSR 1).
    app.type_line(r"printf '\e[1 q'");
    app.wait_on("panes", "a blinking cursor", |r| {
        let pane = &r.get("windows").arr()[0].get("panes").arr()[0];
        matches!(pane.get("cursor"), Json::Obj(c) if c.get("blinking") == Some(&Json::Bool(true)))
    });
    // Without key focus the cursor is hollow and still.
    std::thread::sleep(Duration::from_millis(500));
    let stats = app.stats();
    let w = &stats.get("windows").arr()[0];
    assert!(!w.get("blinking").bool());
    let frames = w.get("frames").int();
    std::thread::sleep(Duration::from_secs(2));
    let w = &app.stats().get("windows").arr()[0].clone();
    assert_eq!(w.get("frames").int(), frames, "no frames while idle");
    // With focus it blinks: shown for 0.6 s, hidden for 0.6 s.
    app.ok("focus-window on");
    // The frame showing the solid cursor comes first.
    std::thread::sleep(Duration::from_millis(200));
    let start = app.stats().get("windows").arr()[0].get("blinks").int();
    let begun = Instant::now();
    // The display link is not left ticking between the blinks.
    let (mut samples, mut ticking) = (0, 0);
    while begun.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(30));
        samples += 1;
        if app.stats().get("windows").arr()[0]
            .get("link_running")
            .bool()
        {
            ticking += 1;
        }
    }
    let w = &app.stats().get("windows").arr()[0].clone();
    let blinks = w.get("blinks").int() - start;
    let seconds = begun.elapsed().as_secs_f64();
    assert!(w.get("blinking").bool());
    eprintln!(
        "blink: {blinks} blinks in {seconds:.2} s, link ticking in {ticking} of {samples} samples"
    );
    assert!(
        (1.0..=2.0).contains(&(blinks as f64 / seconds)),
        "{blinks} blinks in {seconds:.2} s"
    );
    assert!(
        ticking * 10 <= samples,
        "link ticking in {ticking} of {samples} samples"
    );
    app.ok("focus-window off");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!app.stats().get("windows").arr()[0].get("blinking").bool());
    app.quit();
}

/// A cursor blinking on an empty cell is hidden by a layer over the cell and
/// shown by taking the layer away, so the window draws no frames for it.
#[test]
#[ignore = "opens windows"]
fn a_cursor_at_rest_blinks_without_frames() {
    let mut app = App::launch_with("cb", "cursor-style-blink = true\n", &[]);
    app.wait_prompt();
    app.ok("focus-window on");
    let covered = |s: &Json| s.get("windows").arr()[0].get("covered").bool();
    let count = |app: &mut App, what: &str| app.stats().get("windows").arr()[0].get(what).int();

    // The cursor's cell in an image of the window, which has the title bar
    // above the view.
    let cell_at = |app: &mut App| {
        let w = app.first_window();
        let (cell, bar) = (
            w.get("cell").ints(),
            w.get("frame").ints()[3] - w.get("size_px").ints()[1],
        );
        let pane = app.panes()[0].clone();
        let (origin, cursor) = (pane.get("origin").ints(), pane.get("cursor"));
        let x = origin[0] + cursor.get("col").int() * cell[0];
        let y = bar + origin[1] + cursor.get("row").int() * cell[1];
        (x as u32, y as u32, cell[0] as u32, cell[1] as u32)
    };
    // The window as it is on screen, once the cursor's cell is all in one
    // color: the compositor shows a change a moment after it is made.
    let image = |app: &mut App, name: &str, (x, y, w, h): (u32, u32, u32, u32), color: u32| {
        let path = test_dir().join(name);
        let begun = Instant::now();
        loop {
            app.ok(&format!("window-image {}", path.display()));
            let png = Png::read(&path);
            let cell = png.pixels(x, y, w, h);
            if cell.iter().all(|&p| p == color) {
                return png;
            }
            assert!(
                begun.elapsed() < Duration::from_secs(1),
                "{name}: the cursor's cell is {:06x?}, not {color:06x}",
                &cell[..4]
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    let cell = cell_at(&mut app);
    app.wait_on("stats", "the cursor hidden", covered);
    let hidden = image(&mut app, "blink-hidden.png", cell, BG);
    app.wait_on("stats", "the cursor shown", |s| !covered(s));
    let shown = image(&mut app, "blink-shown.png", cell, FG);
    // Nothing but the cursor's cell differs between the two.
    let (x, y, w, h) = cell;
    for py in 0..hidden.height {
        for px in 0..hidden.width {
            let inside = (x..x + w).contains(&px) && (y..y + h).contains(&py);
            assert!(
                inside || hidden.rgb(px, py) == shown.rgb(px, py),
                "pixel {px},{py} outside the cursor's cell changed"
            );
        }
    }

    // It goes on blinking, and no frame is drawn or shown for it.
    let before = ["blinks", "draws", "frames"].map(|what| count(&mut app, what));
    std::thread::sleep(Duration::from_millis(2500));
    let after = ["blinks", "draws", "frames"].map(|what| count(&mut app, what));
    assert!((3..=5).contains(&(after[0] - before[0])), "{before:?} {after:?}");
    assert_eq!(after[1..], before[1..], "frames for the blink");

    // A typed character brings a hidden cursor back with it, at once.
    app.wait_on("stats", "the cursor hidden", covered);
    app.ok("text x");
    assert!(!covered(&app.stats()), "the cover stayed over what was typed");
    app.wait_screen("the typed character", |s| s.contains("% x"));
    let cell = cell_at(&mut app);
    app.wait_on("stats", "the cursor hidden after the x", covered);
    image(&mut app, "blink-after-x.png", cell, BG);

    // On a character the cursor cannot be hidden by a patch of background:
    // there the blink draws its frames.
    app.ok("key left");
    app.wait_on("panes", "the cursor on the x", |r| {
        let pane = &r.get("windows").arr()[0].get("panes").arr()[0];
        pane.get("cursor").get("col").int() == 2
    });
    std::thread::sleep(Duration::from_millis(700));
    let before = ["blinks", "draws"].map(|what| count(&mut app, what));
    let begun = Instant::now();
    while begun.elapsed() < Duration::from_millis(2500) {
        assert!(!covered(&app.stats()), "a cover over a character");
        std::thread::sleep(Duration::from_millis(50));
    }
    let after = ["blinks", "draws"].map(|what| count(&mut app, what));
    assert!(after[0] - before[0] >= 3, "{before:?} {after:?}");
    assert_eq!(
        after[1] - before[1],
        after[0] - before[0],
        "a frame for each blink"
    );
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn an_idle_window_uses_no_cpu() {
    let mut app = App::launch("idle", &[]);
    app.wait_prompt();
    std::thread::sleep(Duration::from_secs(5));
    let before = app.stats();
    std::thread::sleep(Duration::from_secs(5));
    let after = app.stats();
    let cpu_ms = (after.get("cpu_us").int() - before.get("cpu_us").int()) as f64 / 1000.0;
    let frames = |s: &Json| s.get("windows").arr()[0].get("frames").int();
    assert_eq!(frames(&after), frames(&before), "frames while idle");
    let link = after.get("windows").arr()[0].get("link_running").bool();
    assert!(!link, "the display link is paused");
    // What is left is answering the requests and the odd event from the
    // desktop, such as the pointer passing over the window.
    assert!(cpu_ms < 75.0, "{cpu_ms} ms of CPU in 5 s");
    eprintln!("idle: {cpu_ms:.1} ms of CPU in 5 s");
    app.quit();
}

#[test]
#[ignore = "opens windows"]
fn frames_keep_coming_during_a_flood() {
    // 24 MB of numbered lines, written eight times by cat as fast as the
    // terminal reads.
    let file = test_dir().join("flood.txt");
    let line = "The quick brown fox jumps over the lazy dog; 0123456789 abcdefghij.\n";
    let mut text = String::with_capacity(24 << 20);
    let mut n = 0;
    while text.len() < 24 << 20 {
        text.push_str(&format!("{n:08} {line}"));
        n += 1;
    }
    std::fs::write(&file, &text).expect("flood file");
    let mut app = App::launch(
        "fl",
        &[
            "--quit-after-last-window-closed=true",
            "-e",
            "/bin/sh",
            "-c",
            &format!(
                "for i in 1 2 3 4 5 6 7 8; do cat '{}'; done",
                file.display()
            ),
        ],
    );
    let start = Instant::now();
    let mut samples = Vec::new();
    // Until cat is done, its window closes, and the app quits.
    while let Some(reply) = app.try_send("stats") {
        if let Some(w) = reply.get("windows").arr().first() {
            samples.push((
                start.elapsed(),
                w.get("frames").int(),
                w.get("total_us").clone(),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
        // Generous: the point is that frames keep coming, and this machine
        // is often heavily loaded.
        assert!(
            start.elapsed() < Duration::from_secs(90),
            "the flood never ended"
        );
    }
    let took = start.elapsed();
    assert!(app.wait_exit().success());
    let first = samples
        .iter()
        .find(|s| s.1 > 0)
        .expect("a frame was presented");
    let last = samples.last().expect("samples");
    let frames = last.1 - first.1;
    let span = (last.0 - first.0).as_secs_f64();
    eprintln!(
        "flood: {:.1} MB in {:.2} s, {frames} frames in {span:.2} s ({:.0} per second), frame time {:?}",
        8.0 * text.len() as f64 / 1e6,
        took.as_secs_f64(),
        frames as f64 / span.max(0.001),
        last.2
    );
    // Frames came all along, not only at the end.
    let rising = samples.windows(2).filter(|w| w[1].1 > w[0].1).count();
    assert!(rising >= samples.len() / 3, "frames stalled: {samples:?}");
    assert!(
        frames as f64 / span.max(0.001) > 20.0,
        "{frames} frames in {span} s"
    );
}

#[test]
#[ignore = "opens windows"]
fn vim_and_less_leave_the_configured_blinking_alone() {
    let mut app = App::launch_with("vb", "cursor-style-blink = true\n", &[]);
    app.wait_prompt();
    let cursor_blinks = |r: &Json| {
        let pane = &r.get("windows").arr()[0].get("panes").arr()[0];
        matches!(pane.get("cursor"), Json::Obj(c) if c.get("blinking") == Some(&Json::Bool(true)))
    };
    let foreground = |name: &'static str| {
        move |r: &Json| {
            r.get("windows").arr()[0].get("panes").arr()[0].get("foreground")
                == &Json::Str(name.into())
        }
    };
    app.wait_on("panes", "a blinking cursor", cursor_blinks);
    // Both reset mode 12 on the way out (terminfo's cnorm).
    for (program, quit) in [
        ("/usr/bin/vim -u NONE", ":q\r"),
        ("/usr/bin/less /etc/hosts", "q"),
    ] {
        app.type_line(program);
        let name = if program.contains("vim") {
            "vim"
        } else {
            "less"
        };
        app.wait_on("panes", name, foreground(name));
        app.ok(&format!("text {}", escape(quit)));
        app.wait_prompt();
        app.wait_on("panes", "the shell again", |r| {
            r.get("windows").arr()[0].get("panes").arr()[0].get("foreground") == &Json::Null
        });
        let panes = app.ok("panes");
        assert!(cursor_blinks(&panes), "after {name}: {panes:?}");
    }
    app.quit();
}
